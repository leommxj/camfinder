use crate::frequency::MHZ;
use crate::mask::FrequencyMask;
use crate::sdr::DeviceSettings;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::path::PathBuf;

mod request;
mod settings;
pub use request::{apply_profile_settings, Backend, ResolvedScan, ScanRequest};
pub use settings::{PreferenceField, ProfilePreset, SettingsStore};

pub const DEFAULT_CONFIG_TOML: &str = include_str!("../camfinder.default.toml");

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub profile_defaults: ProfileDefaults,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<ProfilePreset>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profile_files: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<DetectionProfile>,
    #[serde(default)]
    pub scan: ScanSettings,
    #[serde(default)]
    pub masks: Vec<FrequencyMask>,
    #[serde(default)]
    pub device: DeviceSettings,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::load_defaults().expect("embedded default configuration must parse")
    }
}

impl AppConfig {
    pub fn default_toml() -> &'static str {
        DEFAULT_CONFIG_TOML
    }

    pub fn load_defaults() -> Result<Self> {
        Ok(toml::from_str(DEFAULT_CONFIG_TOML)?)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        Self::parse(&raw, path)
    }

    fn parse(raw: &str, path: &Path) -> Result<Self> {
        let mut document = parse_document(raw)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        let profile_defaults: ProfileDefaults = document
            .get("profile_defaults")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()?
            .unwrap_or_default();
        if let Some(profiles) = document
            .get_mut("profiles")
            .and_then(toml::Value::as_array_mut)
        {
            let defaults = toml::Value::try_from(&profile_defaults)?;
            for profile in profiles {
                fill_defaults(profile, &defaults);
                fill_device_defaults(profile)?;
            }
        }
        let mut config: Self = document
            .try_into()
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        config.resolve_paths_and_profiles(path)?;
        Ok(config)
    }

    fn resolve_paths_and_profiles(&mut self, path: &Path) -> Result<()> {
        let path = std::path::absolute(path)?;
        let base_dir = path.parent().unwrap_or(Path::new("."));
        resolve_data_paths(Some(&mut self.scan), Some(&mut self.device), base_dir);
        for profile in &mut self.profiles {
            profile.resolve_paths(base_dir);
        }
        for path in std::mem::take(&mut self.profile_files) {
            self.profiles.extend(load_profiles_from_path(
                &resolve_profile_path(base_dir, &path),
                &self.profile_defaults,
            )?);
        }
        let profiles = self.selected_profiles()?;
        apply_profile_settings(self, &profiles)?;
        if let Some(backend) = &mut self.device.backend {
            *backend = backend.parse::<Backend>()?.name().into();
        }
        self.device.airspy.gain.mode = self.device.airspy.gain.mode.trim().to_ascii_lowercase();
        self.device.soapy.gain.mode = self.device.soapy.gain.mode.trim().to_ascii_lowercase();
        for profile in &mut self.profiles {
            profile.scan = None;
            profile.device = None;
        }
        Ok(())
    }

    pub fn selected_profiles(&self) -> Result<Vec<DetectionProfile>> {
        if let Some(preset) = self.preset {
            anyhow::ensure!(
                self.profiles.is_empty() && self.profile_files.is_empty(),
                "select either a preset or custom profiles in the configuration"
            );
            return preset.profiles();
        }
        let mut profiles = self.profiles.clone();
        for path in &self.profile_files {
            profiles.extend(load_profiles_from_path(path, &self.profile_defaults)?);
        }
        for profile in &profiles {
            profile.validate(Path::new("configuration"))?;
        }
        Ok(profiles)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScanSettings {
    #[serde(default = "default_peak_search_window_hz")]
    pub peak_search_window_hz: f64,
    #[serde(default = "default_min_peak_snr_db")]
    pub min_peak_snr_db: f64,
    #[serde(default = "default_max_peaks_per_freq")]
    pub max_peaks_per_freq: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_path: Option<PathBuf>,
    #[serde(default)]
    pub record: bool,
    #[serde(default = "default_record_spectrum_bins")]
    pub record_spectrum_bins: usize,
    #[serde(default = "default_record_band_decimation")]
    pub record_band_decimation: usize,
    #[serde(default = "default_record_mode")]
    pub record_mode: RecordMode,
    #[serde(default = "default_output")]
    pub output: String,
}

impl Default for ScanSettings {
    fn default() -> Self {
        default_scan_settings()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordMode {
    Peaks,
    Summary,
    #[default]
    Spectrum,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectionProfile {
    pub name: String,
    #[serde(default)]
    pub freqs_mhz: Vec<f64>,
    #[serde(default)]
    pub offset_model: OffsetModel,
    #[serde(default = "default_min_hits")]
    pub min_hits: usize,
    #[serde(default = "default_min_score")]
    pub min_score: f64,
    #[serde(default = "default_max_offset_spread_hz")]
    pub max_offset_spread_hz: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<DetectionGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan: Option<ScanSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceSettings>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectionGroup {
    pub name: String,
    pub freqs_mhz: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_model: Option<OffsetModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_hits: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_offset_spread_hz: Option<f64>,
}

impl DetectionProfile {
    pub fn parse(raw: &str, source: &Path) -> Result<Self> {
        Self::parse_with_defaults(raw, source, &ProfileDefaults::default())
    }

    fn parse_with_defaults(raw: &str, source: &Path, defaults: &ProfileDefaults) -> Result<Self> {
        let mut document = parse_document(raw)
            .with_context(|| format!("failed to parse profile {}", source.display()))?;
        fill_defaults(&mut document, &toml::Value::try_from(defaults)?);
        let profile: Self = document
            .try_into()
            .with_context(|| format!("failed to parse profile {}", source.display()))?;
        profile.validate(source)?;
        Ok(profile)
    }

    pub fn load(path: &Path) -> Result<Self> {
        Self::load_with_defaults(path, &ProfileDefaults::default())
    }

    fn load_with_defaults(path: &Path, defaults: &ProfileDefaults) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read profile file {}", path.display()))?;
        let mut profile = Self::parse_with_defaults(&raw, path, defaults)?;
        profile.resolve_paths(std::path::absolute(path)?.parent().unwrap());
        Ok(profile)
    }

    fn resolve_paths(&mut self, base: &Path) {
        resolve_data_paths(self.scan.as_mut(), self.device.as_mut(), base);
    }

    pub fn nominal_freqs_hz(&self) -> Vec<f64> {
        self.freqs_mhz.iter().map(|freq| freq * MHZ).collect()
    }

    pub fn detection_profiles(&self) -> Vec<Self> {
        if self.groups.is_empty() {
            let mut profile = self.clone();
            profile.groups.clear();
            return vec![profile];
        }

        self.groups
            .iter()
            .map(|group| DetectionProfile {
                name: format!("{}:{}", self.name, group.name),
                freqs_mhz: group.freqs_mhz.clone(),
                offset_model: group.offset_model.unwrap_or(self.offset_model),
                min_hits: group.min_hits.unwrap_or(self.min_hits),
                min_score: group.min_score.unwrap_or(self.min_score),
                max_offset_spread_hz: group
                    .max_offset_spread_hz
                    .unwrap_or(self.max_offset_spread_hz),
                groups: Vec::new(),
                scan: self.scan.clone(),
                device: self.device.clone(),
            })
            .collect()
    }

    pub fn with_overrides(
        mut self,
        min_hits: Option<usize>,
        min_score: Option<f64>,
        match_window_hz: Option<f64>,
        offset_model: Option<OffsetModel>,
    ) -> Self {
        if let Some(min_hits) = min_hits {
            self.min_hits = min_hits;
            for group in &mut self.groups {
                group.min_hits = Some(min_hits);
            }
        }
        if let Some(min_score) = min_score {
            self.min_score = min_score;
            for group in &mut self.groups {
                group.min_score = Some(min_score);
            }
        }
        if let Some(match_window_hz) = match_window_hz {
            self.max_offset_spread_hz = match_window_hz;
            for group in &mut self.groups {
                group.max_offset_spread_hz = Some(match_window_hz);
            }
        }
        if let Some(offset_model) = offset_model {
            self.offset_model = offset_model;
            for group in &mut self.groups {
                group.offset_model = Some(offset_model);
            }
        }
        self
    }

    pub fn validate(&self, path: &Path) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(anyhow!(
                "profile file {} has an empty profile name",
                path.display()
            ));
        }
        if self.groups.is_empty() && self.freqs_mhz.is_empty() {
            return Err(anyhow!(
                "profile '{}' in {} must contain freqs_mhz or at least one [[groups]] entry",
                self.name,
                path.display()
            ));
        }
        if !self.groups.is_empty() && !self.freqs_mhz.is_empty() {
            return Err(anyhow!(
                "profile '{}' in {} cannot mix top-level freqs_mhz with [[groups]]",
                self.name,
                path.display()
            ));
        }
        for group in &self.groups {
            if group.name.trim().is_empty() {
                return Err(anyhow!(
                    "profile '{}' in {} has a group with an empty name",
                    self.name,
                    path.display()
                ));
            }
            if group.freqs_mhz.is_empty() {
                return Err(anyhow!(
                    "group '{}' in profile '{}' in {} must contain at least one frequency",
                    group.name,
                    self.name,
                    path.display()
                ));
            }
        }
        for profile in self.detection_profiles() {
            anyhow::ensure!(
                profile
                    .freqs_mhz
                    .iter()
                    .all(|value| value.is_finite() && *value > 0.0),
                "profile '{}' requires positive finite frequencies",
                profile.name
            );
            anyhow::ensure!(
                profile.min_hits > 0
                    && profile.min_score.is_finite()
                    && profile.max_offset_spread_hz.is_finite()
                    && profile.max_offset_spread_hz > 0.0,
                "profile '{}' has invalid detection thresholds",
                profile.name
            );
        }
        Ok(())
    }
}

pub fn target_frequencies(profiles: &[DetectionProfile]) -> Vec<f64> {
    let mut frequencies = profiles
        .iter()
        .flat_map(DetectionProfile::detection_profiles)
        .flat_map(|profile| profile.nominal_freqs_hz())
        .collect::<Vec<_>>();
    frequencies.sort_by(f64::total_cmp);
    frequencies.dedup_by(|a, b| (*a - *b).abs() < 1.0);
    frequencies
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OffsetModel {
    #[default]
    FixedHz,
    Ppm,
    Auto,
}

fn default_peak_search_window_hz() -> f64 {
    default_scan_settings().peak_search_window_hz
}

fn default_min_peak_snr_db() -> f64 {
    default_scan_settings().min_peak_snr_db
}

fn default_max_peaks_per_freq() -> usize {
    default_scan_settings().max_peaks_per_freq
}

fn default_record_spectrum_bins() -> usize {
    default_scan_settings().record_spectrum_bins
}

fn default_record_band_decimation() -> usize {
    default_scan_settings().record_band_decimation
}

fn default_record_mode() -> RecordMode {
    default_scan_settings().record_mode
}

fn default_max_offset_spread_hz() -> f64 {
    default_profile_fields().max_offset_spread_hz
}

fn default_min_hits() -> usize {
    default_profile_fields().min_hits
}

fn default_min_score() -> f64 {
    default_profile_fields().min_score
}

fn default_output() -> String {
    default_scan_settings().output
}

fn default_scan_settings() -> ScanSettings {
    toml::from_str::<ScanDefaults>(DEFAULT_CONFIG_TOML)
        .expect("embedded default scan settings must parse")
        .scan
}

fn default_profile_fields() -> ProfileDefaults {
    toml::from_str::<ProfileDefaultsDocument>(DEFAULT_CONFIG_TOML)
        .expect("embedded default profile fields must parse")
        .profile_defaults
}

#[derive(Deserialize)]
struct ScanDefaults {
    scan: ScanSettings,
}

#[derive(Deserialize)]
struct ProfileDefaultsDocument {
    profile_defaults: ProfileDefaults,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileDefaults {
    #[serde(default = "default_min_hits")]
    pub min_hits: usize,
    #[serde(default = "default_min_score")]
    pub min_score: f64,
    #[serde(default = "default_max_offset_spread_hz")]
    pub max_offset_spread_hz: f64,
}

impl Default for ProfileDefaults {
    fn default() -> Self {
        default_profile_fields()
    }
}

fn parse_document(raw: &str) -> Result<toml::Value> {
    let mut document = toml::from_str(raw)?;
    fill_device_defaults(&mut document)?;
    Ok(document)
}

fn fill_device_defaults(document: &mut toml::Value) -> Result<()> {
    let defaults: toml::Value = toml::from_str(DEFAULT_CONFIG_TOML)?;
    if let Some(device) = document.get_mut("device") {
        for backend in ["airspy", "soapy"] {
            if let Some(settings) = device.get_mut(backend) {
                let mut base = defaults["device"][backend].clone();
                if settings.get("dwell_ms").is_some() && settings.get("fft_frames").is_none() {
                    base.as_table_mut().unwrap().remove("fft_frames");
                }
                fill_defaults(settings, &base);
            }
        }
    }
    Ok(())
}

fn fill_defaults(value: &mut toml::Value, defaults: &toml::Value) {
    if let (Some(value), Some(defaults)) = (value.as_table_mut(), defaults.as_table()) {
        for (key, default) in defaults {
            if let Some(value) = value.get_mut(key) {
                fill_defaults(value, default);
            } else {
                value.insert(key.clone(), default.clone());
            }
        }
    }
}

fn resolve_profile_path(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn resolve_optional_path(path: &mut Option<PathBuf>, base: &Path) {
    if let Some(path) = path {
        *path = resolve_profile_path(base, path);
    }
}

fn resolve_data_paths(
    scan: Option<&mut ScanSettings>,
    device: Option<&mut DeviceSettings>,
    base: &Path,
) {
    if let Some(scan) = scan {
        resolve_optional_path(&mut scan.record_path, base);
    }
    if let Some(device) = device {
        resolve_optional_path(&mut device.peaks_file.path, base);
        resolve_optional_path(&mut device.replay.manifest, base);
    }
}

fn load_profiles_from_path(
    path: &Path,
    defaults: &ProfileDefaults,
) -> Result<Vec<DetectionProfile>> {
    if path.is_dir() {
        let mut entries = fs::read_dir(path)
            .with_context(|| format!("failed to read profile directory {}", path.display()))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("failed to read profile directory {}", path.display()))?;
        entries.retain(|entry| entry.extension().is_some_and(|ext| ext == "toml"));
        entries.sort();

        if entries.is_empty() {
            return Err(anyhow!(
                "profile directory {} contains no .toml files",
                path.display()
            ));
        }

        entries
            .iter()
            .map(|entry| DetectionProfile::load_with_defaults(entry, defaults))
            .collect()
    } else {
        Ok(vec![DetectionProfile::load_with_defaults(path, defaults)?])
    }
}
