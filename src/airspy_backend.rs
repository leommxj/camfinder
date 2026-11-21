use crate::config::{DetectionProfile, ScanSettings};
use crate::dsp::Complex32;
use crate::peak::Peak;
use crate::sdr::{
    scan_sdr_peaks, AirspyGainSettings, AirspySettings, CaptureStatistics, IqBackend, IqCapture,
    SdrScanConfig,
};
use anyhow::{anyhow, Context, Result};
use rs_spy::{Airspy, IqConverter, RECOMMENDED_BUFFER_SIZE};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct AirspyDeviceInfo {
    pub index: usize,
    pub label: String,
}

pub fn list_airspy_devices() -> Result<Vec<AirspyDeviceInfo>> {
    let labels = Airspy::list_devices().context("failed to enumerate Airspy devices")?;
    Ok(labels
        .into_iter()
        .enumerate()
        .map(|(index, label)| AirspyDeviceInfo { index, label })
        .collect())
}

pub struct AirspyScanner {
    device: Option<Airspy>,
    settings: AirspySettings,
}

impl AirspyScanner {
    pub fn open(settings: AirspySettings) -> Result<Self> {
        settings.validate()?;
        let device = Airspy::open_by_index(settings.device_index).with_context(|| {
            format!(
                "failed to open Airspy device index {}",
                settings.device_index
            )
        })?;
        configure_device(&device, &settings)?;
        Ok(Self {
            device: Some(device),
            settings,
        })
    }

    pub fn scan_peaks(
        &mut self,
        profiles: &[DetectionProfile],
        scan: &ScanSettings,
    ) -> Result<Vec<Peak>> {
        self.scan_peaks_logged(profiles, scan, &mut |_| {})
    }

    pub fn scan_peaks_logged(
        &mut self,
        profiles: &[DetectionProfile],
        scan: &ScanSettings,
        log: &mut dyn FnMut(String),
    ) -> Result<Vec<Peak>> {
        scan_sdr_peaks(self, profiles, scan, log)
    }
}

impl IqBackend for AirspyScanner {
    fn source_name(&self) -> &str {
        "airspy"
    }

    fn scan_config(&self) -> SdrScanConfig {
        self.settings.scan_config()
    }

    fn tune(&mut self, center_hz: f64) -> Result<f64> {
        let center_hz = center_hz.round() as u32;
        self.device
            .as_ref()
            .ok_or_else(|| anyhow!("Airspy device unavailable after stream failure"))?
            .set_freq(center_hz)?;
        Ok(center_hz as f64)
    }

    fn collect_iq(&mut self) -> Result<Vec<Complex32>> {
        Ok(self.capture(&Arc::new(AtomicBool::new(false)))?.iq)
    }

    fn settle(&mut self, _stop: &Arc<AtomicBool>) -> Result<()> {
        Ok(())
    }

    fn capture(&mut self, stop: &Arc<AtomicBool>) -> Result<IqCapture> {
        let device = self
            .device
            .take()
            .ok_or_else(|| anyhow!("Airspy device unavailable after stream failure"))?;
        let (device, result) = collect_iq(device, &self.settings, stop);
        self.device = device;
        result
    }

    fn gain_label(&self) -> Option<String> {
        Some(gain_label(&self.settings.gain))
    }
}

pub fn scan_airspy_peaks(
    profiles: &[DetectionProfile],
    scan: &ScanSettings,
    settings: &AirspySettings,
) -> Result<Vec<Peak>> {
    let mut scanner = AirspyScanner::open(settings.clone())?;
    scanner.scan_peaks(profiles, scan)
}

fn configure_device(device: &Airspy, settings: &AirspySettings) -> Result<()> {
    device
        .set_sample_rate_for_iq(settings.sample_rate_hz)
        .with_context(|| format!("failed to set sample rate {}", settings.sample_rate_hz))?;
    device.set_packing(false)?;
    apply_gain(device, &settings.gain)?;
    Ok(())
}

fn apply_gain(device: &Airspy, gain: &AirspyGainSettings) -> Result<()> {
    match gain.mode.as_str() {
        "manual" => {
            device.set_lna_agc(gain.lna_agc)?;
            device.set_mixer_agc(gain.mixer_agc)?;
            if !gain.lna_agc {
                device.set_lna_gain(gain.lna)?;
            }
            if !gain.mixer_agc {
                device.set_mixer_gain(gain.mixer)?;
            }
            device.set_vga_gain(gain.vga)?;
        }
        "sensitivity" => {
            device.set_sensitivity_gain(gain.preset)?;
        }
        "linearity" => {
            device.set_linearity_gain(gain.preset)?;
        }
        "agc" => {
            device.set_lna_agc(true)?;
            device.set_mixer_agc(true)?;
            device.set_vga_gain(gain.vga)?;
        }
        other => {
            return Err(anyhow!(
                "unknown Airspy gain mode '{other}', expected manual/sensitivity/linearity/agc"
            ));
        }
    }
    Ok(())
}

