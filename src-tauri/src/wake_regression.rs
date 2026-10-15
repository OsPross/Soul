//! Offline wake word harness (no API): detection rate and false positives of the rustpotter
//! detector, through the same `wake::WakeEngine` the app runs.
//!
//!   npm run test:voice -- --wake
//!   cargo test --manifest-path src-tauri/Cargo.toml --lib wake_regression -- --ignored --nocapture
//!
//! `tests/voice/wake/`:
//! - `train/`: 6–8 takes of "Soul" → the model (trimmed exactly like the Settings wizard);
//! - `positive/`: must fire (same speaker) — each also replayed with white noise (15 dB SNR)
//!   and 12 dB quieter;
//! - `positive_other/`: "Soul" by other speakers (reported, not gated: the model is per-speaker);
//! - `negative/`: must not fire; the real recordings in `tests/voice/*.wav` and 60 s of
//!   synthetic noise are added as negatives too;
//! - `negative_hard/`: similar-sounding single words ("sól", "stół", "sold"…): reported
//!   separately, not gated (a template matcher can't fully reject them).
//!
//! `make-samples.ps1` regenerates the synthetic (Windows TTS) set. Report: `tests/voice/wake/report.md`.
//!
//! Gate (default sensitivity): same-speaker clean detection ≥ `SOUL_WAKE_MIN_RATE` (default 0.8)
//! and zero false positives outside `negative_hard/`. `SOUL_WAKE_REPORT_ONLY=1` never fails.
//! `SOUL_WAKE_SENS=0.3,0.5,0.7` picks the sensitivities to sweep.

use crate::dev_samples::voice_test_dir;
use crate::wake::{self, WakeEngine, SAMPLE_RATE};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEFAULT_SENSITIVITY: f32 = crate::config::DEFAULT_WAKE_SENSITIVITY;

struct Clip {
    set: &'static str,
    name: String,
    samples: Vec<f32>,
}

impl Clip {
    fn secs(&self) -> f32 {
        self.samples.len() as f32 / SAMPLE_RATE as f32
    }
}

fn wavs(dir: &Path) -> Vec<PathBuf> {
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

fn load(path: &Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    wake::read_wav_16k(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn name_of(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().to_string()
}

/// xorshift noise, uniform in [-1, 1).
fn noise(len: usize, seed: u32) -> Vec<f32> {
    let mut x = seed.max(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as f32 / u32::MAX as f32 * 2.0 - 1.0
        })
        .collect()
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|v| v * v).sum::<f32>() / s.len().max(1) as f32).sqrt()
}

/// Add white noise at `snr_db` relative to the RMS of the spoken part.
fn with_noise(samples: &[f32], snr_db: f32, seed: u32) -> Vec<f32> {
    let speech = wake::trim_word(samples).map_or(samples, |(s, e)| &samples[s..e]);
    // Uniform noise RMS = 1/sqrt(3).
    let gain = rms(speech) / 10f32.powf(snr_db / 20.0) * 3f32.sqrt();
    samples
        .iter()
        .zip(noise(samples.len(), seed))
        .map(|(s, n)| s + n * gain)
        .collect()
}

fn scaled(samples: &[f32], db: f32) -> Vec<f32> {
    let g = 10f32.powf(db / 20.0);
    samples.iter().map(|s| s * g).collect()
}

/// Train like the Settings wizard: 16 kHz, trimmed to the word, 16-bit WAV.
fn train_model(files: &[PathBuf]) -> Vec<u8> {
    let mut buffers = HashMap::new();
    for f in files {
        let s = load(f);
        let (a, b) = wake::trim_word(&s).unwrap_or((0, s.len()));
        buffers.insert(name_of(f), wake::wav_16k_bytes(&s[a..b]).unwrap());
    }
    wake::build_model(buffers).expect("build wakeword model")
}

struct Outcome {
    hits: usize,
    best: f32,
    first_score: Option<f32>,
}

