use crate::peak::{Peak, PeakShape, SpectrumBand, SpectrumBandFeatures, SpectrumPoint};
use anyhow::{anyhow, Result};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

pub type Complex32 = Complex<f32>;

#[derive(Clone, Debug)]
pub struct SpectrumSettings {
    pub sample_rate_hz: f64,
    pub fft_size: usize,
    pub search_window_hz: f64,
    pub dc_notch_hz: f64,
    pub min_peak_snr_db: f64,
    pub max_peaks: usize,
    pub spectrum_window_bins: usize,
    pub band_decimation: usize,
}

#[derive(Clone, Debug)]
pub struct PeakCandidate {
    pub freq_hz: f64,
    pub power_db: f64,
    pub floor_db: f64,
    pub shape: PeakShape,
    pub spectrum_window: Vec<SpectrumPoint>,
}

#[derive(Clone, Debug)]
pub struct SpectrumAnalysis {
    pub candidates: Vec<PeakCandidate>,
    pub band: Option<SpectrumBand>,
}

impl PeakCandidate {
    pub fn into_peak(self, id: usize, source: impl Into<String>) -> Peak {
        Peak {
            id,
            freq_hz: self.freq_hz,
            power_db: self.power_db,
            floor_db: self.floor_db,
            source: Some(source.into()),
            shape: Some(self.shape),
            spectrum_window: self.spectrum_window,
        }
    }
}

pub fn u16le_to_f32_real(raw: &[u8]) -> Vec<f32> {
    raw.chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32768.0)
        .collect()
}

pub fn find_peak_near(
    iq: &[Complex32],
    center_hz: f64,
    target_hz: f64,
    settings: &SpectrumSettings,
) -> Result<Option<PeakCandidate>> {
    Ok(find_peaks_near(iq, center_hz, target_hz, settings)?
        .into_iter()
        .max_by(|left, right| {
            left.power_db
                .partial_cmp(&right.power_db)
                .unwrap_or(std::cmp::Ordering::Equal)
        }))
}

pub fn find_peaks_near(
    iq: &[Complex32],
    center_hz: f64,
    target_hz: f64,
    settings: &SpectrumSettings,
) -> Result<Vec<PeakCandidate>> {
    Ok(analyze_spectrum_near(iq, center_hz, target_hz, settings)?.candidates)
}

pub fn analyze_spectrum_near(
    iq: &[Complex32],
    center_hz: f64,
    target_hz: f64,
    settings: &SpectrumSettings,
) -> Result<SpectrumAnalysis> {
    SpectrumAnalyzer::new().analyze(iq, center_hz, target_hz, settings)
}

