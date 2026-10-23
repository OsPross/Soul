//! Call audio: mic or system loopback → 16 kHz mono f32, long-audio splitting, streaming WAV.
//! Separate from `crate::audio` (PTT recorder) so the dictation path stays untouched.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

pub const SAMPLE_RATE: u32 = 16_000;
const HALF_TAPS: isize = 16;

/// 32-tap Blackman-windowed sinc resampler (any rate → 16 kHz), streaming.
pub struct Resampler {
    step: f64,
    pos: f64,
    buf: Vec<f32>,
    cutoff: f64,
}

impl Resampler {
    pub fn new(input_rate: u32) -> Self {
        let step = input_rate as f64 / SAMPLE_RATE as f64;
        Self {
            step,
            pos: HALF_TAPS as f64,
            buf: vec![0.0; HALF_TAPS as usize],
            cutoff: (1.0 / step).min(1.0) * 0.92,
        }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if (self.step - 1.0).abs() < 1e-9 {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        let fc = self.cutoff;
        while self.pos + (HALF_TAPS as f64) < self.buf.len() as f64 {
            let centre = self.pos.floor() as isize;
            let frac = self.pos - centre as f64;
            let mut acc = 0.0f64;
            let mut norm = 0.0f64;
            for k in (-HALF_TAPS + 1)..=HALF_TAPS {
                let idx = centre + k;
                if idx < 0 || idx as usize >= self.buf.len() {
                    continue;
                }
                let x = k as f64 - frac;
                let sinc = if x.abs() < 1e-9 {
                    1.0
                } else {
                    (std::f64::consts::PI * x * fc).sin() / (std::f64::consts::PI * x * fc)
                };
                let n = (x + HALF_TAPS as f64) / (2.0 * HALF_TAPS as f64);
                let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * n).cos()
                    + 0.08 * (4.0 * std::f64::consts::PI * n).cos();
                let c = sinc * w;
                acc += self.buf[idx as usize] as f64 * c;
                norm += c;
            }
            out.push(if norm.abs() > 1e-9 { (acc / norm) as f32 } else { 0.0 });
            self.pos += self.step;
        }
        let keep_from = (self.pos.floor() as isize - HALF_TAPS).max(0) as usize;
        if keep_from > 0 {
            self.buf.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
    }
}

#[derive(Clone, Debug)]
pub enum Source {
    /// Preferred mic name (None / missing = default input).
    Mic(Option<String>),
    /// Everything the computer plays (WASAPI loopback / PulseAudio monitor).
    System,
}

pub type Sink = Box<dyn FnMut(&[f32]) + Send>;

/// A running capture on its own thread (`cpal::Stream` is `!Send` on Windows).
pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn pick_device(source: &Source) -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    let host = cpal::default_host();
    match source {
        Source::Mic(name) => {
            let wanted = name.as_deref().filter(|n| !n.trim().is_empty());
            let device = wanted
                .and_then(|w| {
                    host.input_devices()
                        .ok()?
                        .find(|d| d.name().map(|n| n == w).unwrap_or(false))
                })
                .or_else(|| host.default_input_device())
                .ok_or("No microphone found.")?;
            let config = device.default_input_config().map_err(|e| e.to_string())?;
            Ok((device, config))
        }
        #[cfg(windows)]
        Source::System => {
            // cpal's WASAPI backend opens an output device's input stream in loopback mode.
            let device = host.default_output_device().ok_or("No output device for loopback.")?;
            let config = device.default_output_config().map_err(|e| e.to_string())?;
            Ok((device, config))
        }
        #[cfg(not(windows))]
        Source::System => {
            let device = host
                .input_devices()
                .map_err(|e| e.to_string())?
                .find(|d| {
                    d.name()
                        .map(|n| n.starts_with("Monitor of") || n.ends_with(".monitor"))
                        .unwrap_or(false)
                })
                .ok_or("No system audio monitor device.")?;
            let config = device.default_input_config().map_err(|e| e.to_string())?;
            Ok((device, config))
        }
    }
}

