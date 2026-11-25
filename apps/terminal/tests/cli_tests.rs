use std::process::Command;

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_camfinder"));
    command.env(
        "XDG_CONFIG_HOME",
        std::env::temp_dir().join(format!("camfinder-cli-default-{}", std::process::id())),
    );
    command
}

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
        let output = command()
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
fn cli_reads_the_same_saved_toml_as_the_device_application() {
    use camfinder::config::{AppConfig, PreferenceField, SettingsStore};
    use camfinder::engine::ScanEngine;
    use std::sync::{atomic::AtomicBool, Arc};
    let directory =
        std::env::temp_dir().join(format!("camfinder-cli-config-{}", std::process::id()));
    let mut config = AppConfig::default();
    config.adjust(PreferenceField::Profile, 2);
    config.device.backend = Some("soapy".into());
    config.device.soapy.device_args = "driver=airspy".into();
    config.device.soapy.sample_rate_hz = 3_000_000;
    let store = SettingsStore::in_directories(directory.clone(), directory.clone());
    store.save(&config).unwrap();
    let resolved = store.resolve(&store.load().unwrap(), true).unwrap();
    let expected = ScanEngine::new(resolved.engine)
        .unwrap()
        .scan(&Arc::new(AtomicBool::new(false)), &mut |_| {}, &mut |_| {})
        .unwrap()
        .unwrap();
    let output = command()
        .current_dir(std::env::temp_dir())
        .env("XDG_CONFIG_HOME", &directory)
        .args([
            "scan",
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
    let shown = command()
        .env("XDG_CONFIG_HOME", &directory)
        .arg("show-config")
        .output()
        .unwrap();
    assert!(shown.status.success());
    let loaded: AppConfig = toml::from_str(std::str::from_utf8(&shown.stdout).unwrap()).unwrap();
    assert_eq!(loaded.device, config.device);
    assert_eq!(loaded.preset, config.preset);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn cli_reports_invalid_duration_without_panicking() {
    let output = command()
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
