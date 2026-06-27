#[cfg(not(feature = "soapy"))]
use crate::dsp::Complex32;
#[cfg(not(feature = "soapy"))]
use crate::sdr::{IqBackend, SdrScanConfig, SoapySettings};
#[cfg(not(feature = "soapy"))]
use anyhow::{anyhow, Result};

#[derive(Clone, Debug)]
pub struct SoapyDeviceInfo {
    pub index: usize,
    pub args: String,
}

#[cfg(not(feature = "soapy"))]
pub fn list_soapy_devices(_filter: &str) -> Result<Vec<SoapyDeviceInfo>> {
    Err(anyhow!(
        "SoapySDR support is not built; rebuild with `--features soapy`"
    ))
}

#[cfg(not(feature = "soapy"))]
pub struct SoapyScanner;

#[cfg(not(feature = "soapy"))]
impl SoapyScanner {
    pub fn open(_settings: SoapySettings) -> Result<Self> {
        Err(anyhow!(
            "SoapySDR support is not built; rebuild with `--features soapy`"
        ))
    }
}

#[cfg(not(feature = "soapy"))]
impl IqBackend for SoapyScanner {
    fn source_name(&self) -> &str {
        "soapy"
    }

    fn scan_config(&self) -> SdrScanConfig {
        SoapySettings::default().scan_config(2_400_000.0)
    }

    fn tune(&mut self, _center_hz: f64) -> Result<f64> {
        Err(anyhow!(
            "SoapySDR support is not built; rebuild with `--features soapy`"
        ))
    }

    fn collect_iq(&mut self) -> Result<Vec<Complex32>> {
        Err(anyhow!(
            "SoapySDR support is not built; rebuild with `--features soapy`"
        ))
    }
}

#[cfg(feature = "soapy")]
mod real {
    use super::SoapyDeviceInfo;
    use crate::dsp::Complex32;
    use crate::sdr::{
        IqBackend, SdrScanConfig, SoapyGainElement, SoapyGainSettings, SoapySettings,
    };
    use anyhow::{anyhow, Context, Result};
    use soapysdr::{Device, Direction, ErrorCode, RxStream};
    use std::time::{Duration, Instant};

    pub fn list_soapy_devices(filter: &str) -> Result<Vec<SoapyDeviceInfo>> {
        let devices = soapysdr::enumerate(filter)
            .with_context(|| format!("failed to enumerate SoapySDR devices for '{filter}'"))?;
        Ok(devices
            .iter()
            .enumerate()
            .map(|(index, args)| SoapyDeviceInfo {
                index,
                args: args.to_string(),
            })
            .collect())
    }

    pub struct SoapyScanner {
        device: Device,
        stream: RxStream<Complex32>,
        settings: SoapySettings,
        actual_sample_rate_hz: f64,
    }

    impl SoapyScanner {
        pub fn open(settings: SoapySettings) -> Result<Self> {
            let selected_args = select_device_args(&settings)?;
            let device = Device::new(selected_args.as_str())
                .with_context(|| format!("failed to open SoapySDR device '{selected_args}'"))?;
            configure_device(&device, &settings)?;
            let stream = device
                .rx_stream::<Complex32>(&[settings.channel])
                .context("failed to create SoapySDR RX stream")?;
            let actual_sample_rate_hz = device
                .sample_rate(Direction::Rx, settings.channel)
                .unwrap_or(settings.sample_rate_hz as f64);
            Ok(Self {
                device,
                stream,
                settings,
                actual_sample_rate_hz,
            })
        }
    }

    impl IqBackend for SoapyScanner {
        fn source_name(&self) -> &str {
            "soapy"
        }

        fn scan_config(&self) -> SdrScanConfig {
            self.settings.scan_config(self.actual_sample_rate_hz)
        }

        fn tune(&mut self, center_hz: f64) -> Result<f64> {
            self.device
                .set_frequency(Direction::Rx, self.settings.channel, center_hz, ())
                .with_context(|| format!("failed to set SoapySDR frequency {center_hz:.0} Hz"))?;
            Ok(self
                .device
                .frequency(Direction::Rx, self.settings.channel)
                .unwrap_or(center_hz))
        }

        fn collect_iq(&mut self) -> Result<Vec<Complex32>> {
            collect_iq(&mut self.stream, &self.settings, self.actual_sample_rate_hz)
        }

        fn gain_label(&self) -> Option<String> {
            Some(gain_label(&self.settings.gain))
        }
    }

    fn select_device_args(settings: &SoapySettings) -> Result<String> {
        let mut devices =
            soapysdr::enumerate(settings.device_args.as_str()).with_context(|| {
                format!(
                    "failed to enumerate SoapySDR devices for '{}'",
                    settings.device_args
                )
            })?;
        if devices.is_empty() {
            return Err(anyhow!(
                "no SoapySDR devices matched '{}'",
                settings.device_args
            ));
        }
        if settings.device_index >= devices.len() {
            return Err(anyhow!(
                "SoapySDR device index {} is out of range; {} device(s) matched '{}'",
                settings.device_index,
                devices.len(),
                settings.device_args
            ));
        }
        Ok(devices.remove(settings.device_index).to_string())
    }

