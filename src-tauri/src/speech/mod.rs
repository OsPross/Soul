//! Offline speech: NVIDIA Parakeet TDT 0.6B v3 (sherpa-onnx) + pyannote speaker diarization.
//! Models are downloaded on demand into `<app data>/models/<id>/`, SHA-256 verified.

pub mod audio;

use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use serde::Serialize;
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSpeakerDiarization,
    OfflineSpeakerDiarizationConfig,
};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

pub struct ModelFile {
    pub name: &'static str,
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

pub struct ModelSet {
    pub id: &'static str,
    pub files: &'static [ModelFile],
}

macro_rules! parakeet_url {
    ($f:literal) => {
        concat!(
            "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/2bda32ec70b097a55adaa07d9a7173915b43cc78/",
            $f
        )
    };
}

pub const PARAKEET: ModelSet = ModelSet {
    id: "parakeet-tdt-0.6b-v3-int8",
    files: &[
        ModelFile {
            name: "encoder.int8.onnx",
            url: parakeet_url!("encoder.int8.onnx"),
            size: 652_184_281,
            sha256: "acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247",
        },
        ModelFile {
            name: "decoder.int8.onnx",
            url: parakeet_url!("decoder.int8.onnx"),
            size: 11_845_275,
            sha256: "179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e",
        },
        ModelFile {
            name: "joiner.int8.onnx",
            url: parakeet_url!("joiner.int8.onnx"),
            size: 6_355_277,
            sha256: "3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3",
        },
        ModelFile {
            name: "tokens.txt",
            url: parakeet_url!("tokens.txt"),
            size: 93_939,
            sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
        },
    ],
};

pub const DIARIZER: ModelSet = ModelSet {
    id: "diarizer",
    files: &[
        ModelFile {
            name: "segmentation.int8.onnx",
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/9403a6902bb58e3d5ae8c7e77c3422de279db2e0/model.int8.onnx",
            size: 1_540_506,
            sha256: "d582f4b4c6b48205de7e0643c57df0df5615a3c176189be3fc461e9d18827b5d",
        },
        ModelFile {
            name: "embedding.onnx",
            url: "https://huggingface.co/csukuangfj/speaker-embedding-models/resolve/0743f301363dec56491a490f6d6cbc9d67f9a3bf/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx",
            size: 28_281_164,
            sha256: "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2",
        },
    ],
};

const SETS: [&ModelSet; 2] = [&PARAKEET, &DIARIZER];

#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// One diarized speaker turn (`speaker` = cluster index as text).
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone)]
pub struct Transcript {
    pub text: String,
    pub words: Vec<TimedWord>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStatus {
    pub installed: bool,
    pub downloading: bool,
    pub downloaded: u64,
    pub total: u64,
    pub error: Option<String>,
}

static ROOT: OnceCell<PathBuf> = OnceCell::new();
static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static PROGRESS: Mutex<(u64, Option<String>)> = Mutex::new((0, None));
static RECOGNIZER: Mutex<Option<Arc<OfflineRecognizer>>> = Mutex::new(None);
static DIARIZATION: Mutex<Option<Arc<OfflineSpeakerDiarization>>> = Mutex::new(None);

pub fn init(app: &AppHandle) {
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = ROOT.set(dir.join("models"));
    }
    // Warm the recognizer when it is the preferred dictation engine (first load takes seconds).
    let prefer = crate::config::load_settings(app).map(|s| s.prefer_offline_stt).unwrap_or(false);
    if prefer && is_ready() {
        std::thread::spawn(|| {
            let _ = recognizer();
        });
    }
}

fn set_dir(set: &ModelSet) -> Option<PathBuf> {
    ROOT.get().map(|r| r.join(set.id))
}

fn total_size() -> u64 {
    SETS.iter().flat_map(|s| s.files).map(|f| f.size).sum()
}

fn installed(set: &ModelSet, file: &ModelFile) -> bool {
    set_dir(set)
        .and_then(|d| std::fs::metadata(d.join(file.name)).ok())
        .is_some_and(|m| m.len() == file.size)
}

/// Every model file is on disk (size-checked; the hash was verified before the rename).
pub fn is_ready() -> bool {
    SETS.iter().all(|s| s.files.iter().all(|f| installed(s, f)))
}

pub fn status() -> ModelsStatus {
    let (downloaded, error) = PROGRESS.lock().clone();
    ModelsStatus {
        installed: is_ready(),
        downloading: DOWNLOADING.load(Ordering::SeqCst),
        downloaded,
        total: total_size(),
        error,
    }
}

