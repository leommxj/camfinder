use crate::config::{DetectionProfile, RecordMode, ScanSettings, DEFAULT_CONFIG_TOML};
use crate::dsp::{Complex32, PeakCandidate, SpectrumAnalyzer, SpectrumSettings};
use crate::frequency::{format_frequency_hz, MHZ};
use crate::peak::{Peak, SpectrumBand};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceSettings {
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub peaks_file: PeaksFileSettings,
    #[serde(default)]
    pub airspy: AirspySettings,
    #[serde(default)]
    pub soapy: SoapySettings,
    #[serde(default)]
    pub replay: ReplaySettings,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplaySettings {
    #[serde(default)]
    pub manifest: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PeaksFileSettings {
    #[serde(default)]
    pub path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
pub struct SdrScanConfig {
    pub sample_rate_hz: f64,
    pub dwell_ms: u64,
    pub fft_size: usize,
    pub dc_notch_hz: f64,
    pub settle_ms: u64,
}

pub trait IqBackend {
    fn source_name(&self) -> &str;
    fn scan_config(&self) -> SdrScanConfig;
    fn tune(&mut self, center_hz: f64) -> Result<f64>;
    fn collect_iq(&mut self) -> Result<Vec<Complex32>>;

    fn capture(&mut self, _stop: &Arc<AtomicBool>) -> Result<IqCapture> {
        let started = Instant::now();
        let iq = self.collect_iq()?;
        Ok(IqCapture {
            statistics: CaptureStatistics {
                capture_ms: started.elapsed().as_secs_f64() * 1000.0,
                ..Default::default()
            },
            iq,
        })
    }

    fn settle(&mut self, stop: &Arc<AtomicBool>) -> Result<()> {
        interruptible_sleep(Duration::from_millis(self.scan_config().settle_ms), stop);
        Ok(())
    }

    fn gain_label(&self) -> Option<String> {
        None
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CaptureStatistics {
    pub capture_ms: f64,
    pub settle_ms: f64,
    pub conversion_ms: f64,
    pub raw_bytes: u64,
    pub timeouts: u64,
    pub overflows: u64,
    pub discarded_iq_samples: usize,
    pub interrupted: bool,
}

pub struct IqCapture {
    pub iq: Vec<Complex32>,
    pub statistics: CaptureStatistics,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TargetStatistics {
    pub source: String,
    pub target_hz: f64,
    pub actual_center_hz: f64,
    pub tune_ms: f64,
    pub settle_ms: f64,
    pub conversion_ms: f64,
    pub iq_samples: usize,
    pub fft_frames: usize,
    pub capture_ms: f64,
    pub analysis_ms: f64,
    pub queue_wait_ms: f64,
    pub raw_bytes: u64,
    pub timeouts: u64,
    pub overflows: u64,
    pub discarded_iq_samples: usize,
    pub interrupted: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SdrScanOutput {
    pub peaks: Vec<Peak>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spectrum_bands: Vec<SpectrumBand>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statistics: Vec<TargetStatistics>,
}

pub fn scan_sdr_peaks<B: IqBackend + ?Sized>(
    backend: &mut B,
    profiles: &[DetectionProfile],
    scan: &ScanSettings,
    log: &mut dyn FnMut(String),
) -> Result<Vec<Peak>> {
    Ok(scan_sdr(backend, profiles, scan, log)?.peaks)
}

pub fn scan_sdr<B: IqBackend + ?Sized>(
    backend: &mut B,
    profiles: &[DetectionProfile],
    scan: &ScanSettings,
    log: &mut dyn FnMut(String),
) -> Result<SdrScanOutput> {
    SdrPipeline::new().scan(
        backend,
        profiles,
        scan,
        &Arc::new(AtomicBool::new(false)),
        log,
    )
}

pub struct SdrPipeline {
    sender: Option<mpsc::SyncSender<AnalysisTask>>,
    receiver: mpsc::Receiver<Result<AnalysisResult>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Default for SdrPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl SdrPipeline {
    pub fn new() -> Self {
        let (sender, tasks) = mpsc::sync_channel::<AnalysisTask>(1);
        let (results, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut analyzer = SpectrumAnalyzer::new();
            for task in tasks {
                let started = Instant::now();
                let result = analyzer
                    .analyze(&task.iq, task.center_hz, task.target_hz, &task.settings)
                    .map(|analysis| {
                        let mut statistics = task.statistics;
                        statistics.analysis_ms = started.elapsed().as_secs_f64() * 1000.0;
                        AnalysisResult {
                            sequence: task.sequence,
                            source_name: task.source_name,
                            target_hz: task.target_hz,
                            candidates: analysis.candidates,
                            band: analysis.band,
                            statistics,
                        }
                    });
                if results.send(result).is_err() {
                    break;
                }
            }
        });
        Self {
            sender: Some(sender),
            receiver,
            worker: Some(worker),
        }
    }

    pub fn scan<B: IqBackend + ?Sized>(
        &mut self,
        backend: &mut B,
        profiles: &[DetectionProfile],
        scan: &ScanSettings,
        stop: &Arc<AtomicBool>,
        log: &mut dyn FnMut(String),
    ) -> Result<SdrScanOutput> {
        self.scan_with_progress(backend, profiles, scan, stop, log, &mut |_| {})
    }

    pub fn scan_with_progress<B: IqBackend + ?Sized>(
        &mut self,
        backend: &mut B,
        profiles: &[DetectionProfile],
        scan: &ScanSettings,
        stop: &Arc<AtomicBool>,
        log: &mut dyn FnMut(String),
        progress: &mut dyn FnMut(&TargetStatistics),
    ) -> Result<SdrScanOutput> {
        let mut batches = Vec::new();
        let mut pending = 0usize;
        let capture_result = (|| -> Result<()> {
            for (sequence, target_hz) in unique_target_freqs_hz(profiles).into_iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let wait_started = Instant::now();
                while pending >= 2 {
                    pending -= 1;
                    batches.push(self.receive(log, progress)?);
                }
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let queue_wait_ms = wait_started.elapsed().as_secs_f64() * 1000.0;
                let source_name = backend.source_name().to_string();
                let settings = backend.scan_config();
                let gain = backend
                    .gain_label()
                    .map(|value| format!(" gain={value}"))
                    .unwrap_or_default();
                log(format!(
                    "{source_name} scan: tune={} dwell={} ms fft={}{}",
                    format_frequency_hz(target_hz),
                    settings.dwell_ms,
                    settings.fft_size,
                    gain
                ));
                let tune_started = Instant::now();
                let center_hz = backend.tune(target_hz.round()).with_context(|| {
                    format!("failed to tune {source_name} to {target_hz:.0} Hz")
                })?;
                let tune_ms = tune_started.elapsed().as_secs_f64() * 1000.0;
                let settle_started = Instant::now();
                backend.settle(stop)?;
                let settle_ms = settle_started.elapsed().as_secs_f64() * 1000.0;
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let capture = backend.capture(stop).with_context(|| {
                    format!(
                        "failed while collecting {source_name} samples at {}",
                        format_frequency_hz(target_hz)
                    )
                })?;
                if capture.statistics.interrupted || stop.load(Ordering::Relaxed) {
                    break;
                }
                let stats = capture.statistics;
                let statistics = TargetStatistics {
                    source: source_name.clone(),
                    target_hz,
                    actual_center_hz: center_hz,
                    tune_ms,
                    settle_ms: settle_ms + stats.settle_ms,
                    conversion_ms: stats.conversion_ms,
                    iq_samples: capture.iq.len(),
                    queue_wait_ms,
                    fft_frames: capture.iq.len() / settings.fft_size.max(1),
                    capture_ms: stats.capture_ms,
                    raw_bytes: stats.raw_bytes,
                    timeouts: stats.timeouts,
                    overflows: stats.overflows,
                    discarded_iq_samples: stats.discarded_iq_samples,
                    interrupted: stats.interrupted,
                    ..Default::default()
                };
                let record = scan.record_path.is_some() && scan.record_mode != RecordMode::Peaks;
                let settings = SpectrumSettings {
                    sample_rate_hz: settings.sample_rate_hz,
                    fft_size: settings.fft_size,
                    search_window_hz: scan.peak_search_window_hz,
                    dc_notch_hz: settings.dc_notch_hz,
                    min_peak_snr_db: scan.min_peak_snr_db,
                    max_peaks: scan.max_peaks_per_freq,
                    spectrum_window_bins: if record && scan.record_mode == RecordMode::Spectrum {
                        scan.record_spectrum_bins
                    } else {
                        0
                    },
                    band_decimation: if !record {
                        0
                    } else if scan.record_mode == RecordMode::Summary {
                        usize::MAX
                    } else {
                        scan.record_band_decimation.max(1)
                    },
                };
                self.sender
                    .as_ref()
                    .ok_or_else(|| anyhow!("SDR analysis worker stopped"))?
                    .send(AnalysisTask {
                        sequence,
                        source_name,
                        target_hz,
                        center_hz,
                        iq: capture.iq,
                        settings,
                        statistics,
                    })
                    .map_err(|_| anyhow!("SDR analysis worker stopped"))?;
                pending += 1;
            }
            Ok(())
        })();
        let mut analysis_error = None;
        for _ in 0..pending {
            match self.receive(log, progress) {
                Ok(result) => batches.push(result),
                Err(error) => {
                    analysis_error.get_or_insert(error);
                }
            }
        }
        capture_result?;
        if let Some(error) = analysis_error {
            return Err(error);
        }
        batches.sort_by_key(|batch| batch.sequence);
        let mut output = SdrScanOutput::default();
        for batch in batches {
            if let Some(mut band) = batch.band {
                band.source = Some(batch.source_name.clone());
                output.spectrum_bands.push(band);
            }
            for candidate in batch.candidates {
                output
                    .peaks
                    .push(candidate.into_peak(output.peaks.len() + 1, batch.source_name.clone()));
            }
            output.statistics.push(batch.statistics);
        }
        Ok(output)
    }

    fn receive(
        &self,
        log: &mut dyn FnMut(String),
        progress: &mut dyn FnMut(&TargetStatistics),
    ) -> Result<AnalysisResult> {
        let result = self
            .receiver
            .recv()
            .map_err(|_| anyhow!("SDR analysis worker stopped before returning results"))??;
        log(format!(
            "{} peaks: target={} count={} samples={} frames={} capture={:.1} ms analysis={:.1} ms",
            result.source_name,
            format_frequency_hz(result.target_hz),
            result.candidates.len(),
            result.statistics.iq_samples,
            result.statistics.fft_frames,
            result.statistics.capture_ms,
            result.statistics.analysis_ms
        ));
        progress(&result.statistics);
        Ok(result)
    }
}

impl Drop for SdrPipeline {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct AnalysisTask {
    sequence: usize,
    source_name: String,
    target_hz: f64,
    center_hz: f64,
    iq: Vec<Complex32>,
    settings: SpectrumSettings,
    statistics: TargetStatistics,
}

struct AnalysisResult {
    sequence: usize,
    source_name: String,
    target_hz: f64,
    candidates: Vec<PeakCandidate>,
    band: Option<SpectrumBand>,
    statistics: TargetStatistics,
}

pub fn interruptible_sleep(duration: Duration, stop: &Arc<AtomicBool>) {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AirspySettings {
    #[serde(default)]
    pub device_index: usize,
    #[serde(default = "default_airspy_sample_rate")]
    pub sample_rate_hz: u32,
    #[serde(default = "default_dwell_ms")]
    pub dwell_ms: u64,
    #[serde(default)]
    pub fft_frames: Option<usize>,
    #[serde(default = "default_capture_timeout_ms")]
    pub capture_timeout_ms: u64,
    #[serde(default = "default_fft_size")]
    pub fft_size: usize,
    #[serde(default = "default_dc_notch_hz")]
    pub dc_notch_hz: f64,
    #[serde(default = "default_settle_ms")]
    pub settle_ms: u64,
    #[serde(default)]
    pub gain: AirspyGainSettings,
}

impl Default for AirspySettings {
    fn default() -> Self {
        default_airspy_settings()
    }
}

impl AirspySettings {
    pub fn validate(&self) -> Result<()> {
        validate_capture_budget(
            self.sample_rate_hz,
            self.fft_size,
            self.fft_frames,
            self.capture_timeout_ms,
        )
    }

    pub fn scan_config(&self) -> SdrScanConfig {
        SdrScanConfig {
            sample_rate_hz: self.sample_rate_hz as f64,
            dwell_ms: self.dwell_ms,
            fft_size: self.fft_size,
            dc_notch_hz: self.dc_notch_hz,
            settle_ms: self.settle_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AirspyGainSettings {
    #[serde(default = "default_gain_mode")]
    pub mode: String,
    #[serde(default = "default_gain_preset")]
    pub preset: u8,
    #[serde(default = "default_lna_gain")]
    pub lna: u8,
    #[serde(default = "default_mixer_gain")]
    pub mixer: u8,
    #[serde(default = "default_vga_gain")]
    pub vga: u8,
    #[serde(default)]
    pub lna_agc: bool,
    #[serde(default)]
    pub mixer_agc: bool,
}

impl Default for AirspyGainSettings {
    fn default() -> Self {
        default_airspy_gain_settings()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoapySettings {
    #[serde(default)]
    pub device_args: String,
    #[serde(default)]
    pub device_index: usize,
    #[serde(default)]
    pub channel: usize,
    #[serde(default = "default_soapy_sample_rate")]
    pub sample_rate_hz: u32,
    #[serde(default = "default_soapy_dwell_ms")]
    pub dwell_ms: u64,
    #[serde(default)]
    pub fft_frames: Option<usize>,
    #[serde(default = "default_capture_timeout_ms")]
    pub capture_timeout_ms: u64,
    #[serde(default = "default_soapy_fft_size")]
    pub fft_size: usize,
    #[serde(default = "default_soapy_dc_notch_hz")]
    pub dc_notch_hz: f64,
    #[serde(default = "default_soapy_settle_ms")]
    pub settle_ms: u64,
    #[serde(default)]
    pub antenna: Option<String>,
    #[serde(default)]
    pub bandwidth_hz: Option<f64>,
    #[serde(default)]
    pub gain: SoapyGainSettings,
}

impl Default for SoapySettings {
    fn default() -> Self {
        default_soapy_settings()
    }
}

impl SoapySettings {
    pub fn validate(&self) -> Result<()> {
        validate_capture_budget(
            self.sample_rate_hz,
            self.fft_size,
            self.fft_frames,
            self.capture_timeout_ms,
        )
    }

    pub fn scan_config(&self, sample_rate_hz: f64) -> SdrScanConfig {
        SdrScanConfig {
            sample_rate_hz,
            dwell_ms: self.dwell_ms,
            fft_size: self.fft_size,
            dc_notch_hz: self.dc_notch_hz,
            settle_ms: self.settle_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoapyGainSettings {
    #[serde(default = "default_soapy_gain_mode")]
    pub mode: String,
    #[serde(default)]
    pub gain_db: Option<f64>,
    #[serde(default)]
    pub elements: Vec<SoapyGainElement>,
}

impl Default for SoapyGainSettings {
    fn default() -> Self {
        default_soapy_gain_settings()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoapyGainElement {
    pub name: String,
    pub gain_db: f64,
}

fn validate_capture_budget(
    sample_rate_hz: u32,
    fft_size: usize,
    fft_frames: Option<usize>,
    timeout_ms: u64,
) -> Result<()> {
    anyhow::ensure!(sample_rate_hz > 0, "sample_rate_hz must be positive");
    anyhow::ensure!(
        fft_size > 0 && fft_size.is_power_of_two() && fft_size <= 1_048_576,
        "fft_size must be a power of two between 1 and 1048576"
    );
    anyhow::ensure!(timeout_ms > 0, "capture_timeout_ms must be positive");
    if let Some(frames) = fft_frames {
        anyhow::ensure!(frames > 0, "fft_frames must be positive");
        let samples = frames
            .checked_mul(fft_size)
            .ok_or_else(|| anyhow!("FFT capture budget is too large"))?;
        anyhow::ensure!(
            samples <= 16_777_216,
            "FFT capture budget exceeds 16777216 IQ samples"
        );
        anyhow::ensure!(
            samples as f64 / sample_rate_hz as f64 * 1000.0 < timeout_ms as f64,
            "capture_timeout_ms must exceed the FFT sample budget duration"
        );
    }
    Ok(())
}

fn default_capture_timeout_ms() -> u64 {
    3000
}

fn default_airspy_sample_rate() -> u32 {
    default_airspy_settings().sample_rate_hz
}

fn default_soapy_sample_rate() -> u32 {
    default_soapy_settings().sample_rate_hz
}

fn default_soapy_dwell_ms() -> u64 {
    default_soapy_settings().dwell_ms
}

fn default_soapy_fft_size() -> usize {
    default_soapy_settings().fft_size
}

fn default_soapy_dc_notch_hz() -> f64 {
    default_soapy_settings().dc_notch_hz
}

fn default_soapy_settle_ms() -> u64 {
    default_soapy_settings().settle_ms
}

fn default_dwell_ms() -> u64 {
    default_airspy_settings().dwell_ms
}

fn default_fft_size() -> usize {
    default_airspy_settings().fft_size
}

fn default_dc_notch_hz() -> f64 {
    default_airspy_settings().dc_notch_hz
}

fn default_settle_ms() -> u64 {
    default_airspy_settings().settle_ms
}

fn default_gain_mode() -> String {
    default_airspy_gain_settings().mode
}

fn default_gain_preset() -> u8 {
    default_airspy_gain_settings().preset
}

fn default_lna_gain() -> u8 {
    default_airspy_gain_settings().lna
}

fn default_mixer_gain() -> u8 {
    default_airspy_gain_settings().mixer
}

fn default_vga_gain() -> u8 {
    default_airspy_gain_settings().vga
}

fn default_soapy_gain_mode() -> String {
    default_soapy_gain_settings().mode
}

fn default_device_settings() -> DeviceSettings {
    toml::from_str::<DeviceDefaults>(DEFAULT_CONFIG_TOML)
        .expect("embedded default device settings must parse")
        .device
}

fn default_airspy_settings() -> AirspySettings {
    default_device_settings().airspy
}

fn default_airspy_gain_settings() -> AirspyGainSettings {
    default_airspy_settings().gain
}

fn default_soapy_settings() -> SoapySettings {
    default_device_settings().soapy
}

fn default_soapy_gain_settings() -> SoapyGainSettings {
    default_soapy_settings().gain
}

#[derive(Deserialize)]
struct DeviceDefaults {
    device: DeviceSettings,
}

fn unique_target_freqs_hz(profiles: &[DetectionProfile]) -> Vec<f64> {
    let mut freqs = profiles
        .iter()
        .flat_map(|profile| profile.freqs_mhz.iter().map(|freq_mhz| freq_mhz * MHZ))
        .collect::<Vec<_>>();
    freqs.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    freqs.dedup_by(|left, right| (*left - *right).abs() < 1.0);
    freqs
}
