use camfinder::dsp::{
    analyze_spectrum_near, find_peak_near, find_peaks_near, Complex32, SpectrumSettings,
};

fn spectrum_settings(
    sample_rate_hz: f64,
    fft_size: usize,
    search_window_hz: f64,
    band_decimation: usize,
) -> SpectrumSettings {
    SpectrumSettings {
        sample_rate_hz,
        fft_size,
        search_window_hz,
        dc_notch_hz: 1_000.0,
        min_peak_snr_db: 10.0,
        max_peaks: 4,
        spectrum_window_bins: 16,
        band_decimation,
    }
}

fn tone_iq(sample_rate_hz: f64, offset_hz: f64, samples: usize) -> Vec<Complex32> {
    (0..samples)
        .map(|i| {
            let phase =
                2.0 * std::f32::consts::PI * offset_hz as f32 * i as f32 / sample_rate_hz as f32;
            Complex32::new(phase.cos(), phase.sin())
        })
        .collect()
}

#[test]
fn finds_complex_tone_peak() {
    let sample_rate_hz = 1_000_000.0;
    let center_hz = 100_000_000.0;
    let tone_offset_hz = 12_500.0;
    let fft_size = 16_384;
    let iq = tone_iq(sample_rate_hz, tone_offset_hz, fft_size * 4);

    let peak = find_peak_near(
        &iq,
        center_hz,
        center_hz,
        &spectrum_settings(sample_rate_hz, fft_size, 50_000.0, 1),
    )
    .unwrap()
    .unwrap();

    assert!((peak.freq_hz - (center_hz + tone_offset_hz)).abs() < 100.0);
    assert!(peak.shape.prominence_db >= 10.0);
    assert!(!peak.spectrum_window.is_empty());
}

#[test]
fn keeps_multiple_peaks_in_target_window() {
    let sample_rate_hz = 1_000_000.0;
    let center_hz = 100_000_000.0;
    let fft_size = 16_384;
    let iq = (0..fft_size * 4)
        .map(|i| {
            let t = i as f32 / sample_rate_hz as f32;
            let a = 2.0 * std::f32::consts::PI * 12_500.0f32 * t;
            let b = 2.0 * std::f32::consts::PI * 52_000.0f32 * t;
            Complex32::new(a.cos(), a.sin()) + Complex32::new(b.cos(), b.sin()) * 0.5
        })
        .collect::<Vec<_>>();

    let peaks = find_peaks_near(
        &iq,
        center_hz,
        center_hz,
        &spectrum_settings(sample_rate_hz, fft_size, 80_000.0, 1),
    )
    .unwrap();

    assert!(peaks.len() >= 2);
    assert!(peaks
        .iter()
        .any(|peak| (peak.freq_hz - (center_hz + 12_500.0)).abs() < 100.0));
    assert!(peaks
        .iter()
        .any(|peak| (peak.freq_hz - (center_hz + 52_000.0)).abs() < 100.0));
}

#[test]
fn records_decimated_search_band_for_later_analysis() {
    let sample_rate_hz = 1_000_000.0;
    let center_hz = 100_000_000.0;
    let tone_offset_hz = 12_500.0;
    let fft_size = 16_384;
    let iq = tone_iq(sample_rate_hz, tone_offset_hz, fft_size * 4);

    let analysis = analyze_spectrum_near(
        &iq,
        center_hz,
        center_hz,
        &spectrum_settings(sample_rate_hz, fft_size, 50_000.0, 2),
    )
    .unwrap();

    let band = analysis.band.unwrap();
    assert_eq!(band.target_hz, center_hz);
    assert_eq!(band.center_hz, center_hz);
    assert_eq!(band.sample_rate_hz, sample_rate_hz);
    assert_eq!(band.fft_size, fft_size);
    assert_eq!(band.iq_samples, fft_size * 4);
    assert_eq!(band.fft_frames, 4);
    assert_eq!(band.dc_notch_hz, 1_000.0);
    assert_eq!(band.min_peak_snr_db, 10.0);
    assert_eq!(band.max_peaks, 4);
    assert_eq!(band.point_decimation, 2);
    assert!(band.features.bins > 0);
    assert!(band.features.local_maxima > 0);
    assert!(band.features.peaks_above_min_snr > 0);
    assert!(!band.points.is_empty());
    assert!(band.features.power_max_db >= band.features.power_p90_db);
}
