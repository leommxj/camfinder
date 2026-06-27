use crate::config::{DetectionProfile, ScanSettings, DEFAULT_CONFIG_TOML};
use crate::dsp::{analyze_spectrum_near, Complex32, PeakCandidate, SpectrumSettings};
use crate::frequency::{format_frequency_hz, MHZ};
use crate::peak::{Peak, SpectrumBand};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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

    fn gain_label(&self) -> Option<String> {
        None
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SdrScanOutput {
    pub peaks: Vec<Peak>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spectrum_bands: Vec<SpectrumBand>,
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
    let target_freqs_hz = unique_target_freqs_hz(profiles);
    if target_freqs_hz.is_empty() {
        return Ok(SdrScanOutput {
            peaks: Vec::new(),
            spectrum_bands: Vec::new(),
        });
    }

    let mut peaks = Vec::new();
    let mut spectrum_bands = Vec::new();
    let mut next_id = 1;
    let mut batches = Vec::new();

    thread::scope(|scope| -> Result<()> {
        let (task_sender, task_receiver) = mpsc::channel::<AnalysisTask>();
        let (result_sender, result_receiver) = mpsc::channel::<Result<AnalysisResult>>();
        let worker = scope.spawn(move || {
            for task in task_receiver {
                let result = analyze_spectrum_near(
                    &task.iq,
                    task.center_hz,
                    task.target_hz,
                    &task.spectrum_settings,
                )
                .map(|analysis| AnalysisResult {
                    sequence: task.sequence,
                    source_name: task.source_name,
                    target_hz: task.target_hz,
                    candidates: analysis.candidates,
                    band: analysis.band,
                });

                if result_sender.send(result).is_err() {
                    break;
                }
            }
        });

        let mut expected_results = 0usize;
        for (sequence, target_hz) in target_freqs_hz.iter().copied().enumerate() {
            let source_name = backend.source_name().to_string();
            let settings = backend.scan_config();
            let center_hz = target_hz.round();
            let gain = backend
                .gain_label()
                .map(|gain| format!(" gain={gain}"))
                .unwrap_or_default();
            log(format!(
                "{source_name} scan: tune={} dwell={} ms fft={}{}",
                format_frequency_hz(target_hz),
                settings.dwell_ms,
                settings.fft_size,
                gain
            ));

            let actual_center_hz = backend
                .tune(center_hz)
                .with_context(|| format!("failed to tune {source_name} to {center_hz:.0} Hz"))?;
            if settings.settle_ms > 0 {
                thread::sleep(Duration::from_millis(settings.settle_ms));
            }

            let iq = backend.collect_iq().with_context(|| {
                format!(
                    "failed while collecting {source_name} samples at {}",
                    format_frequency_hz(target_hz)
                )
            })?;

            let spectrum_settings = SpectrumSettings {
                sample_rate_hz: settings.sample_rate_hz,
                fft_size: settings.fft_size,
                search_window_hz: scan.peak_search_window_hz,
                dc_notch_hz: settings.dc_notch_hz,
                min_peak_snr_db: scan.min_peak_snr_db,
                max_peaks: scan.max_peaks_per_freq,
                spectrum_window_bins: if scan.record_path.is_some() {
                    scan.record_spectrum_bins
                } else {
                    0
                },
                band_decimation: if scan.record_path.is_some() {
                    scan.record_band_decimation.max(1)
                } else {
                    0
                },
            };

            task_sender
                .send(AnalysisTask {
                    sequence,
                    source_name,
                    target_hz,
                    center_hz: actual_center_hz,
                    iq,
                    spectrum_settings,
                })
                .map_err(|_| anyhow!("SDR analysis worker stopped"))?;
            expected_results += 1;
        }
        drop(task_sender);

        for _ in 0..expected_results {
            let result = result_receiver
                .recv()
                .map_err(|_| anyhow!("SDR analysis worker stopped before returning results"))??;
            log(format!(
                "{} peaks: target={} count={}",
                result.source_name,
                format_frequency_hz(result.target_hz),
                result.candidates.len()
            ));
            batches.push(result);
        }

        worker
            .join()
            .map_err(|_| anyhow!("SDR analysis worker panicked"))?;
        Ok(())
    })?;

    batches.sort_by_key(|batch| batch.sequence);
    for mut batch in batches {
        if let Some(mut band) = batch.band.take() {
            band.source = Some(batch.source_name.clone());
            spectrum_bands.push(band);
        }
        for candidate in batch.candidates {
            peaks.push(candidate.into_peak(next_id, batch.source_name.clone()));
            next_id += 1;
        }
    }

    Ok(SdrScanOutput {
        peaks,
        spectrum_bands,
    })
}

struct AnalysisTask {
    sequence: usize,
    source_name: String,
    target_hz: f64,
    center_hz: f64,
    iq: Vec<Complex32>,
    spectrum_settings: SpectrumSettings,
}

struct AnalysisResult {
    sequence: usize,
    source_name: String,
    target_hz: f64,
    candidates: Vec<PeakCandidate>,
    band: Option<SpectrumBand>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AirspySettings {
    #[serde(default)]
    pub device_index: usize,
    #[serde(default = "default_airspy_sample_rate")]
    pub sample_rate_hz: u32,
    #[serde(default = "default_dwell_ms")]
    pub dwell_ms: u64,
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
