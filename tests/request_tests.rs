use camfinder::config::{
    apply_profile_settings, target_frequencies, AppConfig, Backend, PreferenceField, ProfilePreset,
    ScanRequest, SettingsStore,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "camfinder-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn store(&self) -> SettingsStore {
        SettingsStore::in_directories(self.0.join("config"), self.0.join("data"))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn request() -> ScanRequest {
    ScanRequest {
        preset: Some(ProfilePreset::High24And27),
        backend: Some(Backend::Synthetic),
        ..Default::default()
    }
}

#[test]
fn presets_change_targets_without_overwriting_receiver_or_scan_settings() {
    let mut config = AppConfig::default();
    config.device.backend = Some("soapy".into());
    config.scan.min_peak_snr_db = 9.0;
    let device = config.device.clone();
    let scan = config.scan.clone();
    for (preset, count) in ProfilePreset::ALL.into_iter().zip([7, 5, 12, 4, 4]) {
        assert_eq!(config.preset, Some(preset));
        let resolved = ScanRequest::default()
            .resolve_config(config.clone())
            .unwrap();
        assert_eq!(target_frequencies(&resolved.engine.profiles).len(), count);
        assert_eq!(resolved.engine.device, device);
        assert_eq!(resolved.engine.scan, scan);
        config.adjust(PreferenceField::Profile, 1);
    }
    assert_eq!(config.preset, Some(ProfilePreset::High24And27));
}

#[test]
fn configuration_precedence_is_defaults_then_profile_then_request() {
    let mut base: AppConfig = toml::from_str(AppConfig::default_toml()).unwrap();
    base.scan.min_peak_snr_db = 99.0;
    base.device.airspy.gain.vga = 0;
    let configured = ScanRequest {
        min_peak_snr: Some(7.0),
        vga: Some(9),
        fft_frames: Some(5),
        preset: None,
        profile: Some(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("profiles/best_high_airspy.toml"),
        ),
        ..request()
    }
    .resolve_config(base)
    .unwrap();
    assert_eq!(configured.engine.scan.min_peak_snr_db, 7.0);
    assert_eq!(configured.engine.device.airspy.gain.vga, 9);
    assert_eq!(configured.engine.device.airspy.sample_rate_hz, 3_000_000);
    assert_eq!(configured.engine.device.airspy.fft_frames, Some(5));
    assert_eq!(configured.engine.scan.peak_search_window_hz, 250_000.0);
}

#[test]
fn incompatible_profile_settings_are_reported_instead_of_selecting_one() {
    let mut profiles = ProfilePreset::All.profiles().unwrap();
    let mut base = AppConfig::default();
    for profile in &mut profiles {
        profile.scan = Some(base.scan.clone());
        profile.device = Some(base.device.clone());
    }
    profiles[1].scan.as_mut().unwrap().min_peak_snr_db += 1.0;
    assert!(apply_profile_settings(&mut base, &profiles)
        .unwrap_err()
        .to_string()
        .contains("conflicting scan"));
    profiles[1].scan = profiles[0].scan.clone();
    profiles[1].device.as_mut().unwrap().airspy.sample_rate_hz += 1;
    assert!(apply_profile_settings(&mut base, &profiles)
        .unwrap_err()
        .to_string()
        .contains("conflicting device"));
}

#[test]
fn invalid_requests_fail_before_opening_a_device() {
    for invalid in [
        ScanRequest {
            fft_frames: Some(0),
            backend: Some(Backend::Airspy),
            ..request()
        },
        ScanRequest {
            fft_frames: Some(2),
            dwell_ms: Some(100),
            ..request()
        },
        ScanRequest {
            profile_freqs: Some("800M".into()),
            ..request()
        },
        ScanRequest {
            once: true,
            continuous: true,
            ..request()
        },
        ScanRequest {
            confirm_ratio: Some(f64::NAN),
            ..request()
        },
        ScanRequest {
            search_window: Some("NaN".into()),
            ..request()
        },
        ScanRequest {
            airspy_sample_rate: Some("-1".into()),
            ..request()
        },
        ScanRequest {
            duration: Some("inf".into()),
            ..request()
        },
    ] {
        assert!(
            invalid.resolve_config(AppConfig::default()).is_err(),
            "{invalid:?}"
        );
    }
}

#[test]
fn edited_settings_roundtrip_without_persisting_recording_paths() {
    let dir = Directory::new();
    let store = dir.store();
    let mut config = store.load().unwrap();
    config.adjust(PreferenceField::Gain, 100);
    assert_eq!(config.device.airspy.gain.vga, 15);
    config.adjust(PreferenceField::Gain, -100);
    assert_eq!(config.device.airspy.gain.vga, 0);
    config.adjust(PreferenceField::Profile, -1);
    assert_eq!(config.preset, Some(ProfilePreset::High27));
    config.adjust(PreferenceField::Recording, -1);
    store.save(&config).unwrap();
    let restored = store.load().unwrap();
    assert_eq!(
        serde_json::to_value(&restored).unwrap(),
        serde_json::to_value(&config).unwrap()
    );
    assert_eq!(restored.record_label(), "Spectrum");
    assert!(restored.scan.record_path.is_none());
    let first = store
        .resolve(&restored, true)
        .unwrap()
        .engine
        .scan
        .record_path
        .unwrap();
    let second = store
        .resolve(&restored, true)
        .unwrap()
        .engine
        .scan
        .record_path
        .unwrap();
    assert_ne!(first, second);
    assert!(first.starts_with(dir.0.join("data/camfinder/recordings")));
    assert!(!dir.0.join("data").exists());
}

#[test]
fn request_preserves_backend_overrides_masks_and_run_policy() {
    let plan = ScanRequest {
        backend: Some(Backend::Soapy),
        soapy_sample_rate: Some("2M".into()),
        soapy_gain_element: vec!["LNA=7.5".into()],
        fft_frames: Some(3),
        mask: vec!["840M:20k".into()],
        duration: Some("2m".into()),
        repeat_delay: Some("10ms".into()),
        ..request()
    }
    .resolve_config(AppConfig::default())
    .unwrap();
    assert_eq!(plan.engine.device.soapy.sample_rate_hz, 2_000_000);
    assert_eq!(plan.engine.device.soapy.gain.mode, "manual");
    assert_eq!(plan.engine.device.soapy.gain.elements[0].gain_db, 7.5);
    assert_eq!(plan.engine.masks[0].width_hz, 20_000.0);
    assert!(plan.run.repeat);
    assert_eq!(plan.run.duration.unwrap().as_secs(), 120);
    assert_eq!(plan.run.repeat_delay.as_millis(), 10);
}

#[test]
fn toml_preserves_soapy_parameters_when_loading_editing_and_saving() {
    let dir = Directory::new();
    let path = dir.0.join("scan.toml");
    std::fs::write(
        &path,
        r#"
preset = "24+27"
[device]
backend = "soapy"
[device.soapy]
device_args = "driver=airspy"
channel = 1
sample_rate_hz = 3000000
fft_size = 32768
fft_frames = 5
capture_timeout_ms = 1700
antenna = "RX"
bandwidth_hz = 2000000.0
[device.soapy.gain]
mode = "none"
gain_db = 12.5
[[masks]]
center_hz = 864000000.0
width_hz = 1000.0
label = "local interference"
"#,
    )
    .unwrap();
    let store = SettingsStore::for_path(path.clone());
    let mut config = store.load().unwrap();
    assert_eq!(config.gain_label(), "Driver default");
    let app = store.resolve(&config, false).unwrap();
    let cli = ScanRequest::default().resolve(Some(&path)).unwrap();
    assert_eq!(app.engine.device, cli.engine.device);
    assert_eq!(app.engine.scan, cli.engine.scan);
    assert_eq!(app.engine.device.backend.as_deref(), Some("soapy"));
    let device = app.engine.device.clone();
    config.adjust(PreferenceField::Profile, 1);
    store.save(&config).unwrap();
    let restored = store.load().unwrap();
    let changed = store.resolve(&restored, false).unwrap();
    assert_eq!(changed.engine.device, device);
    assert_eq!(target_frequencies(&changed.engine.profiles).len(), 5);
    assert_eq!(
        changed.engine.masks[0].label.as_deref(),
        Some("local interference")
    );
    config.device.soapy.gain.mode = "agc".into();
    assert_eq!(config.gain_label(), "Auto");
    config.adjust(PreferenceField::Gain, 1);
    assert_eq!(config.gain_label(), "13.5 dB");
    store.save(&config).unwrap();
    let adjusted = store.load().unwrap();
    assert_eq!(adjusted.device.soapy.gain.mode, "manual");
    assert_eq!(adjusted.device.soapy.gain.gain_db, Some(13.5));
    let before = std::fs::read(&path).unwrap();
    config.device.soapy.fft_frames = Some(0);
    assert!(store.save(&config).is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn referenced_profiles_resolve_relative_paths_and_save_without_external_dependencies() {
    let dir = Directory::new();
    let profile_dir = dir.0.join("profiles");
    std::fs::create_dir(&profile_dir).unwrap();
    let profile_path = profile_dir.join("receiver.toml");
    let profile = r#"
name = "custom"
freqs_mhz = [800.0, 825.0, 850.0]
[device]
backend = "soapy"
[device.soapy]
device_args = "driver=uhd"
sample_rate_hz = 2000000
[device.peaks_file]
path = "input.json"
[scan]
min_peak_snr_db = 9.0
record_path = "recordings/session.jsonl"
"#;
    let path = dir.0.join("config.toml");
    let store = SettingsStore::for_path(path.clone());
    let raw = "profile_files = ['profiles/receiver.toml']\n";
    for load_from_file in [false, true] {
        std::fs::write(&profile_path, profile).unwrap();
        std::fs::write(&path, raw).unwrap();
        let config = if load_from_file {
            store.load().unwrap()
        } else {
            toml::from_str(raw).unwrap()
        };
        let resolved = store.resolve(&config, false).unwrap();
        store.save(&config).unwrap();
        assert_eq!(std::fs::read_to_string(&profile_path).unwrap(), profile);
        std::fs::remove_file(&profile_path).unwrap();
        let mut restored = store.load().unwrap();
        assert_eq!(restored.profile_label(), "Custom");
        assert!(restored.profile_files.is_empty());
        assert_eq!(restored.profiles.len(), 1);
        assert_eq!(restored.profiles[0].name, "custom");
        assert!(restored.profiles[0].scan.is_none());
        assert!(restored.profiles[0].device.is_none());
        assert_eq!(
            restored.scan.record_path,
            Some(profile_dir.join("recordings/session.jsonl"))
        );
        assert_eq!(
            restored.device.peaks_file.path,
            Some(profile_dir.join("input.json"))
        );
        assert_eq!(restored.device, resolved.engine.device);
        assert_eq!(restored.scan, resolved.engine.scan);
        restored.adjust(PreferenceField::Gain, 3);
        store.save(&restored).unwrap();
        let plan = ScanRequest::default().resolve(Some(&path)).unwrap();
        assert_eq!(plan.engine.device.backend.as_deref(), Some("soapy"));
        assert_eq!(plan.engine.device.soapy.gain.gain_db, Some(3.0));
        assert_eq!(plan.engine.scan.min_peak_snr_db, 9.0);
    }
}

#[test]
fn partial_device_settings_preserve_defaults_and_explicit_capture_budgets() {
    let dir = Directory::new();
    let path = dir.0.join("config.toml");
    for (device, frames) in [
        ("", Some(14)),
        ("[device.airspy.gain]\nvga = 7\n", Some(14)),
        ("[device.airspy]\ndwell_ms = 125\n", None),
        ("[device.airspy]\ndwell_ms = 125\nfft_frames = 3\n", Some(3)),
    ] {
        std::fs::write(&path, format!("preset = '24+27'\n{device}")).unwrap();
        let config = AppConfig::load(&path).unwrap();
        let resolved = ScanRequest::default().resolve(Some(&path)).unwrap();
        assert_eq!(resolved.engine.device.backend.as_deref(), Some("airspy"));
        assert_eq!(resolved.engine.device.airspy.fft_frames, frames);
        assert_eq!(config.scan, AppConfig::default().scan);
    }
}

#[test]
fn configured_profile_defaults_apply_to_inline_files_and_request_profiles() {
    let dir = Directory::new();
    let path = dir.0.join("config.toml");
    let profile_path = dir.0.join("profile.toml");
    let profile = "name = 'custom'\nfreqs_mhz = [800.0, 825.0]\nmin_score = 17.0\n";
    std::fs::write(&profile_path, profile).unwrap();
    for selection in [
        "profile_files = ['profile.toml']\n".to_string(),
        format!("[[profiles]]\n{profile}"),
    ] {
        std::fs::write(
            &path,
            format!("{selection}\n[profile_defaults]\nmin_hits = 1\nmin_score = 99.0\nmax_offset_spread_hz = 300.0\n[device]\nbackend = 'synthetic'\n"),
        )
        .unwrap();
        for request in [
            ScanRequest::default(),
            ScanRequest {
                profile: Some(profile_path.clone()),
                ..Default::default()
            },
        ] {
            let resolved = request.resolve(Some(&path)).unwrap();
            let profile = &resolved.engine.profiles[0];
            assert_eq!(profile.min_hits, 1);
            assert_eq!(profile.min_score, 17.0);
            assert_eq!(profile.max_offset_spread_hz, 300.0);
        }
    }
}

#[test]
fn invalid_configurations_fail_without_overwriting_saved_settings() {
    let dir = Directory::new();
    let store = dir.store();
    store.save(&AppConfig::default()).unwrap();
    let saved = dir.0.join("config/camfinder/config.toml");
    let before = std::fs::read(&saved).unwrap();
    for profile in [
        "name = ''\n[[profiles.groups]]\nname = 'group'\nfreqs_mhz = [800.0]",
        "name = 'mixed'\nfreqs_mhz = [825.0]\n[[profiles.groups]]\nname = 'group'\nfreqs_mhz = [800.0]",
        "name = 'empty group'\n[[profiles.groups]]\nname = ''\nfreqs_mhz = [800.0]",
    ] {
        let raw = format!("[device]\nbackend = 'synthetic'\n[[profiles]]\n{profile}");
        let path = dir.0.join("invalid.toml");
        std::fs::write(&path, &raw).unwrap();
        assert!(AppConfig::load(&path).is_err(), "{profile}");
        let config: AppConfig = toml::from_str(&raw).unwrap();
        assert!(ScanRequest::default().resolve_config(config.clone()).is_err());
        assert!(store.save(&config).is_err());
        assert_eq!(std::fs::read(&saved).unwrap(), before);
    }
    std::fs::write(&saved, "broken").unwrap();
    assert!(store.load().is_err());
    assert!(SettingsStore::for_path(dir.0.join("missing.toml"))
        .load()
        .is_err());
}
