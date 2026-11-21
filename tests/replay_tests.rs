use camfinder::config::{DetectionProfile, OffsetModel, ScanSettings};
use camfinder::replay::ReplayScanner;
use camfinder::sdr::{scan_sdr, IqBackend};
use std::io::Write;

#[test]
fn replay_resolves_paths_and_preserves_frequency_and_sample_count() {
    let directory =
        std::env::temp_dir().join(format!("camfinder-iq-replay-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let mut file = std::fs::File::create(directory.join("tone.cf32")).unwrap();
    for i in 0..4096 {
        let phase = std::f32::consts::TAU * 128.0 * i as f32 / 4096.0;
        file.write_all(&phase.cos().to_le_bytes()).unwrap();
        file.write_all(&phase.sin().to_le_bytes()).unwrap();
    }
    drop(file);
    std::fs::write(
        directory.join("manifest.json"),
        r#"{
        "sample_rate_hz": 409600,
        "fft_size": 4096,
        "captures": [{"target_hz": 100000000, "center_hz": 100001000, "path": "tone.cf32"}]
    }"#,
    )
    .unwrap();
    let mut backend = ReplayScanner::open(&directory.join("manifest.json")).unwrap();
    assert!(backend.tune(200_000_000.0).is_err());
    let profile = DetectionProfile {
        name: "replay".into(),
        freqs_mhz: vec![100.0],
        offset_model: OffsetModel::FixedHz,
        min_hits: 1,
        min_score: 8.0,
        max_offset_spread_hz: 1200.0,
        groups: Vec::new(),
        scan: None,
        device: None,
    };
    let result = scan_sdr(
        &mut backend,
        &[profile],
        &ScanSettings::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result.statistics[0].iq_samples, 4096);
    assert_eq!(result.statistics[0].fft_frames, 1);
    assert!(result
        .peaks
        .iter()
        .any(|peak| (peak.freq_hz - 100_013_800.0).abs() < 1.0));
    std::fs::remove_dir_all(directory).unwrap();
}
