use super::{AppConfig, DetectionProfile, OffsetModel, ProfilePreset, RecordMode, SettingsStore};
use crate::engine::{EngineConfig, RunOptions};
use crate::frequency::{parse_frequency_hz, parse_frequency_list_mhz};
use crate::mask::{parse_mask_spec, MaskSet};
use crate::sdr::{AirspySettings, DeviceSettings, SoapyGainElement, SoapySettings};
use crate::synthetic::{SyntheticInterferer, SyntheticOptions};
use crate::timeparse::parse_duration;
use anyhow::{anyhow, ensure, Result};
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Synthetic,
    PeaksFile,
    IqFile,
    Airspy,
    Soapy,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            Self::PeaksFile => "peaks-file",
            Self::IqFile => "iq-file",
            Self::Airspy => "airspy",
            Self::Soapy => "soapy",
        }
    }
}
impl FromStr for Backend {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "synthetic" => Ok(Self::Synthetic),
            "peaks-file" => Ok(Self::PeaksFile),
            "iq-file" => Ok(Self::IqFile),
            "airspy" => Ok(Self::Airspy),
            "soapy" => Ok(Self::Soapy),
            _ => Err(anyhow!(
                "unknown backend '{value}', expected synthetic/peaks-file/iq-file/airspy/soapy"
            )),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScanRequest {
    pub preset: Option<ProfilePreset>,
    pub backend: Option<Backend>,
    pub peaks_file: Option<PathBuf>,
    pub iq_file: Option<PathBuf>,
    pub profile: Option<PathBuf>,
    pub profile_freqs: Option<String>,
    pub min_hits: Option<usize>,
    pub min_score: Option<f64>,
    pub search_window: Option<String>,
    pub match_window: Option<String>,
    pub offset_model: Option<OffsetModel>,
    pub min_peak_snr: Option<f64>,
    pub max_peaks: Option<usize>,
    pub record: Option<PathBuf>,
    pub record_mode: Option<RecordMode>,
    pub record_only: bool,
    pub record_spectrum_bins: Option<usize>,
    pub record_band_decimation: Option<usize>,
    pub output: Option<String>,
    pub duration: Option<String>,
    pub continuous: bool,
    pub once: bool,
    pub repeat_delay: Option<String>,
    pub confirm_passes: Option<usize>,
    pub confirm_ratio: Option<f64>,
    pub mask: Vec<String>,
    pub dwell_ms: Option<u64>,
    pub fft_frames: Option<usize>,
    pub capture_timeout_ms: Option<u64>,
    pub settle_ms: Option<u64>,
    pub synthetic_offset: Option<String>,
    pub synthetic_snr: Option<f64>,
    pub synthetic_floor: Option<f64>,
    pub synthetic_jitter: Option<String>,
    pub synthetic_missing: Vec<String>,
    pub interferer: Vec<String>,
    pub airspy_device_index: Option<usize>,
    pub airspy_sample_rate: Option<String>,
    pub airspy_dwell_ms: Option<u64>,
    pub airspy_fft_size: Option<usize>,
    pub airspy_min_peak_snr: Option<f64>,
    pub airspy_max_peaks: Option<usize>,
    pub airspy_dc_notch: Option<String>,
    pub airspy_settle_ms: Option<u64>,
    pub gain_mode: Option<String>,
    pub gain_preset: Option<u8>,
    pub lna: Option<u8>,
    pub mixer: Option<u8>,
    pub vga: Option<u8>,
    pub lna_agc: bool,
    pub mixer_agc: bool,
    pub soapy_device: Option<String>,
    pub soapy_device_index: Option<usize>,
    pub soapy_channel: Option<usize>,
    pub soapy_sample_rate: Option<String>,
    pub soapy_dwell_ms: Option<u64>,
    pub soapy_fft_size: Option<usize>,
    pub soapy_min_peak_snr: Option<f64>,
    pub soapy_max_peaks: Option<usize>,
    pub soapy_dc_notch: Option<String>,
    pub soapy_settle_ms: Option<u64>,
    pub soapy_antenna: Option<String>,
    pub soapy_bandwidth: Option<String>,
    pub soapy_gain_mode: Option<String>,
    pub soapy_gain: Option<f64>,
    pub soapy_gain_element: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ResolvedScan {
    pub engine: EngineConfig,
    pub run: RunOptions,
    pub output: String,
}

impl ScanRequest {
    pub fn resolve(&self, path: Option<&Path>) -> Result<ResolvedScan> {
        let config = match path {
            Some(path) => AppConfig::load(path)?,
            None => SettingsStore::default().load()?,
        };
        self.resolve_config(config)
    }

    pub fn resolve_config(&self, config: AppConfig) -> Result<ResolvedScan> {
        self.resolve_config_with_store(config, &SettingsStore::default())
    }

    pub(super) fn resolve_config_with_store(
        &self,
        mut config: AppConfig,
        store: &SettingsStore,
    ) -> Result<ResolvedScan> {
        ensure!(
            usize::from(self.profile.is_some())
                + usize::from(self.profile_freqs.is_some())
                + usize::from(self.preset.is_some())
                <= 1,
            "select only one of a profile file, preset or custom frequencies"
        );
        ensure!(
            !(self.once && self.continuous),
            "once and continuous cannot be combined"
        );
        ensure!(
            self.fft_frames.is_none()
                || (self.dwell_ms.is_none()
                    && self.airspy_dwell_ms.is_none()
                    && self.soapy_dwell_ms.is_none()),
            "fft_frames and dwell_ms cannot be combined"
        );
        let match_window = self
            .match_window
            .as_deref()
            .map(parse_frequency_hz)
            .transpose()?;
        let profiles = select_profiles(&config, self, match_window)?;
        apply_profile_settings(&mut config, &profiles)?;
        apply_scan_arg_overrides(&mut config, self)?;
        if config.scan.record && config.scan.record_path.is_none() {
            config.scan.record_path = Some(store.recording_path()?);
        }
        let backend = resolve_backend(&config, self)?;
        let device = active_device_settings(&config.device, self, backend)?;
        let masks = collect_masks(&config, self)?.masks().to_vec();
        let duration = self.duration.as_deref().map(parse_duration).transpose()?;
        let repeat_delay = self
            .repeat_delay
            .as_deref()
            .map(parse_duration)
            .transpose()?
            .unwrap_or_default();
        let output = self
            .output
            .clone()
            .unwrap_or_else(|| config.scan.output.clone());
        let defaults = EngineConfig::default();
        let engine = EngineConfig {
            profiles,
            device,
            scan: config.scan,
            masks,
            confirm_passes: self.confirm_passes.unwrap_or(defaults.confirm_passes),
            confirm_ratio: self.confirm_ratio.unwrap_or(defaults.confirm_ratio),
            record_only: self.record_only,
            synthetic: synthetic_options(self)?,
        };
        engine.validate()?;
        Ok(ResolvedScan {
            engine,
            run: RunOptions {
                repeat: should_repeat(self, backend),
                duration,
                repeat_delay,
            },
            output,
        })
    }
}

pub fn apply_profile_settings(config: &mut AppConfig, profiles: &[DetectionProfile]) -> Result<()> {
    let mut scans = profiles.iter().filter_map(|profile| profile.scan.as_ref());
    if let Some(scan) = scans.next() {
        ensure!(
            scans.all(|other| other == scan),
            "selected profiles have conflicting scan settings"
        );
        config.scan = scan.clone();
    }
    let mut devices = profiles
        .iter()
        .filter_map(|profile| profile.device.as_ref());
    if let Some(device) = devices.next() {
        ensure!(
            devices.all(|other| other == device),
            "selected profiles have conflicting device settings"
        );
        config.device = device.clone();
    }
    Ok(())
}

fn should_repeat(args: &ScanRequest, backend: Backend) -> bool {
    if args.once {
        false
    } else {
        args.continuous
            || args.duration.is_some()
            || matches!(backend, Backend::Airspy | Backend::Soapy)
    }
}

fn active_device_settings(
    base: &DeviceSettings,
    args: &ScanRequest,
    backend: Backend,
) -> Result<DeviceSettings> {
    let mut device = base.clone();
    device.backend = Some(backend.name().to_string());
    device.airspy = airspy_settings_from_args(&base.airspy, args)?;
    device.soapy = soapy_settings_from_args(&base.soapy, args)?;

    match backend {
        Backend::Synthetic => {}
        Backend::PeaksFile => {
            if let Some(path) = &args.peaks_file {
                device.peaks_file.path = Some(path.clone());
            }
        }
        Backend::IqFile => {
            if let Some(path) = &args.iq_file {
                device.replay.manifest = Some(path.clone());
            }
        }
        Backend::Airspy => {}
        Backend::Soapy => {}
    }

    Ok(device)
}

fn resolve_backend(config: &AppConfig, args: &ScanRequest) -> Result<Backend> {
    if let Some(backend) = args.backend {
        return Ok(backend);
    }
    let backend = config
        .device
        .backend
        .as_deref()
        .map(str::trim)
        .filter(|backend| !backend.is_empty())
        .ok_or_else(|| anyhow!("scan backend is required; set --backend or [device].backend"))?;

    backend.parse()
}

fn select_profiles(
    config: &AppConfig,
    args: &ScanRequest,
    match_window_hz: Option<f64>,
) -> Result<Vec<DetectionProfile>> {
    if let Some(freqs) = &args.profile_freqs {
        let freqs_mhz = parse_frequency_list_mhz(freqs)?;
        if freqs_mhz.is_empty() {
            return Err(anyhow!("--profile-freqs cannot be empty"));
        }
        let min_hits = args
            .min_hits
            .unwrap_or_else(|| ((freqs_mhz.len() as f64) * 0.67).ceil() as usize);
        return Ok(vec![DetectionProfile {
            name: "custom".to_string(),
            freqs_mhz,
            offset_model: args.offset_model.unwrap_or_default(),
            min_hits,
            min_score: args.min_score.unwrap_or(8.0),
            max_offset_spread_hz: match_window_hz.unwrap_or(1_200.0),
            groups: Vec::new(),
            scan: None,
            device: None,
        }]);
    }

    let selected = if let Some(path) = &args.profile {
        vec![DetectionProfile::load_with_defaults(
            path,
            &config.profile_defaults,
        )?]
    } else if let Some(preset) = args.preset {
        preset.profiles()?
    } else {
        config.selected_profiles()?
    };
    ensure!(
        !selected.is_empty(),
        "no profile configured; select a preset or a profile file"
    );
    Ok(selected
        .into_iter()
        .flat_map(|profile| {
            profile
                .with_overrides(
                    args.min_hits,
                    args.min_score,
                    match_window_hz,
                    args.offset_model,
                )
                .detection_profiles()
        })
        .collect())
}

fn apply_scan_arg_overrides(config: &mut AppConfig, args: &ScanRequest) -> Result<()> {
    if let Some(search_window) = &args.search_window {
        config.scan.peak_search_window_hz = parse_frequency_hz(search_window)?;
    }
    if let Some(min_peak_snr) = args
        .min_peak_snr
        .or(args.airspy_min_peak_snr)
        .or(args.soapy_min_peak_snr)
    {
        config.scan.min_peak_snr_db = min_peak_snr;
    }
    if let Some(max_peaks) = args
        .max_peaks
        .or(args.airspy_max_peaks)
        .or(args.soapy_max_peaks)
    {
        config.scan.max_peaks_per_freq = max_peaks;
    }
    if let Some(record_path) = &args.record {
        config.scan.record_path = Some(record_path.clone());
    }
    if let Some(mode) = args.record_mode {
        config.scan.record_mode = mode;
    }
    if let Some(record_spectrum_bins) = args.record_spectrum_bins {
        config.scan.record_spectrum_bins = record_spectrum_bins;
    }
    if let Some(record_band_decimation) = args.record_band_decimation {
        config.scan.record_band_decimation = record_band_decimation.max(1);
    }
    Ok(())
}

fn collect_masks(config: &AppConfig, args: &ScanRequest) -> Result<MaskSet> {
    let mut masks = config.masks.clone();
    for mask in &args.mask {
        masks.push(parse_mask_spec(mask)?);
    }
    Ok(MaskSet::new(masks))
}

fn airspy_settings_from_args(base: &AirspySettings, args: &ScanRequest) -> Result<AirspySettings> {
    let mut settings = base.clone();
    if let Some(frames) = args.fft_frames {
        settings.fft_frames = Some(frames);
    }
    if let Some(timeout) = args.capture_timeout_ms {
        settings.capture_timeout_ms = timeout;
    }

    if let Some(dwell_ms) = args.dwell_ms {
        settings.dwell_ms = dwell_ms;
        settings.fft_frames = None;
    }
    if let Some(settle_ms) = args.settle_ms {
        settings.settle_ms = settle_ms;
    }
    if let Some(index) = args.airspy_device_index {
        settings.device_index = index;
    }
    if let Some(sample_rate) = &args.airspy_sample_rate {
        settings.sample_rate_hz = sample_rate_hz(sample_rate)?;
    }
    if let Some(dwell_ms) = args.airspy_dwell_ms {
        settings.dwell_ms = dwell_ms;
        settings.fft_frames = None;
    }
    if let Some(fft_size) = args.airspy_fft_size {
        settings.fft_size = fft_size;
    }
    if let Some(dc_notch) = &args.airspy_dc_notch {
        settings.dc_notch_hz = parse_frequency_hz(dc_notch)?;
    }
    if let Some(settle_ms) = args.airspy_settle_ms {
        settings.settle_ms = settle_ms;
    }
    if let Some(mode) = &args.gain_mode {
        settings.gain.mode = mode.clone();
    }
    if let Some(preset) = args.gain_preset {
        settings.gain.preset = preset;
    }
    if let Some(lna) = args.lna {
        settings.gain.lna = lna;
    }
    if let Some(mixer) = args.mixer {
        settings.gain.mixer = mixer;
    }
    if let Some(vga) = args.vga {
        settings.gain.vga = vga;
    }
    if args.lna_agc {
        settings.gain.lna_agc = true;
    }
    if args.mixer_agc {
        settings.gain.mixer_agc = true;
    }

    Ok(settings)
}

fn soapy_settings_from_args(base: &SoapySettings, args: &ScanRequest) -> Result<SoapySettings> {
    let mut settings = base.clone();
    if let Some(frames) = args.fft_frames {
        settings.fft_frames = Some(frames);
    }
    if let Some(timeout) = args.capture_timeout_ms {
        settings.capture_timeout_ms = timeout;
    }

    if let Some(dwell_ms) = args.dwell_ms {
        settings.dwell_ms = dwell_ms;
        settings.fft_frames = None;
    }
    if let Some(settle_ms) = args.settle_ms {
        settings.settle_ms = settle_ms;
    }
    if let Some(device) = &args.soapy_device {
        settings.device_args = device.clone();
    }
    if let Some(index) = args.soapy_device_index {
        settings.device_index = index;
    }
    if let Some(channel) = args.soapy_channel {
        settings.channel = channel;
    }
    if let Some(sample_rate) = &args.soapy_sample_rate {
        settings.sample_rate_hz = sample_rate_hz(sample_rate)?;
    }
    if let Some(dwell_ms) = args.soapy_dwell_ms {
        settings.dwell_ms = dwell_ms;
        settings.fft_frames = None;
    }
    if let Some(fft_size) = args.soapy_fft_size {
        settings.fft_size = fft_size;
    }
    if let Some(dc_notch) = &args.soapy_dc_notch {
        settings.dc_notch_hz = parse_frequency_hz(dc_notch)?;
    }
    if let Some(settle_ms) = args.soapy_settle_ms {
        settings.settle_ms = settle_ms;
    }
    if let Some(antenna) = &args.soapy_antenna {
        settings.antenna = Some(antenna.clone());
    }
    if let Some(bandwidth) = &args.soapy_bandwidth {
        settings.bandwidth_hz = Some(parse_frequency_hz(bandwidth)?);
    }
    if let Some(mode) = &args.soapy_gain_mode {
        settings.gain.mode = mode.clone();
    }
    if let Some(gain_db) = args.soapy_gain {
        settings.gain.gain_db = Some(gain_db);
        if settings.gain.mode.eq_ignore_ascii_case("none") {
            settings.gain.mode = "manual".to_string();
        }
    }
    if !args.soapy_gain_element.is_empty() {
        settings.gain.elements = args
            .soapy_gain_element
            .iter()
            .map(|input| parse_soapy_gain_element(input))
            .collect::<Result<Vec<_>>>()?;
        if settings.gain.mode.eq_ignore_ascii_case("none") {
            settings.gain.mode = "manual".to_string();
        }
    }

    Ok(settings)
}

fn parse_soapy_gain_element(input: &str) -> Result<SoapyGainElement> {
    let (name, gain) = input
        .split_once('=')
        .or_else(|| input.split_once(':'))
        .ok_or_else(|| anyhow!("invalid Soapy gain element '{input}', expected NAME=DB"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!("invalid Soapy gain element '{input}', empty name"));
    }
    Ok(SoapyGainElement {
        name: name.to_string(),
        gain_db: gain
            .trim()
            .parse::<f64>()
            .map_err(|_| anyhow!("invalid Soapy gain value in '{input}'"))?,
    })
}

fn synthetic_options(args: &ScanRequest) -> Result<SyntheticOptions> {
    let defaults = SyntheticOptions::default();
    let missing_mhz = args
        .synthetic_missing
        .iter()
        .map(|freq| parse_frequency_hz(freq).map(|hz| hz / 1_000_000.0))
        .collect::<Result<Vec<_>>>()?;
    let interferers = args
        .interferer
        .iter()
        .map(|input| SyntheticInterferer::parse(input))
        .collect::<Result<Vec<_>>>()?;

    let options = SyntheticOptions {
        offset_hz: args
            .synthetic_offset
            .as_deref()
            .map(parse_frequency_hz)
            .transpose()?
            .unwrap_or(defaults.offset_hz),
        signal_snr_db: args.synthetic_snr.unwrap_or(defaults.signal_snr_db),
        floor_db: args.synthetic_floor.unwrap_or(defaults.floor_db),
        jitter_hz: args
            .synthetic_jitter
            .as_deref()
            .map(parse_frequency_hz)
            .transpose()?
            .unwrap_or(defaults.jitter_hz),
        missing_mhz,
        interferers,
    };
    Ok(options)
}

fn sample_rate_hz(input: &str) -> Result<u32> {
    let value = parse_frequency_hz(input)?.round();
    ensure!(
        value.is_finite() && value >= 1.0 && value <= u32::MAX as f64,
        "invalid sample rate '{input}'"
    );
    Ok(value as u32)
}
