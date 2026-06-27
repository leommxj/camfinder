use crate::detect::{OffsetEstimate, ProfileDetection};
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StabilityState {
    Stable,
    Candidate,
    Tracking,
    Transient,
    Absent,
}

#[derive(Clone, Debug, Serialize)]
pub struct PersistenceSummary {
    pub suspect_key: String,
    pub display_order: usize,
    pub profile: String,
    pub state: StabilityState,
    pub offset_group_hz: Option<f64>,
    pub offset_tolerance_hz: f64,
    pub total_passes: usize,
    pub detected_passes: usize,
    pub detection_ratio: f64,
    pub consecutive_detected: usize,
    pub supported_passes: usize,
    pub consecutive_supported: usize,
    pub missed_passes: usize,
    pub confirm_passes: usize,
    pub confirm_ratio: f64,
    pub last_detected: bool,
    pub last_supported: bool,
    pub last_weak: bool,
    pub last_hits: usize,
    pub last_score: f64,
    pub min_score: f64,
    pub last_snr_sum_db: f64,
    pub last_snr_avg_db: f64,
    pub last_offset_spread_hz: f64,
    pub best_score: f64,
    pub last_offset: Option<OffsetEstimate>,
}

#[derive(Clone, Debug)]
pub struct PersistenceTracker {
    stats: BTreeMap<String, ProfilePersistence>,
    confirm_passes: usize,
    confirm_ratio: f64,
    next_key_id: usize,
    next_display_order: usize,
}

impl PersistenceTracker {
    pub fn new(confirm_passes: usize, confirm_ratio: f64) -> Self {
        Self {
            stats: BTreeMap::new(),
            confirm_passes: confirm_passes.max(1),
            confirm_ratio: confirm_ratio.clamp(0.0, 1.0),
            next_key_id: 1,
            next_display_order: 0,
        }
    }

    pub fn update(&mut self, results: &[ProfileDetection]) {
        let mut updated_keys = HashSet::new();

        for result in results {
            if result.offset_group_hz.is_none() {
                continue;
            }

            let matched_key = self.matching_key(result);
            let key = if result.detected {
                matched_key.unwrap_or_else(|| self.allocate_key(result))
            } else if let Some(key) = matched_key {
                key
            } else {
                continue;
            };

            if self.stats.contains_key(&key) {
                if let Some(stat) = self.stats.get_mut(&key) {
                    if result.detected {
                        stat.update_detected(result);
                    } else {
                        stat.update_weak(result);
                    }
                }
            } else {
                let order = self.next_display_order;
                self.next_display_order += 1;
                let mut stat = ProfilePersistence::new(
                    key.clone(),
                    order,
                    result,
                    self.confirm_passes,
                    self.confirm_ratio,
                );
                stat.update_detected(result);
                self.stats.insert(key.clone(), stat);
            }
            self.merge_duplicate_matches(result, &key);
            updated_keys.insert(key);
        }

        for (key, stat) in &mut self.stats {
            if !updated_keys.contains(key) {
                stat.update_missing();
            }
        }
    }

    pub fn summaries(&self) -> Vec<PersistenceSummary> {
        let mut summaries = self
            .stats
            .values()
            .map(ProfilePersistence::summary)
            .collect::<Vec<_>>();
        summaries.sort_by(compare_summaries);
        summaries
    }

    fn matching_key(&self, result: &ProfileDetection) -> Option<String> {
        self.matching_candidates(result)
            .into_iter()
            .min_by(compare_match_candidates)
            .map(|candidate| candidate.key)
    }

    fn matching_candidates(&self, result: &ProfileDetection) -> Vec<MatchCandidate> {
        let Some(offset_group_hz) = result.offset_group_hz else {
            return Vec::new();
        };

        self.stats
            .iter()
            .filter(|(_, stat)| stat.profile == result.profile)
            .filter_map(|(key, stat)| {
                let known_hz = stat.offset_group_hz?;
                let tolerance = stat
                    .offset_tolerance_hz
                    .max(result.offset_tolerance_hz)
                    .max(1.0);
                let distance = (known_hz - offset_group_hz).abs();
                (distance <= tolerance).then(|| MatchCandidate {
                    key: key.clone(),
                    display_order: stat.display_order,
                    distance_hz: distance,
                })
            })
            .collect()
    }

