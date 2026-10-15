//! Microphone capture via cpal. Records PCM while the push-to-talk hotkey is held,
//! then encodes a 16 kHz mono WAV in memory for Gemini.
//!
//! Capture runs on a dedicated audio thread because `cpal::Stream` is not `Send` on
//! all platforms (e.g. WASAPI on Windows).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, StreamConfig};
use hound::{WavSpec, WavWriter};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

const TARGET_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneInfo {
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MicLevelPayload {
    level: f32,
}

pub struct StartResult {
    pub device_name: String,
    pub used_fallback: bool,
    pub notice: Option<String>,
}

/// Receives ~100 ms chunks of 16 kHz mono s16le PCM while recording (must not block).
pub type PcmSink = Box<dyn FnMut(Vec<u8>) + Send>;

/// Throttled level callback for the Settings meter (keeps the capture path free of Tauri types).
pub(crate) type LevelEmitter = Arc<dyn Fn(f32) + Send + Sync>;

/// 100 ms at 16 kHz — the chunk size the Live API docs recommend.
const PCM_CHUNK_SAMPLES: usize = 1_600;

enum AudioCmd {
    Start {
        preferred: Option<String>,
        /// When set, a feeder thread streams 16 kHz PCM chunks while recording.
        pcm_tx: Option<PcmSink>,
        reply: Sender<Result<StartResult, String>>,
    },
    Stop {
        reply: Sender<Result<Vec<u8>, String>>,
    },
    IsRecording {
        reply: Sender<bool>,
    },
    StartMonitor {
        preferred: Option<String>,
        app: AppHandle,
        reply: Sender<Result<StartResult, String>>,
    },
    StopMonitor {
        reply: Sender<()>,
    },
}

pub struct AudioRecorder {
    tx: Sender<AudioCmd>,
    monitoring: Arc<AtomicBool>,
    level_bits: Arc<AtomicU32>,
    recording: Arc<AtomicBool>,
}

/// While recording, `soul://mic-level` is emitted at ~30 Hz for the Island spirit / waveform.
const LEVEL_EMIT_EVERY: Duration = Duration::from_millis(33);

impl AudioRecorder {
    pub fn new() -> Self {
        let monitoring = Arc::new(AtomicBool::new(false));
        let monitoring_flag = Arc::clone(&monitoring);
        let level_bits = Arc::new(AtomicU32::new(0f32.to_bits()));
        let level_bits_thread = Arc::clone(&level_bits);
        let (tx, rx) = mpsc::channel::<AudioCmd>();

        let recording_flag = Arc::new(AtomicBool::new(false));
        let recording_flag_thread = Arc::clone(&recording_flag);
        let recording = Arc::clone(&recording_flag);

        thread::Builder::new()
            .name("soul-audio".into())
            .spawn(move || {
                let samples = Arc::new(Mutex::new(Vec::<f32>::new()));
                let last_error = Arc::new(Mutex::new(None::<String>));
                let level_bits = level_bits_thread;
                let recording_flag = recording_flag_thread;
                let mut sample_rate = TARGET_SAMPLE_RATE;
                let mut record_stream: Option<cpal::Stream> = None;
                let mut monitor_stream: Option<cpal::Stream> = None;
                let mut feeder: Option<thread::JoinHandle<()>> = None;
                let mut recording = false;

                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        AudioCmd::Start {
                            preferred,
                            pcm_tx,
                            reply,
                        } => {
                            // Recording takes the mic — stop the Settings level meter.
                            monitor_stream = None;
                            monitoring_flag.store(false, Ordering::SeqCst);

                            let result = (|| {
                                if recording {
                                    return Err("Already recording".into());
                                }
                                samples.lock().clear();
                                *last_error.lock() = None;

                                let resolved = resolve_input_device(preferred.as_deref())?;
                                eprintln!(
                                    "[soul][audio] recording with device: {}{}",
                                    resolved.name,
                                    if resolved.used_fallback {
                                        " (fallback to default)"
                                    } else {
                                        ""
                                    }
                                );

                                let (built, rate) = build_capture_stream(
                                    &resolved.device,
                                    Some(Arc::clone(&samples)),
                                    Arc::clone(&last_error),
                                    None,
                                    Arc::clone(&level_bits),
                                )?;
                                sample_rate = rate;
                                built
                                    .play()
                                    .map_err(|e| format!("Failed to start microphone: {e}"))?;
                                record_stream = Some(built);
                                recording = true;
                                recording_flag.store(true, Ordering::SeqCst);

                                if let Some(sink) = pcm_tx {
                                    let samples_feeder = Arc::clone(&samples);
                                    let flag = Arc::clone(&recording_flag);
                                    feeder = thread::Builder::new()
                                        .name("soul-pcm-feeder".into())
                                        .spawn(move || run_pcm_feeder(samples_feeder, flag, rate, sink))
                                        .ok();
                                }

                                Ok(StartResult {
                                    device_name: resolved.name,
                                    used_fallback: resolved.used_fallback,
                                    notice: resolved.notice,
                                })
                            })();
                            let _ = reply.send(result);
                        }
                        AudioCmd::Stop { reply } => {
                            let result = (|| {
                                let t0 = Instant::now();
                                if !recording {
                                    return Err("Not currently recording".into());
                                }
                                // Stop the device first, then let the feeder flush the tail so
                                // every captured sample is streamed before stop() returns.
                                record_stream = None;
                                recording_flag.store(false, Ordering::SeqCst);
                                if let Some(handle) = feeder.take() {
                                    let _ = handle.join();
                                }
                                recording = false;
                                eprintln!(
                                    "[soul][dictation] stage=recording_stop ms={}",
                                    t0.elapsed().as_millis()
                                );

                                if let Some(err) = last_error.lock().take() {
                                    return Err(err);
                                }

                                let captured = std::mem::take(&mut *samples.lock());
                                if captured.is_empty() {
                                    return Err(
                                        "No audio captured. Hold the hotkey a bit longer and speak."
                                            .into(),
                                    );
                                }
                                let t1 = Instant::now();
                                let wav = encode_wav_16k_mono(&captured, sample_rate)?;
                                eprintln!(
                                    "[soul][dictation] stage=wav_encode ms={} in_rate={} samples={} wav_kb={:.1}",
                                    t1.elapsed().as_millis(),
                                    sample_rate,
                                    captured.len(),
                                    wav.len() as f64 / 1024.0
                                );
                                Ok(wav)
                            })();
                            let _ = reply.send(result);
                        }
                        AudioCmd::IsRecording { reply } => {
                            let _ = reply.send(recording);
                        }
                        AudioCmd::StartMonitor {
                            preferred,
                            app,
                            reply,
                        } => {
                            let result = (|| {
                                if recording {
                                    return Err(
                                        "Can't monitor while recording. Release the hotkey first."
                                            .into(),
                                    );
                                }
                                monitor_stream = None;

                                let resolved = resolve_input_device(preferred.as_deref())?;
                                eprintln!(
                                    "[soul][audio] monitoring device: {}{}",
                                    resolved.name,
                                    if resolved.used_fallback {
                                        " (fallback to default)"
                                    } else {
                                        ""
                                    }
                                );

                                let mon_err = Arc::new(Mutex::new(None::<String>));
                                let (built, _) = build_capture_stream(
                                    &resolved.device,
                                    None,
                                    mon_err,
                                    Some(level_emitter(app.clone())),
                                    Arc::clone(&level_bits),
                                )?;
                                built
                                    .play()
                                    .map_err(|e| format!("Failed to start mic monitor: {e}"))?;
                                monitor_stream = Some(built);
                                monitoring_flag.store(true, Ordering::SeqCst);

                                Ok(StartResult {
                                    device_name: resolved.name,
                                    used_fallback: resolved.used_fallback,
                                    notice: resolved.notice,
                                })
                            })();
                            let _ = reply.send(result);
                        }
                        AudioCmd::StopMonitor { reply } => {
                            monitor_stream = None;
                            monitoring_flag.store(false, Ordering::SeqCst);
                            level_bits.store(0f32.to_bits(), Ordering::Relaxed);
                            let _ = reply.send(());
                        }
                    }
                }
            })
            .expect("failed to spawn soul-audio thread");

        Self {
            tx,
            monitoring,
            level_bits,
            recording,
        }
    }

    /// Emit the live input level while recording (throttled, own thread — never the cpal
    /// callback). Sends one final 0 when recording stops so the UI settles.
    pub fn attach_level_emitter(&self, app: AppHandle) {
        let level_bits = Arc::clone(&self.level_bits);
        let recording = Arc::clone(&self.recording);
        let _ = thread::Builder::new()
            .name("soul-mic-level".into())
            .spawn(move || {
                let mut was = false;
                loop {
                    thread::sleep(LEVEL_EMIT_EVERY);
                    let now = recording.load(Ordering::Relaxed);
                    if now {
                        let level = f32::from_bits(level_bits.load(Ordering::Relaxed));
                        let _ = app.emit("soul://mic-level", MicLevelPayload { level });
                    } else if was {
                        let _ = app.emit("soul://mic-level", MicLevelPayload { level: 0.0 });
                    }
                    was = now;
                }
            });
    }

    /// Latest input level 0.0–1.0 (updated while recording or monitoring).
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level_bits.load(Ordering::Relaxed))
    }

    pub fn start(&self, preferred: Option<String>) -> Result<StartResult, String> {
        self.start_with_pcm_stream(preferred, None)
    }

    /// Start recording; optionally stream 16 kHz PCM chunks to `pcm_tx` while held.
    pub fn start_with_pcm_stream(
        &self,
        preferred: Option<String>,
        pcm_tx: Option<PcmSink>,
    ) -> Result<StartResult, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(AudioCmd::Start {
                preferred,
                pcm_tx,
                reply: reply_tx,
            })
            .map_err(|_| "Audio thread stopped".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "Audio thread stopped".to_string())?
    }

    pub fn stop(&self) -> Result<Vec<u8>, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(AudioCmd::Stop { reply: reply_tx })
            .map_err(|_| "Audio thread stopped".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "Audio thread stopped".to_string())?
    }

    pub fn is_recording(&self) -> bool {
        let (reply_tx, reply_rx) = mpsc::channel();
        if self
            .tx
            .send(AudioCmd::IsRecording { reply: reply_tx })
            .is_err()
        {
            return false;
        }
        reply_rx.recv().unwrap_or(false)
    }

    pub fn start_monitor(
        &self,
        preferred: Option<String>,
        app: AppHandle,
    ) -> Result<StartResult, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(AudioCmd::StartMonitor {
                preferred,
                app,
                reply: reply_tx,
            })
            .map_err(|_| "Audio thread stopped".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "Audio thread stopped".to_string())?
    }

    pub fn stop_monitor(&self) {
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.tx.send(AudioCmd::StopMonitor { reply: reply_tx });
        let _ = reply_rx.recv_timeout(Duration::from_millis(500));
    }

    /// Recording right now (lock-free; no round trip to the audio thread).
    pub fn is_capturing(&self) -> bool {
        self.recording.load(Ordering::SeqCst)
    }

    pub fn is_monitoring(&self) -> bool {
        self.monitoring.load(Ordering::SeqCst)
    }
}

