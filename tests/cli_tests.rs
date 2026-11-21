#![cfg(feature = "cli")]

use std::process::Command;

#[test]
fn explicit_scans_and_default_config_work_outside_the_repository() {
    let profile = format!(
        "{}/profiles/best_high_airspy.toml",
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
