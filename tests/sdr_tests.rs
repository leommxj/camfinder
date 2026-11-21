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
        record_mode: Default::default(),
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

#[test]
fn persistent_pipeline_recovers_results_after_capture_error() {
    use camfinder::sdr::SdrPipeline;
    use std::sync::{atomic::AtomicBool, Arc};

    struct FailingBackend {
        inner: FakeBackend,
        captures: usize,
    }
    impl IqBackend for FailingBackend {
        fn source_name(&self) -> &str {
            "failing"
        }
        fn scan_config(&self) -> SdrScanConfig {
            self.inner.config
        }
        fn tune(&mut self, center_hz: f64) -> Result<f64> {
            self.inner.tune(center_hz)
        }
        fn collect_iq(&mut self) -> Result<Vec<Complex32>> {
            self.captures += 1;
            anyhow::ensure!(self.captures != 2, "capture disconnected");
            self.inner.collect_iq()
        }
    }
    let mut failing = FailingBackend {
        inner: backend(),
        captures: 0,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let mut pipeline = SdrPipeline::new();
    let error = pipeline
        .scan(
            &mut failing,
            &[profile()],
            &scan(None, 1),
            &stop,
            &mut |_| {},
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("capture disconnected"));
    let mut healthy = backend();
    let output = pipeline
        .scan(
            &mut healthy,
            &[profile()],
            &scan(None, 1),
            &stop,
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(output.statistics.len(), 3);
    assert!(output
        .statistics
        .iter()
        .all(|statistics| statistics.source == "fake" && statistics.fft_frames == 1));
}

#[test]
fn persistent_pipeline_drains_analysis_errors_before_reuse() {
    use camfinder::sdr::SdrPipeline;
    use std::sync::{atomic::AtomicBool, Arc};
    let mut invalid = backend();
    invalid.config.fft_size = 0;
    let stop = Arc::new(AtomicBool::new(false));
    let mut pipeline = SdrPipeline::new();
    assert!(pipeline
        .scan(
            &mut invalid,
            &[profile()],
            &scan(None, 1),
            &stop,
            &mut |_| {}
        )
        .is_err());
    let mut healthy = backend();
    let output = pipeline
        .scan(
            &mut healthy,
            &[profile()],
            &scan(None, 1),
            &stop,
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(output.statistics.len(), 3);
}

#[test]
fn stopped_pipeline_does_not_tune() {
    use camfinder::sdr::SdrPipeline;
    use std::sync::{atomic::AtomicBool, Arc};
    let mut backend = backend();
    let output = SdrPipeline::new()
        .scan(
            &mut backend,
            &[profile()],
            &scan(None, 1),
            &Arc::new(AtomicBool::new(true)),
            &mut |_| {},
        )
        .unwrap();
    assert!(backend.tuned_hz.is_empty());
    assert!(output.statistics.is_empty());
}

#[test]
fn capture_summary_and_peak_recording_have_independent_spectrum_costs() {
    use camfinder::config::RecordMode;
    let mut backend = backend();
    let mut settings = scan(Some(PathBuf::from("capture.jsonl")), 1);
    settings.record_mode = RecordMode::Summary;
    let summary = scan_sdr(&mut backend, &[profile()], &settings, &mut |_| {}).unwrap();
    assert_eq!(summary.statistics.len(), 3);
    assert_eq!(summary.spectrum_bands.len(), 3);
    assert!(summary
        .spectrum_bands
        .iter()
        .all(|band| band.points.len() <= 2));
    settings.record_mode = RecordMode::Peaks;
    let peaks = scan_sdr(&mut backend, &[profile()], &settings, &mut |_| {}).unwrap();
    assert!(peaks.spectrum_bands.is_empty());
    assert_eq!(peaks.statistics.len(), 3);
}

#[test]
fn device_capture_budget_validation_rejects_empty_and_excessive_targets() {
    use camfinder::sdr::AirspySettings;
    let mut settings = AirspySettings::default();
    settings.fft_frames = Some(0);
    assert!(settings.validate().is_err());
    settings.fft_frames = Some(usize::MAX);
    assert!(settings.validate().is_err());
    settings.fft_frames = Some(14);
    settings.fft_size = 3;
    assert!(settings.validate().is_err());
    settings.fft_size = 65536;
    assert!(settings.validate().is_ok());
}

#[test]
fn cancellation_during_analysis_backpressure_does_not_tune_another_target() {
    use camfinder::sdr::SdrPipeline;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let mut backend = backend();
    let stop = Arc::new(AtomicBool::new(false));
    let output = SdrPipeline::new()
        .scan_with_progress(
            &mut backend,
            &[profile()],
            &scan(None, 1),
            &stop,
            &mut |_| {},
            &mut |_| stop.store(true, Ordering::Relaxed),
        )
        .unwrap();
    assert_eq!(backend.tuned_hz, vec![816_000_000.0, 840_000_000.0]);
    assert_eq!(output.statistics.len(), 2);
}

#[test]
fn cancellation_during_capture_drains_results_before_pipeline_reuse() {
    use camfinder::sdr::SdrPipeline;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    struct CancelBackend {
        inner: FakeBackend,
        stop: Arc<AtomicBool>,
        captures: usize,
    }
    impl IqBackend for CancelBackend {
        fn source_name(&self) -> &str {
            "cancel"
        }
        fn scan_config(&self) -> SdrScanConfig {
            self.inner.config
        }
        fn tune(&mut self, center_hz: f64) -> Result<f64> {
            self.inner.tune(center_hz)
        }
        fn collect_iq(&mut self) -> Result<Vec<Complex32>> {
            self.captures += 1;
            if self.captures == 2 {
                self.stop.store(true, Ordering::Relaxed);
            }
            self.inner.collect_iq()
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let mut cancelling = CancelBackend {
        inner: backend(),
        stop: Arc::clone(&stop),
        captures: 0,
    };
    let mut pipeline = SdrPipeline::new();
    let partial = pipeline
        .scan(
            &mut cancelling,
            &[profile()],
            &scan(None, 1),
            &stop,
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(partial.statistics.len(), 1);
    assert_eq!(partial.statistics[0].source, "cancel");
    stop.store(false, Ordering::Relaxed);
    let mut healthy = backend();
    let complete = pipeline
        .scan(
            &mut healthy,
            &[profile()],
            &scan(None, 1),
            &stop,
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(complete.statistics.len(), 3);
    assert!(complete
        .statistics
        .iter()
        .all(|statistics| statistics.source == "fake"));
}