pub fn list_microphones() -> Result<Vec<MicrophoneInfo>, String> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok());

    let devices = host
        .input_devices()
        .map_err(|e| format!("Failed to enumerate microphones: {e}"))?;

    let mut out = Vec::new();
    for device in devices {
        let name = match device.name() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let is_default = default_name.as_ref().is_some_and(|d| d == &name);
        out.push(MicrophoneInfo { name, is_default });
    }

    // Put default first for nicer UI.
    out.sort_by(|a, b| b.is_default.cmp(&a.is_default).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

pub(crate) struct ResolvedDevice {
    pub device: Device,
    pub name: String,
    pub used_fallback: bool,
    pub notice: Option<String>,
}

pub(crate) fn resolve_input_device(preferred: Option<&str>) -> Result<ResolvedDevice, String> {
    let host = cpal::default_host();
    let preferred = preferred.map(str::trim).filter(|s| !s.is_empty());

    if let Some(want) = preferred {
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                if let Ok(name) = device.name() {
                    if name == want {
                        return Ok(ResolvedDevice {
                            device,
                            name,
                            used_fallback: false,
                            notice: None,
                        });
                    }
                }
            }
        }

        let default = host.default_input_device().ok_or_else(|| {
            format!(
                "Microphone '{want}' not found, and no system default mic is available."
            )
        })?;
        let name = default
            .name()
            .unwrap_or_else(|_| "System default".to_string());
        let notice = Some(format!(
            "Microphone '{want}' not found — using system default ({name})."
        ));
        eprintln!("[soul][audio] {}", notice.as_ref().unwrap());
        return Ok(ResolvedDevice {
            device: default,
            name,
            used_fallback: true,
            notice,
        });
    }

    let default = host.default_input_device().ok_or_else(|| {
        "No microphone found. Check that a mic is connected and permitted.".to_string()
    })?;
    let name = default
        .name()
        .unwrap_or_else(|_| "System default".to_string());
    Ok(ResolvedDevice {
        device: default,
        name,
        used_fallback: false,
        notice: None,
    })
}