    fn allocate_key(&mut self, result: &ProfileDetection) -> String {
        let base = result.suspect_key.clone();
        if !self.stats.contains_key(&base) {
            return base;
        }

        loop {
            let key = format!("{}#{}", base, self.next_key_id);
            self.next_key_id += 1;
            if !self.stats.contains_key(&key) {
                return key;
            }
        }
    }

    fn merge_duplicate_matches(&mut self, result: &ProfileDetection, keeper_key: &str) {
        let duplicate_keys = self
            .matching_candidates(result)
            .into_iter()
            .map(|candidate| candidate.key)
            .filter(|key| key != keeper_key)
            .collect::<Vec<_>>();

        for duplicate_key in duplicate_keys {
            let Some(duplicate) = self.stats.remove(&duplicate_key) else {
                continue;
            };

            if let Some(keeper) = self.stats.get_mut(keeper_key) {
                keeper.merge_duplicate(duplicate);
            } else {
                self.stats.insert(duplicate_key, duplicate);
            }
        }
    }
}

struct MatchCandidate {
    key: String,
    display_order: usize,
    distance_hz: f64,
}

fn compare_match_candidates(left: &MatchCandidate, right: &MatchCandidate) -> std::cmp::Ordering {
    left.display_order
        .cmp(&right.display_order)
        .then_with(|| {
            left.distance_hz
                .partial_cmp(&right.distance_hz)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| left.key.cmp(&right.key))
}

#[derive(Clone, Debug)]
struct ProfilePersistence {
    suspect_key: String,
    display_order: usize,
    profile: String,
    offset_group_hz: Option<f64>,
    offset_tolerance_hz: f64,
    confirm_passes: usize,
    confirm_ratio: f64,
    total_passes: usize,
    detected_passes: usize,
    consecutive_detected: usize,
    supported_passes: usize,
    consecutive_supported: usize,
    missed_passes: usize,
    last_detected: bool,
    last_supported: bool,
    last_weak: bool,
    last_hits: usize,
    last_score: f64,
    min_score: f64,
    last_snr_sum_db: f64,
    last_snr_avg_db: f64,
    last_offset_spread_hz: f64,
    best_score: f64,
    last_offset: Option<OffsetEstimate>,
}

impl ProfilePersistence {
    fn new(
        suspect_key: String,
        display_order: usize,
        result: &ProfileDetection,
        confirm_passes: usize,
        confirm_ratio: f64,
    ) -> Self {
        Self {
            suspect_key,
            display_order,
            profile: result.profile.clone(),
            offset_group_hz: result.offset_group_hz,
            offset_tolerance_hz: result.offset_tolerance_hz,
            confirm_passes,
            confirm_ratio,
            total_passes: 0,
            detected_passes: 0,
            consecutive_detected: 0,
            supported_passes: 0,
            consecutive_supported: 0,
            missed_passes: 0,
            last_detected: false,
            last_supported: false,
            last_weak: false,
            last_hits: 0,
            last_score: 0.0,
            min_score: result.min_score,
            last_snr_sum_db: 0.0,
            last_snr_avg_db: 0.0,
            last_offset_spread_hz: 0.0,
            best_score: 0.0,
            last_offset: None,
        }
    }

    fn update_detected(&mut self, result: &ProfileDetection) {
        self.total_passes += 1;
        self.last_detected = true;
        self.last_hits = result.hits;
        self.last_score = result.score;
        self.min_score = result.min_score;
        self.last_snr_sum_db = result.snr_sum_db;
        self.last_snr_avg_db = result.snr_avg_db;
        self.last_offset_spread_hz = result.offset_spread_hz;
        self.best_score = self.best_score.max(result.score);
        self.offset_group_hz = result.offset_group_hz;
        self.offset_tolerance_hz = result.offset_tolerance_hz;
        self.detected_passes += 1;
        self.consecutive_detected += 1;
        self.supported_passes += 1;
        self.consecutive_supported += 1;
        self.missed_passes = 0;
        self.last_supported = true;
        self.last_weak = false;
        self.last_offset = result.offset;
    }

    fn update_weak(&mut self, result: &ProfileDetection) {
        self.total_passes += 1;
        self.last_detected = false;
        self.last_supported = true;
        self.last_weak = true;
        self.last_hits = result.hits;
        self.last_score = result.score;
        self.min_score = result.min_score;
        self.last_snr_sum_db = result.snr_sum_db;
        self.last_snr_avg_db = result.snr_avg_db;
        self.last_offset_spread_hz = result.offset_spread_hz;
        self.best_score = self.best_score.max(result.score);
        self.offset_group_hz = result.offset_group_hz;
        self.offset_tolerance_hz = result.offset_tolerance_hz;
        self.supported_passes += 1;
        self.consecutive_detected = 0;
        self.consecutive_supported += 1;
        self.missed_passes = 0;
        self.last_offset = result.offset;
    }

    fn merge_duplicate(&mut self, duplicate: ProfilePersistence) {
        self.display_order = self.display_order.min(duplicate.display_order);
        self.total_passes = self.total_passes.max(duplicate.total_passes);
        self.detected_passes = self.detected_passes.max(duplicate.detected_passes);
        self.supported_passes = self.supported_passes.max(duplicate.supported_passes);
        self.best_score = self.best_score.max(duplicate.best_score);
    }

    fn update_missing(&mut self) {
        self.total_passes += 1;
        self.last_detected = false;
        self.last_supported = false;
        self.last_weak = false;
        self.last_hits = 0;
        self.last_score = 0.0;
        self.last_snr_sum_db = 0.0;
        self.last_snr_avg_db = 0.0;
        self.last_offset_spread_hz = 0.0;
        self.consecutive_detected = 0;
        self.consecutive_supported = 0;
        self.missed_passes += 1;
    }

    fn summary(&self) -> PersistenceSummary {
        let detection_ratio = if self.total_passes == 0 {
            0.0
        } else {
            self.detected_passes as f64 / self.total_passes as f64
        };
        let stable = self.last_detected
            && self.detected_passes >= self.confirm_passes
            && (self.consecutive_detected >= self.confirm_passes
                || detection_ratio >= self.confirm_ratio);
        let state = if stable {
            StabilityState::Stable
        } else if self.last_detected {
            StabilityState::Candidate
        } else if self.last_supported {
            StabilityState::Tracking
        } else if self.detected_passes > 0 && self.missed_passes < self.confirm_passes {
            StabilityState::Transient
        } else {
            StabilityState::Absent
        };

        PersistenceSummary {
            suspect_key: self.suspect_key.clone(),
            display_order: self.display_order,
            profile: self.profile.clone(),
            state,
            offset_group_hz: self.offset_group_hz,
            offset_tolerance_hz: self.offset_tolerance_hz,
            total_passes: self.total_passes,
            detected_passes: self.detected_passes,
            detection_ratio,
            consecutive_detected: self.consecutive_detected,
            supported_passes: self.supported_passes,
            consecutive_supported: self.consecutive_supported,
            missed_passes: self.missed_passes,
            confirm_passes: self.confirm_passes,
            confirm_ratio: self.confirm_ratio,
            last_detected: self.last_detected,
            last_supported: self.last_supported,
            last_weak: self.last_weak,
            last_hits: self.last_hits,
            last_score: self.last_score,
            min_score: self.min_score,
            last_snr_sum_db: self.last_snr_sum_db,
            last_snr_avg_db: self.last_snr_avg_db,
            last_offset_spread_hz: self.last_offset_spread_hz,
            best_score: self.best_score,
            last_offset: self.last_offset,
        }
    }
}

fn compare_summaries(left: &PersistenceSummary, right: &PersistenceSummary) -> std::cmp::Ordering {
    left.display_order.cmp(&right.display_order)
}
