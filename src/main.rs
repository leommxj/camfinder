use anyhow::{anyhow, Result};
#[cfg(feature = "airspy")]
use camfinder::airspy_backend::list_airspy_devices;
use camfinder::config::{
    AppConfig, Backend, DetectionProfile, OffsetModel, ProfilePreset, RecordMode, ScanRequest,
};
use camfinder::detect::{OffsetEstimate, ProfileDetection};
use camfinder::engine::{ScanSession, SessionStatus};
use camfinder::frequency::{format_frequency_hz, format_offset_hz};
use camfinder::mask::MaskSet;
use camfinder::peak::Peak;
use camfinder::persistence::{PersistenceSummary, StabilityState};
use camfinder::sdr::TargetStatistics;
use camfinder::soapy_backend::list_soapy_devices;
use camfinder::timeparse::format_duration;
use camfinder::tui::run_tui_loop;
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(name = "camfinder")]
#[command(about = "Experimental SDR camera leakage detector")]
struct Cli {
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Devices,
    Profiles,
    DefaultConfig,
    ShowConfig,
    Scan(Box<ScanArgs>),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BackendArg {
    Synthetic,
    PeaksFile,
    IqFile,
    Airspy,
    Soapy,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputArg {
    Tui,
    Table,
    Json,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RecordModeArg {
    Peaks,
    Summary,
    Spectrum,
}

impl From<RecordModeArg> for RecordMode {
    fn from(value: RecordModeArg) -> Self {
        match value {
            RecordModeArg::Peaks => Self::Peaks,
            RecordModeArg::Summary => Self::Summary,
            RecordModeArg::Spectrum => Self::Spectrum,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OffsetModelArg {
    #[value(name = "fixed_hz", alias = "fixed-hz")]
    FixedHz,
    Ppm,
    Auto,
}

impl From<OffsetModelArg> for OffsetModel {
    fn from(value: OffsetModelArg) -> Self {
        match value {
            OffsetModelArg::FixedHz => OffsetModel::FixedHz,
            OffsetModelArg::Ppm => OffsetModel::Ppm,
            OffsetModelArg::Auto => OffsetModel::Auto,
        }
    }
}

#[derive(Debug, Parser)]
struct ScanArgs {
    #[arg(long)]
    preset: Option<ProfilePreset>,
    #[arg(long, value_enum)]
    backend: Option<BackendArg>,
    #[arg(long)]
    peaks_file: Option<PathBuf>,
    #[arg(long)]
    iq_file: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
    #[arg(long)]
    profile_freqs: Option<String>,
    #[arg(long)]
    min_hits: Option<usize>,
    #[arg(long)]
    min_score: Option<f64>,
    #[arg(long)]
    search_window: Option<String>,
    #[arg(long)]
    match_window: Option<String>,
    #[arg(long, value_enum)]
    offset_model: Option<OffsetModelArg>,
    #[arg(long)]
    min_peak_snr: Option<f64>,
    #[arg(long)]
    max_peaks: Option<usize>,
    #[arg(long)]
    record: Option<PathBuf>,
    #[arg(long, value_enum)]
    record_mode: Option<RecordModeArg>,
    #[arg(long)]
    record_only: bool,
    #[arg(long)]
    record_spectrum_bins: Option<usize>,
    #[arg(long)]
    record_band_decimation: Option<usize>,
    #[arg(long, value_enum)]
    output: Option<OutputArg>,
    #[arg(long)]
    duration: Option<String>,
    #[arg(long, action = ArgAction::SetTrue)]
    continuous: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    once: bool,
    #[arg(long)]
    repeat_delay: Option<String>,
    #[arg(long)]
    confirm_passes: Option<usize>,
    #[arg(long)]
    confirm_ratio: Option<f64>,
    #[arg(long)]
    mask: Vec<String>,
    #[arg(long)]
    dwell_ms: Option<u64>,
    #[arg(long, conflicts_with_all = ["dwell_ms", "airspy_dwell_ms", "soapy_dwell_ms"])]
    fft_frames: Option<usize>,
    #[arg(long)]
    capture_timeout_ms: Option<u64>,
    #[arg(long)]
    settle_ms: Option<u64>,
    #[arg(long)]
    synthetic_offset: Option<String>,
    #[arg(long)]
    synthetic_snr: Option<f64>,
    #[arg(long)]
    synthetic_floor: Option<f64>,
    #[arg(long)]
    synthetic_jitter: Option<String>,
    #[arg(long)]
    synthetic_missing: Vec<String>,
    #[arg(long)]
    interferer: Vec<String>,
    #[arg(long)]
    airspy_device_index: Option<usize>,
    #[arg(long)]
    airspy_sample_rate: Option<String>,
    #[arg(long)]
    airspy_dwell_ms: Option<u64>,
    #[arg(long)]
    airspy_fft_size: Option<usize>,
    #[arg(long)]
    airspy_min_peak_snr: Option<f64>,
    #[arg(long)]
    airspy_max_peaks: Option<usize>,
    #[arg(long)]
    airspy_dc_notch: Option<String>,
    #[arg(long)]
    airspy_settle_ms: Option<u64>,
    #[arg(long)]
    gain_mode: Option<String>,
    #[arg(long)]
    gain_preset: Option<u8>,
    #[arg(long)]
    lna: Option<u8>,
    #[arg(long)]
    mixer: Option<u8>,
    #[arg(long)]
    vga: Option<u8>,
    #[arg(long, action = ArgAction::SetTrue)]
    lna_agc: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    mixer_agc: bool,
    #[arg(long)]
    soapy_device: Option<String>,
    #[arg(long)]
    soapy_device_index: Option<usize>,
    #[arg(long)]
    soapy_channel: Option<usize>,
    #[arg(long)]
    soapy_sample_rate: Option<String>,
    #[arg(long)]
    soapy_dwell_ms: Option<u64>,
    #[arg(long)]
    soapy_fft_size: Option<usize>,
    #[arg(long)]
    soapy_min_peak_snr: Option<f64>,
    #[arg(long)]
    soapy_max_peaks: Option<usize>,
    #[arg(long)]
    soapy_dc_notch: Option<String>,
    #[arg(long)]
    soapy_settle_ms: Option<u64>,
    #[arg(long)]
    soapy_antenna: Option<String>,
    #[arg(long)]
    soapy_bandwidth: Option<String>,
    #[arg(long)]
    soapy_gain_mode: Option<String>,
    #[arg(long)]
    soapy_gain: Option<f64>,
    #[arg(long)]
    soapy_gain_element: Vec<String>,
}

impl ScanArgs {
    fn into_request(self) -> ScanRequest {
        ScanRequest {
            preset: self.preset,
            backend: self.backend.map(Into::into),
            peaks_file: self.peaks_file,
            iq_file: self.iq_file,
            profile: self.profile,
            profile_freqs: self.profile_freqs,
            min_hits: self.min_hits,
            min_score: self.min_score,
            search_window: self.search_window,
            match_window: self.match_window,
            offset_model: self.offset_model.map(Into::into),
            min_peak_snr: self.min_peak_snr,
            max_peaks: self.max_peaks,
            record: self.record,
            record_mode: self.record_mode.map(Into::into),
            record_only: self.record_only,
            record_spectrum_bins: self.record_spectrum_bins,
            record_band_decimation: self.record_band_decimation,
            output: self.output.map(|value| {
                match value {
                    OutputArg::Tui => "tui",
                    OutputArg::Table => "table",
                    OutputArg::Json => "json",
                }
                .to_string()
            }),
            duration: self.duration,
            continuous: self.continuous,
            once: self.once,
            repeat_delay: self.repeat_delay,
            confirm_passes: self.confirm_passes,
            confirm_ratio: self.confirm_ratio,
            mask: self.mask,
            dwell_ms: self.dwell_ms,
            fft_frames: self.fft_frames,
            capture_timeout_ms: self.capture_timeout_ms,
            settle_ms: self.settle_ms,
            synthetic_offset: self.synthetic_offset,
            synthetic_snr: self.synthetic_snr,
            synthetic_floor: self.synthetic_floor,
            synthetic_jitter: self.synthetic_jitter,
            synthetic_missing: self.synthetic_missing,
            interferer: self.interferer,
            airspy_device_index: self.airspy_device_index,
            airspy_sample_rate: self.airspy_sample_rate,
            airspy_dwell_ms: self.airspy_dwell_ms,
            airspy_fft_size: self.airspy_fft_size,
            airspy_min_peak_snr: self.airspy_min_peak_snr,
            airspy_max_peaks: self.airspy_max_peaks,
            airspy_dc_notch: self.airspy_dc_notch,
            airspy_settle_ms: self.airspy_settle_ms,
            gain_mode: self.gain_mode,
            gain_preset: self.gain_preset,
            lna: self.lna,
            mixer: self.mixer,
            vga: self.vga,
            lna_agc: self.lna_agc,
            mixer_agc: self.mixer_agc,
            soapy_device: self.soapy_device,
            soapy_device_index: self.soapy_device_index,
            soapy_channel: self.soapy_channel,
            soapy_sample_rate: self.soapy_sample_rate,
            soapy_dwell_ms: self.soapy_dwell_ms,
            soapy_fft_size: self.soapy_fft_size,
            soapy_min_peak_snr: self.soapy_min_peak_snr,
            soapy_max_peaks: self.soapy_max_peaks,
            soapy_dc_notch: self.soapy_dc_notch,
            soapy_settle_ms: self.soapy_settle_ms,
            soapy_antenna: self.soapy_antenna,
            soapy_bandwidth: self.soapy_bandwidth,
            soapy_gain_mode: self.soapy_gain_mode,
            soapy_gain: self.soapy_gain,
            soapy_gain_element: self.soapy_gain_element,
        }
    }
}

impl From<BackendArg> for Backend {
    fn from(value: BackendArg) -> Self {
        match value {
            BackendArg::Synthetic => Self::Synthetic,
            BackendArg::PeaksFile => Self::PeaksFile,
            BackendArg::IqFile => Self::IqFile,
            BackendArg::Airspy => Self::Airspy,
            BackendArg::Soapy => Self::Soapy,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Devices => print_devices(),
        Commands::DefaultConfig => {
            print!("{}", AppConfig::default_toml());
            Ok(())
        }
        Commands::Scan(args) => run_scan(cli.config.as_deref(), *args),
        command => {
            let config = match cli.config {
                Some(path) => AppConfig::load(&path)?,
                None => AppConfig::load_defaults()?,
            };
            match command {
                Commands::Profiles => print_profiles(&config),
                Commands::ShowConfig => {
                    println!("{}", toml::to_string_pretty(&config)?);
                    Ok(())
                }
                _ => unreachable!(),
            }
        }
    }
}

fn print_profiles(config: &AppConfig) -> Result<()> {
    println!(
        "{:<28} {:<48} {:<10} {:<8} {:<10}",
        "profile", "freqs_mhz", "offset", "min_hit", "match_hz"
    );
    for profile in config
        .profiles
        .iter()
        .flat_map(DetectionProfile::detection_profiles)
    {
        let freqs = profile
            .freqs_mhz
            .iter()
            .map(|freq| format!("{freq:.3}"))
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{:<28} {:<48} {:<10} {:<8} {:<10.0}",
            profile.name,
            freqs,
            offset_model_name(profile.offset_model),
            profile.min_hits,
            profile.max_offset_spread_hz
        );
    }
    Ok(())
}

fn offset_model_name(model: OffsetModel) -> &'static str {
    match model {
        OffsetModel::FixedHz => "fixed_hz",
        OffsetModel::Ppm => "ppm",
        OffsetModel::Auto => "auto",
    }
}

fn run_scan(path: Option<&std::path::Path>, args: ScanArgs) -> Result<()> {
    let resolved = args.into_request().resolve(path)?;
    let output = resolve_output(OutputArg::from_str(&resolved.output, true).map_err(|_| {
        anyhow!(
            "unknown output '{}', expected tui/table/json",
            resolved.output
        )
    })?);
    let repeat = resolved.run.repeat;
    let profiles = resolved.engine.profiles.clone();
    let masks = MaskSet::new(resolved.engine.masks.clone());
    let stop = setup_stop_flag()?;
    if matches!(output, OutputArg::Tui) {
        let mut session = ScanSession::start_with_options(resolved.engine, resolved.run)?;
        let result = run_tui_loop(&mut session, stop);
        let stopped = session.stop();
        result?;
        stopped?;
        let snapshot = session.snapshot();
        if repeat {
            print_final_persistence_table(
                Duration::from_secs_f64(snapshot.elapsed_seconds),
                snapshot.pass,
                &snapshot.persistence,
            )?;
        }
        return Ok(());
    }
    let (mut session, passes) = ScanSession::observe(resolved.engine, resolved.run)?;
    let result = (|| -> Result<()> {
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let data = match passes.recv_timeout(Duration::from_millis(50)) {
                Ok(data) => data,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if matches!(
                        session.snapshot().status,
                        SessionStatus::Stopped | SessionStatus::Error
                    ) {
                        break;
                    }
                    continue;
                }
            };
            let results = data
                .results
                .iter()
                .filter(|result| result.detected)
                .cloned()
                .collect::<Vec<_>>();
            match output {
                OutputArg::Table if repeat => print_scan_pass_table(
                    data.pass,
                    Duration::from_secs_f64(data.elapsed_seconds),
                    Duration::from_secs_f64(data.pass_seconds),
                    &data.peaks,
                    &data.results,
                    &data.persistence,
                )?,
                OutputArg::Table => print_scan_table(&profiles, &data.peaks, &masks, &results)?,
                OutputArg::Json if repeat => {
                    serde_json::to_writer(
                        io::stdout().lock(),
                        &JsonPass {
                            kind: "scan_pass",
                            pass: data.pass,
                            elapsed_seconds: data.elapsed_seconds,
                            pass_seconds: data.pass_seconds,
                            profiles: &profiles,
                            masks: masks.masks(),
                            peaks: &data.peaks,
                            results: &results,
                            persistence: &data.persistence,
                            statistics: &data.statistics,
                            matching_ms: data.matching_ms,
                            recording_ms: data.recording_ms,
                        },
                    )?;
                    println!();
                }
                OutputArg::Json => {
                    serde_json::to_writer_pretty(
                        io::stdout().lock(),
                        &JsonResult {
                            profiles: &profiles,
                            masks: masks.masks(),
                            peaks: &data.peaks,
                            results: &results,
                            statistics: &data.statistics,
                        },
                    )?;
                    println!();
                }
                OutputArg::Tui => unreachable!(),
            }
        }
        Ok(())
    })();
    let stopped = session.stop();
    result?;
    stopped?;
    let snapshot = session.snapshot();
    if repeat {
        match output {
            OutputArg::Json => {
                serde_json::to_writer(
                    io::stdout().lock(),
                    &JsonFinal {
                        kind: "scan_final",
                        elapsed_seconds: snapshot.elapsed_seconds,
                        passes: snapshot.pass,
                        persistence: &snapshot.persistence,
                    },
                )?;
                println!();
            }
            _ => print_final_persistence_table(
                Duration::from_secs_f64(snapshot.elapsed_seconds),
                snapshot.pass,
                &snapshot.persistence,
            )?,
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct JsonPass<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    pass: usize,
    elapsed_seconds: f64,
    pass_seconds: f64,
    profiles: &'a [DetectionProfile],
    masks: &'a [camfinder::mask::FrequencyMask],
    peaks: &'a [Peak],
    results: &'a [ProfileDetection],
    persistence: &'a [PersistenceSummary],
    statistics: &'a [TargetStatistics],
    matching_ms: f64,
    recording_ms: f64,
}

#[derive(Serialize)]
struct JsonResult<'a> {
    profiles: &'a [DetectionProfile],
    masks: &'a [camfinder::mask::FrequencyMask],
    peaks: &'a [Peak],
    results: &'a [ProfileDetection],
    statistics: &'a [TargetStatistics],
}

#[derive(Serialize)]
struct JsonFinal<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    elapsed_seconds: f64,
    passes: usize,
    persistence: &'a [PersistenceSummary],
}

fn resolve_output(requested: OutputArg) -> OutputArg {
    if matches!(requested, OutputArg::Tui) && !io::stdout().is_terminal() {
        OutputArg::Table
    } else {
        requested
    }
}

fn setup_stop_flag() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let handler_stop = Arc::clone(&stop);
        ctrlc::set_handler(move || {
            handler_stop.store(true, Ordering::Relaxed);
        })?;
    }
    Ok(stop)
}

fn print_devices() -> Result<()> {
    println!("Airspy:");
    #[cfg(feature = "airspy")]
    match list_airspy_devices() {
        Ok(devices) if devices.is_empty() => println!("  No Airspy devices found."),
        Ok(devices) => {
            println!("  {:<8} device", "index");
            for device in devices {
                println!("  {:<8} {}", device.index, device.label);
            }
        }
        Err(err) => println!("  unavailable: {err:#}"),
    }

    println!("SoapySDR:");
    match list_soapy_devices("") {
        Ok(devices) if devices.is_empty() => println!("  No SoapySDR devices found."),
        Ok(devices) => {
            println!("  {:<8} args", "index");
            for device in devices {
                println!("  {:<8} {}", device.index, device.args);
            }
        }
        Err(err) => println!("  unavailable: {err:#}"),
    }
    Ok(())
}

fn print_scan_table(
    profiles: &[DetectionProfile],
    peaks: &[Peak],
    masks: &MaskSet,
    results: &[ProfileDetection],
) -> Result<()> {
    println!("Profiles:");
    for profile in profiles {
        println!(
            "  {} freqs={} offset_model={} min_hits={} min_score={:.1} match_window={:.0} Hz",
            profile.name,
            profile
                .freqs_mhz
                .iter()
                .map(|freq| format!("{freq:.3}"))
                .collect::<Vec<_>>()
                .join(","),
            offset_model_name(profile.offset_model),
            profile.min_hits,
            profile.min_score,
            profile.max_offset_spread_hz
        );
    }

    if !masks.masks().is_empty() {
        println!("Masks:");
        for mask in masks.masks() {
            println!(
                "  {} width {}",
                format_frequency_hz(mask.center_hz),
                format_frequency_hz(mask.width_hz)
            );
        }
    }

    println!("Peaks:");
    for peak in peaks {
        let masked = if masks.is_masked(peak.freq_hz) {
            " masked"
        } else {
            ""
        };
        println!(
            "  #{:<3} {:>16} snr={:>5.1} dB source={}{}",
            peak.id,
            format_frequency_hz(peak.freq_hz),
            peak.snr_db(),
            peak.source.as_deref().unwrap_or("-"),
            masked
        );
    }

    println!("Results:");
    println!(
        "{:<16} {:<8} {:<7} {:<8} {:<8} {:<8} {:<18} model",
        "profile", "state", "hits", "score", "snr", "spread", "offset"
    );
    if results.is_empty() {
        println!("No suspects detected.");
    }
    for result in results {
        let state = if result.detected { "detect" } else { "miss" };
        let (offset, model) = format_detection_offset(result);
        println!(
            "{:<16} {:<8} {:<7} {:<8.2} {:<8.1} {:<8.0} {:<18} {}",
            result.profile,
            state,
            format!("{}/{}", result.hits, result.required_hits),
            result.score,
            result.snr_sum_db,
            result.offset_spread_hz,
            offset,
            model
        );
        for hit in &result.hit_details {
            println!(
                "  nominal={:.3} MHz peak=#{:<3} observed={} err={} snr={:.1} dB",
                hit.nominal_mhz,
                hit.peak_id,
                format_frequency_hz(hit.observed_hz),
                format_hit_error(result, hit),
                hit.snr_db
            );
        }
        if !result.misses_mhz.is_empty() {
            println!(
                "  misses={}",
                result
                    .misses_mhz
                    .iter()
                    .map(|freq| format!("{freq:.3}"))
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
    }

    Ok(())
}

fn print_scan_pass_table(
    pass: usize,
    elapsed: Duration,
    pass_duration: Duration,
    peaks: &[Peak],
    results: &[ProfileDetection],
    summaries: &[PersistenceSummary],
) -> Result<()> {
    println!(
        "\nPass #{pass} elapsed={} pass={} peaks={}",
        format_duration(elapsed),
        format_duration(pass_duration),
        peaks.len()
    );
    println!(
        "{:<5} {:<16} {:<10} {:<7} {:<8} {:<8} {:<8} {:<18} {:<13} {:<8} {:<10}",
        "id",
        "profile",
        "state",
        "hits",
        "score",
        "snr",
        "spread",
        "offset",
        "seen",
        "consec",
        "now"
    );

    if summaries.is_empty() {
        println!("No suspects.");
    }

    for summary in summaries {
        let result = summary.current_result(results);
        let (hits, required_hits, score, snr_sum, spread, offset) = if let Some(result) = result {
            (
                result.hits,
                result.required_hits,
                result.score,
                result.snr_sum_db,
                result.offset_spread_hz,
                format_detection_offset(result).0,
            )
        } else {
            (
                summary.last_hits,
                0,
                summary.last_score,
                summary.last_snr_sum_db,
                summary.last_offset_spread_hz,
                format_summary_offset(summary),
            )
        };
        let required = if required_hits == 0 {
            "-".to_string()
        } else {
            required_hits.to_string()
        };
        let seen = format!(
            "{}/{} {:>3.0}%",
            summary.detected_passes,
            summary.total_passes,
            summary.detection_ratio * 100.0
        );
        let now = if let Some(result) = result {
            if result.detected {
                "hit".to_string()
            } else {
                "weak".to_string()
            }
        } else {
            format!("missed={}", summary.missed_passes)
        };
        println!(
            "{:<5} {:<16} {:<10} {:<7} {:<8.2} {:<8.1} {:<8.0} {:<18} {:<13} {:<8} {:<10}",
            format!("#{:02}", summary.display_order + 1),
            summary.profile,
            stability_label(&summary.state),
            format!("{hits}/{required}"),
            score,
            snr_sum,
            spread,
            offset,
            seen,
            summary.consecutive_detected,
            now
        );
    }

    if !peaks.is_empty() {
        let peak_line = peaks
            .iter()
            .map(|peak| {
                format!(
                    "#{} {} {:.1}dB",
                    peak.id,
                    format_frequency_hz(peak.freq_hz),
                    peak.snr_db()
                )
            })
            .collect::<Vec<_>>()
            .join(" | ");
        println!("peaks: {peak_line}");
    }

    Ok(())
}

fn print_final_persistence_table(
    elapsed: Duration,
    passes: usize,
    summaries: &[PersistenceSummary],
) -> Result<()> {
    println!(
        "\nFinal persistence elapsed={} passes={passes}",
        format_duration(elapsed)
    );
    println!(
        "{:<5} {:<16} {:<10} {:<13} {:<8} {:<8} {:<8} {:<8} {:<18}",
        "id", "profile", "state", "seen", "consec", "missed", "best", "last", "last_offset"
    );
    for summary in summaries {
        let offset = format_summary_offset(summary);
        println!(
            "{:<5} {:<16} {:<10} {:<13} {:<8} {:<8} {:<8.2} {:<8.2} {:<18}",
            format!("#{:02}", summary.display_order + 1),
            summary.profile,
            stability_label(&summary.state),
            format!(
                "{}/{} {:>3.0}%",
                summary.detected_passes,
                summary.total_passes,
                summary.detection_ratio * 100.0
            ),
            summary.consecutive_detected,
            summary.missed_passes,
            summary.best_score,
            summary.last_score,
            offset
        );
    }
    Ok(())
}

fn stability_label(state: &StabilityState) -> &'static str {
    match state {
        StabilityState::Stable => "stable",
        StabilityState::Candidate => "candidate",
        StabilityState::Tracking => "tracking",
        StabilityState::Transient => "transient",
        StabilityState::Absent => "absent",
    }
}

fn format_offset(offset: Option<OffsetEstimate>) -> (String, String) {
    match offset {
        Some(OffsetEstimate::FixedHz { hz }) => (format_offset_hz(hz), "fixed_hz".to_string()),
        Some(OffsetEstimate::Ppm { ppm }) => (format!("{ppm:+.3} ppm"), "ppm".to_string()),
        None => ("-".to_string(), "-".to_string()),
    }
}

fn format_detection_offset(result: &ProfileDetection) -> (String, String) {
    match result.offset {
        Some(OffsetEstimate::Ppm { .. }) => format_offset(result.offset),
        _ => result
            .offset_group_hz
            .map(|hz| (format_offset_hz(hz), "fixed_hz".to_string()))
            .unwrap_or_else(|| format_offset(result.offset)),
    }
}

fn format_summary_offset(summary: &PersistenceSummary) -> String {
    match summary.last_offset {
        Some(OffsetEstimate::Ppm { .. }) => format_offset(summary.last_offset).0,
        _ => summary
            .offset_group_hz
            .map(format_offset_hz)
            .unwrap_or_else(|| format_offset(summary.last_offset).0),
    }
}

fn format_hit_error(result: &ProfileDetection, hit: &camfinder::detect::DetectionHit) -> String {
    match result.offset {
        Some(OffsetEstimate::Ppm { .. }) => {
            format!(
                "{:+.3} ppm ({})",
                hit.error_hz / hit.nominal_mhz,
                format_offset_hz(hit.error_hz)
            )
        }
        _ => format_offset_hz(hit.error_hz),
    }
}
