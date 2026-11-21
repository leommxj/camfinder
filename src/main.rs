use anyhow::{anyhow, Result};
#[cfg(feature = "airspy")]
use camfinder::airspy_backend::list_airspy_devices;
use camfinder::config::{AppConfig, DetectionProfile, OffsetModel, RecordMode};
use camfinder::detect::{OffsetEstimate, ProfileDetection};
use camfinder::engine::{EngineConfig, ScanEngine};
use camfinder::frequency::{
    format_frequency_hz, format_offset_hz, parse_frequency_hz, parse_frequency_list_mhz,
};
use camfinder::mask::{parse_mask_spec, MaskSet};
use camfinder::peak::Peak;
use camfinder::persistence::{PersistenceSummary, StabilityState};
use camfinder::sdr::{
    AirspySettings, DeviceSettings, SoapyGainElement, SoapySettings, TargetStatistics,
};
use camfinder::soapy_backend::list_soapy_devices;
use camfinder::synthetic::{SyntheticInterferer, SyntheticOptions};
use camfinder::timeparse::{format_duration, parse_duration};
use camfinder::tui::{run_tui_loop, TuiScanSnapshot};
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::VecDeque;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

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
    #[arg(long, default_value = "0s")]
    repeat_delay: String,
    #[arg(long, default_value_t = 3)]
    confirm_passes: usize,
    #[arg(long, default_value_t = 0.6)]
    confirm_ratio: f64,
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
    #[arg(long, default_value = "+12.5k")]
    synthetic_offset: String,
    #[arg(long, default_value_t = 14.0)]
    synthetic_snr: f64,
    #[arg(long, default_value_t = -88.0)]
    synthetic_floor: f64,
    #[arg(long, default_value = "150")]
    synthetic_jitter: String,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Commands::Devices) {
        return print_devices();
    }
    if matches!(cli.command, Commands::DefaultConfig) {
        print!("{}", AppConfig::default_toml());
        return Ok(());
    }
    let config = match &cli.config {
        Some(path) => AppConfig::load(path)?,
        None if matches!(&cli.command, Commands::Scan(args) if args.profile.is_some() || args.profile_freqs.is_some()) => {
            toml::from_str(AppConfig::default_toml())?
        }
        None => AppConfig::load_defaults()?,
    };

    match cli.command {
        Commands::Devices => print_devices(),
        Commands::Profiles => print_profiles(&config),
        Commands::DefaultConfig => {
            print!("{}", AppConfig::default_toml());
            Ok(())
        }
        Commands::ShowConfig => {
            println!("{}", toml::to_string_pretty(&config)?);
            Ok(())
        }
        Commands::Scan(args) => run_scan(config, *args),
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

fn run_scan(mut config: AppConfig, args: ScanArgs) -> Result<()> {
    let match_window_hz = args
        .match_window
        .as_deref()
        .map(parse_frequency_hz)
        .transpose()?;
    let profiles = select_profiles(&config, &args, match_window_hz)?;
    apply_selected_profile_overrides(&mut config, &profiles);
    apply_scan_arg_overrides(&mut config, &args)?;
    let backend = resolve_backend(&config, &args)?;
    let active_device = active_device_settings(&config.device, &args, backend)?;
    let masks = collect_masks(&config, &args)?;
    let duration = args.duration.as_deref().map(parse_duration).transpose()?;
    let repeat_delay = parse_duration(&args.repeat_delay)?;
    let repeat = should_repeat(&args, backend);
    let output = resolve_output(resolve_output_arg(&config, &args)?);
    let stop = setup_stop_flag(true)?;
    let mut engine = ScanEngine::new(EngineConfig {
        profiles: profiles.clone(),
        device: active_device,
        scan: config.scan.clone(),
        masks: masks.masks().to_vec(),
        confirm_passes: args.confirm_passes,
        confirm_ratio: args.confirm_ratio,
        record_only: args.record_only,
        synthetic: synthetic_options(&args)?,
    })?;
    let mut logs = VecDeque::new();
    push_log(
        &mut logs,
        format!("scan started backend={}", backend_config_name(backend)),
    );
    let started_at = Instant::now();
    let (tui_sender, tui_handle) = if matches!(output, OutputArg::Tui) {
        let (sender, receiver) = mpsc::sync_channel(1);
        let tui_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let result = run_tui_loop(receiver, Arc::clone(&tui_stop));
            tui_stop.store(true, Ordering::Relaxed);
            result
        });
        (Some(sender), Some(handle))
    } else {
        (None, None)
    };
    send_tui_snapshot(
        &tui_sender,
        TuiScanSnapshot {
            pass: 0,
            elapsed: Duration::ZERO,
            pass_duration: Duration::ZERO,
            peaks: Vec::new(),
            results: Vec::new(),
            summaries: Vec::new(),
            logs: logs.clone(),
        },
    );
    let deadline = duration.map(|duration| started_at + duration);
    let mut pass = 0;
    let mut final_summaries = Vec::new();
    let scan_result = (|| -> Result<()> {
        loop {
            if stop.load(Ordering::Relaxed)
                || (pass > 0 && deadline.is_some_and(|deadline| Instant::now() >= deadline))
            {
                break;
            }
            let Some(data) =
                engine.scan(&stop, &mut |line| push_log(&mut logs, line), &mut |_| {})?
            else {
                break;
            };
            pass = data.pass;
            let pass_duration = Duration::from_secs_f64(data.pass_seconds);
            let results: Vec<_> = data
                .results
                .iter()
                .filter(|result| result.detected)
                .cloned()
                .collect();
            final_summaries = data.persistence;
            push_log(
                &mut logs,
                format!(
                    "pass #{pass} finished peaks={} elapsed={}",
                    data.peaks.len(),
                    format_duration(pass_duration)
                ),
            );
            match output {
                OutputArg::Tui => {
                    let mut peaks = data.peaks;
                    for peak in &mut peaks {
                        peak.spectrum_window.clear();
                    }
                    send_tui_snapshot(
                        &tui_sender,
                        TuiScanSnapshot {
                            pass,
                            elapsed: started_at.elapsed(),
                            pass_duration,
                            peaks,
                            results: data.results,
                            summaries: final_summaries.clone(),
                            logs: logs.clone(),
                        },
                    );
                }
                OutputArg::Table if repeat => print_scan_pass_table(
                    pass,
                    started_at.elapsed(),
                    pass_duration,
                    &data.peaks,
                    &data.results,
                    &final_summaries,
                )?,
                OutputArg::Table => print_scan_table(&profiles, &data.peaks, &masks, &results)?,
                OutputArg::Json if repeat => {
                    serde_json::to_writer(
                        io::stdout().lock(),
                        &JsonPass {
                            kind: "scan_pass",
                            pass,
                            elapsed_seconds: started_at.elapsed().as_secs_f64(),
                            pass_seconds: data.pass_seconds,
                            profiles: &profiles,
                            masks: masks.masks(),
                            peaks: &data.peaks,
                            results: &results,
                            persistence: &final_summaries,
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
            }
            if !repeat {
                break;
            }
            sleep_interruptible(repeat_delay, deadline, &stop);
        }
        Ok(())
    })();
    drop(engine);
    drop(tui_sender);
    if let Some(handle) = tui_handle {
        handle
            .join()
            .map_err(|_| anyhow!("TUI thread panicked"))??;
    }
    scan_result?;
    if repeat && matches!(output, OutputArg::Table | OutputArg::Tui) {
        print_final_persistence_table(started_at.elapsed(), pass, &final_summaries)?;
    } else if repeat && matches!(output, OutputArg::Json) {
        serde_json::to_writer(
            io::stdout().lock(),
            &JsonFinal {
                kind: "scan_final",
                elapsed_seconds: started_at.elapsed().as_secs_f64(),
                passes: pass,
                persistence: &final_summaries,
            },
        )?;
        println!();
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

fn should_repeat(args: &ScanArgs, backend: BackendArg) -> bool {
    if args.once {
        false
    } else {
        args.continuous
            || args.duration.is_some()
            || matches!(backend, BackendArg::Airspy | BackendArg::Soapy)
    }
}

fn setup_stop_flag(enabled: bool) -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    if enabled {
        let handler_stop = Arc::clone(&stop);
        ctrlc::set_handler(move || {
            handler_stop.store(true, Ordering::Relaxed);
        })?;
    }
    Ok(stop)
}

fn active_device_settings(
    base: &DeviceSettings,
    args: &ScanArgs,
    backend: BackendArg,
) -> Result<DeviceSettings> {
    let mut device = base.clone();
    device.backend = Some(backend_config_name(backend).to_string());

    match backend {
        BackendArg::Synthetic => {}
        BackendArg::PeaksFile => {
            if let Some(path) = &args.peaks_file {
                device.peaks_file.path = Some(path.clone());
            }
        }
        BackendArg::IqFile => {
            if let Some(path) = &args.iq_file {
                device.replay.manifest = Some(path.clone());
            }
        }
        BackendArg::Airspy => {
            device.airspy = airspy_settings_from_args(&base.airspy, args)?;
        }
        BackendArg::Soapy => {
            device.soapy = soapy_settings_from_args(&base.soapy, args)?;
        }
    }

    Ok(device)
}

fn backend_config_name(backend: BackendArg) -> &'static str {
    match backend {
        BackendArg::Synthetic => "synthetic",
        BackendArg::PeaksFile => "peaks-file",
        BackendArg::IqFile => "iq-file",
        BackendArg::Airspy => "airspy",
        BackendArg::Soapy => "soapy",
    }
}

fn resolve_backend(config: &AppConfig, args: &ScanArgs) -> Result<BackendArg> {
    if let Some(backend) = args.backend {
        return Ok(backend);
    }
    let backend = config
        .device
        .backend
        .as_deref()
        .map(str::trim)
        .filter(|backend| !backend.is_empty())
        .ok_or_else(|| anyhow!("scan backend is required; set --backend or [device].backend"))?;

    BackendArg::from_str(backend, true).map_err(|_| {
        anyhow!("unknown backend '{backend}', expected synthetic/peaks-file/iq-file/airspy/soapy")
    })
}

fn resolve_output_arg(config: &AppConfig, args: &ScanArgs) -> Result<OutputArg> {
    if let Some(output) = args.output {
        return Ok(output);
    }
    let output = config.scan.output.trim();
    OutputArg::from_str(output, true)
        .map_err(|_| anyhow!("unknown output '{output}', expected tui/table/json"))
}

fn push_log(logs: &mut VecDeque<String>, line: impl Into<String>) {
    const MAX_LOGS: usize = 500;
    if logs.len() >= MAX_LOGS {
        logs.pop_front();
    }
    logs.push_back(line.into());
}

fn sleep_interruptible(delay: Duration, deadline: Option<Instant>, stop: &AtomicBool) {
    if delay.is_zero() {
        return;
    }

    let wake_at = Instant::now() + delay;
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return;
        }

        let now = Instant::now();
        if now >= wake_at {
            return;
        }

        let mut sleep_for = wake_at
            .saturating_duration_since(now)
            .min(Duration::from_millis(100));
        if let Some(deadline) = deadline {
            sleep_for = sleep_for.min(deadline.saturating_duration_since(now));
        }
        if sleep_for.is_zero() {
            return;
        }
        thread::sleep(sleep_for);
    }
}