pub(crate) fn build_capture_stream(
    device: &Device,
    samples: Option<Arc<Mutex<Vec<f32>>>>,
    last_error: Arc<Mutex<Option<String>>>,
    level_app: Option<LevelEmitter>,
    level_bits: Arc<AtomicU32>,
) -> Result<(cpal::Stream, u32), String> {
    let config = device
        .default_input_config()
        .map_err(|e| format!("Failed to query microphone config: {e}"))?;

    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();
    let stream_config: StreamConfig = config.into();

    let err_slot = Arc::clone(&last_error);
    let err_fn = move |err: cpal::StreamError| {
        *err_slot.lock() = Some(format!("Microphone stream error: {err}"));
    };

    let last_emit = Arc::new(Mutex::new(Instant::now() - Duration::from_millis(100)));

    let stream = match sample_format {
        SampleFormat::F32 => {
            let samples_cb = samples.clone();
            let level_bits = Arc::clone(&level_bits);
            let last_emit = Arc::clone(&last_emit);
            let level_app = level_app.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], _| {
                    if let Some(buf) = samples_cb.as_ref() {
                        push_mono(data, channels, &mut buf.lock());
                    }
                    update_level(data, channels, &level_bits, &last_emit, level_app.as_ref());
                },
                err_fn,
                None,
            )
        }
        SampleFormat::I16 => {
            let samples_cb = samples.clone();
            let level_bits = Arc::clone(&level_bits);
            let last_emit = Arc::clone(&last_emit);
            let level_app = level_app.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    let converted: Vec<f32> =
                        data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                    if let Some(buf) = samples_cb.as_ref() {
                        push_mono(&converted, channels, &mut buf.lock());
                    }
                    update_level(
                        &converted,
                        channels,
                        &level_bits,
                        &last_emit,
                        level_app.as_ref(),
                    );
                },
                err_fn,
                None,
            )
        }
        SampleFormat::U16 => {
            let samples_cb = samples.clone();
            let level_bits = Arc::clone(&level_bits);
            let last_emit = Arc::clone(&last_emit);
            let level_app = level_app.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[u16], _| {
                    let converted: Vec<f32> = data
                        .iter()
                        .map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        .collect();
                    if let Some(buf) = samples_cb.as_ref() {
                        push_mono(&converted, channels, &mut buf.lock());
                    }
                    update_level(
                        &converted,
                        channels,
                        &level_bits,
                        &last_emit,
                        level_app.as_ref(),
                    );
                },
                err_fn,
                None,
            )
        }
        other => {
            return Err(format!("Unsupported microphone sample format: {other:?}"));
        }
    }
    .map_err(|e| format!("Failed to open microphone: {e}"))?;

    Ok((stream, sample_rate))
}