impl Capture {
    /// Start capturing `source`; `sink` gets 16 kHz mono chunks on the audio callback.
    pub fn start(source: Source, mut sink: Sink) -> Result<Capture, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let stop_t = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("soul-call-audio".into())
            .spawn(move || {
                let opened = (|| {
                    let (device, config) = pick_device(&source)?;
                    let channels = config.channels().max(1) as usize;
                    let mut resampler = Resampler::new(config.sample_rate().0);
                    let mut mono = Vec::new();
                    let mut out = Vec::new();
                    let mut feed = move |frames: &mut dyn Iterator<Item = f32>| {
                        mono.clear();
                        let mut acc = 0.0f32;
                        for (i, s) in frames.enumerate() {
                            acc += s;
                            if i % channels == channels - 1 {
                                mono.push(acc / channels as f32);
                                acc = 0.0;
                            }
                        }
                        out.clear();
                        resampler.process(&mono, &mut out);
                        if !out.is_empty() {
                            sink(&out);
                        }
                    };
                    let err = |e| eprintln!("[soul][audio] call stream error: {e}");
                    let cfg: cpal::StreamConfig = config.clone().into();
                    let stream = match config.sample_format() {
                        cpal::SampleFormat::F32 => device.build_input_stream(
                            &cfg,
                            move |d: &[f32], _| feed(&mut d.iter().copied()),
                            err,
                            None,
                        ),
                        cpal::SampleFormat::I16 => device.build_input_stream(
                            &cfg,
                            move |d: &[i16], _| feed(&mut d.iter().map(|&s| s as f32 / 32768.0)),
                            err,
                            None,
                        ),
                        cpal::SampleFormat::U16 => device.build_input_stream(
                            &cfg,
                            move |d: &[u16], _| {
                                feed(&mut d.iter().map(|&s| (s as f32 - 32768.0) / 32768.0))
                            },
                            err,
                            None,
                        ),
                        other => return Err(format!("Unsupported sample format {other:?}")),
                    }
                    .map_err(|e| e.to_string())?;
                    stream.play().map_err(|e| e.to_string())?;
                    eprintln!(
                        "[soul][audio] call capture {source:?} on {} @ {} Hz",
                        device.name().unwrap_or_default(),
                        config.sample_rate().0
                    );
                    Ok(stream)
                })();
                match opened {
                    Ok(stream) => {
                        let _ = ready_tx.send(Ok(()));
                        while !stop_t.load(Ordering::SeqCst) {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Capture { stop, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("Call audio thread exited.".into()),
        }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Chunks of at most `max_seconds`, cut at the quietest 100 ms of each window's last 8 s.
pub fn split_for_model(samples: &[f32], rate: u32, max_seconds: f32) -> Vec<&[f32]> {
    let max = (rate as f32 * max_seconds) as usize;
    let search = (rate as usize * 8).min(max / 2);
    let slice = (rate as usize / 10).max(1);
    let mut out = Vec::new();
    let mut start = 0;
    while samples.len() - start > max {
        let window_end = start + max;
        let mut best = window_end;
        let mut best_energy = f32::MAX;
        let mut s = window_end - search;
        while s + slice <= window_end {
            let e: f32 = samples[s..s + slice].iter().map(|x| x * x).sum();
            if e < best_energy {
                best_energy = e;
                best = s + slice / 2;
            }
            s += slice / 2;
        }
        out.push(&samples[start..best]);
        start = best;
    }
    if start < samples.len() {
        out.push(&samples[start..]);
    }
    out
}

/// 16 kHz mono 16-bit WAV written as samples arrive; the header is fixed up on `finish`.
pub struct WavWriter {
    file: BufWriter<File>,
    samples: u32,
}

fn header(samples: u32) -> [u8; 44] {
    let data = samples * 2;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&1u16.to_le_bytes());
    h[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

impl WavWriter {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        file.write_all(&header(0))?;
        Ok(Self { file, samples: 0 })
    }

    pub fn write(&mut self, samples: &[f32]) -> std::io::Result<()> {
        for s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            self.file.write_all(&v.to_le_bytes())?;
        }
        self.samples += samples.len() as u32;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<()> {
        self.file.flush()?;
        let mut f = self.file.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&header(self.samples))?;
        f.sync_all()
    }
}

/// Samples of a 16 kHz mono 16-bit WAV, read from byte 44 to EOF (recovers a crashed call).
pub fn read_wav(path: &Path) -> std::io::Result<Vec<f32>> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    Ok(bytes
        .get(44..)
        .unwrap_or_default()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c) as f32 / 32768.0)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn resampler_keeps_length_and_tone() {
        let input = tone(48_000, 440.0, 1.0);
        let mut r = Resampler::new(48_000);
        let mut out = Vec::new();
        for c in input.chunks(480) {
            r.process(c, &mut out);
        }
        assert!((out.len() as i64 - 16_000).abs() < 40, "len {}", out.len());
        // Zero crossings ≈ 2 × 440 per second.
        let mid = &out[1000..15000];
        let crossings = mid.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        let expected = 2.0 * 440.0 * mid.len() as f64 / 16_000.0;
        assert!((crossings as f64 - expected).abs() < 6.0, "{crossings} vs {expected}");
        let peak = mid.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!((peak - 0.5).abs() < 0.05, "peak {peak}");
    }

    #[test]
    fn resampler_passthrough_at_16k() {
        let mut r = Resampler::new(16_000);
        let mut out = Vec::new();
        r.process(&[0.1, 0.2], &mut out);
        assert_eq!(out, vec![0.1, 0.2]);
    }

    #[test]
    fn split_cuts_in_quietest_window() {
        let rate = 1000;
        let mut s = vec![0.5f32; 40 * rate as usize];
        // Silence at 25.0–25.2 s, inside the last 8 s of the first 28 s window.
        for x in &mut s[25_000..25_200] {
            *x = 0.0;
        }
        let parts = split_for_model(&s, rate, 28.0);
        assert_eq!(parts.len(), 2);
        let cut = parts[0].len();
        assert!((25_000..=25_200).contains(&cut), "cut {cut}");
        assert_eq!(parts.iter().map(|p| p.len()).sum::<usize>(), s.len());
        assert_eq!(split_for_model(&s[..1000], rate, 28.0).len(), 1);
    }

    #[test]
    fn wav_round_trip_and_crash_recovery() {
        let dir = std::env::temp_dir().join(format!("soul-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        let mut w = WavWriter::create(&path).unwrap();
        w.write(&[0.0, 0.5, -0.5]).unwrap();
        w.finish().unwrap();
        let back = read_wav(&path).unwrap();
        assert_eq!(back.len(), 3);
        assert!((back[1] - 0.5).abs() < 1e-3 && (back[2] + 0.5).abs() < 1e-3);
        let r = hound::WavReader::open(&path).unwrap();
        assert_eq!(r.spec().sample_rate, 16_000);
        assert_eq!(r.len(), 3);
        // Unfinished writer: header still says 0, samples still recovered.
        let crashed = dir.join("b.wav");
        let mut w = WavWriter::create(&crashed).unwrap();
        w.write(&[0.25; 100]).unwrap();
        w.file.flush().unwrap();
        drop(w);
        assert_eq!(read_wav(&crashed).unwrap().len(), 100);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
