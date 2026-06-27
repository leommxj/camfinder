use camfinder::frequency::parse_frequency_hz;
use camfinder::mask::parse_mask_spec;
use camfinder::timeparse::parse_duration;
use std::time::Duration;

#[test]
fn parses_frequency_units() {
    assert_eq!(parse_frequency_hz("816M").unwrap(), 816_000_000.0);
    assert_eq!(parse_frequency_hz("12.5kHz").unwrap(), 12_500.0);
    assert_eq!(parse_frequency_hz("+12.5k").unwrap(), 12_500.0);
}

#[test]
fn parses_mask_specs() {
    let mask = parse_mask_spec("864.0125M:50k").unwrap();
    assert!(mask.contains(864_012_500.0));
    assert!(mask.contains(864_000_000.0));
    assert!(!mask.contains(864_100_000.0));
}

#[test]
fn parses_duration_units() {
    assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
    assert_eq!(parse_duration("1.5m").unwrap(), Duration::from_secs(90));
    assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
}
