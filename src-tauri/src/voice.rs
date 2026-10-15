//! Spoken replies: plays Live API native audio (24 kHz s16le mono) through cpal.
//!
//! The output stream lives on its own thread (cpal streams are not `Send` on every backend) and
//! is opened on the first chunk and closed after a short idle, so Soul doesn't hold the audio
//! device. Every push carries a *generation*; `stop()` (barge-in) bumps it, clears the buffer and
//! makes late chunks of the interrupted reply fall on the floor.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Live API output rate.
pub const REPLY_SAMPLE_RATE: u32 = 24_000;
/// Close the output device after this long without audio.
const IDLE_CLOSE: Duration = Duration::from_millis(1_500);
const LEVEL_EVERY: Duration = Duration::from_millis(50);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceLevelPayload {
    level: f32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceSpeakingPayload {
    speaking: bool,
}

enum PlayerCmd {
    Push { generation: u64, pcm: Vec<u8> },
    Attach(AppHandle),
}

pub struct VoicePlayer {
    tx: Sender<PlayerCmd>,
    generation: Arc<AtomicU64>,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    speaking: Arc<AtomicBool>,
    /// Output gain 0.0–1.0 ("Głośność głosu Soul"), read by the audio callback.
    gain_bits: Arc<AtomicU32>,
}

impl VoicePlayer {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = mpsc::channel::<PlayerCmd>();
        let generation = Arc::new(AtomicU64::new(1));
        let buffer = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        let speaking = Arc::new(AtomicBool::new(false));
        let level_bits = Arc::new(AtomicU32::new(0f32.to_bits()));
        let gain_bits = Arc::new(AtomicU32::new(1f32.to_bits()));

        let thread_gen = Arc::clone(&generation);
        let thread_gain = Arc::clone(&gain_bits);
        let thread_buf = Arc::clone(&buffer);
        let thread_speaking = Arc::clone(&speaking);
        thread::Builder::new()
            .name("soul-voice".into())
            .spawn(move || {
                let mut out: Option<Output> = None;
                let mut app: Option<AppHandle> = None;
                let mut last_audio = Instant::now();
                let mut last_level = Instant::now();
                let mut resampler: Option<(u64, Resampler)> = None;
                loop {
                    match rx.recv_timeout(LEVEL_EVERY) {
                        Ok(PlayerCmd::Attach(handle)) => app = Some(handle),
                        Ok(PlayerCmd::Push { generation, pcm }) => {
                            if generation != thread_gen.load(Ordering::SeqCst) {
                                continue;
                            }
                            if out.is_none() {
                                let t0 = Instant::now();
                                match Output::open(
                                    Arc::clone(&thread_buf),
                                    Arc::clone(&level_bits),
                                    Arc::clone(&thread_gain),
                                )
                                {
                                    Ok(o) => {
                                        eprintln!(
                                            "[soul][voice] output open ms={} device={} rate={}",
                                            t0.elapsed().as_millis(),
                                            o.device_name,
                                            o.rate
                                        );
                                        out = Some(o);
                                    }
                                    Err(e) => {
                                        eprintln!("[soul][voice] output unavailable: {e}");
                                        continue;
                                    }
                                }
                            }
                            let rate = out.as_ref().map(|o| o.rate).unwrap_or(REPLY_SAMPLE_RATE);
                            // A new reply (generation) restarts the resampler phase.
                            if resampler.as_ref().map(|(g, _)| *g) != Some(generation) {
                                resampler =
                                    Some((generation, Resampler::new(REPLY_SAMPLE_RATE, rate)));
                            }
                            let samples: Vec<f32> = pcm
                                .chunks_exact(2)
                                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                                .collect();
                            let mut resampled = Vec::with_capacity(samples.len() * 2 + 4);
                            if let Some((_, r)) = resampler.as_mut() {
                                r.process(&samples, &mut resampled);
                            }
                            thread_buf.lock().extend(resampled);
                            last_audio = Instant::now();
                            if !thread_speaking.swap(true, Ordering::SeqCst) {
                                emit_speaking(app.as_ref(), true);
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }

                    let drained = thread_buf.lock().is_empty();
                    if thread_speaking.load(Ordering::SeqCst) {
                        if last_level.elapsed() >= LEVEL_EVERY {
                            last_level = Instant::now();
                            let level = f32::from_bits(level_bits.load(Ordering::Relaxed));
                            if let Some(app) = app.as_ref() {
                                let _ = app.emit("soul://voice-level", VoiceLevelPayload { level });
                            }
                        }
                        if drained {
                            thread_speaking.store(false, Ordering::SeqCst);
                            level_bits.store(0f32.to_bits(), Ordering::Relaxed);
                            emit_speaking(app.as_ref(), false);
                        }
                    }
                    if drained && out.is_some() && last_audio.elapsed() >= IDLE_CLOSE {
                        out = None;
                        resampler = None;
                        crate::soul_debug!("[soul][voice] output closed (idle)");
                    }
                }
            })
            .expect("failed to spawn soul-voice thread");

        Arc::new(Self {
            tx,
            generation,
            buffer,
            speaking,
            gain_bits,
        })
    }

    /// Soul's voice volume, 0–100 (applies to audio already queued too).
    pub fn set_volume(&self, percent: u8) {
        let gain = (percent.min(100) as f32 / 100.0).powf(1.6); // perceptual curve
        self.gain_bits.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// Let the player emit `soul://voice-level` / `soul://voice-speaking` for the Island waveform.
    pub fn attach(&self, app: AppHandle) {
        let _ = self.tx.send(PlayerCmd::Attach(app));
    }

    /// Current generation; pass it with every chunk of one reply.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Queue 24 kHz s16le mono PCM. Dropped when `generation` is stale (barge-in happened).
    pub fn push(&self, generation: u64, pcm: Vec<u8>) {
        if !pcm.is_empty() {
            let _ = self.tx.send(PlayerCmd::Push { generation, pcm });
        }
    }

    /// Barge-in: silence now and invalidate everything queued for the current reply.
    pub fn stop(&self) -> bool {
        let was = self.is_speaking();
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.buffer.lock().clear();
        was
    }

    /// True while queued audio is still playing.
    pub fn is_speaking(&self) -> bool {
        self.speaking.load(Ordering::SeqCst) || !self.buffer.lock().is_empty()
    }
}

fn emit_speaking(app: Option<&AppHandle>, speaking: bool) {
    crate::soul_debug!("[soul][voice] speaking={speaking}");
    if let Some(app) = app {
        let _ = app.emit("soul://voice-speaking", VoiceSpeakingPayload { speaking });
        crate::on_voice_speaking(app, speaking);
    }
}

struct Output {
    _stream: cpal::Stream,
    rate: u32,
    device_name: String,
}

impl Output {
    fn open(
        buffer: Arc<Mutex<VecDeque<f32>>>,
        level_bits: Arc<AtomicU32>,
        gain_bits: Arc<AtomicU32>,
    ) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "No audio output device".to_string())?;
        let device_name = device.name().unwrap_or_else(|_| "default".into());
        let config = device
            .default_output_config()
            .map_err(|e| format!("Output config: {e}"))?;
        let rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        let format = config.sample_format();
        let stream_config: StreamConfig = config.into();
        let err_fn = |e: cpal::StreamError| eprintln!("[soul][voice] output stream error: {e}");

        // Pull mono samples from the shared queue, duplicate to every channel, track the level.
        fn fill<T: Copy>(
            data: &mut [T],
            channels: usize,
            buffer: &Mutex<VecDeque<f32>>,
            level_bits: &AtomicU32,
            gain_bits: &AtomicU32,
            convert: impl Fn(f32) -> T,
        ) {
            let gain = f32::from_bits(gain_bits.load(Ordering::Relaxed));
            let mut q = buffer.lock();
            let mut sum_sq = 0f32;
            let mut n = 0usize;
            for frame in data.chunks_mut(channels.max(1)) {
                let s = q.pop_front().unwrap_or(0.0) * gain;
                sum_sq += s * s;
                n += 1;
                for slot in frame.iter_mut() {
                    *slot = convert(s);
                }
            }
            drop(q);
            if n > 0 {
                let rms = (sum_sq / n as f32).sqrt();
                level_bits.store((rms * 3.0).clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
            }
        }

        let stream = match format {
            SampleFormat::F32 => device.build_output_stream(
                &stream_config,
                move |data: &mut [f32], _| {
                    fill(data, channels, &buffer, &level_bits, &gain_bits, |s| s)
                },
                err_fn,
                None,
            ),
            SampleFormat::I16 => device.build_output_stream(
                &stream_config,
                move |data: &mut [i16], _| {
                    fill(data, channels, &buffer, &level_bits, &gain_bits, |s| {
                        (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                    })
                },
                err_fn,
                None,
            ),
            SampleFormat::U16 => device.build_output_stream(
                &stream_config,
                move |data: &mut [u16], _| {
                    fill(data, channels, &buffer, &level_bits, &gain_bits, |s| {
                        ((s.clamp(-1.0, 1.0) + 1.0) * 0.5 * u16::MAX as f32) as u16
                    })
                },
                err_fn,
                None,
            ),
            other => return Err(format!("Unsupported output sample format: {other:?}")),
        }
        .map_err(|e| format!("Failed to open audio output: {e}"))?;
        stream
            .play()
            .map_err(|e| format!("Failed to start audio output: {e}"))?;
        Ok(Self {
            _stream: stream,
            rate,
            device_name,
        })
    }
}

/// Streaming linear resampler that keeps its phase across chunks (no clicks at boundaries).
struct Resampler {
    /// Source samples advanced per output sample.
    step: f64,
    /// Read position in the virtual signal `[last, chunk...]`.
    pos: f64,
    last: f32,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self {
            step: from as f64 / to.max(1) as f64,
            pos: 1.0,
            last: 0.0,
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        let n = input.len();
        let at = |i: usize| if i == 0 { self.last } else { input[i - 1] };
        while self.pos < n as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let (a, b) = (at(i), at(i + 1));
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.last = input[n - 1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_doubles_rate_without_gaps() {
        let mut r = Resampler::new(24_000, 48_000);
        let mut out = Vec::new();
        let chunk: Vec<f32> = (0..100).map(|i| i as f32).collect();
        r.process(&chunk[..50], &mut out);
        r.process(&chunk[50..], &mut out);
        assert!((out.len() as i64 - 200).abs() <= 2, "len {}", out.len());
        // Monotonic ramp in → monotonic ramp out, also across the chunk boundary.
        assert!(out.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn resampler_identity_rate() {
        let mut r = Resampler::new(48_000, 48_000);
        let mut out = Vec::new();
        r.process(&[0.1, 0.2, 0.3], &mut out);
        // One sample of latency: the chunk's last sample is emitted with the next chunk.
        assert_eq!(out, vec![0.1, 0.2]);
        r.process(&[0.4], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }
}
