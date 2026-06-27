use crate::config::{DetectionProfile, ScanSettings};
use crate::dsp::{u16le_to_f32_real, Complex32};
use crate::peak::Peak;
use crate::sdr::{scan_sdr_peaks, AirspyGainSettings, AirspySettings, IqBackend, SdrScanConfig};
use anyhow::{anyhow, Context, Result};
use rs_spy::{Airspy, IqConverter, RECOMMENDED_BUFFER_SIZE};
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
    device: Airspy,
    settings: AirspySettings,
}

impl AirspyScanner {
    pub fn open(settings: AirspySettings) -> Result<Self> {
        let device = Airspy::open_by_index(settings.device_index).with_context(|| {
            format!(
                "failed to open Airspy device index {}",
                settings.device_index
            )
        })?;
        configure_device(&device, &settings)?;
        Ok(Self { device, settings })
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
        self.device.set_freq(center_hz)?;
        Ok(center_hz as f64)
    }

    fn collect_iq(&mut self) -> Result<Vec<Complex32>> {
        collect_iq(&self.device, &self.settings)
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
    let rates = device.supported_sample_rates()?;
    if !rates.is_empty() && !rates.contains(&settings.sample_rate_hz) {
        // Some firmware accepts fallback rates even when they are not advertised.
    }

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

fn collect_iq(device: &Airspy, settings: &AirspySettings) -> Result<Vec<Complex32>> {
    let duration = Duration::from_millis(settings.dwell_ms);
    let deadline = Instant::now() + duration;
    let mut raw = vec![0u8; RECOMMENDED_BUFFER_SIZE];
    let mut converter = IqConverter::new();
    let mut iq = Vec::new();
    let mut chunks_to_discard = 2usize;

    device.start_rx()?;
    let result = (|| -> Result<Vec<Complex32>> {
        while Instant::now() < deadline {
            let n = device.read_sync_timeout(&mut raw, Duration::from_millis(1500))?;
            if n == 0 {
                continue;
            }
            if chunks_to_discard > 0 {
                chunks_to_discard -= 1;
                continue;
            }
            let mut real = u16le_to_f32_real(&raw[..n]);
            iq.extend(converter.process_to_complex(&mut real));
        }
        Ok(iq)
    })();
    let stop_result = device.stop_rx();

    match (result, stop_result) {
        (Ok(iq), Ok(())) => Ok(iq),
        (Err(err), _) => Err(err),
        (Ok(_), Err(err)) => Err(err.into()),
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
