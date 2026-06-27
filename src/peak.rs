use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Peak {
    pub id: usize,
    pub freq_hz: f64,
    pub power_db: f64,
    pub floor_db: f64,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<PeakShape>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spectrum_window: Vec<SpectrumPoint>,
}

impl Peak {
    pub fn snr_db(&self) -> f64 {
        self.power_db - self.floor_db
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeakShape {
    pub bin_hz: f64,
    pub prominence_db: f64,
    pub width_3db_hz: f64,
    pub width_6db_hz: f64,
    pub width_10db_hz: f64,
    pub width_threshold_hz: f64,
    pub plateau_1db_hz: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpectrumPoint {
    pub offset_hz: f64,
    pub power_db: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpectrumBand {
    #[serde(default)]
    pub source: Option<String>,
    pub target_hz: f64,
    pub center_hz: f64,
    #[serde(default)]
    pub sample_rate_hz: f64,
    #[serde(default)]
    pub fft_size: usize,
    #[serde(default)]
    pub iq_samples: usize,
    #[serde(default)]
    pub fft_frames: usize,
    pub bin_hz: f64,
    pub search_window_hz: f64,
    #[serde(default)]
    pub dc_notch_hz: f64,
    #[serde(default)]
    pub min_peak_snr_db: f64,
    #[serde(default)]
    pub max_peaks: usize,
    pub floor_db: f64,
    pub point_decimation: usize,
    pub features: SpectrumBandFeatures,
    pub points: Vec<SpectrumPoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpectrumBandFeatures {
    pub bins: usize,
    pub local_maxima: usize,
    pub peaks_above_min_snr: usize,
    pub fraction_above_floor_3db: f64,
    pub fraction_above_floor_6db: f64,
    pub fraction_above_floor_10db: f64,
    pub fraction_above_floor_20db: f64,
    pub occupied_width_floor_6db_hz: f64,
    pub occupied_width_floor_10db_hz: f64,
    pub power_p10_db: f64,
    pub power_p50_db: f64,
    pub power_p90_db: f64,
    pub power_max_db: f64,
}
