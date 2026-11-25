use super::{AppConfig, Backend, DetectionProfile, RecordMode, ResolvedScan, ScanRequest};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfilePreset {
    #[serde(rename = "24+27")]
    High24And27,
    #[serde(rename = "25")]
    High25,
    #[serde(rename = "all")]
    All,
    #[serde(rename = "24")]
    High24,
    #[serde(rename = "27")]
    High27,
}

impl ProfilePreset {
    pub const ALL: [Self; 5] = [
        Self::High24And27,
        Self::High25,
        Self::All,
        Self::High24,
        Self::High27,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::High24And27 => "24 + 27 MHz",
            Self::High25 => "25 MHz",
            Self::All => "All: 24/27/25",
            Self::High24 => "24 MHz",
            Self::High27 => "27 MHz",
        }
    }

    pub fn profiles(self) -> Result<Vec<DetectionProfile>> {
        let mut best = DetectionProfile::parse(
            include_str!("../../profiles/best_high_airspy.toml"),
            Path::new("best_high_airspy"),
        )?;
        let extra = DetectionProfile::parse(
            include_str!("../../profiles/25mhz_high_airspy.toml"),
            Path::new("25mhz_high_airspy"),
        )?;
        let mut profiles = match self {
            Self::High25 => vec![extra],
            Self::All => vec![best, extra],
            Self::High24 | Self::High27 => {
                let group = if self == Self::High24 {
                    "24mhz"
                } else {
                    "27mhz"
                };
                best.groups.retain(|candidate| candidate.name == group);
                vec![best]
            }
            Self::High24And27 => vec![best],
        };
        for profile in &mut profiles {
            profile.scan = None;
            profile.device = None;
        }
        Ok(profiles)
    }
}

impl FromStr for ProfilePreset {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "24+27" => Ok(Self::High24And27),
            "25" => Ok(Self::High25),
            "all" => Ok(Self::All),
            "24" => Ok(Self::High24),
            "27" => Ok(Self::High27),
            _ => Err(anyhow!(
                "unknown preset '{value}'; choose 24+27, 25, all, 24 or 27"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PreferenceField {
    Profile,
    Gain,
    Recording,
}

impl AppConfig {
    pub fn adjust(&mut self, field: PreferenceField, steps: i32) {
        if steps == 0 {
            return;
        }
        match field {
            PreferenceField::Profile => {
                let current = ProfilePreset::ALL
                    .iter()
                    .position(|preset| Some(*preset) == self.preset);
                let index = current.map_or(if steps > 0 { -1 } else { 0 }, |index| index as i64);
                self.preset = Some(
                    ProfilePreset::ALL[(index + steps as i64)
                        .rem_euclid(ProfilePreset::ALL.len() as i64)
                        as usize],
                );
                self.profiles.clear();
                self.profile_files.clear();
            }
            PreferenceField::Gain => match self.device.backend.as_deref() {
                Some("airspy") => {
                    let gain = &mut self.device.airspy.gain;
                    if matches!(gain.mode.as_str(), "sensitivity" | "linearity") {
                        gain.preset = (gain.preset as i64 + steps as i64).clamp(0, 21) as u8;
                    } else {
                        gain.vga = (gain.vga as i64 + steps as i64).clamp(0, 15) as u8;
                    }
                }
                Some("soapy") => {
                    let gain = &mut self.device.soapy.gain;
                    gain.gain_db = Some(gain.gain_db.unwrap_or(0.0) + f64::from(steps));
                    gain.mode = "manual".into();
                    gain.elements.clear();
                }
                _ => {}
            },
            PreferenceField::Recording => {
                let current = if self.scan.record || self.scan.record_path.is_some() {
                    match self.scan.record_mode {
                        RecordMode::Peaks => 1,
                        RecordMode::Summary => 2,
                        RecordMode::Spectrum => 3,
                    }
                } else {
                    0
                };
                let next = (current + i64::from(steps)).rem_euclid(4);
                self.scan.record = next != 0;
                if next == 0 {
                    self.scan.record_path = None;
                } else {
                    self.scan.record_mode = match next {
                        1 => RecordMode::Peaks,
                        2 => RecordMode::Summary,
                        _ => RecordMode::Spectrum,
                    };
                }
            }
        }
    }

    pub fn profile_label(&self) -> &'static str {
        self.preset.map_or("Custom", ProfilePreset::label)
    }

    pub fn gain_label(&self) -> String {
        match self.device.backend.as_deref() {
            Some("airspy") => {
                let gain = &self.device.airspy.gain;
                if matches!(gain.mode.as_str(), "sensitivity" | "linearity") {
                    format!("{} {}", gain.mode, gain.preset)
                } else {
                    format!("VGA {}", gain.vga)
                }
            }
            Some("soapy") => {
                let gain = &self.device.soapy.gain;
                if gain.mode == "agc" {
                    "Auto".into()
                } else if matches!(gain.mode.as_str(), "" | "none") {
                    "Driver default".into()
                } else if !gain.elements.is_empty() {
                    "Per-stage".into()
                } else if let Some(db) = gain.gain_db {
                    format!("{db:.1} dB")
                } else {
                    "Driver default".into()
                }
            }
            _ => "N/A".into(),
        }
    }

    pub fn record_label(&self) -> &'static str {
        if !self.scan.record && self.scan.record_path.is_none() {
            return "Off";
        }
        match self.scan.record_mode {
            RecordMode::Peaks => "Peaks",
            RecordMode::Summary => "Summary",
            RecordMode::Spectrum => "Spectrum",
        }
    }
}

pub struct SettingsStore {
    settings: PathBuf,
    recordings: PathBuf,
    required: bool,
}

impl Default for SettingsStore {
    fn default() -> Self {
        Self::in_directories(
            directory("XDG_CONFIG_HOME", ".config"),
            directory("XDG_DATA_HOME", ".local/share"),
        )
    }
}

impl SettingsStore {
    pub fn in_directories(config: PathBuf, data: PathBuf) -> Self {
        Self {
            settings: config.join("camfinder/config.toml"),
            recordings: data.join("camfinder/recordings"),
            required: false,
        }
    }

