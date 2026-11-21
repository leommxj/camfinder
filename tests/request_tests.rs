use camfinder::config::{
    apply_profile_settings, target_frequencies, AppConfig, Backend, PreferenceField, ProfilePreset,
    ScanPreferences, ScanRequest, SettingsStore,
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
fn builtin_profiles_and_preferences_share_configuration_and_targets() {
    let dir = Directory::new();
    let store = dir.store();
    let mut preferences = ScanPreferences::default();
    for (index, count) in [7, 5, 12, 4, 4].into_iter().enumerate() {
        let app = preferences.resolve(&store, true).unwrap();
        let command = ScanRequest {
            preset: Some(ProfilePreset::ALL[index]),
            backend: Some(Backend::Synthetic),
            vga: Some(2),
            fft_frames: Some(14),
            capture_timeout_ms: Some(3000),
            continuous: true,
            synthetic_offset: Some("0".into()),
            synthetic_jitter: Some("80".into()),
            ..Default::default()
        }
        .resolve(None)
        .unwrap();
        assert_eq!(target_frequencies(&app.engine.profiles).len(), count);
        assert_eq!(
            target_frequencies(&app.engine.profiles),
            target_frequencies(&command.engine.profiles)
        );
        assert_eq!(app.engine.scan, command.engine.scan);
        assert_eq!(app.engine.device, command.engine.device);
        assert_eq!(
            serde_json::to_value(&app.engine.profiles).unwrap(),
            serde_json::to_value(&command.engine.profiles).unwrap()
        );
        assert_eq!(app.engine.device.airspy.fft_frames, Some(14));
        assert_eq!(app.engine.synthetic.offset_hz, 0.0);
        assert!(!dir.0.join("data").exists());
        preferences.adjust(PreferenceField::Profile, 1);
    }
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
    let mut base: AppConfig = toml::from_str(AppConfig::default_toml()).unwrap();
    profiles[1].scan.as_mut().unwrap().min_peak_snr_db += 1.0;
    assert!(apply_profile_settings(&mut base, &profiles)
        .unwrap_err()
        .to_string()
        .contains("conflicting scan"));
    profiles = ProfilePreset::All.profiles().unwrap();
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
        assert!(invalid.resolve(None).is_err(), "{invalid:?}");
    }
}

#[test]
fn saved_settings_migrate_without_changing_the_legacy_file() {
    let dir = Directory::new();
    let legacy = dir.0.join("config/camfinder-cardputer-zero/settings.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let raw = br#"{"profile":2,"gain":7,"recording":3}"#;
    std::fs::write(&legacy, raw).unwrap();
    let store = dir.store();
    let mut preferences = store.load().unwrap();
    assert_eq!(preferences.preset(), ProfilePreset::All);
    assert_eq!(preferences.gain(), 7);
    preferences.adjust(PreferenceField::Gain, 1);
    store.save(&preferences).unwrap();
    assert_eq!(store.load().unwrap().gain(), 8);
    assert_eq!(std::fs::read(legacy).unwrap(), raw);
    assert!(dir.0.join("config/camfinder/settings.json").exists());
    let resolved = preferences.resolve(&store, true).unwrap();
    assert!(resolved
        .engine
        .scan
        .record_path
        .unwrap()
        .starts_with(dir.0.join("data/camfinder/recordings")));
}

#[test]
fn invalid_saved_json_is_reported_and_adjustments_use_shared_limits() {
    let dir = Directory::new();
    let store = dir.store();
    let mut preferences = store.load().unwrap();
    preferences.adjust(PreferenceField::Gain, 100);
    assert_eq!(preferences.gain(), 15);
    preferences.adjust(PreferenceField::Gain, -100);
    assert_eq!(preferences.gain(), 0);
    preferences.adjust(PreferenceField::Profile, -1);
    assert_eq!(preferences.preset(), ProfilePreset::High27);
    preferences.adjust(PreferenceField::Recording, -1);
    assert_eq!(preferences.record_label(), "Spectrum");
    store.save(&preferences).unwrap();
    assert_eq!(store.load().unwrap(), preferences);
    std::fs::write(dir.0.join("config/camfinder/settings.json"), "broken").unwrap();
    assert!(store.load().is_err());
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
    .resolve(None)
    .unwrap();
    assert_eq!(plan.engine.device.soapy.sample_rate_hz, 2_000_000);
    assert_eq!(plan.engine.device.soapy.gain.mode, "manual");
    assert_eq!(plan.engine.device.soapy.gain.elements[0].gain_db, 7.5);
    assert_eq!(plan.engine.masks[0].width_hz, 20_000.0);
    assert!(plan.run.repeat);
    assert_eq!(plan.run.duration.unwrap().as_secs(), 120);
    assert_eq!(plan.run.repeat_delay.as_millis(), 10);
}
