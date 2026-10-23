//! Offline wake word "Soul" (rustpotter: MFCC + DTW against the user's own samples).
//!
//! A dedicated low-priority thread (`soul-wake`) owns its own cpal input stream on the selected
//! mic, decimates it to 16 kHz mono and feeds the local detector. Nothing leaves the machine until
//! the detector fires; then the turn behaves exactly like a talk-key hold: `talk::begin_capture`
//! (listening state + start cue), an energy endpointer (1.2 s of silence, 8 s cap) and
//! `lib.rs::on_smart_released` (transcript → `agent::classify_smart` → paste or command).
//!
//! The stream is closed (mic released) whenever detection is paused: disabled / untrained, Soul
//! speaking (+ a short echo tail), any recording (talk key, PTT, training), work in progress,
//! Circle-to-Ask, or a fullscreen app when "pause in games" is on. 2 s cooldown after a detection.
//!
//! Training (Settings → Głos → Słowo-klucz): 6–8 short recordings of "Soul" → a `WakewordRef`
//! saved as `<app data>/wakeword/soul.rpw`; samples stay in `wakeword/samples/` until reset.

use crate::config::{load_settings, save_settings, Settings};
use crate::talk::{begin_capture, cancel_capture, CaptureTarget};
use crate::{agent, ask, report_error, schedule_hide, AppState};
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use rustpotter::{
    DetectorConfig, Rustpotter, RustpotterConfig, ScoreMode, WakewordRef,
    WakewordRefBuildFromBuffers, WakewordSave,
};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

pub const SAMPLE_RATE: usize = 16_000;
/// rustpotter's frame: 30 ms at 16 kHz.
const FRAME_LEN: usize = 480;
const MFCC_SIZE: u16 = 16;
const WAKEWORD_NAME: &str = "soul";
pub const MIN_SAMPLES: usize = 6;
pub const MAX_SAMPLES: usize = 8;

const COOLDOWN: Duration = Duration::from_secs(2);
/// Room echo of Soul's own voice after playback ends.
const SPEAK_TAIL: Duration = Duration::from_millis(700);
const TICK_LISTENING: Duration = Duration::from_millis(60);
const TICK_IDLE: Duration = Duration::from_millis(400);
const FULLSCREEN_POLL: Duration = Duration::from_secs(1);
const MIC_RETRY: Duration = Duration::from_secs(5);
const TEST_EMIT_EVERY: Duration = Duration::from_millis(100);
/// Settings closed without stopping the test: give up after this long.
const TEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Training: fixed recording window per sample, accepted word length.
const RECORD_WINDOW: Duration = Duration::from_millis(1800);
const WORD_MIN_MS: usize = 180;
const WORD_MAX_MS: usize = 1300;
const WORD_MIN_PEAK: f32 = 0.03;

/// Endpointing after the wake word.
pub const SILENCE_END_MS: u64 = 1200;
pub const MAX_CAPTURE_MS: u64 = 8000;
/// No speech at all this long after the wake word → treat it as a false trigger.
pub const NO_SPEECH_MS: u64 = 4000;
const VAD_TICK: Duration = Duration::from_millis(30);

// ───────────────────────── detector ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub threshold: f32,
    pub avg_threshold: f32,
    pub min_scores: usize,
}

/// DTW band; wider than rustpotter's default 5 (tuned with `wake_regression::wake_tune`).
const BAND_SIZE: u16 = 8;

/// Sensitivity 0..1 → rustpotter thresholds. rustpotter's own defaults (0.5 / 5 hits) fire on
/// most speech; 0.5 here = score 0.64 held for ≥ 8 frames (tests/voice/wake/report.md).
pub fn thresholds(sensitivity: f32) -> Thresholds {
    let s = sensitivity.clamp(0.0, 1.0);
    let threshold = 0.64 + (0.5 - s) * 0.06;
    Thresholds {
        threshold,
        // Cheap pre-filter against the averaged template; well below real matches.
        avg_threshold: threshold - 0.2,
        min_scores: (8.0 + (0.5 - s) * 8.0).round() as usize,
    }
}

fn detector_config(t: Thresholds) -> DetectorConfig {
    DetectorConfig {
        threshold: t.threshold,
        avg_threshold: t.avg_threshold,
        min_scores: t.min_scores,
        band_size: BAND_SIZE,
        score_mode: ScoreMode::Max,
        // rustpotter's VAD (mean |MFCC|) misreads quiet rooms and blocks real detections; the
        // banded DTW it would skip costs well under 1% of a core, so it stays off.
        vad_mode: None,
        ..DetectorConfig::default()
    }
}

/// Probe for the Settings "Test" meter: accepts every frame so the best score is always visible.
fn probe_config() -> DetectorConfig {
    DetectorConfig {
        threshold: 0.0,
        avg_threshold: 0.0,
        min_scores: 1,
        band_size: BAND_SIZE,
        score_mode: ScoreMode::Max,
        vad_mode: None,
        ..DetectorConfig::default()
    }
}

fn new_detector(model: &[u8], detector: DetectorConfig) -> Result<Rustpotter, String> {
    let mut d = Rustpotter::new(&RustpotterConfig {
        detector,
        ..RustpotterConfig::default()
    })?;
    d.add_wakeword_from_buffer(WAKEWORD_NAME, model)?;
    Ok(d)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub score: f32,
    pub avg_score: f32,
    pub hits: usize,
}

/// Streaming detector over 16 kHz mono f32 (shared by the app, training check and the harness).
pub struct WakeEngine {
    det: Rustpotter,
    probe: Option<Rustpotter>,
    frame: Vec<f32>,
    probe_max: f32,
}

