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
        .windows(2)
        .all(|pair| pair[0].freq_hz <= pair[1].freq_hz));
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

fn analysis_json(analysis: camfinder::dsp::SpectrumAnalysis) -> serde_json::Value {
    serde_json::json!({
        "peaks": analysis.candidates.into_iter().enumerate()
            .map(|(index, candidate)| candidate.into_peak(index, "test")).collect::<Vec<_>>(),
        "band": analysis.band,
    })
}

#[test]
fn analyzer_reuse_handles_new_samples_sizes_and_settings() {
    let mut analyzer = camfinder::dsp::SpectrumAnalyzer::new();
    for (fft_size, offset_hz, decimation) in [
        (4096, 125_000.0, 1),
        (4096, -250_000.0, 0),
        (16384, 187_500.0, 3),
        (65536, -187_500.0, 0),
        (1024, -125_000.0, 1),
        (4096, 125_000.0, 1),
    ] {
        let settings = spectrum_settings(1_000_000.0, fft_size, 400_000.0, decimation);
        let iq = tone_iq(1_000_000.0, offset_hz, fft_size * 3 + 17);
        let actual = analyzer
            .analyze(&iq, 100_000_000.0, 100_000_000.0, &settings)
            .unwrap();
        assert!(actual
            .candidates
            .iter()
            .any(|peak| (peak.freq_hz - 100_000_000.0 - offset_hz).abs()
                <= 1_000_000.0 / fft_size as f64));
        let fresh = analyze_spectrum_near(&iq, 100_000_000.0, 100_000_000.0, &settings).unwrap();
        assert_eq!(analysis_json(actual), analysis_json(fresh));
    }
}

#[test]
fn analyzer_recovers_after_short_input_empty_window_and_invalid_size() {
    let mut analyzer = camfinder::dsp::SpectrumAnalyzer::default();
    let mut settings = spectrum_settings(1_000_000.0, 1024, 400_000.0, 1);
    let iq = tone_iq(1_000_000.0, 125_000.0, 2048);
    let first = analyzer.analyze(&iq, 0.0, 0.0, &settings).unwrap();
    assert!(analyzer
        .analyze(&iq[..100], 0.0, 0.0, &settings)
        .unwrap()
        .band
        .is_none());
    assert!(analyzer
        .analyze(&iq, 0.0, 2_000_000.0, &settings)
        .unwrap()
        .band
        .is_none());
    settings.fft_size = 1000;
    assert!(analyzer.analyze(&iq, 0.0, 0.0, &settings).is_err());
    settings.fft_size = 1024;
    let last = analyzer.analyze(&iq, 0.0, 0.0, &settings).unwrap();
    assert_eq!(analysis_json(first), analysis_json(last));
}

#[test]
fn full_band_statistics_and_local_floors_match_sorted_reference() {
    let fft_size = 2048;
    let iq = (0..fft_size * 3)
        .map(|i| {
            let phase = i as f32 * 0.173;
            let other = i as f32 * -0.719;
            Complex32::new(phase.cos(), phase.sin())
                + Complex32::new(other.cos(), other.sin()) * 0.7
                + Complex32::new((i * 31 % 113) as f32, (i * 19 % 127) as f32) * 0.0001
        })
        .collect::<Vec<_>>();
    for window_hz in [399_900.0, 400_500.0] {
        let settings = spectrum_settings(1_000_000.0, fft_size, window_hz, 1);
        let result = analyze_spectrum_near(&iq, 100_000_000.0, 100_000_000.0, &settings).unwrap();
        let band = result.band.unwrap();
        assert!(result.candidates.len() >= 2);
        let mut sorted = band
            .points
            .iter()
            .map(|point| point.power_db)
            .collect::<Vec<_>>();
        sorted.sort_by(f64::total_cmp);
        let quantile = |fraction: f64| {
            let position = (sorted.len() - 1) as f64 * fraction;
            let lower = position.floor() as usize;
            let upper = position.ceil() as usize;
            let weight = position - lower as f64;
            sorted[lower] * (1.0 - weight) + sorted[upper] * weight
        };
        assert_eq!(band.features.power_p10_db, quantile(0.1));
        assert_eq!(band.features.power_p50_db, quantile(0.5));
        assert_eq!(band.features.power_p90_db, quantile(0.9));
        let guard_hz = (band.bin_hz * 4.0).max(2000.0);
        for peak in result.candidates {
            let mut floor = band
                .points
                .iter()
                .filter(|point| (100_000_000.0 + point.offset_hz - peak.freq_hz).abs() > guard_hz)
                .map(|point| point.power_db)
                .collect::<Vec<_>>();
            floor.sort_by(f64::total_cmp);
            let mid = floor.len() / 2;
            let expected = if floor.len().is_multiple_of(2) {
                (floor[mid - 1] + floor[mid]) / 2.0
            } else {
                floor[mid]
            };
            assert_eq!(peak.floor_db, expected);
        }
    }
}

#[test]
fn spectrum_window_larger_than_the_band_keeps_all_available_points() {
    let iq = tone_iq(1_000_000.0, 125_000.0, 4096);
    let mut settings = spectrum_settings(1_000_000.0, 1024, 400_000.0, 1);
    settings.spectrum_window_bins = usize::MAX;
    let result = analyze_spectrum_near(&iq, 100_000_000.0, 100_000_000.0, &settings).unwrap();
    let band = result.band.unwrap();
    assert!(!result.candidates.is_empty());
    for candidate in result.candidates {
        assert_eq!(candidate.spectrum_window.len(), band.points.len());
        for (window, point) in candidate.spectrum_window.iter().zip(&band.points) {
            assert_eq!(window.power_db, point.power_db);
            assert_eq!(
                window.offset_hz,
                100_000_000.0 + point.offset_hz - candidate.freq_hz
            );
        }
    }
}
