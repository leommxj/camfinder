use camfinder::detect::{OffsetEstimate, ProfileDetection};
use camfinder::persistence::{PersistenceTracker, StabilityState};

fn result(profile: &str, detected: bool) -> ProfileDetection {
    result_at(profile, detected, 12_500.0)
}

fn result_at(profile: &str, detected: bool, offset_hz: f64) -> ProfileDetection {
    ProfileDetection {
        suspect_key: if detected {
            format!("{profile}@{offset_hz:+.0}Hz")
        } else {
            format!("{profile}@none")
        },
        profile: profile.to_string(),
        detected,
        score: if detected { 10.0 } else { 0.0 },
        hits: if detected { 2 } else { 0 },
        required_hits: 2,
        min_score: 8.0,
        snr_sum_db: if detected { 20.0 } else { 0.0 },
        snr_avg_db: if detected { 10.0 } else { 0.0 },
        offset_spread_hz: 0.0,
        offset: if detected {
            Some(OffsetEstimate::FixedHz { hz: offset_hz })
        } else {
            None
        },
        offset_group_hz: detected.then_some(offset_hz),
        offset_tolerance_hz: 1_000.0,
        hit_details: Vec::new(),
        misses_mhz: Vec::new(),
    }
}

fn weak_result_at(profile: &str, offset_hz: f64) -> ProfileDetection {
    let mut result = result_at(profile, true, offset_hz);
    result.detected = false;
    result.score = 4.75;
    result.hits = 1;
    result.snr_sum_db = 4.0;
    result.snr_avg_db = 4.0;
    result
}

#[test]
fn requires_persistent_hits_for_stable_state() {
    let mut tracker = PersistenceTracker::new(3, 0.6);
    tracker.update(&[result("p", true)]);
    assert_eq!(tracker.summaries()[0].state, StabilityState::Candidate);

    tracker.update(&[result("p", false)]);
    assert_eq!(tracker.summaries()[0].state, StabilityState::Transient);

    tracker.update(&[result("p", true)]);
    tracker.update(&[result("p", true)]);
    assert_eq!(tracker.summaries()[0].state, StabilityState::Stable);
}

#[test]
fn tracks_distinct_offsets_independently() {
    let mut tracker = PersistenceTracker::new(3, 0.6);
    tracker.update(&[result_at("p", true, 7_500.0)]);
    tracker.update(&[result_at("p", true, 4_000.0)]);

    let summaries = tracker.summaries();
    assert_eq!(summaries.len(), 2);
    assert!((summaries[0].offset_group_hz.unwrap() - 7_500.0).abs() < 100.0);
    assert!((summaries[1].offset_group_hz.unwrap() - 4_000.0).abs() < 100.0);
    assert!(summaries.iter().any(|summary| {
        summary.state == StabilityState::Transient
            && (summary.offset_group_hz.unwrap() - 7_500.0).abs() < 100.0
    }));
    assert!(summaries.iter().any(|summary| {
        summary.state == StabilityState::Candidate
            && (summary.offset_group_hz.unwrap() - 4_000.0).abs() < 100.0
    }));
}

#[test]
fn weak_same_offset_support_keeps_existing_suspect_tracking() {
    let mut tracker = PersistenceTracker::new(3, 0.6);
    tracker.update(&[result_at("p", true, 7_500.0)]);
    tracker.update(&[weak_result_at("p", 7_700.0)]);

    let summary = &tracker.summaries()[0];
    assert_eq!(summary.state, StabilityState::Tracking);
    assert_eq!(summary.detected_passes, 1);
    assert_eq!(summary.supported_passes, 2);
    assert_eq!(summary.missed_passes, 0);
    assert!(summary.last_weak);
}

#[test]
fn merges_split_suspect_when_jitter_returns_to_overlapping_window() {
    let mut tracker = PersistenceTracker::new(3, 0.6);
    tracker.update(&[result_at("p", true, 0.0)]);
    tracker.update(&[result_at("p", true, 1_900.0)]);
    assert_eq!(tracker.summaries().len(), 2);

    tracker.update(&[result_at("p", true, 950.0)]);

    let summaries = tracker.summaries();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].display_order, 0);
    assert!((summaries[0].offset_group_hz.unwrap() - 950.0).abs() < 100.0);
}
