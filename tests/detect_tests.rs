use camfinder::config::{DetectionProfile, OffsetModel, ScanSettings};
use camfinder::detect::{
    detect_profile, detect_profile_candidates, detect_profile_suspects, OffsetEstimate,
};
use camfinder::mask::{FrequencyMask, MaskSet};
use camfinder::peak::Peak;

fn profile(
    freqs_mhz: Vec<f64>,
    offset_model: OffsetModel,
    min_hits: usize,
    min_score: f64,
    max_offset_spread_hz: f64,
) -> DetectionProfile {
    DetectionProfile {
        name: "test".to_string(),
        freqs_mhz,
        offset_model,
        min_hits,
        min_score,
        max_offset_spread_hz,
        groups: Vec::new(),
        scan: None,
        device: None,
    }
}

fn fixed_profile(min_hits: usize) -> DetectionProfile {
    profile(
        vec![816.0, 840.0, 864.0],
        OffsetModel::FixedHz,
        min_hits,
        8.0,
        1_000.0,
    )
}

fn peak(id: usize, freq_hz: f64, power_db: f64, floor_db: f64) -> Peak {
    Peak {
        id,
        freq_hz,
        power_db,
        floor_db,
        source: None,
        shape: None,
        spectrum_window: Vec::new(),
    }
}

#[test]
fn detects_dynamic_fixed_offset_for_partial_profile() {
    let profile = fixed_profile(2);
    let peaks = vec![
        peak(1, 816_012_500.0, -70.0, -85.0),
        peak(2, 840_012_700.0, -69.0, -85.0),
        peak(3, 864_012_300.0, -72.0, -85.0),
    ];

    let detection = detect_profile(
        &profile,
        &peaks,
        &MaskSet::default(),
        &ScanSettings::default(),
    );

    assert!(detection.detected);
    assert_eq!(detection.hits, 3);
}

#[test]
fn refines_fixed_offset_from_all_hits() {
    let profile = fixed_profile(3);
    let peaks = vec![
        peak(1, 816_012_000.0, -70.0, -85.0),
        peak(2, 840_012_500.0, -70.0, -85.0),
        peak(3, 864_013_000.0, -70.0, -85.0),
    ];

    let detection = detect_profile(
        &profile,
        &peaks,
        &MaskSet::default(),
        &ScanSettings::default(),
    );

    assert!(detection.detected);
    assert!(matches!(
        detection.offset,
        Some(OffsetEstimate::FixedHz { hz }) if (hz - 12_500.0).abs() < 1.0
    ));
    assert!((detection.offset_group_hz.unwrap() - 12_500.0).abs() < 1.0);
}

#[test]
fn detects_ppm_offset_when_profile_uses_ppm() {
    let profile = profile(vec![810.0, 864.0, 918.0], OffsetModel::Ppm, 3, 8.0, 500.0);
    let ppm = -18.0;
    let peaks = profile
        .nominal_freqs_hz()
        .into_iter()
        .enumerate()
        .map(|(index, nominal_hz)| {
            peak(
                index + 1,
                nominal_hz * (1.0 + ppm / 1_000_000.0),
                -70.0,
                -88.0,
            )
        })
        .collect::<Vec<_>>();

    let detection = detect_profile(
        &profile,
        &peaks,
        &MaskSet::default(),
        &ScanSettings::default(),
    );

    assert!(detection.detected);
    assert_eq!(detection.hits, 3);
    assert!(matches!(
        detection.offset,
        Some(OffsetEstimate::Ppm { ppm: detected_ppm }) if (detected_ppm - ppm).abs() < 0.01
    ));
    assert!(detection.offset_spread_hz < 1.0);
    assert!((detection.offset_group_hz.unwrap() + 15_552.0).abs() < 1.0);
}

#[test]
fn mask_can_remove_a_peak_from_scoring() {
    let profile = fixed_profile(3);
    let peaks = vec![
        peak(1, 816_012_500.0, -70.0, -85.0),
        peak(2, 840_012_500.0, -69.0, -85.0),
        peak(3, 864_012_500.0, -72.0, -85.0),
    ];
    let masks = MaskSet::new(vec![FrequencyMask {
        center_hz: 840_012_500.0,
        width_hz: 1_000.0,
        label: None,
    }]);

    let detection = detect_profile(&profile, &peaks, &masks, &ScanSettings::default());

    assert!(!detection.detected);
    assert_eq!(detection.hits, 2);
}