fn collect_iq(
    device: Airspy,
    settings: &AirspySettings,
    stop: &Arc<AtomicBool>,
) -> (Option<Airspy>, Result<IqCapture>) {
    if let Err(error) = device.start_rx() {
        let stopped = device.stop_rx();
        return (stopped.ok().map(|_| device), Err(error.into()));
    }
    let reader = match device.into_multi_transfer_reader(4, RECOMMENDED_BUFFER_SIZE) {
        Ok(reader) => reader,
        Err(error) => return (None, Err(error.into())),
    };
    let started = Instant::now();
    let settle_deadline = started + Duration::from_millis(settings.settle_ms);
    let timeout_ms = if settings.fft_frames.is_some() {
        settings.capture_timeout_ms
    } else {
        settings.capture_timeout_ms.max(
            settings
                .settle_ms
                .saturating_add(settings.dwell_ms)
                .saturating_add(250),
        )
    };
    let timeout = Duration::from_millis(timeout_ms);
    let deadline = started + timeout;
    let dwell_deadline = settle_deadline + Duration::from_millis(settings.dwell_ms);
    let target_samples = settings
        .fft_frames
        .map(|frames| frames.saturating_mul(settings.fft_size));
    let mut statistics = CaptureStatistics::default();
    let result = (|| -> Result<Vec<Complex32>> {
        let mut converter = IqConverter::new();
        let mut real = Vec::with_capacity(RECOMMENDED_BUFFER_SIZE / 2);
        let mut iq = Vec::with_capacity(target_samples.unwrap_or(settings.fft_size));
        let mut warmup_chunks = 2;
        let settle_samples = settings.sample_rate_hz as u128 * settings.settle_ms as u128 / 1000;
        let mut settled_samples = 0u128;
        let mut last_data = Instant::now();
        loop {
            if stop.load(Ordering::Relaxed) {
                statistics.interrupted = true;
                break;
            }
            if target_samples.is_some_and(|target| iq.len() >= target) {
                break;
            }
            if target_samples.is_none() && Instant::now() >= dwell_deadline {
                break;
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "Airspy capture timed out after {} ms ({} IQ samples, {} raw bytes)",
                    timeout_ms,
                    iq.len(),
                    statistics.raw_bytes
                ));
            }
            let Some(raw) = reader.recv_timeout(Duration::from_millis(20))? else {
                if last_data.elapsed() >= Duration::from_millis(250) {
                    statistics.timeouts += 1;
                    return Err(anyhow!("Airspy USB receive timed out"));
                }
                continue;
            };
            last_data = Instant::now();
            statistics.raw_bytes += raw.len() as u64;
            if raw.len() % 4 != 0 {
                return Err(anyhow!(
                    "Airspy returned an incomplete IQ transfer ({} bytes)",
                    raw.len()
                ));
            }
            let conversion_started = Instant::now();
            real.clear();
            real.extend(
                raw.chunks_exact(2)
                    .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32768.0),
            );
            converter.process(&mut real);
            statistics.conversion_ms += conversion_started.elapsed().as_secs_f64() * 1000.0;
            let converted_len = real.len() / 2;
            if Instant::now() < settle_deadline || settled_samples < settle_samples {
                settled_samples += converted_len as u128;
                statistics.discarded_iq_samples += converted_len;
                continue;
            }
            if warmup_chunks > 0 {
                warmup_chunks -= 1;
                statistics.discarded_iq_samples += converted_len;
                continue;
            }
            if iq.is_empty() {
                statistics.settle_ms =
                    conversion_started.duration_since(started).as_secs_f64() * 1000.0;
            }
            let count = target_samples
                .map(|target| target.saturating_sub(iq.len()).min(converted_len))
                .unwrap_or(converted_len);
            statistics.discarded_iq_samples += converted_len - count;
            let complex_started = Instant::now();
            iq.extend(
                real.chunks_exact(2)
                    .take(count)
                    .map(|pair| Complex32::new(pair[0], pair[1])),
            );
            statistics.conversion_ms += complex_started.elapsed().as_secs_f64() * 1000.0;
        }
        Ok(iq)
    })();
    statistics.capture_ms = started.elapsed().as_secs_f64() * 1000.0;
    let reclaimed = reader.stop_and_reclaim();
    match reclaimed {
        Ok(device) => (Some(device), result.map(|iq| IqCapture { iq, statistics })),
        Err(error) => (
            None,
            Err(error)
                .context("Airspy async stream failed while stopping or reclaiming the device"),
        ),
    }
}

fn gain_label(gain: &AirspyGainSettings) -> String {
    match gain.mode.as_str() {
        "manual" => format!(
            "manual(lna={},mixer={},vga={},lna_agc={},mixer_agc={})",
            gain.lna, gain.mixer, gain.vga, gain.lna_agc, gain.mixer_agc
        ),
        "sensitivity" | "linearity" => format!("{}({})", gain.mode, gain.preset),
        "agc" => format!("agc(vga={})", gain.vga),
        other => other.to_string(),
    }
}
