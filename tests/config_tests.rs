use camfinder::config::{
    target_frequencies, AppConfig, DetectionProfile, OffsetModel, ProfilePreset,
};

#[test]
fn embedded_defaults_match_partial_profiles() {
    let config = AppConfig::default();
    assert_eq!(config.preset, Some(ProfilePreset::High24And27));
    assert!(config.profile_files.is_empty());
    assert_eq!(
        target_frequencies(&config.selected_profiles().unwrap()).len(),
        7
    );
    assert_eq!(config.device.backend.as_deref(), Some("airspy"));
    assert_eq!(config.device.airspy.dwell_ms, 300);
    assert_eq!(config.device.airspy.fft_frames, Some(14));
    assert_eq!(config.device.airspy.settle_ms, 40);
    assert_eq!(config.device.soapy.sample_rate_hz, 2_400_000);
    let profile: DetectionProfile = toml::from_str(
        r#"
        name = "minimal"
        freqs_mhz = [864.0]
        "#,
    )
    .unwrap();

    assert_eq!(profile.min_hits, config.profile_defaults.min_hits);
    assert_eq!(profile.min_score, config.profile_defaults.min_score);
    assert_eq!(
        profile.max_offset_spread_hz,
        config.profile_defaults.max_offset_spread_hz
    );
}

#[test]
fn grouped_profile_expands_into_detection_profiles() {
    let profile: DetectionProfile = toml::from_str(
        r#"
        name = "best"
        offset_model = "ppm"
        min_hits = 3
        min_score = 20.0
        max_offset_spread_hz = 1200.0

        [[groups]]
        name = "24mhz"
        freqs_mhz = [792.0, 816.0]

        [[groups]]
        name = "27mhz"
        freqs_mhz = [810.0, 837.0]
        min_hits = 2
        "#,
    )
    .unwrap();

    let groups = profile.detection_profiles();

    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].name, "best:24mhz");
    assert_eq!(groups[0].freqs_mhz, vec![792.0, 816.0]);
    assert_eq!(groups[0].offset_model, OffsetModel::Ppm);
    assert_eq!(groups[0].min_hits, 3);
    assert_eq!(groups[1].name, "best:27mhz");
    assert_eq!(groups[1].freqs_mhz, vec![810.0, 837.0]);
    assert_eq!(groups[1].min_hits, 2);
}