/// Stream the growing capture buffer as 16 kHz s16le chunks. Resamples against absolute
/// positions in the source buffer, so chunk boundaries never click or drift. When `recording`
/// goes false, flushes everything left (including a partial chunk) and returns.
fn run_pcm_feeder(
    samples: Arc<Mutex<Vec<f32>>>,
    recording: Arc<AtomicBool>,
    from_rate: u32,
    mut sink: PcmSink,
) {
    let ratio = from_rate as f64 / TARGET_SAMPLE_RATE as f64;
    let mut produced = 0usize;
    let mut pending: Vec<i16> = Vec::with_capacity(PCM_CHUNK_SAMPLES * 2);
    loop {
        let running = recording.load(Ordering::SeqCst);
        {
            let buf = samples.lock();
            loop {
                let src = produced as f64 * ratio;
                let i = src as usize;
                let Some(&a) = buf.get(i) else {
                    break;
                };
                let b = match buf.get(i + 1) {
                    Some(&b) => b,
                    None if !running => a,
                    None => break,
                };
                let s = a + (b - a) * (src - i as f64) as f32;
                pending.push((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                produced += 1;
            }
        }
        while pending.len() >= PCM_CHUNK_SAMPLES {
            let chunk: Vec<i16> = pending.drain(..PCM_CHUNK_SAMPLES).collect();
            sink(pcm_bytes(&chunk));
        }
        if !running {
            if !pending.is_empty() {
                sink(pcm_bytes(&pending));
            }
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

fn level_emitter(app: AppHandle) -> LevelEmitter {
    Arc::new(move |level| {
        let _ = app.emit("soul://mic-level", MicLevelPayload { level });
    })
}

fn update_level(
    interleaved: &[f32],
    channels: usize,
    level_bits: &AtomicU32,
    last_emit: &Mutex<Instant>,
    app: Option<&LevelEmitter>,
) {
    if interleaved.is_empty() {
        return;
    }

    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f32;
    let mut n = 0usize;

    if channels <= 1 {
        for &s in interleaved {
            let a = s.abs();
            if a > peak {
                peak = a;
            }
            sum_sq += s * s;
            n += 1;
        }
    } else {
        for frame in interleaved.chunks(channels) {
            let mono = frame.iter().sum::<f32>() / channels as f32;
            let a = mono.abs();
            if a > peak {
                peak = a;
            }
            sum_sq += mono * mono;
            n += 1;
        }
    }

    let rms = if n > 0 { (sum_sq / n as f32).sqrt() } else { 0.0 };
    // Blend peak + RMS for a responsive but readable meter.
    let level = (peak * 0.65 + rms * 1.35).clamp(0.0, 1.0);
    level_bits.store(level.to_bits(), Ordering::Relaxed);

    let Some(emit) = app else {
        return;
    };

    let mut last = last_emit.lock();
    if last.elapsed() < Duration::from_millis(50) {
        return;
    }
    *last = Instant::now();
    emit(level);
}

fn push_mono(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    for frame in interleaved.chunks(channels) {
        let sum: f32 = frame.iter().sum();
        out.push(sum / channels as f32);
    }
}

fn encode_wav_16k_mono(samples: &[f32], from_rate: u32) -> Result<Vec<u8>, String> {
    // Downsample to 16 kHz mono, then trim leading/trailing silence.
    let resampled = resample_linear(samples, from_rate, TARGET_SAMPLE_RATE);
    let (trimmed, trim_info) = trim_silence(&resampled, TARGET_SAMPLE_RATE);
    eprintln!(
        "[soul][dictation] stage=silence_trim {}",
        trim_info
    );

    if trimmed.is_empty() {
        return Err("Only silence captured. Hold the hotkey and speak.".into());
    }

    let spec = WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = WavWriter::new(&mut cursor, spec)
            .map_err(|e| format!("Failed to create WAV: {e}"))?;
        for sample in &trimmed {
            let clipped = sample.clamp(-1.0, 1.0);
            let i = (clipped * i16::MAX as f32) as i16;
            writer
                .write_sample(i)
                .map_err(|e| format!("Failed to write WAV sample: {e}"))?;
        }
        writer
            .finalize()
            .map_err(|e| format!("Failed to finalize WAV: {e}"))?;
    }

    Ok(cursor.into_inner())
}

/// Trim leading/trailing silence. Keeps a small pad so speech onsets aren't clipped.
fn trim_silence(samples: &[f32], sample_rate: u32) -> (Vec<f32>, String) {
    if samples.is_empty() {
        return (Vec::new(), "empty".into());
    }

    const THRESHOLD: f32 = 0.012;
    let pad = (sample_rate as usize * 80) / 1000; // 80 ms pad

    let mut start = 0usize;
    while start < samples.len() && samples[start].abs() < THRESHOLD {
        start += 1;
    }
    if start >= samples.len() {
        return (Vec::new(), "all_silence".into());
    }
    start = start.saturating_sub(pad);

    let mut end = samples.len();
    while end > start && samples[end - 1].abs() < THRESHOLD {
        end -= 1;
    }
    end = (end + pad).min(samples.len());

    let before_ms = samples.len() as f64 * 1000.0 / sample_rate as f64;
    let after_ms = (end - start) as f64 * 1000.0 / sample_rate as f64;
    let info = format!(
        "before_ms={before_ms:.0} after_ms={after_ms:.0} dropped_ms={:.0}",
        before_ms - after_ms
    );
    (samples[start..end].to_vec(), info)
}

fn resample_linear(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }

    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = ((samples.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let src = i as f64 * ratio;
        let idx = src.floor() as usize;
        let frac = (src - idx as f64) as f32;
        let a = samples[idx];
        let b = samples.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}