fn run(engine: &mut WakeEngine, clip: &Clip) -> Outcome {
    engine.reset();
    let (hits, best) = wake::scan(engine, &clip.samples);
    Outcome {
        hits: hits.len(),
        best,
        first_score: hits.first().map(|(_, d)| d.score),
    }
}

struct Sweep {
    sensitivity: f32,
    rate: HashMap<&'static str, (usize, usize)>,
    false_positives: usize,
    fp_files: Vec<String>,
    hard_fp: usize,
    hard_files: usize,
    neg_secs: f32,
    elapsed: Duration,
    audio_secs: f32,
}

fn env_f32(name: &str, default: f32) -> f32 {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

#[test]
#[ignore]
fn wake_regression() {
    let base = voice_test_dir().expect("tests/voice");
    let dir = base.join("wake");
    let train = wavs(&dir.join("train"));
    assert!(
        train.len() >= wake::MIN_SAMPLES,
        "need ≥{} WAVs in {} (run tests/voice/wake/make-samples.ps1)",
        wake::MIN_SAMPLES,
        dir.join("train").display()
    );
    let t_build = Instant::now();
    let model = train_model(&train[..train.len().min(wake::MAX_SAMPLES)]);
    let build_ms = t_build.elapsed().as_millis();

    let mut clips: Vec<Clip> = Vec::new();
    for (i, f) in wavs(&dir.join("positive")).iter().enumerate() {
        let s = load(f);
        clips.push(Clip {
            set: "pos_noisy",
            name: format!("{} +noise15dB", name_of(f)),
            samples: with_noise(&s, 15.0, 7 + i as u32),
        });
        clips.push(Clip {
            set: "pos_quiet",
            name: format!("{} -12dB", name_of(f)),
            samples: scaled(&s, -12.0),
        });
        clips.push(Clip {
            set: "pos_clean",
            name: name_of(f),
            samples: s,
        });
    }
    for f in wavs(&dir.join("positive_other")) {
        clips.push(Clip {
            set: "pos_other",
            name: name_of(&f),
            samples: load(&f),
        });
    }
    for f in wavs(&dir.join("negative")) {
        clips.push(Clip {
            set: "neg_tts",
            name: name_of(&f),
            samples: load(&f),
        });
    }
    for f in wavs(&dir.join("negative_hard")) {
        clips.push(Clip {
            set: "neg_hard",
            name: name_of(&f),
            samples: load(&f),
        });
    }
    for f in wavs(&base) {
        clips.push(Clip {
            set: "neg_real",
            name: format!("../{}", name_of(&f)),
            samples: load(&f),
        });
    }
    clips.push(Clip {
        set: "neg_noise",
        name: "white noise 60 s (-30 dBFS)".into(),
        samples: noise(SAMPLE_RATE * 60, 99).iter().map(|s| s * 0.055).collect(),
    });
    clips.sort_by_key(|c| c.set);

    let sensitivities: Vec<f32> = std::env::var("SOUL_WAKE_SENS")
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .filter(|v: &Vec<f32>| !v.is_empty())
        .unwrap_or_else(|| vec![0.3, DEFAULT_SENSITIVITY, 0.7]);

    let mut sweeps = Vec::new();
    let mut detail: Vec<(String, &'static str, Outcome, f32)> = Vec::new();
    for &sens in &sensitivities {
        let mut engine = WakeEngine::new(&model, sens, false).expect("engine");
        let mut sw = Sweep {
            sensitivity: sens,
            rate: HashMap::new(),
            false_positives: 0,
            fp_files: Vec::new(),
            hard_fp: 0,
            hard_files: 0,
            neg_secs: 0.0,
            elapsed: Duration::ZERO,
            audio_secs: 0.0,
        };
        for clip in &clips {
            let t0 = Instant::now();
            let o = run(&mut engine, clip);
            sw.elapsed += t0.elapsed();
            sw.audio_secs += clip.secs() + 2.0; // scan pads 1 s each side
            if clip.set.starts_with("pos") {
                let e = sw.rate.entry(clip.set).or_default();
                e.0 += usize::from(o.hits > 0);
                e.1 += 1;
            } else if clip.set == "neg_hard" {
                sw.hard_files += 1;
                sw.hard_fp += usize::from(o.hits > 0);
            } else {
                sw.neg_secs += clip.secs();
                if o.hits > 0 {
                    sw.false_positives += o.hits;
                    sw.fp_files.push(format!("{} ({}×)", clip.name, o.hits));
                }
            }
        }
        sweeps.push(sw);
    }

    // Per-file view at the default sensitivity, with the probe's best score.
    let mut probe = WakeEngine::new(&model, DEFAULT_SENSITIVITY, true).expect("engine");
    for clip in &clips {
        let o = run(&mut probe, clip);
        detail.push((clip.name.clone(), clip.set, o, clip.secs()));
    }

    let report = render(&sweeps, &detail, train.len(), build_ms);
    println!("{report}");
    let out = dir.join("report.md");
    std::fs::write(&out, &report).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
    println!("report → {}", out.display());

    if std::env::var("SOUL_WAKE_REPORT_ONLY").is_ok_and(|v| v == "1") {
        return;
    }
    let default = sweeps
        .iter()
        .find(|s| (s.sensitivity - DEFAULT_SENSITIVITY).abs() < 1e-3)
        .unwrap_or(&sweeps[0]);
    let (ok, n) = default.rate.get("pos_clean").copied().unwrap_or((0, 0));
    let min_rate = env_f32("SOUL_WAKE_MIN_RATE", 0.8);
    assert!(n > 0, "no positives in tests/voice/wake/positive");
    assert!(
        ok as f32 / n as f32 >= min_rate,
        "same-speaker detection {ok}/{n} < {min_rate}"
    );
    assert_eq!(
        default.false_positives, 0,
        "false positives at default sensitivity: {:?}",
        default.fp_files
    );
}

fn pct(v: (usize, usize)) -> String {
    if v.1 == 0 {
        "–".into()
    } else {
        format!("{}/{} ({:.0}%)", v.0, v.1, v.0 as f32 * 100.0 / v.1 as f32)
    }
}

fn render(
    sweeps: &[Sweep],
    detail: &[(String, &'static str, Outcome, f32)],
    train: usize,
    build_ms: u128,
) -> String {
    let mut s = String::new();
    s.push_str("# Wake word report\n\n");
    s.push_str(&format!(
        "rustpotter 3.0.2 · model from {train} takes (`train/`, built in {build_ms} ms) · {} · offline, no API\n\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    ));
    s.push_str("| sensitivity | threshold / hits | same speaker (clean) | +noise 15 dB | −12 dB | other speakers | false positives | negative audio | FP / hour | similar words fired | CPU (1 core) |\n");
    s.push_str("|---|---|---|---|---|---|---|---|---|---|---|\n");
    for sw in sweeps {
        let g = |k: &str| sw.rate.get(k).copied().unwrap_or((0, 0));
        let fph = sw.false_positives as f32 / (sw.neg_secs / 3600.0).max(1e-6);
        let rtf = sw.elapsed.as_secs_f32() / sw.audio_secs.max(1e-6);
        s.push_str(&format!(
            "| {:.2} | {:.3} / {} | {} | {} | {} | {} | {} | {:.1} min | {:.1} | {} | {:.2}% |\n",
            sw.sensitivity,
            wake::thresholds(sw.sensitivity).threshold,
            wake::thresholds(sw.sensitivity).min_scores,
            pct(g("pos_clean")),
            pct(g("pos_noisy")),
            pct(g("pos_quiet")),
            pct(g("pos_other")),
            sw.false_positives,
            sw.neg_secs / 60.0,
            fph,
            pct((sw.hard_fp, sw.hard_files)),
            rtf * 100.0,
        ));
    }
    for sw in sweeps.iter().filter(|s| !s.fp_files.is_empty()) {
        s.push_str(&format!(
            "\nFalse positives at {:.2}: {}\n",
            sw.sensitivity,
            sw.fp_files.join(", ")
        ));
    }
    s.push_str(&format!(
        "\nCPU = detector time / audio time on one core of this machine ({} logical cores; divide by \
the core count for whole-machine %). Plain `cargo test` builds are unoptimized; see \
tests/voice/README.md for an optimized run.\n",
        std::thread::available_parallelism().map_or(1, |n| n.get())
    ));
    s.push_str(&format!(
        "\n## Per file (sensitivity {DEFAULT_SENSITIVITY:.2}, threshold {:.2})\n\n",
        wake::thresholds(DEFAULT_SENSITIVITY).threshold
    ));
    s.push_str("| set | file | sec | fired | score | best probe score |\n|---|---|---|---|---|---|\n");
    for (name, set, o, secs) in detail {
        let expect_hit = set.starts_with("pos");
        let mark = match (expect_hit, o.hits > 0) {
            (true, true) | (false, false) => "",
            (true, false) => " ✗ miss",
            (false, true) if *set == "neg_hard" => " (similar word)",
            (false, true) => " ✗ FP",
        };
        s.push_str(&format!(
            "| {set} | {name} | {secs:.1} | {}{mark} | {} | {:.3} |\n",
            o.hits,
            o.first_score.map_or("–".into(), |v| format!("{v:.3}")),
            o.best
        ));
    }
    s
}

/// Parameter sweep for tuning `wake::thresholds` (not part of the gate). Prints, per detector
/// setting, same-speaker recall (clean + noisy) and false positives for each `min_scores`.
///   cargo test --lib wake_regression::wake_tune -- --ignored --nocapture
#[test]
#[ignore]
fn wake_tune() {
    use rustpotter::{DetectorConfig, ScoreMode};
    let base = voice_test_dir().expect("tests/voice");
    let dir = base.join("wake");
    let model = train_model(&wavs(&dir.join("train")));
    let mut pos = Vec::new();
    for (i, f) in wavs(&dir.join("positive")).iter().enumerate() {
        let s = load(f);
        pos.push(with_noise(&s, 15.0, 7 + i as u32));
        pos.push(s);
    }
    let other: Vec<Vec<f32>> = wavs(&dir.join("positive_other")).iter().map(|f| load(f)).collect();
    let mut neg: Vec<Vec<f32>> = wavs(&dir.join("negative")).iter().map(|f| load(f)).collect();
    neg.extend(wavs(&base).iter().map(|f| load(f)));
    let modes = [("max", ScoreMode::Max), ("avg", ScoreMode::Average), ("p50", ScoreMode::Median), ("p25", ScoreMode::P25)];
    let mins = [1usize, 3, 5, 8, 12, 16];
    for (mname, mode) in modes {
        for band in [3u16, 5, 8] {
            for t in [0.55f32, 0.58, 0.61, 0.64, 0.67] {
                let cfg = || DetectorConfig {
                    threshold: t,
                    avg_threshold: 0.0,
                    min_scores: 1,
                    score_mode: mode,
                    band_size: band,
                    vad_mode: None,
                    ..DetectorConfig::default()
                };
                let mut e = WakeEngine::with_config(&model, cfg()).unwrap();
                // Max detection counter per clip.
                let mut counters = |clips: &[Vec<f32>]| -> Vec<Vec<usize>> {
                    clips
                        .iter()
                        .map(|c| {
                            e.reset();
                            wake::scan(&mut e, c).0.iter().map(|(_, d)| d.hits).collect()
                        })
                        .collect()
                };
                let p = counters(&pos);
                let o = counters(&other);
                let n = counters(&neg);
                let mut line = format!("{mname} band={band} t={t:.2} |");
                for m in mins {
                    let rec = p.iter().filter(|h| h.iter().any(|&c| c >= m)).count();
                    let oth = o.iter().filter(|h| h.iter().any(|&c| c >= m)).count();
                    let fp: usize = n.iter().map(|h| h.iter().filter(|&&c| c >= m).count()).sum();
                    line.push_str(&format!(" m{m}: {rec}/{} o{oth} fp{fp} |", p.len()));
                }
                println!("{line}");
            }
        }
    }
}