fn send_tui_snapshot(
    sender: &Option<mpsc::SyncSender<TuiScanSnapshot>>,
    snapshot: TuiScanSnapshot,
) {
    if let Some(sender) = sender {
        let _ = sender.send(snapshot);
    }
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

fn select_profiles(
    config: &AppConfig,
    args: &ScanArgs,
    match_window_hz: Option<f64>,
) -> Result<Vec<DetectionProfile>> {
    if let Some(freqs) = &args.profile_freqs {
        let freqs_mhz = parse_frequency_list_mhz(freqs)?;
        if freqs_mhz.is_empty() {
            return Err(anyhow!("--profile-freqs cannot be empty"));
        }
        let min_hits = args
            .min_hits
            .unwrap_or_else(|| ((freqs_mhz.len() as f64) * 0.67).ceil() as usize);
        return Ok(vec![DetectionProfile {
            name: "cli-custom".to_string(),
            freqs_mhz,
            offset_model: args.offset_model.map(Into::into).unwrap_or_default(),
            min_hits,
            min_score: args.min_score.unwrap_or(8.0),
            max_offset_spread_hz: match_window_hz.unwrap_or(1_200.0),
            groups: Vec::new(),
            scan: None,
            device: None,
        }]);
    }

    let selected = if let Some(path) = &args.profile {
        DetectionProfile::load(path)?
    } else {
        match config.profiles.as_slice() {
            [profile] => profile.clone(),
            [] => {
                return Err(anyhow!(
                    "no profile configured; pass --profile <path> or use --profile-freqs"
                ))
            }
            _ => {
                return Err(anyhow!(
                    "scan requires a single profile; pass --profile <path> or configure one profile file"
                ))
            }
        }
    };

    Ok(selected
        .with_overrides(
            args.min_hits,
            args.min_score,
            match_window_hz,
            args.offset_model.map(Into::into),
        )
        .detection_profiles())
}

fn apply_selected_profile_overrides(config: &mut AppConfig, profiles: &[DetectionProfile]) {
    let Some(first_profile) = profiles.first() else {
        return;
    };

    if let Some(first_scan) = &first_profile.scan {
        if profiles
            .iter()
            .all(|profile| profile.scan.as_ref() == Some(first_scan))
        {
            config.scan = first_scan.clone();
        }
    }

    if let Some(first_device) = &first_profile.device {
        if profiles
            .iter()
            .all(|profile| profile.device.as_ref() == Some(first_device))
        {
            config.device = first_device.clone();
        }
    }
}

fn apply_scan_arg_overrides(config: &mut AppConfig, args: &ScanArgs) -> Result<()> {
    if let Some(search_window) = &args.search_window {
        config.scan.peak_search_window_hz = parse_frequency_hz(search_window)?;
    }
    if let Some(min_peak_snr) = args
        .min_peak_snr
        .or(args.airspy_min_peak_snr)
        .or(args.soapy_min_peak_snr)
    {
        config.scan.min_peak_snr_db = min_peak_snr;
    }
    if let Some(max_peaks) = args
        .max_peaks
        .or(args.airspy_max_peaks)
        .or(args.soapy_max_peaks)
    {
        config.scan.max_peaks_per_freq = max_peaks;
    }
    if let Some(record_path) = &args.record {
        config.scan.record_path = Some(record_path.clone());
    }
    if let Some(mode) = args.record_mode {
        config.scan.record_mode = mode.into();
    }
    if let Some(record_spectrum_bins) = args.record_spectrum_bins {
        config.scan.record_spectrum_bins = record_spectrum_bins;
    }
    if let Some(record_band_decimation) = args.record_band_decimation {
        config.scan.record_band_decimation = record_band_decimation.max(1);
    }
    Ok(())
}

fn collect_masks(config: &AppConfig, args: &ScanArgs) -> Result<MaskSet> {
    let mut masks = config.masks.clone();
    for mask in &args.mask {
        masks.push(parse_mask_spec(mask)?);
    }
    Ok(MaskSet::new(masks))
}

fn airspy_settings_from_args(base: &AirspySettings, args: &ScanArgs) -> Result<AirspySettings> {
    let mut settings = base.clone();
    if let Some(frames) = args.fft_frames {
        settings.fft_frames = Some(frames);
    }
    if let Some(timeout) = args.capture_timeout_ms {
        settings.capture_timeout_ms = timeout;
    }

    if let Some(dwell_ms) = args.dwell_ms {
        settings.dwell_ms = dwell_ms;
        settings.fft_frames = None;
    }
    if let Some(settle_ms) = args.settle_ms {
        settings.settle_ms = settle_ms;
    }
    if let Some(index) = args.airspy_device_index {
        settings.device_index = index;
    }
    if let Some(sample_rate) = &args.airspy_sample_rate {
        settings.sample_rate_hz = parse_frequency_hz(sample_rate)?.round() as u32;
    }
    if let Some(dwell_ms) = args.airspy_dwell_ms {
        settings.dwell_ms = dwell_ms;
        settings.fft_frames = None;
    }
    if let Some(fft_size) = args.airspy_fft_size {
        settings.fft_size = fft_size;
    }
    if let Some(dc_notch) = &args.airspy_dc_notch {
        settings.dc_notch_hz = parse_frequency_hz(dc_notch)?;
    }
    if let Some(settle_ms) = args.airspy_settle_ms {
        settings.settle_ms = settle_ms;
    }
    if let Some(mode) = &args.gain_mode {
        settings.gain.mode = mode.clone();
    }
    if let Some(preset) = args.gain_preset {
        settings.gain.preset = preset;
    }
    if let Some(lna) = args.lna {
        settings.gain.lna = lna;
    }
    if let Some(mixer) = args.mixer {
        settings.gain.mixer = mixer;
    }
    if let Some(vga) = args.vga {
        settings.gain.vga = vga;
    }
    if args.lna_agc {
        settings.gain.lna_agc = true;
    }
    if args.mixer_agc {
        settings.gain.mixer_agc = true;
    }

    Ok(settings)
}

fn soapy_settings_from_args(base: &SoapySettings, args: &ScanArgs) -> Result<SoapySettings> {
    let mut settings = base.clone();
    if let Some(frames) = args.fft_frames {
        settings.fft_frames = Some(frames);
    }
    if let Some(timeout) = args.capture_timeout_ms {
        settings.capture_timeout_ms = timeout;
    }

    if let Some(dwell_ms) = args.dwell_ms {
        settings.dwell_ms = dwell_ms;
        settings.fft_frames = None;
    }
    if let Some(settle_ms) = args.settle_ms {
        settings.settle_ms = settle_ms;
    }
    if let Some(device) = &args.soapy_device {
        settings.device_args = device.clone();
    }
    if let Some(index) = args.soapy_device_index {
        settings.device_index = index;
    }
    if let Some(channel) = args.soapy_channel {
        settings.channel = channel;
    }
    if let Some(sample_rate) = &args.soapy_sample_rate {
        settings.sample_rate_hz = parse_frequency_hz(sample_rate)?.round() as u32;
    }
    if let Some(dwell_ms) = args.soapy_dwell_ms {
        settings.dwell_ms = dwell_ms;
        settings.fft_frames = None;
    }
    if let Some(fft_size) = args.soapy_fft_size {
        settings.fft_size = fft_size;
    }
    if let Some(dc_notch) = &args.soapy_dc_notch {
        settings.dc_notch_hz = parse_frequency_hz(dc_notch)?;
    }
    if let Some(settle_ms) = args.soapy_settle_ms {
        settings.settle_ms = settle_ms;
    }
    if let Some(antenna) = &args.soapy_antenna {
        settings.antenna = Some(antenna.clone());
    }
    if let Some(bandwidth) = &args.soapy_bandwidth {
        settings.bandwidth_hz = Some(parse_frequency_hz(bandwidth)?);
    }
    if let Some(mode) = &args.soapy_gain_mode {
        settings.gain.mode = mode.clone();
    }
    if let Some(gain_db) = args.soapy_gain {
        settings.gain.gain_db = Some(gain_db);
        if settings.gain.mode.eq_ignore_ascii_case("none") {
            settings.gain.mode = "manual".to_string();
        }
    }
    if !args.soapy_gain_element.is_empty() {
        settings.gain.elements = args
            .soapy_gain_element
            .iter()
            .map(|input| parse_soapy_gain_element(input))
            .collect::<Result<Vec<_>>>()?;
        if settings.gain.mode.eq_ignore_ascii_case("none") {
            settings.gain.mode = "manual".to_string();
        }
    }

    Ok(settings)
}

fn parse_soapy_gain_element(input: &str) -> Result<SoapyGainElement> {
    let (name, gain) = input
        .split_once('=')
        .or_else(|| input.split_once(':'))
        .ok_or_else(|| anyhow!("invalid Soapy gain element '{input}', expected NAME=DB"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!("invalid Soapy gain element '{input}', empty name"));
    }
    Ok(SoapyGainElement {
        name: name.to_string(),
        gain_db: gain
            .trim()
            .parse::<f64>()
            .map_err(|_| anyhow!("invalid Soapy gain value in '{input}'"))?,
    })
}

fn synthetic_options(args: &ScanArgs) -> Result<SyntheticOptions> {
    let missing_mhz = args
        .synthetic_missing
        .iter()
        .map(|freq| parse_frequency_hz(freq).map(|hz| hz / 1_000_000.0))
        .collect::<Result<Vec<_>>>()?;
    let interferers = args
        .interferer
        .iter()
        .map(|input| SyntheticInterferer::parse(input))
        .collect::<Result<Vec<_>>>()?;

    let options = SyntheticOptions {
        offset_hz: parse_frequency_hz(&args.synthetic_offset)?,
        signal_snr_db: args.synthetic_snr,
        floor_db: args.synthetic_floor,
        jitter_hz: parse_frequency_hz(&args.synthetic_jitter)?,
        missing_mhz,
        interferers,
    };
    Ok(options)
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
        let result = current_result_for(summary, results);
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

fn current_result_for<'a>(
    summary: &PersistenceSummary,
    results: &'a [ProfileDetection],
) -> Option<&'a ProfileDetection> {
    results.iter().find(|result| {
        if result.profile != summary.profile {
            return false;
        }

        match (summary.offset_group_hz, result.offset_group_hz) {
            (Some(summary_hz), Some(result_hz)) => {
                let tolerance = summary
                    .offset_tolerance_hz
                    .max(result.offset_tolerance_hz)
                    .max(1.0);
                (summary_hz - result_hz).abs() <= tolerance
            }
            (None, None) => true,
            _ => false,
        }
    })
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
