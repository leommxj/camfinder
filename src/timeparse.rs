use anyhow::{anyhow, Result};
use std::time::Duration;

pub fn parse_duration(input: &str) -> Result<Duration> {
    let value = input.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Err(anyhow!("empty duration value"));
    }

    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 0.001)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1.0)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60.0)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3600.0)
    } else {
        (value.as_str(), 1.0)
    };

    let seconds = number
        .trim()
        .parse::<f64>()
        .map_err(|err| anyhow!("invalid duration '{input}': {err}"))?
        * multiplier;

    if seconds < 0.0 {
        return Err(anyhow!("duration cannot be negative: '{input}'"));
    }

    Ok(Duration::from_secs_f64(seconds))
}

pub fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    if total_secs == 0 {
        return format!("0.{:03}s", duration.subsec_millis());
    }
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}
