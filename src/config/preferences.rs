use super::{Backend, DetectionProfile, RecordMode, ResolvedScan, ScanRequest};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfilePreset {
    High24And27,
    High25,
    All,
    High24,
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
        Ok(match self {
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
        })
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanPreferences {
    profile: usize,
    gain: u8,
    recording: usize,
}

impl Default for ScanPreferences {
    fn default() -> Self {
        Self {
            profile: 0,
            gain: 2,
            recording: 0,
        }
    }
}

impl ScanPreferences {
    const RECORD_LABELS: [&'static str; 4] = ["Off", "Peaks", "Summary", "Spectrum"];

    fn normalize(&mut self) {
        self.profile %= ProfilePreset::ALL.len();
        self.gain = self.gain.min(15);
        self.recording %= Self::RECORD_LABELS.len();
    }

    pub fn adjust(&mut self, field: PreferenceField, steps: i32) {
        match field {
            PreferenceField::Profile => {
                self.profile = (self.profile as i64 + steps as i64)
                    .rem_euclid(ProfilePreset::ALL.len() as i64)
                    as usize
            }
            PreferenceField::Gain => {
                self.gain = (self.gain as i64 + steps as i64).clamp(0, 15) as u8
            }
            PreferenceField::Recording => {
                self.recording = (self.recording as i64 + steps as i64)
                    .rem_euclid(Self::RECORD_LABELS.len() as i64)
                    as usize
            }
        }
    }

    pub fn label(&self) -> &'static str {
        self.preset().label()
    }
    pub fn gain(&self) -> u8 {
        self.gain.min(15)
    }
    pub fn preset(&self) -> ProfilePreset {
        ProfilePreset::ALL[self.profile % ProfilePreset::ALL.len()]
    }
    pub fn record_label(&self) -> &'static str {
        Self::RECORD_LABELS[self.recording % Self::RECORD_LABELS.len()]
    }

    pub fn resolve(&self, store: &SettingsStore, demo: bool) -> Result<ResolvedScan> {
        let mut request = ScanRequest {
            preset: Some(self.preset()),
            backend: Some(if demo {
                Backend::Synthetic
            } else {
                Backend::Airspy
            }),
            vga: Some(self.gain()),
            fft_frames: Some(14),
            capture_timeout_ms: Some(3000),
            continuous: true,
            synthetic_offset: Some("0".into()),
            synthetic_jitter: Some("80".into()),
            ..Default::default()
        };
        let recording = self.recording % Self::RECORD_LABELS.len();
        if recording != 0 {
            let time = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            request.record = Some(store.recordings.join(format!("scan-{time}.jsonl")));
            request.record_mode = Some(match recording {
                1 => RecordMode::Peaks,
                2 => RecordMode::Summary,
                _ => RecordMode::Spectrum,
            });
        }
        request.resolve(None)
    }
}

pub struct SettingsStore {
    settings: PathBuf,
    legacy: Option<PathBuf>,
    recordings: PathBuf,
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
            settings: config.join("camfinder/settings.json"),
            legacy: Some(config.join("camfinder-cardputer-zero/settings.json")),
            recordings: data.join("camfinder/recordings"),
        }
    }

    pub fn load(&self) -> Result<ScanPreferences> {
        let mut paths = vec![&self.settings];
        if let Some(legacy) = &self.legacy {
            paths.push(legacy);
        }
        for path in paths {
            match fs::read(path) {
                Ok(bytes) => {
                    let mut settings: ScanPreferences = serde_json::from_slice(&bytes)
                        .with_context(|| format!("read {}", path.display()))?;
                    settings.normalize();
                    return Ok(settings);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error).with_context(|| format!("read {}", path.display()))
                }
            }
        }
        Ok(ScanPreferences::default())
    }

    pub fn save(&self, settings: &ScanPreferences) -> Result<()> {
        let mut settings = settings.clone();
        settings.normalize();
        fs::create_dir_all(self.settings.parent().unwrap())?;
        let temporary = self.settings.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&settings)?)?;
        fs::rename(temporary, &self.settings)?;
        Ok(())
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