fn emit_status(app: &AppHandle) {
    let _ = app.emit("soul://speech-models", status());
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn download_file(
    app: &AppHandle,
    client: &reqwest::blocking::Client,
    dir: &Path,
    file: &ModelFile,
    done_before: u64,
) -> Result<(), String> {
    let part = dir.join(format!("{}.part", file.name));
    let mut resp = client
        .get(file.url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("{}: {}", file.name, e.without_url()))?;
    let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    let mut last = Instant::now();
    loop {
        let n = resp.read(&mut buf).map_err(|e| format!("{}: {e}", file.name))?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got += n as u64;
        if last.elapsed() >= Duration::from_millis(250) {
            last = Instant::now();
            PROGRESS.lock().0 = done_before + got;
            emit_status(app);
        }
    }
    out.sync_all().map_err(|e| e.to_string())?;
    drop(out);
    let digest = hex(ctx.finish().as_ref());
    if got != file.size || digest != file.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{}: checksum mismatch", file.name));
    }
    std::fs::rename(&part, dir.join(file.name)).map_err(|e| e.to_string())
}

fn download_all(app: &AppHandle) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(None)
        .build()
        .map_err(|e| e.to_string())?;
    let mut done = 0u64;
    for set in SETS {
        let dir = set_dir(set).ok_or("App data dir unavailable.")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for file in set.files {
            if !installed(set, file) {
                eprintln!("[soul][speech] downloading {}/{}", set.id, file.name);
                download_file(app, &client, &dir, file, done)?;
            }
            done += file.size;
            PROGRESS.lock().0 = done;
        }
    }
    Ok(())
}

/// Start the background download of every missing model (no-op while one runs).
pub fn download(app: &AppHandle) {
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return;
    }
    *PROGRESS.lock() = (0, None);
    emit_status(app);
    let app = app.clone();
    std::thread::spawn(move || {
        let result = download_all(&app);
        DOWNLOADING.store(false, Ordering::SeqCst);
        if let Err(e) = &result {
            eprintln!("[soul][speech] download failed: {e}");
            PROGRESS.lock().1 = Some(e.clone());
        } else {
            eprintln!("[soul][speech] models ready");
        }
        emit_status(&app);
    });
}

pub fn remove(app: &AppHandle) -> Result<(), String> {
    if DOWNLOADING.load(Ordering::SeqCst) {
        return Err("Download in progress.".into());
    }
    *RECOGNIZER.lock() = None;
    *DIARIZATION.lock() = None;
    for set in SETS {
        if let Some(dir) = set_dir(set) {
            if dir.exists() {
                std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
            }
        }
    }
    *PROGRESS.lock() = (0, None);
    emit_status(app);
    Ok(())
}

fn path_of(set: &ModelSet, name: &str) -> Option<String> {
    set_dir(set).map(|d| d.join(name).to_string_lossy().into_owned())
}

fn threads() -> i32 {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 4) as i32
}

/// Lazily loaded Parakeet recognizer; the lock makes concurrent callers wait for one load.
pub fn recognizer() -> Option<Arc<OfflineRecognizer>> {
    let mut slot = RECOGNIZER.lock();
    if slot.is_none() && is_ready() {
        let t = Instant::now();
        let mut config = OfflineRecognizerConfig::default();
        let m = &mut config.model_config;
        m.transducer.encoder = path_of(&PARAKEET, "encoder.int8.onnx");
        m.transducer.decoder = path_of(&PARAKEET, "decoder.int8.onnx");
        m.transducer.joiner = path_of(&PARAKEET, "joiner.int8.onnx");
        m.tokens = path_of(&PARAKEET, "tokens.txt");
        m.model_type = Some("nemo_transducer".into());
        m.num_threads = threads();
        *slot = OfflineRecognizer::create(&config).map(Arc::new);
        eprintln!(
            "[soul][speech] parakeet load ok={} ms={}",
            slot.is_some(),
            t.elapsed().as_millis()
        );
    }
    slot.clone()
}

fn diarization() -> Option<Arc<OfflineSpeakerDiarization>> {
    let mut slot = DIARIZATION.lock();
    if slot.is_none() && is_ready() {
        let mut config = OfflineSpeakerDiarizationConfig::default();
        config.segmentation.pyannote.model = path_of(&DIARIZER, "segmentation.int8.onnx");
        config.segmentation.num_threads = threads();
        config.embedding.model = path_of(&DIARIZER, "embedding.onnx");
        config.embedding.num_threads = threads();
        config.clustering.num_clusters = -1;
        config.clustering.threshold = 0.7;
        *slot = OfflineSpeakerDiarization::create(&config).map(Arc::new);
        eprintln!("[soul][speech] diarizer load ok={}", slot.is_some());
    }
    slot.clone()
}

