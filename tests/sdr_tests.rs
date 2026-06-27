use anyhow::Result;
use camfinder::config::{DetectionProfile, OffsetModel, ScanSettings};
use camfinder::dsp::Complex32;
use camfinder::sdr::{scan_sdr, scan_sdr_peaks, IqBackend, SdrScanConfig};
use std::path::PathBuf;

struct FakeBackend {
    config: SdrScanConfig,
    tuned_hz: Vec<f64>,
}

impl IqBackend for FakeBackend {
    fn source_name(&self) -> &str {
        "fake"
    }

    fn scan_config(&self) -> SdrScanConfig {
        self.config
    }

    fn tune(&mut self, center_hz: f64) -> Result<f64> {
        self.tuned_hz.push(center_hz);
        Ok(center_hz)
    }

    fn collect_iq(&mut self) -> Result<Vec<Complex32>> {
        Ok(vec![Complex32::new(0.0, 0.0); self.config.fft_size])
    }
}

fn profile() -> DetectionProfile {
    DetectionProfile {
        name: "test".to_string(),
        freqs_mhz: vec![864.0, 816.0, 840.0, 816.0],
        offset_model: OffsetModel::FixedHz,
        min_hits: 2,
        min_score: 8.0,
        max_offset_spread_hz: 2_000.0,
        groups: Vec::new(),
        scan: None,
        device: None,
    }
}

fn backend() -> FakeBackend {
    FakeBackend {
        config: SdrScanConfig {
            sample_rate_hz: 2_400_000.0,
            dwell_ms: 0,
            fft_size: 1024,
            dc_notch_hz: 2_000.0,
            settle_ms: 0,
        },
        tuned_hz: Vec::new(),
    }
}

fn scan(record_path: Option<PathBuf>, record_band_decimation: usize) -> ScanSettings {
    ScanSettings {
        peak_search_window_hz: 250_000.0,
        min_peak_snr_db: 4.0,
        max_peaks_per_freq: 16,
        record_path,
        record_spectrum_bins: 128,
        record_band_decimation,
        output: "table".to_string(),
    }
}

#[test]
fn scan_sdr_peaks_tunes_unique_targets_in_order() {
    let mut backend = backend();
    let scan = scan(None, 1);
    let mut logs = Vec::new();

    let peaks = scan_sdr_peaks(&mut backend, &[profile()], &scan, &mut |line| {
        logs.push(line)
    })
    .unwrap();

    assert!(peaks.is_empty());
    assert_eq!(
        backend.tuned_hz,
        vec![816_000_000.0, 840_000_000.0, 864_000_000.0]
    );
    assert!(logs.iter().any(|line| line.contains("fake scan")));
}

#[test]
fn scan_sdr_records_spectrum_bands_when_recording_is_enabled() {
    let mut backend = backend();
    let scan = scan(Some(PathBuf::from("capture.jsonl")), 2);
    let mut logs = Vec::new();

    let output = scan_sdr(&mut backend, &[profile()], &scan, &mut |line| {
        logs.push(line)
    })
    .unwrap();

    assert!(output.peaks.is_empty());
    assert_eq!(output.spectrum_bands.len(), 3);
    assert!(output
        .spectrum_bands
        .iter()
        .all(|band| band.source.as_deref() == Some("fake")));
    assert!(output
        .spectrum_bands
        .iter()
        .all(|band| band.point_decimation == 2 && !band.points.is_empty()));
    assert!(output.spectrum_bands.iter().all(|band| {
        band.sample_rate_hz == 2_400_000.0
            && band.fft_size == 1024
            && band.iq_samples >= 1024
            && band.fft_frames >= 1
            && band.dc_notch_hz == 2_000.0
            && band.min_peak_snr_db == 4.0
            && band.max_peaks == 16
    }));
}
