use camfinder::config::{DetectionProfile, OffsetModel, ScanSettings};
use camfinder::engine::{EngineConfig, ScanEngine, ScanSession, SessionCommand, SessionStatus};
use camfinder::sdr::DeviceSettings;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::{Duration, Instant};

#[test]
fn observed_single_pass_matches_the_synchronous_engine() {
    use camfinder::engine::RunOptions;
    let mut engine = ScanEngine::new(config()).unwrap();
    let expected = engine
        .scan(&Arc::new(AtomicBool::new(false)), &mut |_| {}, &mut |_| {})
        .unwrap()
        .unwrap();
    let (mut session, passes) = ScanSession::observe(
        config(),
        RunOptions {
            repeat: false,
            ..Default::default()
        },
    )
    .unwrap();
    let pass = passes.recv_timeout(Duration::from_secs(3)).unwrap();
    wait_for(&session, |snapshot| {
        snapshot.status == SessionStatus::Stopped
    });
    assert_eq!(
        serde_json::to_value(&expected.results).unwrap(),
        serde_json::to_value(&pass.results).unwrap()
    );
    assert_eq!(session.snapshot().pass, 1);
    assert!(passes.try_recv().is_err());
    session.stop().unwrap();
}

#[test]
fn duration_expires_while_paused_and_during_a_long_repeat_delay() {
    use camfinder::engine::RunOptions;
    for paused in [false, true] {
        let mut session = ScanSession::start_with_options(
            config(),
            RunOptions {
                duration: Some(Duration::from_millis(300)),
                repeat_delay: Duration::from_secs(30),
                ..Default::default()
            },
        )
        .unwrap();
        wait_for(&session, |snapshot| snapshot.pass == 1);
        if paused {
            session.command(SessionCommand::Pause);
        }
        wait_for(&session, |snapshot| {
            snapshot.status == SessionStatus::Stopped
        });
        assert_eq!(session.snapshot().pass, 1);
        assert!(session.snapshot().elapsed_seconds < 2.0);
        session.stop().unwrap();
    }
}

#[test]
fn stopping_an_unread_observer_does_not_block_on_its_full_queue() {
    let (mut session, _passes) = ScanSession::observe(config(), Default::default()).unwrap();
    wait_for(&session, |snapshot| snapshot.pass >= 2);
    let start = Instant::now();
    session.stop().unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn applying_invalid_configuration_keeps_the_current_session_running() {
    use camfinder::config::ResolvedScan;
    let mut session = ScanSession::start(config()).unwrap();
    wait_for(&session, |snapshot| snapshot.pass > 0);
    let before = session.snapshot().pass;
    let mut invalid = config();
    invalid.confirm_ratio = f64::NAN;
    assert!(session
        .apply(ResolvedScan {
            engine: invalid,
            run: Default::default(),
            output: "tui".into()
        })
        .is_err());
    wait_for(&session, |snapshot| snapshot.pass > before);
    session.stop().unwrap();
}

fn config() -> EngineConfig {
    EngineConfig {
        profiles: vec![DetectionProfile {
            name: "synthetic".into(),
            freqs_mhz: vec![800.0, 825.0, 850.0],
            offset_model: OffsetModel::FixedHz,
            min_hits: 3,
            min_score: 8.0,
            max_offset_spread_hz: 1200.0,
            groups: Vec::new(),
            scan: None,
            device: None,
        }],
        device: DeviceSettings {
            backend: Some("synthetic".into()),
            ..Default::default()
        },
        scan: ScanSettings::default(),
        ..Default::default()
    }
}

fn wait_for(session: &ScanSession, predicate: impl Fn(&camfinder::engine::ScanSnapshot) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = session.snapshot();
        assert_ne!(
            snapshot.status,
            SessionStatus::Error,
            "{:?}",
            snapshot.error
        );
        if predicate(&snapshot) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out: {snapshot:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn session_and_synchronous_engine_share_results_and_tracking() {
    let mut engine = ScanEngine::new(config()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let first = engine
        .scan(&stop, &mut |_| {}, &mut |_| {})
        .unwrap()
        .unwrap();
    assert!(first.results.iter().any(|result| result.detected));
    let mut session = ScanSession::start(config()).unwrap();
    wait_for(&session, |snapshot| snapshot.pass > 0);
    let snapshot = session.snapshot();
    assert_eq!(
        serde_json::to_value(&snapshot.results).unwrap(),
        serde_json::to_value(&first.results).unwrap()
    );
    session.command(SessionCommand::Pause);
    wait_for(&session, |snapshot| {
        snapshot.status == SessionStatus::Paused
    });
    let paused_pass = session.snapshot().pass;
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(session.snapshot().pass, paused_pass);
    session.command(SessionCommand::Resume);
    wait_for(&session, |snapshot| snapshot.pass > paused_pass);
    session.stop().unwrap();
    assert_eq!(session.snapshot().status, SessionStatus::Stopped);
}

#[test]
fn record_only_skips_matching_and_writes_complete_json_lines() {
    let path =
        std::env::temp_dir().join(format!("camfinder-recording-{}.jsonl", std::process::id()));
    let mut config = config();
    config.record_only = true;
    config.scan.record_path = Some(path.clone());
    let mut engine = ScanEngine::new(config).unwrap();
    let pass = engine
        .scan(&Arc::new(AtomicBool::new(false)), &mut |_| {}, &mut |_| {})
        .unwrap()
        .unwrap();
    assert!(!pass.peaks.is_empty());
    assert!(pass.results.is_empty());
    assert!(pass.persistence.is_empty());
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.ends_with('\n'));
    let record: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(record["format_version"], 3);
    assert_eq!(record["type"], "scan_pass");
    assert_eq!(record["peaks"], serde_json::to_value(&pass.peaks).unwrap());
    assert_eq!(record["results"], serde_json::json!([]));
    assert!(record["statistics"].is_array());
    drop(engine);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn backend_failure_reaches_session_and_stop() {
    let mut config = config();
    config.device.backend = Some("unavailable".into());
    let mut session = ScanSession::start(config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while session.snapshot().status != SessionStatus::Error {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(session
        .snapshot()
        .error
        .unwrap()
        .contains("unknown backend"));
    assert!(session.stop().is_err());
}

#[test]
fn grouped_profiles_scan_and_report_all_targets() {
    let mut config = config();
    config.profiles = vec![toml::from_str(
        r#"
        name = "grouped"
        min_hits = 3
        [[groups]]
        name = "first"
        freqs_mhz = [800.0, 825.0, 850.0]
        [[groups]]
        name = "second"
        freqs_mhz = [825.0, 850.0, 875.0]
        "#,
    )
    .unwrap()];
    let mut engine = ScanEngine::new(config.clone()).unwrap();
    let pass = engine
        .scan(&Arc::new(AtomicBool::new(false)), &mut |_| {}, &mut |_| {})
        .unwrap()
        .unwrap();
    assert!(pass
        .results
        .iter()
        .any(|result| result.detected && result.profile == "grouped:first"));
    assert!(pass
        .results
        .iter()
        .any(|result| result.detected && result.profile == "grouped:second"));
    let mut session = ScanSession::start(config).unwrap();
    wait_for(&session, |snapshot| snapshot.pass > 0);
    assert_eq!(session.snapshot().total_targets, 4);
    session.stop().unwrap();
}
