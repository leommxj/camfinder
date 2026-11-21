use crate::config::{DetectionProfile, ScanSettings};
use crate::detect::ProfileDetection;
use crate::mask::FrequencyMask;
use crate::peak::{Peak, SpectrumBand};
use crate::persistence::PersistenceSummary;
use crate::sdr::{DeviceSettings, TargetStatistics};
use anyhow::{Context, Result};
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

#[derive(Serialize)]
pub struct ScanRecord<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub format_version: u32,
    pub timestamp_unix_seconds: f64,
    pub backend: &'a str,
    pub pass: usize,
    pub elapsed_seconds: f64,
    pub pass_seconds: f64,
    pub scan: &'a ScanSettings,
    pub device: &'a DeviceSettings,
    pub profiles: &'a [DetectionProfile],
    pub masks: &'a [FrequencyMask],
    pub peaks: &'a [Peak],
    pub spectrum_bands: &'a [SpectrumBand],
    pub results: &'a [ProfileDetection],
    pub persistence: &'a [PersistenceSummary],
    pub statistics: &'a [TargetStatistics],
    pub matching_ms: f64,
}

pub struct Recorder {
    writer: BufWriter<File>,
}

impl Recorder {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open recording {}", path.display()))?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    pub fn write(&mut self, record: &ScanRecord<'_>) -> Result<()> {
        serde_json::to_writer(&mut self.writer, record)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}
