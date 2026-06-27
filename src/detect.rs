use crate::config::{DetectionProfile, OffsetModel, ScanSettings};
use crate::frequency::MHZ;
use crate::mask::MaskSet;
use crate::peak::Peak;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const MAX_SUSPECTS_PER_PROFILE: usize = 8;
const MAX_WEAK_CANDIDATES_PER_PROFILE: usize = 16;
const MATCH_RESIDUAL_PENALTY_DB: f64 = 6.0;
const SCORE_RESIDUAL_PENALTY: f64 = 6.0;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OffsetEstimate {
    FixedHz { hz: f64 },
    Ppm { ppm: f64 },
}

impl OffsetEstimate {
    pub fn apply(&self, nominal_hz: f64) -> f64 {
        match self {
            OffsetEstimate::FixedHz { hz } => nominal_hz + hz,
            OffsetEstimate::Ppm { ppm } => nominal_hz * (1.0 + ppm / 1_000_000.0),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            OffsetEstimate::FixedHz { .. } => "fixed_hz",
            OffsetEstimate::Ppm { .. } => "ppm",
        }
    }

    pub fn display_value(&self) -> f64 {
        match self {
            OffsetEstimate::FixedHz { hz } => *hz,
            OffsetEstimate::Ppm { ppm } => *ppm,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DetectionHit {
    pub nominal_mhz: f64,
    pub peak_id: usize,
    pub observed_hz: f64,
    pub expected_hz: f64,
    pub error_hz: f64,
    pub snr_db: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileDetection {
    pub suspect_key: String,
    pub profile: String,
    pub detected: bool,
    pub score: f64,
    pub hits: usize,
    pub required_hits: usize,
    pub min_score: f64,
    pub snr_sum_db: f64,
    pub snr_avg_db: f64,
    pub offset_spread_hz: f64,
    pub offset: Option<OffsetEstimate>,
    pub offset_group_hz: Option<f64>,
    pub offset_tolerance_hz: f64,
    pub hit_details: Vec<DetectionHit>,
    pub misses_mhz: Vec<f64>,
}

pub fn detect_profiles(
    profiles: &[DetectionProfile],
    peaks: &[Peak],
    masks: &MaskSet,
    settings: &ScanSettings,
) -> Vec<ProfileDetection> {
    profiles
        .iter()
        .flat_map(|profile| detect_profile_suspects(profile, peaks, masks, settings))
        .collect()
}

pub fn detect_profile_candidates(
    profiles: &[DetectionProfile],
    peaks: &[Peak],
    masks: &MaskSet,
    settings: &ScanSettings,
) -> Vec<ProfileDetection> {
    profiles
        .iter()
        .flat_map(|profile| detect_profile_candidate_offsets(profile, peaks, masks, settings))
        .collect()
}

pub fn detect_profile(
    profile: &DetectionProfile,
    peaks: &[Peak],
    masks: &MaskSet,
    settings: &ScanSettings,
) -> ProfileDetection {
    let mut best = None;

    for evaluated in evaluate_profile_offsets(profile, peaks, masks, settings) {
        best = choose_better(best, evaluated);
    }

    let mut result = best.unwrap_or_else(|| empty_result(profile));
    apply_detection_threshold(profile, &mut result);
    result
}

pub fn detect_profile_suspects(
    profile: &DetectionProfile,
    peaks: &[Peak],
    masks: &MaskSet,
    settings: &ScanSettings,
) -> Vec<ProfileDetection> {
    detect_profile_candidate_offsets(profile, peaks, masks, settings)
        .into_iter()
        .filter(|result| result.detected)
        .collect()
}

fn detect_profile_candidate_offsets(
    profile: &DetectionProfile,
    peaks: &[Peak],
    masks: &MaskSet,
    settings: &ScanSettings,
) -> Vec<ProfileDetection> {
    let mut evaluated = evaluate_profile_offsets(profile, peaks, masks, settings)
        .into_iter()
        .filter(|result| !result.hit_details.is_empty())
        .collect::<Vec<_>>();

    for result in &mut evaluated {
        apply_detection_threshold(profile, result);
    }

    evaluated.sort_by(compare_detections);

    let mut candidates: Vec<ProfileDetection> = Vec::new();
    let mut detected_candidates = 0usize;
    let mut weak_candidates = 0usize;
    for candidate in evaluated {
        if candidates
            .iter()
            .any(|selected| same_offset_group(selected, &candidate))
        {
            continue;
        }

        if candidate.detected {
            if detected_candidates >= MAX_SUSPECTS_PER_PROFILE {
                continue;
            }
            detected_candidates += 1;
            candidates.push(candidate);
        } else if weak_candidates < MAX_WEAK_CANDIDATES_PER_PROFILE {
            weak_candidates += 1;
            candidates.push(candidate);
        }

        if detected_candidates >= MAX_SUSPECTS_PER_PROFILE
            && weak_candidates >= MAX_WEAK_CANDIDATES_PER_PROFILE
        {
            break;
        }
    }

    candidates
}

fn evaluate_profile_offsets(
    profile: &DetectionProfile,
    peaks: &[Peak],
    masks: &MaskSet,
    settings: &ScanSettings,
) -> Vec<ProfileDetection> {
    let active_peaks: Vec<&Peak> = peaks
        .iter()
        .filter(|peak| !masks.is_masked(peak.freq_hz))
        .collect();

    offset_seeds(profile, &active_peaks, settings.peak_search_window_hz)
        .into_iter()
        .map(|seed| evaluate_offset(profile, &active_peaks, seed))
        .collect()
}

fn offset_seeds(
    profile: &DetectionProfile,
    peaks: &[&Peak],
    search_window_hz: f64,
) -> Vec<OffsetEstimate> {
    let mut seeds = Vec::new();
    for nominal_hz in profile.nominal_freqs_hz() {
        for peak in peaks {
            if (peak.freq_hz - nominal_hz).abs() <= search_window_hz {
                let offset_hz = peak.freq_hz - nominal_hz;
                match profile.offset_model {
                    OffsetModel::FixedHz => {
                        seeds.push(OffsetEstimate::FixedHz { hz: offset_hz });
                    }
                    OffsetModel::Ppm => {
                        seeds.push(OffsetEstimate::Ppm {
                            ppm: offset_hz / nominal_hz * 1_000_000.0,
                        });
                    }
                    OffsetModel::Auto => {
                        seeds.push(OffsetEstimate::FixedHz { hz: offset_hz });
                        seeds.push(OffsetEstimate::Ppm {
                            ppm: offset_hz / nominal_hz * 1_000_000.0,
                        });
                    }
                }
            }
        }
    }
    seeds
}

fn evaluate_offset(
    profile: &DetectionProfile,
    peaks: &[&Peak],
    offset: OffsetEstimate,
) -> ProfileDetection {
    let initial = evaluate_offset_once(profile, peaks, offset);
    let Some(fitted_offset) = fitted_offset(initial.offset.unwrap_or(offset), &initial.hit_details)
    else {
        return initial;
    };
    evaluate_offset_once(profile, peaks, fitted_offset)
}

fn evaluate_offset_once(
    profile: &DetectionProfile,
    peaks: &[&Peak],
    offset: OffsetEstimate,
) -> ProfileDetection {
    let mut used_peak_ids = HashSet::new();
    let mut hit_details = Vec::new();
    let mut misses_mhz = Vec::new();

    for nominal_hz in profile.nominal_freqs_hz() {
        let expected_hz = offset.apply(nominal_hz);
        let best_peak = peaks
            .iter()
            .filter(|peak| !used_peak_ids.contains(&peak.id))
            .filter_map(|peak| {
                let error_hz = peak.freq_hz - expected_hz;
                if error_hz.abs() <= profile.max_offset_spread_hz {
                    Some((
                        *peak,
                        error_hz,
                        peak_match_score(peak, error_hz, profile.max_offset_spread_hz),
                    ))
                } else {
                    None
                }
            })
            .max_by(
                |(left_peak, left_error, left_score), (right_peak, right_error, right_score)| {
                    left_score
                        .partial_cmp(right_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| {
                            right_error
                                .abs()
                                .partial_cmp(&left_error.abs())
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .then_with(|| {
                            left_peak
                                .snr_db()
                                .partial_cmp(&right_peak.snr_db())
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                },
            );

        if let Some((peak, error_hz, _)) = best_peak {
            used_peak_ids.insert(peak.id);
            hit_details.push(DetectionHit {
                nominal_mhz: nominal_hz / MHZ,
                peak_id: peak.id,
                observed_hz: peak.freq_hz,
                expected_hz,
                error_hz,
                snr_db: peak.snr_db(),
            });
        } else {
            misses_mhz.push(nominal_hz / MHZ);
        }
    }

    let snr_sum_db: f64 = hit_details.iter().map(|hit| hit.snr_db.max(0.0)).sum();
    let snr_avg_db = if hit_details.is_empty() {
        0.0
    } else {
        snr_sum_db / hit_details.len() as f64
    };
    let offset_spread_hz = offset_spread_hz(&hit_details);
    let score = score_hits(&hit_details, misses_mhz.len(), profile.max_offset_spread_hz);
    let offset_group_hz = offset_group_hz(profile, offset, &hit_details);
    let suspect_key = suspect_key(&profile.name, offset_group_hz);

    ProfileDetection {
        suspect_key,
        profile: profile.name.clone(),
        detected: false,
        score,
        hits: hit_details.len(),
        required_hits: profile.min_hits,
        min_score: profile.min_score,
        snr_sum_db,
        snr_avg_db,
        offset_spread_hz,
        offset: Some(offset),
        offset_group_hz,
        offset_tolerance_hz: profile.max_offset_spread_hz,
        hit_details,
        misses_mhz,
    }
}

fn peak_match_score(peak: &Peak, error_hz: f64, tolerance_hz: f64) -> f64 {
    peak.snr_db().max(0.0) - MATCH_RESIDUAL_PENALTY_DB * normalized_error(error_hz, tolerance_hz)
}

fn normalized_error(error_hz: f64, tolerance_hz: f64) -> f64 {
    let tolerance_hz = tolerance_hz.max(1.0);
    let normalized = (error_hz.abs() / tolerance_hz).min(1.0);
    normalized * normalized
}

fn fitted_offset(seed: OffsetEstimate, hits: &[DetectionHit]) -> Option<OffsetEstimate> {
    if hits.is_empty() {
        return None;
    }

    match seed {
        OffsetEstimate::FixedHz { .. } => {
            let mean_hz = hits
                .iter()
                .map(|hit| hit.observed_hz - hit.nominal_mhz * MHZ)
                .sum::<f64>()
                / hits.len() as f64;
            Some(OffsetEstimate::FixedHz { hz: mean_hz })
        }
        OffsetEstimate::Ppm { .. } => {
            let mut numerator = 0.0;
            let mut denominator = 0.0;
            for hit in hits {
                let nominal_hz = hit.nominal_mhz * MHZ;
                let offset_hz = hit.observed_hz - nominal_hz;
                numerator += nominal_hz * offset_hz;
                denominator += nominal_hz * nominal_hz;
            }
            (denominator > 0.0).then_some(OffsetEstimate::Ppm {
                ppm: numerator / denominator * 1_000_000.0,
            })
        }
    }
}

fn score_hits(hits: &[DetectionHit], miss_count: usize, tolerance_hz: f64) -> f64 {
    let snr_sum: f64 = hits.iter().map(|hit| hit.snr_db.max(0.0)).sum();
    let consistency_penalty: f64 = hits
        .iter()
        .map(|hit| SCORE_RESIDUAL_PENALTY * normalized_error(hit.error_hz, tolerance_hz))
        .sum();
    snr_sum + hits.len() as f64 * 0.75 - miss_count as f64 * 0.5 - consistency_penalty
}

fn offset_spread_hz(hits: &[DetectionHit]) -> f64 {
    if hits.len() < 2 {
        return 0.0;
    }
    let min_error = hits
        .iter()
        .map(|hit| hit.error_hz)
        .fold(f64::INFINITY, f64::min);
    let max_error = hits
        .iter()
        .map(|hit| hit.error_hz)
        .fold(f64::NEG_INFINITY, f64::max);
    max_error - min_error
}

fn offset_group_hz(
    profile: &DetectionProfile,
    offset: OffsetEstimate,
    hits: &[DetectionHit],
) -> Option<f64> {
    if hits.is_empty() {
        return None;
    }

    match offset {
        OffsetEstimate::FixedHz { .. } => {
            let sum: f64 = hits
                .iter()
                .map(|hit| hit.observed_hz - hit.nominal_mhz * MHZ)
                .sum();
            Some(sum / hits.len() as f64)
        }
        OffsetEstimate::Ppm { .. } => {
            let reference_hz = profile_reference_hz(profile);
            Some(offset.apply(reference_hz) - reference_hz)
        }
    }
}

fn profile_reference_hz(profile: &DetectionProfile) -> f64 {
    let freqs = profile.nominal_freqs_hz();
    if freqs.is_empty() {
        return 0.0;
    }
    freqs.iter().sum::<f64>() / freqs.len() as f64
}

fn empty_result(profile: &DetectionProfile) -> ProfileDetection {
    ProfileDetection {
        suspect_key: suspect_key(&profile.name, None),
        profile: profile.name.clone(),
        detected: false,
        score: 0.0,
        hits: 0,
        required_hits: profile.min_hits,
        min_score: profile.min_score,
        snr_sum_db: 0.0,
        snr_avg_db: 0.0,
        offset_spread_hz: 0.0,
        offset: None,
        offset_group_hz: None,
        offset_tolerance_hz: profile.max_offset_spread_hz,
        hit_details: Vec::new(),
        misses_mhz: profile.freqs_mhz.clone(),
    }
}

fn apply_detection_threshold(profile: &DetectionProfile, result: &mut ProfileDetection) {
    result.detected = result.hits >= profile.min_hits && result.score >= profile.min_score;
}

fn choose_better(
    current: Option<ProfileDetection>,
    candidate: ProfileDetection,
) -> Option<ProfileDetection> {
    match current {
        None => Some(candidate),
        Some(current) => {
            if is_better_detection(&candidate, &current) {
                Some(candidate)
            } else {
                Some(current)
            }
        }
    }
}

fn compare_detections(left: &ProfileDetection, right: &ProfileDetection) -> std::cmp::Ordering {
    right
        .hits
        .cmp(&left.hits)
        .then_with(|| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| {
            left.offset_group_hz
                .unwrap_or(0.0)
                .partial_cmp(&right.offset_group_hz.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn is_better_detection(candidate: &ProfileDetection, current: &ProfileDetection) -> bool {
    candidate.hits > current.hits
        || (candidate.hits == current.hits && candidate.score > current.score)
}

pub fn same_offset_group(left: &ProfileDetection, right: &ProfileDetection) -> bool {
    if left.profile != right.profile {
        return false;
    }

    match (left.offset_group_hz, right.offset_group_hz) {
        (Some(left_hz), Some(right_hz)) => {
            let tolerance = left
                .offset_tolerance_hz
                .max(right.offset_tolerance_hz)
                .max(1.0);
            (left_hz - right_hz).abs() <= tolerance
        }
        (None, None) => true,
        _ => false,
    }
}

fn suspect_key(profile: &str, offset_group_hz: Option<f64>) -> String {
    match offset_group_hz {
        Some(hz) => format!("{profile}@{hz:+.0}Hz"),
        None => format!("{profile}@none"),
    }
}