impl WakeEngine {
    pub fn new(model: &[u8], sensitivity: f32, with_probe: bool) -> Result<Self, String> {
        Ok(Self {
            det: new_detector(model, detector_config(thresholds(sensitivity)))?,
            probe: if with_probe {
                Some(new_detector(model, probe_config())?)
            } else {
                None
            },
            frame: Vec::with_capacity(FRAME_LEN),
            probe_max: 0.0,
        })
    }

    /// Harness / tuning: an explicit detector config, no probe.
    #[cfg(test)]
    pub fn with_config(model: &[u8], detector: DetectorConfig) -> Result<Self, String> {
        Ok(Self {
            det: new_detector(model, detector)?,
            probe: None,
            frame: Vec::with_capacity(FRAME_LEN),
            probe_max: 0.0,
        })
    }

    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.det
            .update_detector_config(&detector_config(thresholds(sensitivity)));
    }

    pub fn set_probe(&mut self, model: &[u8], on: bool) -> Result<(), String> {
        self.probe = if on {
            Some(new_detector(model, probe_config())?)
        } else {
            None
        };
        self.probe_max = 0.0;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.det.reset();
        if let Some(p) = self.probe.as_mut() {
            p.reset();
        }
        self.frame.clear();
        self.probe_max = 0.0;
    }

    /// Feed samples; returns the detection if one fired inside this chunk.
    pub fn push(&mut self, samples: &[f32]) -> Option<Detection> {
        let mut fired = None;
        for &s in samples {
            self.frame.push(s);
            if self.frame.len() < FRAME_LEN {
                continue;
            }
            let frame = std::mem::replace(&mut self.frame, Vec::with_capacity(FRAME_LEN));
            if let Some(p) = self.probe.as_mut() {
                let done = p.process_samples(frame.clone());
                let partial = p.get_partial_detection().map(|d| d.score);
                for score in done.map(|d| d.score).into_iter().chain(partial) {
                    self.probe_max = self.probe_max.max(score);
                }
            }
            if let Some(d) = self.det.process_samples(frame) {
                fired = Some(Detection {
                    score: d.score,
                    avg_score: d.avg_score,
                    hits: d.counter,
                });
            }
        }
        fired
    }

    /// Best probe score since the last call (0 when there is no probe).
    pub fn take_probe_score(&mut self) -> f32 {
        std::mem::take(&mut self.probe_max)
    }
}

/// Every detection in a 16 kHz clip, as (seconds, detection). Pads with quiet dither so a word at
/// the very edge still fills the window and the detection countdown completes.
pub fn scan(engine: &mut WakeEngine, samples: &[f32]) -> (Vec<(f32, Detection)>, f32) {
    let pad = dither(SAMPLE_RATE);
    let mut out = Vec::new();
    let mut best = 0.0f32;
    let mut pos = 0usize;
    for chunk in [&pad[..], samples, &pad[..]] {
        for block in chunk.chunks(FRAME_LEN) {
            if let Some(d) = engine.push(block) {
                out.push((pos.saturating_sub(SAMPLE_RATE) as f32 / SAMPLE_RATE as f32, d));
            }
            best = best.max(engine.take_probe_score());
            pos += block.len();
        }
    }
    (out, best)
}

/// Deterministic near-silent noise (digital zeros make the MFCC log blow up).
fn dither(len: usize) -> Vec<f32> {
    let mut x: u32 = 0x9E37_79B9;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32 - 0.5) * 2e-4
        })
        .collect()
}

/// Build a wakeword file from named WAV buffers (any rate / format hound reads).
pub fn build_model(samples: HashMap<String, Vec<u8>>) -> Result<Vec<u8>, String> {
    let model = WakewordRef::new_from_sample_buffers(
        WAKEWORD_NAME.to_string(),
        None,
        None,
        samples,
        MFCC_SIZE,
    )?;
    model.save_to_buffer()
}

// ───────────────────────── audio helpers ─────────────────────────

/// Streaming decimator to 16 kHz: averages the source span of every output sample (cheap
/// anti-aliasing, unlike plain linear interpolation). Sample-and-hold when upsampling.
pub struct Decimator {
    ratio: f64,
    consumed: u64,
    produced: u64,
    buf: Vec<f32>,
}

impl Decimator {
    pub fn new(from_rate: u32) -> Self {
        Self {
            ratio: from_rate as f64 / SAMPLE_RATE as f64,
            consumed: 0,
            produced: 0,
            buf: Vec::new(),
        }
    }

    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buf.extend_from_slice(input);
        let available = self.consumed + self.buf.len() as u64;
        loop {
            let start = (self.produced as f64 * self.ratio) as u64;
            let end = (((self.produced + 1) as f64 * self.ratio) as u64).max(start + 1);
            if end > available {
                break;
            }
            let s = (start - self.consumed) as usize;
            let e = (end - self.consumed) as usize;
            out.push(self.buf[s..e].iter().sum::<f32>() / (e - s) as f32);
            self.produced += 1;
        }
        let next = ((self.produced as f64 * self.ratio) as u64).min(available);
        let drop = (next - self.consumed) as usize;
        self.buf.drain(..drop);
        self.consumed = next;
    }
}

/// Span of the spoken word in a 16 kHz clip: 10 ms frames whose RMS is above 12% of the loudest
/// frame (and above a floor), plus 60 ms padding. None = nothing audible.
pub fn trim_word(samples: &[f32]) -> Option<(usize, usize)> {
    const HOP: usize = SAMPLE_RATE / 100;
    let rms: Vec<f32> = samples
        .chunks(HOP)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let peak = rms.iter().copied().fold(0.0f32, f32::max);
    let gate = (peak * 0.12).max(0.006);
    let first = rms.iter().position(|&r| r >= gate)?;
    let last = rms.iter().rposition(|&r| r >= gate)?;
    let pad = 6 * HOP;
    let start = (first * HOP).saturating_sub(pad);
    let end = ((last + 1) * HOP + pad).min(samples.len());
    (end > start).then_some((start, end))
}

