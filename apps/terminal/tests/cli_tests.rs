use std::process::Command;

#[test]
fn explicit_scans_and_default_config_work_outside_the_repository() {
    let profile = format!(
        "{}/../../profiles/best_high_airspy.toml",
        env!("CARGO_MANIFEST_DIR")
    );
    for args in [
        vec!["default-config"],
        vec![
            "scan",
            "--backend",
            "synthetic",
            "--profile-freqs",
            "800M,825M,850M",
            "--once",
            "--output",
            "json",
        ],
        vec![
            "scan",
            "--backend",
            "synthetic",
            "--profile",
            &profile,
            "--once",
            "--output",
            "json",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_camfinder"))
            .current_dir(std::env::temp_dir())
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if args[0] == "scan" {
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(!result["peaks"].as_array().unwrap().is_empty());
        }
    }
}

#[test]
fn cli_preset_matches_the_shared_preferences_engine() {
    use camfinder::config::{PreferenceField, ScanPreferences, SettingsStore};
    use camfinder::engine::ScanEngine;
    use std::sync::{atomic::AtomicBool, Arc};
    let mut preferences = ScanPreferences::default();
    preferences.adjust(PreferenceField::Profile, 2);
    let store = SettingsStore::in_directories(std::env::temp_dir(), std::env::temp_dir());
    let resolved = preferences.resolve(&store, true).unwrap();
    let expected = ScanEngine::new(resolved.engine)
        .unwrap()
        .scan(&Arc::new(AtomicBool::new(false)), &mut |_| {}, &mut |_| {})
        .unwrap()
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_camfinder"))
        .current_dir(std::env::temp_dir())
        .args([
            "scan",
            "--preset",
            "all",
            "--backend",
            "synthetic",
            "--synthetic-offset",
            "0",
            "--synthetic-jitter",
            "80",
            "--once",
            "--output",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        actual["peaks"],
        serde_json::to_value(&expected.peaks).unwrap()
    );
    let detected: Vec<_> = expected
        .results
        .iter()
        .filter(|result| result.detected)
        .collect();
    assert_eq!(
        actual["results"],
        serde_json::from_slice::<serde_json::Value>(&serde_json::to_vec(&detected).unwrap())
            .unwrap()
    );
}

#[test]
fn cli_reports_invalid_duration_without_panicking() {
    let output = Command::new(env!("CARGO_BIN_EXE_camfinder"))
        .args([
            "scan",
            "--preset",
            "24+27",
            "--backend",
            "synthetic",
            "--duration",
            "NaN",
            "--output",
            "json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("duration"));
    assert!(!error.contains("panicked"));
}