    fn configure_device(device: &Device, settings: &SoapySettings) -> Result<()> {
        let channels = device
            .num_channels(Direction::Rx)
            .context("failed to query SoapySDR RX channel count")?;
        if settings.channel >= channels {
            return Err(anyhow!(
                "SoapySDR RX channel {} is out of range; device has {} RX channel(s)",
                settings.channel,
                channels
            ));
        }

        device
            .set_sample_rate(
                Direction::Rx,
                settings.channel,
                settings.sample_rate_hz as f64,
            )
            .with_context(|| {
                format!(
                    "failed to set SoapySDR sample rate {}",
                    settings.sample_rate_hz
                )
            })?;

        if let Some(antenna) = settings
            .antenna
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            device
                .set_antenna(Direction::Rx, settings.channel, antenna)
                .with_context(|| format!("failed to set SoapySDR antenna '{antenna}'"))?;
        }

        if let Some(bandwidth_hz) = settings.bandwidth_hz {
            if bandwidth_hz > 0.0 {
                device
                    .set_bandwidth(Direction::Rx, settings.channel, bandwidth_hz)
                    .with_context(|| {
                        format!("failed to set SoapySDR bandwidth {bandwidth_hz:.0} Hz")
                    })?;
            }
        }

        apply_gain(device, settings.channel, &settings.gain)?;
        Ok(())
    }

    fn apply_gain(device: &Device, channel: usize, gain: &SoapyGainSettings) -> Result<()> {
        match gain.mode.trim().to_ascii_lowercase().as_str() {
            "" | "none" => Ok(()),
            "agc" => {
                if !device
                    .has_gain_mode(Direction::Rx, channel)
                    .unwrap_or(false)
                {
                    return Err(anyhow!(
                        "SoapySDR device does not report RX AGC support on channel {channel}"
                    ));
                }
                device
                    .set_gain_mode(Direction::Rx, channel, true)
                    .context("failed to enable SoapySDR RX AGC")
            }
            "manual" | "overall" => {
                if device
                    .has_gain_mode(Direction::Rx, channel)
                    .unwrap_or(false)
                {
                    device
                        .set_gain_mode(Direction::Rx, channel, false)
                        .context("failed to disable SoapySDR RX AGC")?;
                }
                if let Some(gain_db) = gain.gain_db {
                    device
                        .set_gain(Direction::Rx, channel, gain_db)
                        .with_context(|| format!("failed to set SoapySDR gain {gain_db:.1} dB"))?;
                }
                for element in &gain.elements {
                    set_gain_element(device, channel, element)?;
                }
                Ok(())
            }
            other => Err(anyhow!(
                "unknown SoapySDR gain mode '{other}', expected none/manual/overall/agc"
            )),
        }
    }

    fn set_gain_element(device: &Device, channel: usize, element: &SoapyGainElement) -> Result<()> {
        device
            .set_gain_element(
                Direction::Rx,
                channel,
                element.name.as_str(),
                element.gain_db,
            )
            .with_context(|| {
                format!(
                    "failed to set SoapySDR gain element {}={:.1} dB",
                    element.name, element.gain_db
                )
            })
    }

    fn collect_iq(
        stream: &mut RxStream<Complex32>,
        settings: &SoapySettings,
        actual_sample_rate_hz: f64,
    ) -> Result<Vec<Complex32>> {
        let duration = Duration::from_millis(settings.dwell_ms);
        let deadline = Instant::now() + duration;
        let mtu = stream.mtu().unwrap_or(16_384).max(1024);
        let mut buffer = vec![Complex32::new(0.0, 0.0); mtu];
        let expected_len =
            ((actual_sample_rate_hz * settings.dwell_ms as f64) / 1000.0).ceil() as usize;
        let mut iq = Vec::with_capacity(expected_len.max(settings.fft_size));
        let mut chunks_to_discard = 2usize;

        stream
            .activate(None)
            .context("failed to activate SoapySDR RX stream")?;
        let result = (|| -> Result<Vec<Complex32>> {
            while Instant::now() < deadline {
                match stream.read(&mut [&mut buffer[..]], 1_500_000) {
                    Ok(0) => {}
                    Ok(n) => {
                        if chunks_to_discard > 0 {
                            chunks_to_discard -= 1;
                            continue;
                        }
                        iq.extend_from_slice(&buffer[..n]);
                    }
                    Err(err) if matches!(err.code, ErrorCode::Timeout | ErrorCode::Overflow) => {}
                    Err(err) => return Err(err.into()),
                }
            }
            Ok(iq)
        })();
        let deactivate_result = if stream.active() {
            stream.deactivate(None)
        } else {
            Ok(())
        };

        match (result, deactivate_result) {
            (Ok(iq), Ok(())) => Ok(iq),
            (Err(err), _) => Err(err),
            (Ok(_), Err(err)) => Err(err.into()),
        }
    }

    fn gain_label(gain: &SoapyGainSettings) -> String {
        let mut parts = vec![format!("mode={}", gain.mode)];
        if let Some(gain_db) = gain.gain_db {
            parts.push(format!("gain={gain_db:.1}"));
        }
        if !gain.elements.is_empty() {
            let elements = gain
                .elements
                .iter()
                .map(|element| format!("{}={:.1}", element.name, element.gain_db))
                .collect::<Vec<_>>()
                .join(",");
            parts.push(format!("elements=[{elements}]"));
        }
        parts.join(",")
    }
}

#[cfg(feature = "soapy")]
pub use real::{list_soapy_devices, SoapyScanner};