#[derive(Default)]
pub struct SpectrumAnalyzer {
    fft: Option<Arc<dyn Fft<f32>>>,
    window: Vec<f32>,
    input: Vec<Complex32>,
    scratch: Vec<Complex32>,
    spectrum: Vec<f64>,
    search_bins: Vec<(usize, f64, f64)>,
    sorted_powers: Vec<f64>,
    floor_values: Vec<f64>,
    candidates: Vec<usize>,
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn analyze(
        &mut self,
        iq: &[Complex32],
        center_hz: f64,
        target_hz: f64,
        settings: &SpectrumSettings,
    ) -> Result<SpectrumAnalysis> {
        if settings.fft_size == 0 || !settings.fft_size.is_power_of_two() {
            return Err(anyhow!("fft_size must be a non-zero power of two"));
        }
        if iq.len() < settings.fft_size {
            return Ok(SpectrumAnalysis {
                candidates: Vec::new(),
                band: None,
            });
        }

        self.prepare(settings.fft_size);
        self.average_power(iq);
        let bin_hz = settings.sample_rate_hz / settings.fft_size as f64;
        let search_bins = &mut self.search_bins;
        search_bins.clear();

        for (bin, power) in self.spectrum.iter().enumerate() {
            let offset_hz = bin_offset_hz(bin, settings.fft_size, bin_hz);
            if offset_hz.abs() < settings.dc_notch_hz {
                continue;
            }
            let freq_hz = center_hz + offset_hz;
            if (freq_hz - target_hz).abs() <= settings.search_window_hz {
                search_bins.push((bin, freq_hz, db(*power)));
            }
        }
        search_bins.sort_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if search_bins.is_empty() {
            return Ok(SpectrumAnalysis {
                candidates: Vec::new(),
                band: None,
            });
        }

        let sorted_powers = &mut self.sorted_powers;
        sorted_powers.clear();
        sorted_powers.extend(search_bins.iter().map(|(_, _, power_db)| *power_db));
        let global_floor_db = if settings.band_decimation > 0 {
            sorted_powers.sort_by(|left, right| {
                left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
            });
            sorted_median(sorted_powers)
        } else {
            median(sorted_powers)
        }
        .unwrap_or(-300.0);

        let candidates = &mut self.candidates;
        candidates.clear();
        let mut local_maxima_count = 0;
        for index in local_maxima(search_bins) {
            local_maxima_count += 1;
            if search_bins[index].2 - global_floor_db >= settings.min_peak_snr_db {
                candidates.push(index);
            }
        }
        let peaks_above_min_snr = candidates.len();
        candidates.sort_by(|left, right| {
            search_bins[*right]
                .2
                .partial_cmp(&search_bins[*left].2)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let guard_hz = (bin_hz * 4.0).max(2_000.0);
        let mut selected = Vec::new();
        let floor_values = &mut self.floor_values;
        for &peak_index in candidates.iter() {
            let (peak_bin, peak_freq_hz, peak_power_db) = search_bins[peak_index];
            if selected
                .iter()
                .any(|peak: &PeakCandidate| (peak.freq_hz - peak_freq_hz).abs() <= guard_hz)
            {
                continue;
            }

            floor_values.clear();
            floor_values.extend(
                search_bins
                    .iter()
                    .filter(|(bin, freq_hz, _)| {
                        *bin != peak_bin && (*freq_hz - peak_freq_hz).abs() > guard_hz
                    })
                    .map(|(_, _, power_db)| *power_db),
            );

            if floor_values.len() < 8 {
                floor_values.clear();
                floor_values.extend(
                    search_bins
                        .iter()
                        .filter(|(bin, _, _)| *bin != peak_bin)
                        .map(|(_, _, power_db)| *power_db),
                );
            }

            let floor_db = median(floor_values).unwrap_or(global_floor_db);
            if peak_power_db - floor_db < settings.min_peak_snr_db {
                continue;
            }

            let shape = peak_shape(
                search_bins,
                peak_index,
                peak_power_db,
                floor_db,
                bin_hz,
                settings.min_peak_snr_db,
            );
            let spectrum_window = spectrum_window(
                search_bins,
                peak_index,
                peak_freq_hz,
                settings.spectrum_window_bins,
            );

            selected.push(PeakCandidate {
                freq_hz: peak_freq_hz,
                power_db: peak_power_db,
                floor_db,
                shape,
                spectrum_window,
            });

            if selected.len() >= settings.max_peaks.max(1) {
                break;
            }
        }

        selected.sort_by(|left, right| {
            left.freq_hz
                .partial_cmp(&right.freq_hz)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let band = (settings.band_decimation > 0).then(|| {
            spectrum_band(
                search_bins,
                center_hz,
                target_hz,
                settings,
                sorted_powers,
                SpectrumBandMeta {
                    bin_hz,
                    iq_samples: iq.len(),
                    fft_frames: iq.len() / settings.fft_size,
                    floor_db: global_floor_db,
                    peaks_above_min_snr,
                    local_maxima: local_maxima_count,
                },
            )
        });
        Ok(SpectrumAnalysis {
            candidates: selected,
            band,
        })
    }

    fn prepare(&mut self, fft_size: usize) {
        if self.input.len() == fft_size {
            return;
        }
        let fft = FftPlanner::<f32>::new().plan_fft_forward(fft_size);
        self.scratch
            .resize(fft.get_inplace_scratch_len(), Complex32::default());
        self.fft = Some(fft);
        self.window = hann_window(fft_size);
        self.input.resize(fft_size, Complex32::default());
        self.spectrum.resize(fft_size, 0.0);
    }

    fn average_power(&mut self, iq: &[Complex32]) {
        self.spectrum.fill(0.0);
        let fft = self.fft.as_ref().unwrap();
        let fft_size = self.input.len();
        for frame in iq.chunks_exact(fft_size) {
            for ((out, sample), window_value) in self.input.iter_mut().zip(frame).zip(&self.window)
            {
                *out = *sample * *window_value;
            }
            fft.process_with_scratch(&mut self.input, &mut self.scratch);
            for (sum, value) in self.spectrum.iter_mut().zip(&self.input) {
                *sum += value.norm_sqr() as f64;
            }
        }
        let frame_count = iq.len() / fft_size;
        for power in &mut self.spectrum {
            *power /= frame_count as f64;
        }
    }
}

fn spectrum_band(
    search_bins: &[(usize, f64, f64)],
    center_hz: f64,
    target_hz: f64,
    settings: &SpectrumSettings,
    sorted_powers: &[f64],
    meta: SpectrumBandMeta,
) -> SpectrumBand {
    let decimation = settings.band_decimation.max(1);
    let bins = search_bins.len();
    let points = search_bins
        .iter()
        .enumerate()
        .filter(|(index, _)| index % decimation == 0 || *index + 1 == bins)
        .map(|(_, (_, freq_hz, power_db))| SpectrumPoint {
            offset_hz: *freq_hz - target_hz,
            power_db: *power_db,
        })
        .collect();

    SpectrumBand {
        source: None,
        target_hz,
        center_hz,
        sample_rate_hz: settings.sample_rate_hz,
        fft_size: settings.fft_size,
        iq_samples: meta.iq_samples,
        fft_frames: meta.fft_frames,
        bin_hz: meta.bin_hz,
        search_window_hz: settings.search_window_hz,
        dc_notch_hz: settings.dc_notch_hz,
        min_peak_snr_db: settings.min_peak_snr_db,
        max_peaks: settings.max_peaks,
        floor_db: meta.floor_db,
        point_decimation: decimation,
        features: SpectrumBandFeatures {
            bins,
            local_maxima: meta.local_maxima,
            peaks_above_min_snr: meta.peaks_above_min_snr,
            fraction_above_floor_3db: fraction_above(sorted_powers, meta.floor_db + 3.0),
            fraction_above_floor_6db: fraction_above(sorted_powers, meta.floor_db + 6.0),
            fraction_above_floor_10db: fraction_above(sorted_powers, meta.floor_db + 10.0),
            fraction_above_floor_20db: fraction_above(sorted_powers, meta.floor_db + 20.0),
            occupied_width_floor_6db_hz: occupied_width_hz(search_bins, meta.floor_db + 6.0),
            occupied_width_floor_10db_hz: occupied_width_hz(search_bins, meta.floor_db + 10.0),
            power_p10_db: percentile(sorted_powers, 0.10).unwrap_or(meta.floor_db),
            power_p50_db: percentile(sorted_powers, 0.50).unwrap_or(meta.floor_db),
            power_p90_db: percentile(sorted_powers, 0.90).unwrap_or(meta.floor_db),
            power_max_db: sorted_powers
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max),
        },
        points,
    }
}

#[derive(Clone, Copy, Debug)]
struct SpectrumBandMeta {
    bin_hz: f64,
    iq_samples: usize,
    fft_frames: usize,
    floor_db: f64,
    peaks_above_min_snr: usize,
    local_maxima: usize,
}

fn fraction_above(values: &[f64], threshold: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().filter(|value| **value >= threshold).count() as f64 / values.len() as f64
}

fn occupied_width_hz(search_bins: &[(usize, f64, f64)], threshold: f64) -> f64 {
    let mut min_hz = f64::INFINITY;
    let mut max_hz = f64::NEG_INFINITY;
    let mut any = false;
    for (_, freq_hz, power_db) in search_bins {
        if *power_db >= threshold {
            min_hz = min_hz.min(*freq_hz);
            max_hz = max_hz.max(*freq_hz);
            any = true;
        }
    }
    if any {
        (max_hz - min_hz).abs()
    } else {
        0.0
    }
}

fn peak_shape(
    search_bins: &[(usize, f64, f64)],
    peak_index: usize,
    peak_power_db: f64,
    floor_db: f64,
    bin_hz: f64,
    min_peak_snr_db: f64,
) -> PeakShape {
    PeakShape {
        bin_hz,
        prominence_db: peak_power_db - floor_db,
        width_3db_hz: contiguous_width_hz(search_bins, peak_index, peak_power_db - 3.0, bin_hz),
        width_6db_hz: contiguous_width_hz(search_bins, peak_index, peak_power_db - 6.0, bin_hz),
        width_10db_hz: contiguous_width_hz(search_bins, peak_index, peak_power_db - 10.0, bin_hz),
        width_threshold_hz: contiguous_width_hz(
            search_bins,
            peak_index,
            floor_db + min_peak_snr_db,
            bin_hz,
        ),
        plateau_1db_hz: contiguous_width_hz(search_bins, peak_index, peak_power_db - 1.0, bin_hz),
    }
}

fn contiguous_width_hz(
    search_bins: &[(usize, f64, f64)],
    peak_index: usize,
    threshold_db: f64,
    bin_hz: f64,
) -> f64 {
    if search_bins.is_empty() {
        return 0.0;
    }

    let mut left = peak_index.min(search_bins.len() - 1);
    while left > 0 && search_bins[left - 1].2 >= threshold_db {
        left -= 1;
    }

    let mut right = peak_index.min(search_bins.len() - 1);
    while right + 1 < search_bins.len() && search_bins[right + 1].2 >= threshold_db {
        right += 1;
    }

    (search_bins[right].1 - search_bins[left].1).abs() + bin_hz
}

fn spectrum_window(
    search_bins: &[(usize, f64, f64)],
    peak_index: usize,
    peak_freq_hz: f64,
    half_bins: usize,
) -> Vec<SpectrumPoint> {
    if half_bins == 0 || search_bins.is_empty() {
        return Vec::new();
    }

    let start = peak_index.saturating_sub(half_bins);
    let end = peak_index
        .saturating_add(half_bins)
        .saturating_add(1)
        .min(search_bins.len());
    search_bins[start..end]
        .iter()
        .map(|(_, freq_hz, power_db)| SpectrumPoint {
            offset_hz: *freq_hz - peak_freq_hz,
            power_db: *power_db,
        })
        .collect()
}

fn local_maxima(search_bins: &[(usize, f64, f64)]) -> impl Iterator<Item = usize> + '_ {
    (0..search_bins.len()).filter(|&index| {
        search_bins.len() < 3
            || (index > 0
                && index + 1 < search_bins.len()
                && search_bins[index].2 > search_bins[index - 1].2
                && search_bins[index].2 >= search_bins[index + 1].2)
    })
}

fn hann_window(len: usize) -> Vec<f32> {
    if len == 1 {
        return vec![1.0];
    }
    (0..len)
        .map(|i| {
            let phase = 2.0 * std::f32::consts::PI * i as f32 / (len - 1) as f32;
            0.5 - 0.5 * phase.cos()
        })
        .collect()
}

fn bin_offset_hz(bin: usize, fft_size: usize, bin_hz: f64) -> f64 {
    if bin <= fft_size / 2 {
        bin as f64 * bin_hz
    } else {
        -((fft_size - bin) as f64) * bin_hz
    }
}

fn db(power: f64) -> f64 {
    10.0 * power.max(1e-30).log10()
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let even = values.len().is_multiple_of(2);
    let mid = values.len() / 2;
    let (lower, upper, _) = values.select_nth_unstable_by(mid, f64::total_cmp);
    if !even {
        Some(*upper)
    } else {
        Some((lower.iter().copied().fold(f64::NEG_INFINITY, f64::max) + *upper) / 2.0)
    }
}

fn sorted_median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some((values[mid - 1] + values[mid]) / 2.0)
    } else {
        Some(values[mid])
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let position = (sorted.len() - 1) as f64 * percentile.clamp(0.0, 1.0);
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        Some(sorted[lower])
    } else {
        let upper_weight = position - lower as f64;
        Some(sorted[lower] * (1.0 - upper_weight) + sorted[upper] * upper_weight)
    }
}
