use crate::dsp::Complex32;
use crate::sdr::{CaptureStatistics, IqBackend, IqCapture, SdrScanConfig};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

#[derive(Deserialize)]
struct Manifest {
    sample_rate_hz: f64,
    fft_size: usize,
    #[serde(default)]
    dc_notch_hz: f64,
    captures: Vec<Capture>,
}

#[derive(Deserialize)]
struct Capture {
    target_hz: f64,
    center_hz: f64,
    path: PathBuf,
}

pub struct ReplayScanner {
    config: SdrScanConfig,
    captures: Vec<Capture>,
    selected: Option<usize>,
}

impl ReplayScanner {
    pub fn open(path: &Path) -> Result<Self> {
        let manifest: Manifest = serde_json::from_reader(BufReader::new(File::open(path)?))?;
        if !manifest.sample_rate_hz.is_finite()
            || manifest.sample_rate_hz <= 0.0
            || !manifest.fft_size.is_power_of_two()
            || manifest.captures.is_empty()
        {
            return Err(anyhow!(
                "IQ manifest requires a positive sample rate, power-of-two FFT size and captures"
            ));
        }
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let captures = manifest
            .captures
            .into_iter()
            .map(|mut capture| {
                capture.path = base.join(capture.path);
                capture
            })
            .collect();
        Ok(Self {
            config: SdrScanConfig {
                sample_rate_hz: manifest.sample_rate_hz,
                fft_size: manifest.fft_size,
                dc_notch_hz: manifest.dc_notch_hz,
                dwell_ms: 0,
                settle_ms: 0,
            },
            captures,
            selected: None,
        })
    }
}

impl IqBackend for ReplayScanner {
    fn source_name(&self) -> &str {
        "iq-file"
    }

    fn scan_config(&self) -> SdrScanConfig {
        self.config
    }

    fn tune(&mut self, center_hz: f64) -> Result<f64> {
        let index = self
            .captures
            .iter()
            .position(|capture| (capture.target_hz - center_hz).abs() < 1.0)
            .ok_or_else(|| anyhow!("no IQ capture for {center_hz:.0} Hz"))?;
        self.selected = Some(index);
        Ok(self.captures[index].center_hz)
    }

    fn collect_iq(&mut self) -> Result<Vec<Complex32>> {
        Ok(self.capture(&Arc::new(AtomicBool::new(false)))?.iq)
    }

    fn capture(&mut self, stop: &Arc<AtomicBool>) -> Result<IqCapture> {
        let started = Instant::now();
        let capture = &self.captures[self
            .selected
            .ok_or_else(|| anyhow!("IQ replay must tune before capture"))?];
        let file = File::open(&capture.path)
            .with_context(|| format!("failed to open IQ capture {}", capture.path.display()))?;
        let bytes = file.metadata()?.len();
        if bytes % 8 != 0 || bytes / 8 < self.config.fft_size as u64 {
            return Err(anyhow!("IQ capture must contain complete little-endian f32 I/Q pairs and at least one FFT frame"));
        }
        if bytes / 8 > 16_777_216 {
            return Err(anyhow!(
                "IQ replay capture exceeds 16777216 samples; split it into shorter captures"
            ));
        }
        let mut reader = BufReader::new(file);
        let mut iq = Vec::with_capacity((bytes / 8) as usize);
        let mut buffer = [0; 8192];
        let mut remaining = bytes;
        while remaining > 0 && !stop.load(Ordering::Relaxed) {
            let count = remaining.min(buffer.len() as u64) as usize;
            reader.read_exact(&mut buffer[..count])?;
            for pair in buffer[..count].chunks_exact(8) {
                let sample = Complex32::new(
                    f32::from_le_bytes(pair[..4].try_into().unwrap()),
                    f32::from_le_bytes(pair[4..].try_into().unwrap()),
                );
                if !sample.re.is_finite() || !sample.im.is_finite() {
                    return Err(anyhow!("IQ capture contains a non-finite sample"));
                }
                iq.push(sample);
            }
            remaining -= count as u64;
        }
        Ok(IqCapture {
            statistics: CaptureStatistics {
                capture_ms: started.elapsed().as_secs_f64() * 1000.0,
                raw_bytes: (iq.len() * 8) as u64,
                interrupted: stop.load(Ordering::Relaxed),
                ..Default::default()
            },
            iq,
        })
    }
}