#[test]
fn consistent_weak_offset_beats_strong_single_interferer() {
    let profile = fixed_profile(2);
    let peaks = vec![
        peak(1, 816_012_500.0, -78.0, -88.0),
        peak(2, 840_052_000.0, -58.0, -88.0),
        peak(3, 840_012_400.0, -80.0, -88.0),
        peak(4, 864_012_600.0, -79.0, -88.0),
    ];

    let detection = detect_profile(
        &profile,
        &peaks,
        &MaskSet::default(),
        &ScanSettings::default(),
    );

    assert!(detection.detected);
    assert_eq!(detection.hits, 3);
    assert!(matches!(
        detection.offset,
        Some(OffsetEstimate::FixedHz { hz }) if (hz - 12_500.0).abs() < 200.0
    ));
    assert!(detection.snr_sum_db >= 27.0);
}

#[test]
fn residual_aware_matching_prefers_consistent_offset() {
    let profile = fixed_profile(3);
    let peaks = vec![
        peak(1, 816_012_500.0, -80.0, -88.0),
        peak(2, 840_012_500.0, -80.0, -88.0),
        peak(3, 864_012_500.0, -80.0, -88.0),
        peak(4, 816_013_350.0, -78.0, -88.0),
    ];

    let detection = detect_profile(
        &profile,
        &peaks,
        &MaskSet::default(),
        &ScanSettings::default(),
    );

    assert!(detection.detected);
    assert!((detection.offset_group_hz.unwrap() - 12_500.0).abs() < 100.0);
    assert!(detection.hit_details.iter().all(|hit| hit.peak_id != 4));
}

#[test]
fn returns_distinct_suspects_for_distinct_offsets() {
    let profile = fixed_profile(2);
    let peaks = vec![
        peak(1, 816_007_500.0, -76.0, -88.0),
        peak(2, 840_007_500.0, -76.0, -88.0),
        peak(3, 816_004_000.0, -76.0, -88.0),
        peak(4, 840_004_000.0, -76.0, -88.0),
    ];

    let suspects = detect_profile_suspects(
        &profile,
        &peaks,
        &MaskSet::default(),
        &ScanSettings::default(),
    );

    assert_eq!(suspects.len(), 2);
    assert!(suspects
        .iter()
        .any(|suspect| (suspect.offset_group_hz.unwrap() - 7_500.0).abs() < 100.0));
    assert!(suspects
        .iter()
        .any(|suspect| (suspect.offset_group_hz.unwrap() - 4_000.0).abs() < 100.0));
}

#[test]
fn detects_four_hits_from_wide_harmonic_profile() {
    let mut profile = profile(
        vec![792.0, 810.0, 816.0, 837.0, 840.0, 864.0, 888.0, 891.0],
        OffsetModel::FixedHz,
        4,
        8.0,
        1_000.0,
    );
    profile.name = "wide".to_string();
    let peaks = vec![
        peak(1, 792_012_500.0, -76.0, -88.0),
        peak(2, 816_012_600.0, -76.0, -88.0),
        peak(3, 840_012_400.0, -76.0, -88.0),
        peak(4, 864_012_500.0, -76.0, -88.0),
    ];

    let detection = detect_profile(
        &profile,
        &peaks,
        &MaskSet::default(),
        &ScanSettings::default(),
    );

    assert!(detection.detected);
    assert_eq!(detection.hits, 4);
    assert_eq!(detection.required_hits, 4);
    assert!(matches!(
        detection.offset,
        Some(OffsetEstimate::FixedHz { hz }) if (hz - 12_500.0).abs() < 100.0
    ));
}

#[test]
fn exposes_weak_offset_candidates_for_tracking() {
    let profile = fixed_profile(2);
    let peaks = vec![peak(1, 816_007_500.0, -84.0, -88.0)];

    let candidates = detect_profile_candidates(
        &[profile],
        &peaks,
        &MaskSet::default(),
        &ScanSettings::default(),
    );

    assert_eq!(candidates.len(), 1);
    assert!(!candidates[0].detected);
    assert_eq!(candidates[0].hits, 1);
    assert!((candidates[0].offset_group_hz.unwrap() - 7_500.0).abs() < 100.0);
}