pub fn read_wav_16k(bytes: &[u8]) -> Result<Vec<f32>, String> {
    let reader = hound::WavReader::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.into_samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample.max(1) - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|s| s.unwrap_or(0) as f32 / scale)
                .collect()
        }
    };
    let mono: Vec<f32> = interleaved
        .chunks(channels)
        .map(|f| f.iter().sum::<f32>() / f.len() as f32)
        .collect();
    let mut out = Vec::with_capacity(mono.len());
    Decimator::new(spec.sample_rate).push(&mono, &mut out);
    Ok(out)
}

pub fn wav_16k_bytes(samples: &[f32]) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut w = hound::WavWriter::new(&mut cursor, spec).map_err(|e| e.to_string())?;
        for s in samples {
            w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .map_err(|e| e.to_string())?;
        }
        w.finalize().map_err(|e| e.to_string())?;
    }
    Ok(cursor.into_inner())
}

/// The listener's own capture stream (cpal::Stream is !Send — lives and dies on `soul-wake`).
struct MicTap {
    _stream: cpal::Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    err: Arc<Mutex<Option<String>>>,
    decimator: Decimator,
}

impl MicTap {
    fn open(preferred: Option<&str>) -> Result<(Self, String), String> {
        use cpal::traits::StreamTrait;
        let resolved = crate::audio::resolve_input_device(preferred)?;
        let buf = Arc::new(Mutex::new(Vec::new()));
        let err = Arc::new(Mutex::new(None));
        let (stream, rate) = crate::audio::build_capture_stream(
            &resolved.device,
            Some(Arc::clone(&buf)),
            Arc::clone(&err),
            None,
            Arc::new(AtomicU32::new(0)),
        )?;
        stream
            .play()
            .map_err(|e| format!("Failed to start microphone: {e}"))?;
        Ok((
            Self {
                _stream: stream,
                buf,
                err,
                decimator: Decimator::new(rate),
            },
            resolved.name,
        ))
    }

    fn drain(&mut self, out: &mut Vec<f32>) -> Result<(), String> {
        if let Some(e) = self.err.lock().take() {
            return Err(e);
        }
        let raw = std::mem::take(&mut *self.buf.lock());
        self.decimator.push(&raw, out);
        Ok(())
    }
}

// ───────────────────────── endpointing ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    Continue,
    /// Speech then `SILENCE_END_MS` of silence.
    Silence,
    MaxLength,
    /// Nothing said after the wake word.
    NoSpeech,
}

/// Energy endpointer over the recorder's input level (peak/RMS blend, 0..1).
#[derive(Default)]
pub struct Endpointer {
    elapsed_ms: u64,
    silence_ms: u64,
    loud_ticks: u32,
    speech: bool,
    floor: Option<f32>,
}

impl Endpointer {
    pub fn push(&mut self, level: f32, dt_ms: u64) -> Endpoint {
        self.elapsed_ms += dt_ms;
        // Noise floor: follows quiet levels at once, louder ones very slowly.
        let floor = match self.floor {
            None => level,
            Some(f) if level < f => level,
            Some(f) => f + (level - f) * 0.005,
        };
        self.floor = Some(floor);
        let voiced = level >= (floor * 3.0).max(0.03);
        if voiced {
            self.loud_ticks += 1;
            self.silence_ms = 0;
            // ~90 ms of sound before it counts as speech (ignores clicks).
            if self.loud_ticks >= 3 {
                self.speech = true;
            }
        } else {
            self.loud_ticks = 0;
            self.silence_ms += dt_ms;
        }
        if self.elapsed_ms >= MAX_CAPTURE_MS {
            return if self.speech {
                Endpoint::MaxLength
            } else {
                Endpoint::NoSpeech
            };
        }
        if self.speech && self.silence_ms >= SILENCE_END_MS {
            return Endpoint::Silence;
        }
        if !self.speech && self.elapsed_ms >= NO_SPEECH_MS {
            return Endpoint::NoSpeech;
        }
        Endpoint::Continue
    }

    pub fn heard_speech(&self) -> bool {
        self.speech
    }
}

// ───────────────────────── status / config ─────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WakeStatus {
    pub enabled: bool,
    pub trained: bool,
    /// The local detector is running on the mic right now.
    pub listening: bool,
    /// Why it isn't: disabled | untrained | speaking | recording | busy | ask | fullscreen |
    /// cooldown | mic.
    pub paused: Option<&'static str>,
    pub testing: bool,
    pub samples: usize,
    pub sensitivity: f32,
    pub pause_in_games: bool,
    pub error: Option<String>,
}

static STATUS: Lazy<Mutex<Option<WakeStatus>>> = Lazy::new(|| Mutex::new(None));

static CTRL: OnceCell<Sender<Ctrl>> = OnceCell::new();
static DIR: OnceCell<PathBuf> = OnceCell::new();
/// Tray check item, kept in sync with `wake_enabled`.
static TRAY_ITEM: OnceCell<tauri::menu::CheckMenuItem<tauri::Wry>> = OnceCell::new();

#[derive(Debug, Clone, PartialEq)]
struct Config {
    enabled: bool,
    sensitivity: f32,
    pause_in_games: bool,
    microphone: String,
}

impl Config {
    fn from(settings: &Settings) -> Self {
        Self {
            enabled: settings.wake_enabled,
            sensitivity: settings.wake_sensitivity.clamp(0.0, 1.0),
            pause_in_games: settings.wake_pause_in_games,
            microphone: settings.microphone.trim().to_string(),
        }
    }
}

