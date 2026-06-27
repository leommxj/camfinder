use crate::config::DetectionProfile;
use crate::frequency::MHZ;
use crate::peak::Peak;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyntheticOptions {
    pub offset_hz: f64,
    pub signal_snr_db: f64,
    pub floor_db: f64,
    pub jitter_hz: f64,
    pub missing_mhz: Vec<f64>,
    pub interferers: Vec<SyntheticInterferer>,
}

impl Default for SyntheticOptions {
    fn default() -> Self {
        Self {
            offset_hz: 12_500.0,
            signal_snr_db: 14.0,
            floor_db: -88.0,
            jitter_hz: 150.0,
            missing_mhz: Vec::new(),
            interferers: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyntheticInterferer {
    pub freq_hz: f64,
    pub snr_db: f64,
}

impl SyntheticInterferer {
    pub fn parse(input: &str) -> Result<Self> {
        let mut pieces = input.split(':');
        let freq = pieces
            .next()
            .ok_or_else(|| anyhow!("interferer must be '<freq>:<snr_db>'"))?;
        let snr = pieces
            .next()
            .ok_or_else(|| anyhow!("interferer must be '<freq>:<snr_db>'"))?;
        if pieces.next().is_some() {
            return Err(anyhow!("interferer must be '<freq>:<snr_db>'"));
        }

        Ok(Self {
            freq_hz: crate::frequency::parse_frequency_hz(freq)?,
            snr_db: snr
                .parse::<f64>()
                .map_err(|err| anyhow!("invalid interferer SNR '{snr}': {err}"))?,
        })
    }
}

pub fn generate_peaks(profiles: &[DetectionProfile], options: &SyntheticOptions) -> Vec<Peak> {
    let mut nominal_mhz = Vec::new();
    for profile in profiles {
        for freq_mhz in &profile.freqs_mhz {
            if !nominal_mhz
                .iter()
                .any(|known: &f64| (*known - *freq_mhz).abs() < 0.001)
            {
                nominal_mhz.push(*freq_mhz);
            }
        }
    }

    let mut peaks = Vec::new();
    let mut next_id = 1;
    for freq_mhz in nominal_mhz {
        if options
            .missing_mhz
            .iter()
            .any(|missing| (*missing - freq_mhz).abs() < 0.001)
        {
            continue;
        }

        let jitter_hz = deterministic_jitter(freq_mhz, options.jitter_hz);
        let freq_hz = freq_mhz * MHZ + options.offset_hz + jitter_hz;
        peaks.push(Peak {
            id: next_id,
            freq_hz,
            power_db: options.floor_db + options.signal_snr_db,
            floor_db: options.floor_db,
            source: Some("synthetic_signal".to_string()),
            shape: None,
            spectrum_window: Vec::new(),
        });
        next_id += 1;
    }

    for interferer in &options.interferers {
        peaks.push(Peak {
            id: next_id,
            freq_hz: interferer.freq_hz,
            power_db: options.floor_db + interferer.snr_db,
            floor_db: options.floor_db,
            source: Some("synthetic_interferer".to_string()),
            shape: None,
            spectrum_window: Vec::new(),
        });
        next_id += 1;
    }

    for freq_mhz in [803.125, 821.875, 852.25, 879.5] {
        peaks.push(Peak {
            id: next_id,
            freq_hz: freq_mhz * MHZ,
            power_db: options.floor_db + 3.0,
            floor_db: options.floor_db,
            source: Some("synthetic_noise_peak".to_string()),
            shape: None,
            spectrum_window: Vec::new(),
        });
        next_id += 1;
    }

    peaks.sort_by(|left, right| {
        left.freq_hz
            .partial_cmp(&right.freq_hz)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (index, peak) in peaks.iter_mut().enumerate() {
        peak.id = index + 1;
    }
    peaks
}

fn deterministic_jitter(freq_mhz: f64, max_jitter_hz: f64) -> f64 {
    let bucket = ((freq_mhz * 10.0).round() as i64).rem_euclid(7) - 3;
    bucket as f64 * max_jitter_hz / 3.0
}
