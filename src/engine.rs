use crate::config::{
    target_frequencies, DetectionProfile, RecordMode, ResolvedScan, ScanPreferences, ScanSettings,
    SettingsStore,
};
use crate::detect::{detect_profile_candidates, ProfileDetection};
use crate::mask::{FrequencyMask, MaskSet};
use crate::peak::{Peak, SpectrumBand};
use crate::persistence::{PersistenceSummary, PersistenceTracker};
use crate::recording::{Recorder, ScanRecord};
use crate::sdr::{DeviceSettings, IqBackend, SdrPipeline, TargetStatistics};
use crate::synthetic::{generate_peaks, SyntheticOptions};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub profiles: Vec<DetectionProfile>,
    pub device: DeviceSettings,
    pub scan: ScanSettings,
    pub masks: Vec<FrequencyMask>,
    pub confirm_passes: usize,
    pub confirm_ratio: f64,
    pub record_only: bool,
    pub synthetic: SyntheticOptions,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            device: DeviceSettings::default(),
            scan: ScanSettings::default(),
            masks: Vec::new(),
            confirm_passes: 3,
            confirm_ratio: 0.6,
            record_only: false,
            synthetic: SyntheticOptions::default(),
        }
    }
}

impl EngineConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.profiles.is_empty(),
            "detection profiles must contain target frequencies"
        );
        for profile in &self.profiles {
            profile.validate(Path::new("scan configuration"))?;
        }
        anyhow::ensure!(
            self.confirm_passes > 0
                && self.confirm_ratio.is_finite()
                && (0.0..=1.0).contains(&self.confirm_ratio),
            "invalid persistence settings"
        );
        anyhow::ensure!(
            self.scan.peak_search_window_hz.is_finite()
                && self.scan.peak_search_window_hz > 0.0
                && self.scan.min_peak_snr_db.is_finite()
                && self.scan.max_peaks_per_freq > 0,
            "invalid scan settings"
        );
        for mask in &self.masks {
            anyhow::ensure!(
                mask.center_hz.is_finite() && mask.width_hz.is_finite() && mask.width_hz >= 0.0,
                "invalid frequency mask"
            );
        }
        anyhow::ensure!(
            self.synthetic.offset_hz.is_finite()
                && self.synthetic.jitter_hz.is_finite()
                && self.synthetic.signal_snr_db.is_finite()
                && self.synthetic.floor_db.is_finite(),
            "invalid synthetic settings"
        );
        match self.device.backend.as_deref().unwrap_or("airspy") {
            "airspy" => self.device.airspy.validate()?,
            "soapy" => self.device.soapy.validate()?,
            "peaks-file" => anyhow::ensure!(
                self.device.peaks_file.path.is_some(),
                "peaks-file backend requires a path"
            ),
            "iq-file" => anyhow::ensure!(
                self.device.replay.manifest.is_some(),
                "iq-file backend requires a manifest"
            ),
            "synthetic" => (),
            backend => return Err(anyhow!("unknown backend '{backend}'")),
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub repeat: bool,
    pub duration: Option<Duration>,
    pub repeat_delay: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            repeat: true,
            duration: None,
            repeat_delay: Duration::ZERO,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ScanPass {
    pub pass: usize,
    pub elapsed_seconds: f64,
    pub pass_seconds: f64,
    pub peaks: Vec<Peak>,
    pub spectrum_bands: Vec<SpectrumBand>,
    pub results: Vec<ProfileDetection>,
    pub persistence: Vec<PersistenceSummary>,
    pub statistics: Vec<TargetStatistics>,
    pub matching_ms: f64,
    pub recording_ms: f64,
}

enum Source {
    Synthetic,
    Peaks(Vec<Peak>),
    Sdr(Box<dyn IqBackend>),
}

pub struct ScanEngine {
    config: EngineConfig,
    source: Source,
    pipeline: SdrPipeline,
    masks: MaskSet,
    tracker: PersistenceTracker,
    recorder: Option<Recorder>,
    started: Instant,
    pass: usize,
}

impl ScanEngine {
    pub fn new(mut config: EngineConfig) -> Result<Self> {
        config.validate()?;
        config.profiles = config
            .profiles
            .iter()
            .flat_map(DetectionProfile::detection_profiles)
            .collect();
        let source = match config.device.backend.as_deref().unwrap_or("airspy") {
            "synthetic" => Source::Synthetic,
            "peaks-file" => {
                let path = config
                    .device
                    .peaks_file
                    .path
                    .as_deref()
                    .ok_or_else(|| anyhow!("peaks-file backend requires a path"))?;
                Source::Peaks(load_peaks(path)?)
            }
            "airspy" => open_airspy(&config.device)?,
            "iq-file" => Source::Sdr(Box::new(crate::replay::ReplayScanner::open(
                config
                    .device
                    .replay
                    .manifest
                    .as_deref()
                    .ok_or_else(|| anyhow!("iq-file backend requires a manifest"))?,
            )?)),
            "soapy" => Source::Sdr(Box::new(crate::soapy_backend::SoapyScanner::open(
                config.device.soapy.clone(),
            )?)),
            backend => return Err(anyhow!("unknown backend '{backend}'")),
        };
        let recorder = config
            .scan
            .record_path
            .as_deref()
            .map(Recorder::open)
            .transpose()?;
        let masks = MaskSet::new(config.masks.clone());
        let tracker = PersistenceTracker::new(config.confirm_passes, config.confirm_ratio);
        Ok(Self {
            config,
            source,
            pipeline: SdrPipeline::new(),
            masks,
            tracker,
            recorder,
            started: Instant::now(),
            pass: 0,
        })
    }

    pub fn scan(
        &mut self,
        stop: &Arc<AtomicBool>,
        log: &mut dyn FnMut(String),
        progress: &mut dyn FnMut(&TargetStatistics),
    ) -> Result<Option<ScanPass>> {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let started = Instant::now();
        let (peaks, mut spectrum_bands, statistics) = match &mut self.source {
            Source::Synthetic => {
                let peaks = generate_peaks(&self.config.profiles, &self.config.synthetic);
                log(format!("synthetic generated {} peaks", peaks.len()));
                (peaks, Vec::new(), Vec::new())
            }
            Source::Peaks(peaks) => (peaks.clone(), Vec::new(), Vec::new()),
            Source::Sdr(backend) => {
                let output = self.pipeline.scan_with_progress(
                    backend.as_mut(),
                    &self.config.profiles,
                    &self.config.scan,
                    stop,
                    log,
                    progress,
                )?;
                (output.peaks, output.spectrum_bands, output.statistics)
            }
        };
        if stop.load(Ordering::Relaxed) || statistics.iter().any(|item| item.interrupted) {
            return Ok(None);
        }
        if self.config.scan.record_mode == RecordMode::Summary {
            for band in &mut spectrum_bands {
                band.points.clear();
            }
        }
        let matching_started = Instant::now();
        let results = if self.config.record_only {
            Vec::new()
        } else {
            detect_profile_candidates(
                &self.config.profiles,
                &peaks,
                &self.masks,
                &self.config.scan,
            )
        };
        if !self.config.record_only {
            self.tracker.update(&results);
        }
        let persistence = self.tracker.summaries();
        let matching_ms = matching_started.elapsed().as_secs_f64() * 1000.0;
        self.pass += 1;
        let mut pass = ScanPass {
            pass: self.pass,
            elapsed_seconds: self.started.elapsed().as_secs_f64(),
            pass_seconds: started.elapsed().as_secs_f64(),
            peaks,
            spectrum_bands,
            results,
            persistence,
            statistics,
            matching_ms,
            recording_ms: 0.0,
        };
        if let Some(recorder) = &mut self.recorder {
            let recording_started = Instant::now();
            recorder.write(&ScanRecord {
                kind: "scan_pass",
                format_version: 3,
                timestamp_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64(),
                backend: self.config.device.backend.as_deref().unwrap_or("airspy"),
                pass: pass.pass,
                elapsed_seconds: pass.elapsed_seconds,
                pass_seconds: pass.pass_seconds,
                scan: &self.config.scan,
                device: &self.config.device,
                profiles: &self.config.profiles,
                masks: self.masks.masks(),
                peaks: &pass.peaks,
                spectrum_bands: &pass.spectrum_bands,
                results: &pass.results,
                persistence: &pass.persistence,
                statistics: &pass.statistics,
                matching_ms,
            })?;
            pass.recording_ms = recording_started.elapsed().as_secs_f64() * 1000.0;
        }
        pass.pass_seconds = started.elapsed().as_secs_f64();
        Ok(Some(pass))
    }
}

#[cfg(feature = "airspy")]
fn open_airspy(device: &DeviceSettings) -> Result<Source> {
    Ok(Source::Sdr(Box::new(
        crate::airspy_backend::AirspyScanner::open(device.airspy.clone())?,
    )))
}

#[cfg(not(feature = "airspy"))]
fn open_airspy(_: &DeviceSettings) -> Result<Source> {
    Err(anyhow!(
        "Airspy support is not built; enable the airspy feature"
    ))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PeaksFile {
    List(Vec<Peak>),
    Document { peaks: Vec<Peak> },
}

fn load_peaks(path: &Path) -> Result<Vec<Peak>> {
    let file = fs::File::open(path)
        .with_context(|| format!("failed to open peaks file {}", path.display()))?;
    let document: PeaksFile = serde_json::from_reader(std::io::BufReader::new(file))?;
    Ok(match document {
        PeaksFile::List(peaks) | PeaksFile::Document { peaks } => peaks,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Starting,
    Scanning,
    Paused,
    Stopped,
    Error,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ScanSnapshot {
    pub status: SessionStatus,
    pub pass: usize,
    pub elapsed_seconds: f64,
    pub pass_seconds: f64,
    pub current_target_hz: Option<f64>,
    pub completed_targets: usize,
    pub total_targets: usize,
    pub peaks: Vec<Peak>,
    pub results: Vec<ProfileDetection>,
    pub persistence: Vec<PersistenceSummary>,
    pub statistics: Vec<TargetStatistics>,
    pub error: Option<String>,
    pub logs: VecDeque<String>,
}

#[derive(Clone, Copy, Debug)]
pub enum SessionCommand {
    Pause,
    Resume,
    Stop,
}

struct SessionControl {
    stop: AtomicBool,
    paused: AtomicBool,
    interrupt: Arc<AtomicBool>,
}

pub struct ScanSession {
    state: Arc<Mutex<ScanSnapshot>>,
    control: Arc<SessionControl>,
    worker: Option<JoinHandle<Result<()>>>,
    config: EngineConfig,
    options: RunOptions,
    events: Option<mpsc::SyncSender<Arc<ScanPass>>>,
}

impl ScanSession {
    pub fn start(config: EngineConfig) -> Result<Self> {
        Self::start_with_options(config, RunOptions::default())
    }

    pub fn start_with_options(config: EngineConfig, options: RunOptions) -> Result<Self> {
        Self::launch(config, options, None)
    }

    pub fn observe(
        config: EngineConfig,
        options: RunOptions,
    ) -> Result<(Self, mpsc::Receiver<Arc<ScanPass>>)> {
        let (sender, receiver) = mpsc::sync_channel(1);
        Ok((Self::launch(config, options, Some(sender))?, receiver))
    }

    fn launch(
        config: EngineConfig,
        options: RunOptions,
        events: Option<mpsc::SyncSender<Arc<ScanPass>>>,
    ) -> Result<Self> {
        let state = Arc::new(Mutex::new(ScanSnapshot {
            total_targets: target_frequencies(&config.profiles).len(),
            ..Default::default()
        }));
        let control = Arc::new(SessionControl {
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            interrupt: Arc::new(AtomicBool::new(false)),
        });
        let worker_state = Arc::clone(&state);
        let worker_control = Arc::clone(&control);
        let worker_config = config.clone();
        let worker_options = options.clone();
        let worker_events = events.clone();
        let worker = thread::Builder::new()
            .name("camfinder-scan".into())
            .spawn(move || {
                let started = Instant::now();
                let _deadline = SessionDeadline::start(&worker_control, worker_options.duration);
                let result = run_session(
                    worker_config,
                    worker_options,
                    &worker_state,
                    &worker_control,
                    worker_events,
                );
                let mut snapshot = worker_state.lock().unwrap();
                snapshot.elapsed_seconds = started.elapsed().as_secs_f64();
                match &result {
                    Ok(()) => snapshot.status = SessionStatus::Stopped,
                    Err(error) => {
                        snapshot.status = SessionStatus::Error;
                        snapshot.error = Some(format!("{error:#}"));
                        snapshot.logs.push_back(format!("scan failed: {error:#}"));
                    }
                }
                result
            })?;
        Ok(Self {
            state,
            control,
            worker: Some(worker),
            config,
            options,
            events,
        })
    }

    pub fn command(&self, command: SessionCommand) {
        match command {
            SessionCommand::Pause => {
                self.control.paused.store(true, Ordering::Relaxed);
                self.control.interrupt.store(true, Ordering::Relaxed);
            }
            SessionCommand::Resume => self.control.paused.store(false, Ordering::Relaxed),
            SessionCommand::Stop => {
                self.control.stop.store(true, Ordering::Relaxed);
                self.control.interrupt.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn toggle_pause(&mut self) -> Result<()> {
        match self.snapshot().status {
            SessionStatus::Paused => self.command(SessionCommand::Resume),
            SessionStatus::Error | SessionStatus::Stopped => {
                self.restart(self.config.clone(), self.options.clone())?
            }
            _ => self.command(SessionCommand::Pause),
        }
        Ok(())
    }

    pub fn apply(&mut self, resolved: ResolvedScan) -> Result<()> {
        resolved.engine.validate()?;
        self.restart(resolved.engine, resolved.run)
    }

    pub fn save_preferences(
        &mut self,
        preferences: &ScanPreferences,
        store: &SettingsStore,
        demo: bool,
    ) -> Result<()> {
        let mut resolved = preferences.resolve(store, demo)?;
        resolved.run.duration = self.options.duration;
        store.save(preferences)?;
        self.apply(resolved)
    }

    fn restart(&mut self, config: EngineConfig, options: RunOptions) -> Result<()> {
        let _ = self.stop();
        *self = Self::launch(config, options, self.events.clone())?;
        Ok(())
    }

    pub fn snapshot(&self) -> ScanSnapshot {
        self.state.lock().unwrap().clone()
    }

    pub fn stop(&mut self) -> Result<()> {
        self.command(SessionCommand::Stop);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow!("scan thread panicked"))??;
        }
        Ok(())
    }
}

impl Drop for ScanSession {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct SessionDeadline {
    finished: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SessionDeadline {
    fn start(control: &Arc<SessionControl>, duration: Option<Duration>) -> Self {
        let finished = Arc::new(AtomicBool::new(false));
        let worker = duration.map(|duration| {
            let control = Arc::clone(control);
            let finished = Arc::clone(&finished);
            thread::spawn(move || {
                let started = Instant::now();
                while !finished.load(Ordering::Relaxed) && !control.stop.load(Ordering::Relaxed) {
                    if started.elapsed() >= duration {
                        control.stop.store(true, Ordering::Relaxed);
                        control.interrupt.store(true, Ordering::Relaxed);
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            })
        });
        Self { finished, worker }
    }
}

impl Drop for SessionDeadline {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn push_log(state: &Mutex<ScanSnapshot>, line: String) {
    let mut snapshot = state.lock().unwrap();
    if snapshot.logs.len() == 500 {
        snapshot.logs.pop_front();
    }
    snapshot.logs.push_back(line);
}

fn run_session(
    config: EngineConfig,
    options: RunOptions,
    state: &Arc<Mutex<ScanSnapshot>>,
    control: &SessionControl,
    events: Option<mpsc::SyncSender<Arc<ScanPass>>>,
) -> Result<()> {
    let synthetic = config.device.backend.as_deref() == Some("synthetic");
    push_log(
        state,
        format!(
            "scan started backend={}",
            config.device.backend.as_deref().unwrap_or("airspy")
        ),
    );
    let mut engine = ScanEngine::new(config)?;
    let started = Instant::now();
    let deadline = options
        .duration
        .and_then(|duration| started.checked_add(duration));
    let mut completed = 0;
    let mut next_pass = started;
    while !control.stop.load(Ordering::Relaxed) {
        if completed > 0 && (!options.repeat || deadline.is_some_and(|end| Instant::now() >= end)) {
            break;
        }
        if control.paused.load(Ordering::Relaxed) {
            state.lock().unwrap().status = SessionStatus::Paused;
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        if Instant::now() < next_pass {
            thread::sleep(
                next_pass
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(20)),
            );
            continue;
        }
        control.interrupt.store(false, Ordering::Relaxed);
        if control.stop.load(Ordering::Relaxed) || control.paused.load(Ordering::Relaxed) {
            continue;
        }
        {
            let mut snapshot = state.lock().unwrap();
            snapshot.status = SessionStatus::Scanning;
            snapshot.completed_targets = 0;
        }
        let pass = engine.scan(
            &control.interrupt,
            &mut |line| push_log(state, line),
            &mut |item| {
                let mut snapshot = state.lock().unwrap();
                snapshot.current_target_hz = Some(item.target_hz);
                snapshot.completed_targets += 1;
            },
        )?;
        if let Some(pass) = pass {
            completed = pass.pass;
            push_log(
                state,
                format!(
                    "pass #{} finished peaks={} elapsed={:.3}s",
                    pass.pass,
                    pass.peaks.len(),
                    pass.pass_seconds
                ),
            );
            {
                let mut snapshot = state.lock().unwrap();
                snapshot.pass = pass.pass;
                snapshot.elapsed_seconds = pass.elapsed_seconds;
                snapshot.pass_seconds = pass.pass_seconds;
                snapshot.completed_targets = snapshot.total_targets;
                snapshot.peaks = pass.peaks.clone();
                for peak in &mut snapshot.peaks {
                    peak.spectrum_window.clear();
                }
                snapshot.results = pass.results.clone();
                snapshot.persistence = pass.persistence.clone();
                snapshot.statistics = pass.statistics.clone();
            }
            if let Some(sender) = &events {
                let mut event = Arc::new(pass);
                loop {
                    if control.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    match sender.try_send(event) {
                        Ok(()) => break,
                        Err(mpsc::TrySendError::Full(value)) => {
                            event = value;
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
                    }
                }
            }
            let delay = if synthetic {
                options.repeat_delay.max(Duration::from_millis(200))
            } else {
                options.repeat_delay
            };
            next_pass = Instant::now()
                .checked_add(delay)
                .ok_or_else(|| anyhow!("repeat delay is too large"))?;
        }
    }
    Ok(())
}