    pub fn for_path(path: PathBuf) -> Self {
        Self {
            settings: path,
            required: true,
            ..Self::default()
        }
    }

    pub fn load(&self) -> Result<AppConfig> {
        match fs::read_to_string(&self.settings) {
            Ok(raw) => return AppConfig::parse(&raw, &self.settings),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !self.required => {}
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", self.settings.display()))
            }
        }
        AppConfig::load_defaults()
    }

    pub fn save(&self, config: &AppConfig) -> Result<()> {
        let mut config = config.clone();
        config.resolve_paths_and_profiles(&self.settings)?;
        ScanRequest::default().resolve_config_with_store(config.clone(), self)?;
        let contents = toml::to_string_pretty(&config)?;
        if let Some(parent) = self
            .settings
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.settings.with_extension("toml.tmp");
        fs::write(&temporary, contents)?;
        fs::rename(temporary, &self.settings)?;
        Ok(())
    }

    pub fn resolve(&self, config: &AppConfig, demo: bool) -> Result<ResolvedScan> {
        let mut config = config.clone();
        config.resolve_paths_and_profiles(&self.settings)?;
        ScanRequest {
            backend: demo.then_some(Backend::Synthetic),
            continuous: true,
            synthetic_offset: demo.then(|| "0".into()),
            synthetic_jitter: demo.then(|| "80".into()),
            ..Default::default()
        }
        .resolve_config_with_store(config, self)
    }

    pub(super) fn recording_path(&self) -> Result<PathBuf> {
        let time = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        Ok(self.recordings.join(format!("scan-{time}.jsonl")))
    }
}

fn directory(variable: &str, fallback: &str) -> PathBuf {
    env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(fallback))
        })
        .unwrap_or_else(env::temp_dir)
}