/// Glue subword tokens (a leading space or '▁' starts a word) into timed words.
pub fn words_from_tokens(
    tokens: &[String],
    starts: &[f32],
    durations: Option<&[f32]>,
    offset: f64,
) -> Vec<TimedWord> {
    let mut words: Vec<TimedWord> = Vec::new();
    for (i, tok) in tokens.iter().enumerate() {
        let start = starts.get(i).copied().unwrap_or(0.0) as f64 + offset;
        let end = match durations.and_then(|d| d.get(i)) {
            Some(d) if *d > 0.0 => start + *d as f64,
            _ => starts.get(i + 1).map(|s| *s as f64 + offset).unwrap_or(start + 0.08),
        };
        let new_word = tok.starts_with(' ') || tok.starts_with('▁');
        let piece = tok.trim_start_matches([' ', '▁']);
        if piece.is_empty() {
            continue;
        }
        match words.last_mut() {
            Some(w) if !new_word => {
                w.text.push_str(piece);
                w.end = end;
            }
            _ => words.push(TimedWord { text: piece.to_string(), start, end }),
        }
    }
    words
}

/// Offline transcript of 16 kHz mono samples (None = models missing / failed to load).
pub fn transcribe(samples: &[f32]) -> Option<Transcript> {
    let recognizer = recognizer()?;
    let mut texts = Vec::new();
    let mut words = Vec::new();
    let mut offset = 0usize;
    for chunk in audio::split_for_model(samples, audio::SAMPLE_RATE, 28.0) {
        let stream = recognizer.create_stream();
        stream.accept_waveform(audio::SAMPLE_RATE as i32, chunk);
        recognizer.decode(&stream);
        if let Some(r) = stream.get_result() {
            let secs = offset as f64 / audio::SAMPLE_RATE as f64;
            let text = r.text.trim();
            if !text.is_empty() {
                texts.push(text.to_string());
            }
            if let Some(ts) = &r.timestamps {
                words.extend(words_from_tokens(&r.tokens, ts, r.durations.as_deref(), secs));
            }
        }
        offset += chunk.len();
    }
    Some(Transcript { text: texts.join(" "), words })
}

/// Speaker turns of 16 kHz mono samples, sorted by start (empty when unavailable).
pub fn diarize(samples: &[f32]) -> Vec<Turn> {
    let Some(d) = diarization() else {
        return Vec::new();
    };
    let Some(result) = d.process(samples) else {
        return Vec::new();
    };
    result
        .sort_by_start_time()
        .into_iter()
        .map(|s| Turn { speaker: s.speaker.to_string(), start: s.start as f64, end: s.end as f64 })
        .collect()
}

/// 16 kHz mono f32 samples of a recorder WAV (any bit depth hound reads).
pub fn wav_samples(wav: &[u8]) -> Option<Vec<f32>> {
    let mut r = hound::WavReader::new(std::io::Cursor::new(wav)).ok()?;
    let spec = r.spec();
    let ch = spec.channels.max(1) as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().filter_map(Result::ok).collect(),
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().filter_map(Result::ok).map(|s| s as f32 / scale).collect()
        }
    };
    let mono: Vec<f32> = raw.chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect();
    let mut out = Vec::new();
    audio::Resampler::new(spec.sample_rate).process(&mono, &mut out);
    Some(out)
}

#[tauri::command]
pub fn speech_models_status() -> ModelsStatus {
    status()
}

#[tauri::command]
pub fn speech_models_download(app: AppHandle) {
    download(&app);
}

#[tauri::command]
pub fn speech_models_remove(app: AppHandle) -> Result<(), String> {
    remove(&app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_glue_into_timed_words() {
        let t: Vec<String> = [" Cze", "ść", "▁Ka", "c", "per", "."].iter().map(|s| s.to_string()).collect();
        let w = words_from_tokens(&t, &[0.0, 0.2, 0.5, 0.6, 0.7, 0.9], None, 10.0);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].text, "Cześć");
        assert_eq!(w[1].text, "Kacper.");
        assert!((w[0].start - 10.0).abs() < 1e-6 && (w[0].end - 10.5).abs() < 1e-6);
        assert!((w[1].end - 10.98).abs() < 1e-6);
        let d = words_from_tokens(&t[..2], &[0.0, 0.2], Some(&[0.1, 0.3]), 0.0);
        assert!((d[0].end - 0.5).abs() < 1e-6);
    }

    #[test]
    fn model_sets_are_pinned() {
        for set in SETS {
            for f in set.files {
                assert!(f.size > 0);
                assert_eq!(f.sha256.len(), 64);
                assert!(f.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
                assert!(f.url.starts_with("https://huggingface.co/"));
            }
        }
        assert_eq!(PARAKEET.files.len(), 4);
        assert_eq!(DIARIZER.files.len(), 2);
        let mb = total_size() / 1_000_000;
        assert!((690..=705).contains(&mb), "{mb} MB");
    }
}
