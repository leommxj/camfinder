use crate::frequency::MHZ;
use crate::mask::FrequencyMask;
use crate::sdr::DeviceSettings;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::path::PathBuf;

pub const DEFAULT_CONFIG_TOML: &str = include_str!("../camfinder.default.toml");

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profile_files: Vec<PathBuf>,
    #[serde(default, skip_serializing)]
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
        Self::load_defaults().expect("default profile files must parse")
    }
}

impl AppConfig {
    pub fn default_toml() -> &'static str {
        DEFAULT_CONFIG_TOML
    }

    pub fn load_defaults() -> Result<Self> {
        let mut config: Self = toml::from_str(DEFAULT_CONFIG_TOML)?;
        config.load_referenced_profiles(Path::new("."))?;
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        let mut config: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        config.load_referenced_profiles(base_dir)?;
        Ok(config)
    }

    fn load_referenced_profiles(&mut self, base_dir: &Path) -> Result<()> {
        for path in &self.profile_files {
            let resolved = resolve_profile_path(base_dir, path);
            self.profiles.extend(load_profiles_from_path(&resolved)?);
        }
        Ok(())
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
    #[serde(default = "default_record_spectrum_bins")]
    pub record_spectrum_bins: usize,
    #[serde(default = "default_record_band_decimation")]
    pub record_band_decimation: usize,
    #[serde(default)]
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
    #[serde(default, skip_serializing)]
    pub scan: Option<ScanSettings>,
    #[serde(default, skip_serializing)]
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
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read profile file {}", path.display()))?;
        let profile: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse profile file {}", path.display()))?;
        profile.validate(path)?;
        Ok(profile)
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

    fn validate(&self, path: &Path) -> Result<()> {
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
        Ok(())
    }
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

fn default_profile_fields() -> ProfileFieldDefaults {
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
    profile_defaults: ProfileFieldDefaults,
}

#[derive(Clone, Debug, Deserialize)]
struct ProfileFieldDefaults {
    min_hits: usize,
    min_score: f64,
    max_offset_spread_hz: f64,
}

fn resolve_profile_path(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn load_profiles_from_path(path: &Path) -> Result<Vec<DetectionProfile>> {
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
            .map(|entry| DetectionProfile::load(entry))
            .collect()
    } else {
        Ok(vec![DetectionProfile::load(path)?])
    }
}
