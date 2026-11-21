use crate::config::{DetectionProfile, RecordMode, ScanSettings};
use crate::detect::{detect_profile_candidates, ProfileDetection};
use crate::mask::{FrequencyMask, MaskSet};
use crate::peak::{Peak, SpectrumBand};
use crate::persistence::{PersistenceSummary, PersistenceTracker};
use crate::recording::{Recorder, ScanRecord};
use crate::sdr::{DeviceSettings, IqBackend, SdrPipeline, TargetStatistics};
use crate::synthetic::{generate_peaks, SyntheticOptions};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
        config.profiles = config
            .profiles
            .iter()
            .flat_map(DetectionProfile::detection_profiles)
            .collect();
        if config.profiles.is_empty()
            || config
                .profiles
                .iter()
                .any(|profile| profile.freqs_mhz.is_empty())
        {
            return Err(anyhow!(
                "detection profiles must contain target frequencies"
            ));
        }
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
}

impl ScanSession {
    pub fn start(config: EngineConfig) -> Result<Self> {
        let mut targets: Vec<_> = config
            .profiles
            .iter()
            .flat_map(DetectionProfile::detection_profiles)
            .flat_map(|profile| profile.nominal_freqs_hz())
            .collect();
        targets.sort_by(f64::total_cmp);
        targets.dedup_by(|a, b| (*a - *b).abs() < 1.0);
        let state = Arc::new(Mutex::new(ScanSnapshot {
            total_targets: targets.len(),
            ..Default::default()
        }));
        let control = Arc::new(SessionControl {
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            interrupt: Arc::new(AtomicBool::new(false)),
        });
        let worker_state = Arc::clone(&state);
        let worker_control = Arc::clone(&control);
        let worker = thread::Builder::new()
            .name("camfinder-scan".into())
            .spawn(move || {
                let result = run_session(config, &worker_state, &worker_control);
                let mut snapshot = worker_state.lock().unwrap();
                match &result {
                    Ok(()) => snapshot.status = SessionStatus::Stopped,
                    Err(error) => {
                        snapshot.status = SessionStatus::Error;
                        snapshot.error = Some(format!("{error:#}"));
                    }
                }
                result
            })?;
        Ok(Self {
            state,
            control,
            worker: Some(worker),
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

fn run_session(
    config: EngineConfig,
    state: &Arc<Mutex<ScanSnapshot>>,
    control: &SessionControl,
) -> Result<()> {
    let synthetic = config.device.backend.as_deref() == Some("synthetic");
    let mut engine = ScanEngine::new(config)?;
    while !control.stop.load(Ordering::Relaxed) {
        if control.paused.load(Ordering::Relaxed) {
            state.lock().unwrap().status = SessionStatus::Paused;
            thread::sleep(Duration::from_millis(40));
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
        let pass = engine.scan(&control.interrupt, &mut |_| {}, &mut |item| {
            let mut snapshot = state.lock().unwrap();
            snapshot.current_target_hz = Some(item.target_hz);
            snapshot.completed_targets += 1;
        });
        if let Some(mut pass) = pass? {
            for peak in &mut pass.peaks {
                peak.spectrum_window.clear();
            }
            let mut snapshot = state.lock().unwrap();
            snapshot.pass = pass.pass;
            snapshot.elapsed_seconds = pass.elapsed_seconds;
            snapshot.pass_seconds = pass.pass_seconds;
            snapshot.completed_targets = snapshot.total_targets;
            snapshot.peaks = pass.peaks;
            snapshot.results = pass.results;
            snapshot.persistence = pass.persistence;
            snapshot.statistics = pass.statistics;
        }
        if synthetic {
            for _ in 0..5 {
                if control.stop.load(Ordering::Relaxed) || control.paused.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(40));
            }
        }
    }
    Ok(())
}