enum Ctrl {
    Config(Config),
    /// The wakeword file changed (trained / deleted).
    Reload,
    Test(bool),
}

fn dir() -> Result<&'static PathBuf, String> {
    DIR.get().ok_or_else(|| "Wake word store not initialized".to_string())
}

fn model_path() -> Result<PathBuf, String> {
    Ok(dir()?.join("soul.rpw"))
}

fn samples_dir() -> Result<PathBuf, String> {
    Ok(dir()?.join("samples"))
}

fn sample_files() -> Vec<PathBuf> {
    let Ok(dir) = samples_dir() else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn trained() -> bool {
    model_path().is_ok_and(|p| p.is_file())
}

pub fn status() -> WakeStatus {
    STATUS.lock().clone().unwrap_or(WakeStatus {
        enabled: false,
        trained: trained(),
        listening: false,
        paused: Some("disabled"),
        testing: false,
        samples: sample_files().len(),
        sensitivity: crate::config::DEFAULT_WAKE_SENSITIVITY,
        pause_in_games: true,
        error: None,
    })
}

fn publish(app: &AppHandle, next: WakeStatus) {
    let mut cur = STATUS.lock();
    if cur.as_ref() == Some(&next) {
        return;
    }
    let listening_changed = cur.as_ref().map(|c| c.listening) != Some(next.listening);
    let enabled_changed = cur.as_ref().map(|c| c.enabled) != Some(next.enabled);
    if listening_changed {
        eprintln!(
            "[soul][wake] listening={} paused={:?}",
            next.listening, next.paused
        );
    } else {
        crate::soul_debug!("[soul][wake] paused={:?}", next.paused);
    }
    *cur = Some(next.clone());
    drop(cur);
    if enabled_changed {
        if let Some(item) = TRAY_ITEM.get() {
            let _ = item.set_checked(next.enabled);
        }
    }
    let _ = app.emit("soul://wake", next);
}

// ───────────────────────── listener thread ─────────────────────────

/// Create the store dir and start the listener (idle until enabled + trained).
pub fn init(app: &AppHandle, settings: &Settings) {
    match app.path().app_data_dir() {
        Ok(base) => {
            let dir = base.join("wakeword");
            let _ = std::fs::create_dir_all(dir.join("samples"));
            let _ = DIR.set(dir);
        }
        Err(e) => {
            eprintln!("[soul][wake] no app data dir: {e}");
            return;
        }
    }
    let (tx, rx) = mpsc::channel();
    if CTRL.set(tx).is_err() {
        return;
    }
    let app = app.clone();
    let config = Config::from(settings);
    let spawned = thread::Builder::new()
        .name("soul-wake".into())
        .spawn(move || {
            crate::platform::lower_current_thread_priority();
            Listener::new(app, config).run(rx);
        });
    if let Err(e) = spawned {
        eprintln!("[soul][wake] failed to start listener: {e}");
    }
}

/// Settings changed (mic, wake fields) → listener picks it up on its next tick.
pub fn apply(settings: &Settings) {
    if let Some(tx) = CTRL.get() {
        let _ = tx.send(Ctrl::Config(Config::from(settings)));
    }
}

fn send(ctrl: Ctrl) {
    if let Some(tx) = CTRL.get() {
        let _ = tx.send(ctrl);
    }
}

struct Listener {
    app: AppHandle,
    cfg: Config,
    model: Option<Vec<u8>>,
    engine: Option<WakeEngine>,
    tap: Option<MicTap>,
    testing_since: Option<Instant>,
    cooldown_until: Option<Instant>,
    speaking_until: Option<Instant>,
    fullscreen: (bool, Option<Instant>),
    mic_retry_at: Option<Instant>,
    mic_error: Option<String>,
    last_test_emit: Instant,
    pcm: Vec<f32>,
    /// Recorded training samples (directory listing refreshed on control messages only).
    samples: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScorePayload {
    score: f32,
    threshold: f32,
    detected: bool,
}

impl Listener {
    fn new(app: AppHandle, cfg: Config) -> Self {
        let mut l = Self {
            app,
            cfg,
            model: None,
            engine: None,
            tap: None,
            testing_since: None,
            cooldown_until: None,
            speaking_until: None,
            fullscreen: (false, None),
            mic_retry_at: None,
            mic_error: None,
            last_test_emit: Instant::now(),
            pcm: Vec::with_capacity(4096),
            samples: sample_files().len(),
        };
        l.reload();
        l
    }

    fn reload(&mut self) {
        self.tap = None;
        self.engine = None;
        self.model = model_path().ok().and_then(|p| std::fs::read(p).ok());
        let Some(model) = self.model.as_deref() else {
            return;
        };
        match WakeEngine::new(model, self.cfg.sensitivity, self.testing_since.is_some()) {
            Ok(engine) => self.engine = Some(engine),
            Err(e) => {
                crate::push_error_log(&self.app, format!("Wake word model is unreadable: {e}"));
                self.model = None;
            }
        }
    }

    fn set_testing(&mut self, on: bool) {
        self.testing_since = on.then(Instant::now);
        if let (Some(engine), Some(model)) = (self.engine.as_mut(), self.model.as_deref()) {
            if let Err(e) = engine.set_probe(model, on) {
                eprintln!("[soul][wake] test probe failed: {e}");
            }
        }
        eprintln!("[soul][wake] test mode={on}");
    }

    fn handle(&mut self, ctrl: Ctrl) {
        self.samples = sample_files().len();
        match ctrl {
            Ctrl::Config(cfg) => {
                if cfg.microphone != self.cfg.microphone {
                    self.tap = None;
                    self.mic_retry_at = None;
                    self.mic_error = None;
                }
                if cfg.sensitivity != self.cfg.sensitivity {
                    if let Some(e) = self.engine.as_mut() {
                        e.set_sensitivity(cfg.sensitivity);
                    }
                }
                self.cfg = cfg;
            }
            Ctrl::Reload => self.reload(),
            Ctrl::Test(on) => self.set_testing(on),
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Ctrl>) {
        loop {
            let tick = if self.tap.is_some() {
                TICK_LISTENING
            } else {
                TICK_IDLE
            };
            match rx.recv_timeout(tick) {
                Ok(ctrl) => self.handle(ctrl),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            while let Ok(ctrl) = rx.try_recv() {
                self.handle(ctrl);
            }
            if self
                .testing_since
                .is_some_and(|t| t.elapsed() >= TEST_TIMEOUT)
            {
                self.set_testing(false);
            }
            self.step();
        }
    }

    fn pause_reason(&mut self) -> Option<&'static str> {
        let testing = self.testing_since.is_some();
        if self.engine.is_none() {
            return Some("untrained");
        }
        if !self.cfg.enabled && !testing {
            return Some("disabled");
        }
        let state = self.app.state::<AppState>();
        // A recorded call is full of "Soul"-like speech; push-to-talk still works.
        if state.recorder.is_capturing() || crate::notes::is_recording() {
            return Some("recording");
        }
        if ask::is_active(&state.ask) {
            return Some("ask");
        }
        if *state.busy.lock() {
            return Some("busy");
        }
        let now = Instant::now();
        if state.voice.is_speaking() {
            self.speaking_until = Some(now + SPEAK_TAIL);
            return Some("speaking");
        }
        if self.speaking_until.is_some_and(|t| now < t) {
            return Some("speaking");
        }
        if self.cfg.pause_in_games && !testing {
            let due = self.fullscreen.1.is_none_or(|t| t.elapsed() >= FULLSCREEN_POLL);
            if due {
                let fs = state.platform.power_state().fullscreen.unwrap_or(false);
                self.fullscreen = (fs, Some(now));
            }
            if self.fullscreen.0 {
                return Some("fullscreen");
            }
        }
        None
    }

    fn publish_status(&self, listening: bool, paused: Option<&'static str>) {
        publish(
            &self.app,
            WakeStatus {
                enabled: self.cfg.enabled,
                trained: self.model.is_some(),
                listening,
                paused,
                testing: self.testing_since.is_some(),
                samples: self.samples,
                sensitivity: self.cfg.sensitivity,
                pause_in_games: self.cfg.pause_in_games,
                error: self.mic_error.clone(),
            },
        );
    }

    fn close_tap(&mut self) {
        if self.tap.take().is_some() {
            if let Some(e) = self.engine.as_mut() {
                e.reset();
            }
        }
    }

    fn step(&mut self) {
        if let Some(reason) = self.pause_reason() {
            self.close_tap();
            self.publish_status(false, Some(reason));
            return;
        }
        if self.tap.is_none() {
            if self.mic_retry_at.is_some_and(|t| Instant::now() < t) {
                self.publish_status(false, Some("mic"));
                return;
            }
            let preferred = Some(self.cfg.microphone.as_str()).filter(|m| !m.is_empty());
            match MicTap::open(preferred) {
                Ok((tap, device)) => {
                    eprintln!("[soul][wake] mic open: {device}");
                    self.tap = Some(tap);
                    self.mic_retry_at = None;
                    self.mic_error = None;
                    if let Some(e) = self.engine.as_mut() {
                        e.reset();
                    }
                }
                Err(e) => {
                    if self.mic_error.as_deref() != Some(e.as_str()) {
                        crate::push_error_log(&self.app, format!("Wake word: {e}"));
                    }
                    self.mic_error = Some(e);
                    self.mic_retry_at = Some(Instant::now() + MIC_RETRY);
                    self.publish_status(false, Some("mic"));
                    return;
                }
            }
        }

        let mut pcm = std::mem::take(&mut self.pcm);
        pcm.clear();
        let drained = self.tap.as_mut().map(|t| t.drain(&mut pcm));
        if let Some(Err(e)) = drained {
            eprintln!("[soul][wake] mic stream error: {e}");
            self.tap = None;
            self.mic_retry_at = Some(Instant::now() + Duration::from_millis(500));
            self.pcm = pcm;
            return;
        }
        let cooling = self.cooldown_until.is_some_and(|t| Instant::now() < t);
        let detection = if cooling {
            None
        } else {
            self.engine.as_mut().and_then(|e| e.push(&pcm))
        };
        self.pcm = pcm;
        self.publish_status(true, cooling.then_some("cooldown"));

        let testing = self.testing_since.is_some();
        if testing && self.last_test_emit.elapsed() >= TEST_EMIT_EVERY {
            self.last_test_emit = Instant::now();
            let score = self.engine.as_mut().map_or(0.0, |e| e.take_probe_score());
            self.emit_score(score, false);
        }
        let Some(d) = detection else {
            return;
        };
        self.cooldown_until = Some(Instant::now() + COOLDOWN);
        eprintln!(
            "[soul][wake] detected score={:.3} avg={:.3} hits={} test={testing}",
            d.score, d.avg_score, d.hits
        );
        if testing {
            self.emit_score(d.score, true);
            return;
        }
        // Release the mic before the recorder opens its own stream.
        self.close_tap();
        self.publish_status(false, Some("recording"));
        capture_after_wake(&self.app, d);
    }

    fn emit_score(&self, score: f32, detected: bool) {
        let _ = self.app.emit(
            "soul://wake-score",
            ScorePayload {
                score,
                threshold: thresholds(self.cfg.sensitivity).threshold,
                detected,
            },
        );
    }
}

/// Wake word fired: same flow as holding the talk key, ended by the endpointer instead of the
/// key release. Blocks the listener thread (detection is paused meanwhile anyway).
fn capture_after_wake(app: &AppHandle, d: Detection) {
    hands_free_capture(app, "wake word", &|| {
        eprintln!(
            "[soul][wake] false positive? no speech after wake word score={:.3} avg={:.3} hits={}",
            d.score, d.avg_score, d.hits
        );
        crate::soul_debug!("[soul][wake] false_positive score={:.3}", d.score);
    });
}

/// The Soul icon in Vio was clicked: listen once like after the wake word (ends on silence).
pub fn listen_hands_free(app: &AppHandle, why: &'static str) {
    let app = app.clone();
    thread::spawn(move || hands_free_capture(&app, why, &|| eprintln!("[soul][wake] {why}: no speech")));
}

/// One talk-key-like turn ended by the energy endpointer instead of a key release.
fn hands_free_capture(app: &AppHandle, why: &str, no_speech: &dyn Fn()) {
    let state = app.state::<AppState>();
    if *state.busy.lock() || state.recorder.is_capturing() || ask::is_active(&state.ask) {
        eprintln!("[soul][wake] {why} ignored (busy / recording / Ask open)");
        return;
    }
    agent::barge_in(app);
    let gate = match begin_capture(app, CaptureTarget::Dictation, false) {
        Ok((gate, _notice)) => gate,
        Err(e) => {
            report_error(app, e);
            schedule_hide(app, 8000);
            return;
        }
    };
    let t0 = Instant::now();
    let mut vad = Endpointer::default();
    loop {
        thread::sleep(VAD_TICK);
        if !state.recorder.is_capturing() {
            // Stopped elsewhere (talk key, Settings) — nothing left to do here.
            return;
        }
        match vad.push(state.recorder.level(), VAD_TICK.as_millis() as u64) {
            Endpoint::Continue => {}
            Endpoint::NoSpeech => {
                no_speech();
                cancel_capture(app, &gate);
                schedule_hide(app, 0);
                return;
            }
            end => {
                eprintln!(
                    "[soul][wake] endpoint={end:?} speech={} capture_ms={}",
                    vad.heard_speech(),
                    t0.elapsed().as_millis()
                );
                crate::on_smart_released(app);
                return;
            }
        }
    }
}

// ───────────────────────── tray ─────────────────────────

pub fn tray_item(app: &AppHandle, enabled: bool) -> tauri::Result<tauri::menu::CheckMenuItem<tauri::Wry>> {
    let item = tauri::menu::CheckMenuItem::with_id(
        app,
        "wake",
        "Listen for “Soul”",
        true,
        enabled,
        None::<&str>,
    )?;
    let _ = TRAY_ITEM.set(item.clone());
    Ok(item)
}

/// Tray click: flip `wake_enabled` (opens Settings when there is no trained model yet).
pub fn toggle_from_tray(app: &AppHandle) {
    let app = app.clone();
    thread::spawn(move || {
        let settings = load_settings(&app).unwrap_or_default();
        let enable = !settings.wake_enabled;
        if let Err(e) = set_config(&app, enable, settings.wake_sensitivity, settings.wake_pause_in_games) {
            eprintln!("[soul][wake] tray toggle: {e}");
            if let Some(item) = TRAY_ITEM.get() {
                let _ = item.set_checked(settings.wake_enabled);
            }
            crate::island::toast(
                &app,
                "wake",
                "Najpierw nagraj słowo „Soul” w Ustawieniach → Głos.",
                None,
            );
            let _ = crate::open_settings_page(&app);
        }
    });
}

fn set_config(app: &AppHandle, enabled: bool, sensitivity: f32, pause_in_games: bool) -> Result<(), String> {
    if enabled && !trained() {
        return Err("untrained".into());
    }
    let mut settings = load_settings(app)?;
    settings.wake_enabled = enabled;
    settings.wake_sensitivity = sensitivity.clamp(0.0, 1.0);
    settings.wake_pause_in_games = pause_in_games;
    save_settings(app, &settings)?;
    apply(&settings);
    eprintln!(
        "[soul][wake] config enabled={enabled} sensitivity={:.2} pause_in_games={pause_in_games}",
        settings.wake_sensitivity
    );
    Ok(())
}

// ───────────────────────── commands ─────────────────────────

#[tauri::command]
pub fn wake_status() -> WakeStatus {
    status()
}

/// Autosaved Wake word section (enabled / sensitivity / pause in games). Error "untrained".
#[tauri::command]
pub async fn wake_save_config(
    app: AppHandle,
    enabled: bool,
    sensitivity: f32,
    pause_in_games: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || set_config(&app, enabled, sensitivity, pause_in_games))
        .await
        .map_err(|e| e.to_string())?
}

/// Live score meter for Settings → Test (`soul://wake-score`, ~10 Hz).
#[tauri::command]
pub fn wake_test(on: bool) -> Result<(), String> {
    if on && !trained() {
        return Err("untrained".into());
    }
    send(Ctrl::Test(on));
    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrainSample {
    index: usize,
    duration_ms: usize,
    peak: f32,
}

/// Record one training sample (fixed window). Errors are codes the Settings UI translates:
/// busy | silence | quiet | short | long | (anything else = raw message).
#[tauri::command]
pub async fn wake_train_record(app: AppHandle, index: usize) -> Result<TrainSample, String> {
    tauri::async_runtime::spawn_blocking(move || record_sample(&app, index))
        .await
        .map_err(|e| e.to_string())?
}

fn record_sample(app: &AppHandle, index: usize) -> Result<TrainSample, String> {
    if index >= MAX_SAMPLES {
        return Err(format!("At most {MAX_SAMPLES} samples."));
    }
    let state = app.state::<AppState>();
    if *state.busy.lock() || state.recorder.is_capturing() {
        return Err("busy".into());
    }
    let settings = load_settings(app).unwrap_or_default();
    let preferred = Some(settings.microphone.clone()).filter(|m| !m.trim().is_empty());
    state.recorder.start(preferred)?;
    thread::sleep(RECORD_WINDOW);
    let wav = match state.recorder.stop() {
        Ok(wav) => wav,
        Err(e) if e.contains("silence") || e.contains("No audio") => return Err("silence".into()),
        Err(e) => return Err(e),
    };
    let samples = read_wav_16k(&wav)?;
    let (start, end) = trim_word(&samples).ok_or("silence")?;
    let word = &samples[start..end];
    let peak = word.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let duration_ms = word.len() * 1000 / SAMPLE_RATE;
    eprintln!("[soul][wake] train sample={index} ms={duration_ms} peak={peak:.3}");
    if peak < WORD_MIN_PEAK {
        return Err("quiet".into());
    }
    if duration_ms < WORD_MIN_MS {
        return Err("short".into());
    }
    if duration_ms > WORD_MAX_MS {
        return Err("long".into());
    }
    let dir = samples_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("sample_{}.wav", index + 1));
    std::fs::write(&path, wav_16k_bytes(word)?).map_err(|e| format!("{}: {e}", path.display()))?;
    send(Ctrl::Config(Config::from(&settings))); // refresh the sample count
    Ok(TrainSample {
        index,
        duration_ms,
        peak,
    })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrainResult {
    samples: usize,
    /// Leave-one-out: each sample scored against a model built from the others.
    scores: Vec<f32>,
    threshold: f32,
}

/// Build `soul.rpw` from the recorded samples and reload the listener.
#[tauri::command]
pub async fn wake_train_build(app: AppHandle) -> Result<TrainResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let files = sample_files();
        if files.len() < MIN_SAMPLES {
            return Err(format!("Need at least {MIN_SAMPLES} samples (have {}).", files.len()));
        }
        let mut buffers = HashMap::new();
        for f in files.iter().take(MAX_SAMPLES) {
            let name = f.file_name().unwrap_or_default().to_string_lossy().to_string();
            buffers.insert(name, std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?);
        }
        let sensitivity = load_settings(&app).map(|s| s.wake_sensitivity).unwrap_or(0.5);
        let scores = leave_one_out(&buffers, sensitivity);
        let model = build_model(buffers)?;
        let path = model_path()?;
        std::fs::write(&path, model).map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!(
            "[soul][wake] model built samples={} loo_scores={scores:.2?}",
            files.len().min(MAX_SAMPLES)
        );
        send(Ctrl::Reload);
        Ok(TrainResult {
            samples: files.len().min(MAX_SAMPLES),
            scores,
            threshold: thresholds(sensitivity).threshold,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Score every sample against a model trained on the others (best probe score).
pub fn leave_one_out(buffers: &HashMap<String, Vec<u8>>, sensitivity: f32) -> Vec<f32> {
    let mut names: Vec<&String> = buffers.keys().collect();
    names.sort();
    names
        .iter()
        .map(|held| {
            let rest: HashMap<String, Vec<u8>> = buffers
                .iter()
                .filter(|(k, _)| k != held)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let score = (|| {
                let model = build_model(rest)?;
                let mut engine = WakeEngine::new(&model, sensitivity, true)?;
                let clip = read_wav_16k(&buffers[*held])?;
                Ok::<f32, String>(scan(&mut engine, &clip).1)
            })();
            score.unwrap_or(0.0)
        })
        .collect()
}

/// Forget the recorded samples (and, with `model`, the trained wake word — disables it).
#[tauri::command]
pub async fn wake_train_reset(app: AppHandle, model: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        if let Ok(dir) = samples_dir() {
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::create_dir_all(&dir);
        }
        if model {
            if let Ok(path) = model_path() {
                let _ = std::fs::remove_file(path);
            }
            let settings = load_settings(&app)?;
            set_config(&app, false, settings.wake_sensitivity, settings.wake_pause_in_games)?;
            send(Ctrl::Reload);
        } else {
            send(Ctrl::Config(Config::from(&load_settings(&app)?)));
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, ms: usize, amp: f32) -> Vec<f32> {
        (0..SAMPLE_RATE * ms / 1000)
            .map(|i| (i as f32 * freq * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * amp)
            .collect()
    }

    #[test]
    fn sensitivity_maps_to_tuned_point_at_half() {
        let t = thresholds(0.5);
        assert!((t.threshold - 0.64).abs() < 1e-6);
        assert_eq!(t.min_scores, 8);
        assert_eq!(thresholds(0.0).min_scores, 12);
        assert_eq!(thresholds(1.0).min_scores, 4);
        assert!(thresholds(1.0).threshold < thresholds(0.0).threshold);
        assert_eq!(thresholds(7.0), thresholds(1.0));
    }

    #[test]
    fn decimator_keeps_duration_and_dc() {
        for rate in [48_000u32, 44_100, 16_000, 8_000] {
            let mut d = Decimator::new(rate);
            let mut out = Vec::new();
            // Uneven chunks, one second of DC.
            let input = vec![0.5f32; rate as usize];
            for c in input.chunks(333) {
                d.push(c, &mut out);
            }
            assert!((out.len() as i64 - SAMPLE_RATE as i64).abs() <= 2, "{rate}: {}", out.len());
            assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-5));
        }
    }

    #[test]
    fn trim_word_finds_the_burst() {
        let mut clip = vec![0.001f32; SAMPLE_RATE / 2];
        clip.extend(tone(300.0, 400, 0.3));
        clip.extend(vec![0.001f32; SAMPLE_RATE / 2]);
        let (s, e) = trim_word(&clip).unwrap();
        let ms = (e - s) * 1000 / SAMPLE_RATE;
        assert!((400..=540).contains(&ms), "{ms}");
        assert!(trim_word(&vec![0.0005f32; SAMPLE_RATE]).is_none());
    }

    #[test]
    fn endpointer_stops_after_silence() {
        let mut vad = Endpointer::default();
        let mut t = 0;
        // 300 ms room noise, 1.5 s speech, then silence.
        while t < 300 {
            assert_eq!(vad.push(0.01, 30), Endpoint::Continue);
            t += 30;
        }
        while t < 1800 {
            assert_eq!(vad.push(0.3, 30), Endpoint::Continue);
            t += 30;
        }
        let mut end = Endpoint::Continue;
        let silence_start = t;
        while end == Endpoint::Continue {
            end = vad.push(0.012, 30);
            t += 30;
        }
        assert_eq!(end, Endpoint::Silence);
        let waited = t - silence_start;
        assert!((1200..=1260).contains(&waited), "{waited}");
    }

    #[test]
    fn endpointer_caps_and_flags_no_speech() {
        let mut vad = Endpointer::default();
        let mut end = Endpoint::Continue;
        let mut t = 0;
        while end == Endpoint::Continue {
            end = vad.push(0.01, 30);
            t += 30;
        }
        assert_eq!(end, Endpoint::NoSpeech);
        assert!(t >= NO_SPEECH_MS as usize && t < NO_SPEECH_MS as usize + 60);

        let mut vad = Endpointer::default();
        let mut end = Endpoint::Continue;
        let mut t = 0;
        while end == Endpoint::Continue {
            end = vad.push(if (t / 300) % 2 == 0 { 0.3 } else { 0.01 }, 30);
            t += 30;
        }
        assert_eq!(end, Endpoint::MaxLength);
        assert!(vad.heard_speech());
    }

    #[test]
    fn engine_detects_its_own_template_and_ignores_noise() {
        // Synthetic "word": a short chirp-like sequence of tones (stands in for a voice sample).
        let word = |shift: f32| {
            let mut w = tone(220.0 * shift, 120, 0.3);
            w.extend(tone(440.0 * shift, 160, 0.4));
            w.extend(tone(330.0 * shift, 140, 0.3));
            w
        };
        let mut buffers = HashMap::new();
        for (i, shift) in [1.0f32, 1.01, 0.99, 1.02, 0.98, 1.0].iter().enumerate() {
            buffers.insert(format!("s{i}.wav"), wav_16k_bytes(&word(*shift)).unwrap());
        }
        let model = build_model(buffers).unwrap();
        let mut engine = WakeEngine::new(&model, 0.5, true).unwrap();
        let (hits, best) = scan(&mut engine, &word(1.005));
        assert_eq!(hits.len(), 1, "best={best}");

        engine.reset();
        let noise: Vec<f32> = dither(SAMPLE_RATE * 3).iter().map(|s| s * 1500.0).collect();
        let (hits, _) = scan(&mut engine, &noise);
        assert!(hits.is_empty());
    }
}


/// Live CPU check: the listener's real path (own mic stream → decimator → detector) for
/// `SOUL_WAKE_CPU_SECS` (default 30) on the default mic, model from tests/voice/wake/train.
/// Measure the process CPU from outside (Task Manager / Get-Process) while it runs.
///   cargo test --lib wake::cpu_live -- --ignored --nocapture
#[cfg(test)]
mod cpu_live {
    use super::*;

    #[test]
    #[ignore]
    fn wake_cpu_live() {
        let secs: u64 = std::env::var("SOUL_WAKE_CPU_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        let train = crate::dev_samples::voice_test_dir().unwrap().join("wake").join("train");
        let mut buffers = HashMap::new();
        for e in std::fs::read_dir(&train).unwrap() {
            let p = e.unwrap().path();
            let s = read_wav_16k(&std::fs::read(&p).unwrap()).unwrap();
            let (a, b) = trim_word(&s).unwrap();
            buffers.insert(p.display().to_string(), wav_16k_bytes(&s[a..b]).unwrap());
        }
        let model = build_model(buffers).unwrap();
        let mut engine = WakeEngine::new(&model, 0.5, false).unwrap();
        crate::platform::lower_current_thread_priority();
        let (mut tap, device) = MicTap::open(None).expect("mic");
        eprintln!("[soul][wake] cpu test on '{device}' for {secs}s (pid {})", std::process::id());
        let t0 = Instant::now();
        let mut pcm = Vec::new();
        let mut total = 0usize;
        let mut busy = Duration::ZERO;
        let mut fired = 0;
        while t0.elapsed() < Duration::from_secs(secs) {
            thread::sleep(TICK_LISTENING);
            let w = Instant::now();
            pcm.clear();
            tap.drain(&mut pcm).unwrap();
            total += pcm.len();
            fired += usize::from(engine.push(&pcm).is_some());
            busy += w.elapsed();
        }
        eprintln!(
            "[soul][wake] cpu test done: audio={:.1}s detector_busy={:.0}ms ({:.2}% of one core) detections={fired}",
            total as f32 / SAMPLE_RATE as f32,
            busy.as_secs_f64() * 1000.0,
            busy.as_secs_f64() * 100.0 / t0.elapsed().as_secs_f64()
        );
        assert!(total > SAMPLE_RATE * (secs as usize - 2), "mic delivered too little audio");
    }
}
