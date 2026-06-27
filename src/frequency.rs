use anyhow::{anyhow, Result};

pub const KHZ: f64 = 1_000.0;
pub const MHZ: f64 = 1_000_000.0;

pub fn parse_frequency_hz(input: &str) -> Result<f64> {
    let mut value = input.trim().replace('_', "");
    if value.is_empty() {
        return Err(anyhow!("empty frequency value"));
    }

    value = value.to_ascii_lowercase();
    let (number, multiplier) = if let Some(number) = value.strip_suffix("mhz") {
        (number, MHZ)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, MHZ)
    } else if let Some(number) = value.strip_suffix("khz") {
        (number, KHZ)
    } else if let Some(number) = value.strip_suffix('k') {
        (number, KHZ)
    } else if let Some(number) = value.strip_suffix("hz") {
        (number, 1.0)
    } else {
        (value.as_str(), 1.0)
    };

    let parsed = number
        .trim()
        .parse::<f64>()
        .map_err(|err| anyhow!("invalid frequency '{input}': {err}"))?;

    Ok(parsed * multiplier)
}

pub fn parse_frequency_list_mhz(input: &str) -> Result<Vec<f64>> {
    input
        .split(',')
        .map(|part| parse_frequency_hz(part).map(|hz| hz / MHZ))
        .collect()
}

pub fn format_frequency_hz(hz: f64) -> String {
    if hz.abs() >= MHZ {
        format!("{:.6} MHz", hz / MHZ)
    } else if hz.abs() >= KHZ {
        format!("{:.3} kHz", hz / KHZ)
    } else {
        format!("{hz:.1} Hz")
    }
}

pub fn format_offset_hz(hz: f64) -> String {
    let sign = if hz >= 0.0 { "+" } else { "-" };
    let abs_hz = hz.abs();
    if abs_hz >= MHZ {
        format!("{sign}{:.6} MHz", abs_hz / MHZ)
    } else if abs_hz >= KHZ {
        format!("{sign}{:.3} kHz", abs_hz / KHZ)
    } else {
        format!("{sign}{abs_hz:.1} Hz")
    }
}
