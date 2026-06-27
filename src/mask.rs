use crate::frequency::parse_frequency_hz;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrequencyMask {
    pub center_hz: f64,
    pub width_hz: f64,
    #[serde(default)]
    pub label: Option<String>,
}

impl FrequencyMask {
    pub fn contains(&self, freq_hz: f64) -> bool {
        (freq_hz - self.center_hz).abs() <= self.width_hz / 2.0
    }
}

#[derive(Clone, Debug, Default)]
pub struct MaskSet {
    masks: Vec<FrequencyMask>,
}

impl MaskSet {
    pub fn new(masks: Vec<FrequencyMask>) -> Self {
        Self { masks }
    }

    pub fn is_masked(&self, freq_hz: f64) -> bool {
        self.masks.iter().any(|mask| mask.contains(freq_hz))
    }

    pub fn masks(&self) -> &[FrequencyMask] {
        &self.masks
    }
}

pub fn parse_mask_spec(input: &str) -> Result<FrequencyMask> {
    let mut pieces = input.split(':');
    let center = pieces
        .next()
        .ok_or_else(|| anyhow!("mask must be '<center>:<width>'"))?;
    let width = pieces
        .next()
        .ok_or_else(|| anyhow!("mask must be '<center>:<width>'"))?;
    if pieces.next().is_some() {
        return Err(anyhow!("mask must be '<center>:<width>'"));
    }

    Ok(FrequencyMask {
        center_hz: parse_frequency_hz(center)?,
        width_hz: parse_frequency_hz(width)?,
        label: Some(input.to_string()),
    })
}
