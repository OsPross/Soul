//! Voice end-to-end suite: synthetic Polish speech → the real Soul dev app → Vio, checked on the result.
//!
//!   npm run test:e2e:synth   → TTS samples + noise / quiet / padded variants → tests/e2e/voice/generated/
//!   npm run test:e2e         → every sample through Soul → tests/e2e/voice/report.md
//!
//! The runner starts `target/debug/soul.exe` with `SOUL_E2E=1` (`e2e.rs`) and plays each WAV through
//! `<app data>/dev-utterance.json`: the dev build feeds it into the recorder like a Smart talk-key hold,
//! so capture, Live transcription (REST + silence trim on failure), Smart routing, the pre-router, the
//! Live/REST tools, the computer agent and Vio are exactly what the user gets. Risky tools run in
//! dry-run (`dry_run_tools` in the manifest): only the intended call is checked. Before every case the
//! suite's Vio tabs are closed and the case's pages (`tests/e2e/pages/`, served on 127.0.0.1) opened;
//! afterwards the page is read back (listTabs / getElements / getPage). Soul's settings and memory DB
//! are backed up before the run and restored after it. Optional real recordings: `real/<id>[__x].wav`.
//!
//! Env: SOUL_TEST_KEY (synth; the app uses its saved key), SOUL_E2E_FILTER (case id substring),
//! SOUL_E2E_VARIANTS=0 (base samples only), SOUL_E2E_DELAY_MS (pause between cases, default 5000),
//! SOUL_SYNTH_FORCE=1 (regenerate TTS).

use crate::agent::computer::vio::{self, Client, State};
use crate::voice_regression::{fold_text, pcm16k_to_wav, value_matches};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DERIVED: &[&str] = &["noise", "quiet", "pad"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn suite_dir() -> PathBuf {
    root().join("tests/e2e/voice")
}

fn manifest() -> Value {
    let raw = std::fs::read_to_string(suite_dir().join("manifest.json")).expect("tests/e2e/voice/manifest.json");
    serde_json::from_str(&raw).expect("manifest.json is JSON")
}

fn env_ms(name: &str, default: u64) -> Duration {
    Duration::from_millis(std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default))
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect()
}

// ───────────────────────────── audio variants ─────────────────────────────

fn to_f32(pcm: &[u8]) -> Vec<f32> {
    pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f32 / i16::MAX as f32).collect()
}

fn to_pcm(s: &[f32]) -> Vec<u8> {
    s.iter().flat_map(|v| ((v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes()).collect()
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|v| v * v).sum::<f32>() / s.len().max(1) as f32).sqrt()
}

/// Deterministic white noise in [-1, 1] (xorshift), so variants are reproducible.
fn noise(n: usize, seed: u32) -> Vec<f32> {
    let mut x = seed.max(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as f32 / u32::MAX as f32 * 2.0 - 1.0
        })
        .collect()
}

/// `noise` = white noise at 12 dB SNR, `quiet` = −14 dB, `pad` = 1.5 s before / 2 s after (faint floor).
fn derive(base: &[f32], kind: &str) -> Vec<f32> {
    match kind {
        "noise" => {
            let n = noise(base.len(), 7);
            let gain = rms(base) / 10f32.powf(12.0 / 20.0) / rms(&n).max(1e-9);
            base.iter().zip(n).map(|(s, z)| s + z * gain).collect()
        }
        "quiet" => base.iter().map(|s| s * 10f32.powf(-14.0 / 20.0)).collect(),
        "pad" => {
            let floor = |n: usize, seed| noise(n, seed).into_iter().map(|z| z * 0.002).collect::<Vec<_>>();
            let mut out = floor(24_000, 3);
            out.extend_from_slice(base);
            out.extend(floor(32_000, 5));
            out
        }
        other => panic!("unknown variant {other}"),
    }
}

/// Gemini TTS models, tried in order: each has its own free-tier daily quota (10 requests).
const TTS_MODELS: &[&str] = &[
    "gemini-3.8-flash-lite-tts",
    "gemini-3.8-flash-tts",
    "gemini-3.1-flash-tts-preview",
    "gemini-2.5-flash-preview-tts",
    "gemini-2.5-pro-preview-tts",
];

/// Gemini TTS over the TTS models: a daily-quota 429 moves on to the next model (remembered in
/// `exhausted`), a per-minute one waits 30 s. `None` = no model has quota left today.
fn gemini_tts(text: &str, voice: &str, exhausted: &mut Vec<&'static str>) -> Option<(Vec<u8>, &'static str)> {
    for &model in TTS_MODELS {
        if exhausted.contains(&model) {
            continue;
        }
        for _ in 0..4 {
            match crate::ai::live_smoke::tts_pcm16k_with(model, text, voice) {
                Ok(pcm) => return Some((pcm, model)),
                Err(e) if e.contains("PerDay") || e.contains("limit: 0") || e.contains("\"code\":404") => {
                    println!("[e2e-synth] {model}: no quota left today");
                    exhausted.push(model);
                    break;
                }
                Err(e) if e.contains("429") => {
                    println!("[e2e-synth] {model}: per-minute limit, waiting 30 s");
                    std::thread::sleep(Duration::from_secs(30));
                }
                Err(e) => {
                    println!("[e2e-synth] {model} failed: {e}");
                    exhausted.push(model);
                    break;
                }
            }
        }
    }
    None
}

/// Offline fallback once every Gemini TTS quota is spent: Windows OneCore Polish voices.
#[cfg(windows)]
fn windows_tts(text: &str, female: bool) -> Result<Vec<u8>, String> {
    let out = std::env::temp_dir().join(format!("soul-e2e-tts-{}.wav", std::process::id()));
    let script = r#"$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Runtime.WindowsRuntime
[void][Windows.Media.SpeechSynthesis.SpeechSynthesizer, Windows.Media.SpeechSynthesis, ContentType = WindowsRuntime]
$asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' } | Select-Object -First 1
$s = New-Object Windows.Media.SpeechSynthesis.SpeechSynthesizer
$s.Voice = [Windows.Media.SpeechSynthesis.SpeechSynthesizer]::AllVoices | Where-Object { $_.DisplayName -like '*VOICE*' } | Select-Object -First 1
$t = $asTask.MakeGenericMethod([Windows.Media.SpeechSynthesis.SpeechSynthesisStream]).Invoke($null, @($s.SynthesizeTextToStreamAsync($env:SOUL_TTS_TEXT)))
$t.Wait()
$src = [System.IO.WindowsRuntimeStreamExtensions]::AsStreamForRead($t.Result)
$dst = [System.IO.File]::Create($env:SOUL_TTS_OUT); $src.CopyTo($dst); $dst.Close()"#
        .replace("VOICE", if female { "Paulina" } else { "Adam" });
    let o = command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env("SOUL_TTS_TEXT", text)
        .env("SOUL_TTS_OUT", &out)
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).into_owned());
    }
    let pcm = crate::voice_regression::load_pcm16k(&out);
    let _ = std::fs::remove_file(&out);
    pcm
}

#[cfg(not(windows))]
fn windows_tts(_: &str, _: bool) -> Result<Vec<u8>, String> {
    Err("Gemini TTS quota is spent and there is no offline voice on this OS — retry tomorrow".into())
}

/// One take: Gemini TTS (bare text — a style prompt would be read aloud), else the offline voice;
/// `fast` / `slow` = the same take played 12 % faster / 10 % slower. Returns (16 kHz PCM, source).
pub(crate) fn speak(text: &str, voice: &str, speed: &str, exhausted: &mut Vec<&'static str>) -> (Vec<u8>, String) {
    let (pcm, source) = match gemini_tts(text, voice, exhausted) {
        Some((pcm, model)) => (pcm, format!("gemini:{model}:{voice}")),
        None => {
            let female = matches!(voice, "Kore" | "Aoede" | "Zephyr" | "Leda");
            let pcm = windows_tts(text, female).unwrap_or_else(|e| panic!("offline TTS failed for {text:?}: {e}"));
            (pcm, format!("windows:{}", if female { "Paulina" } else { "Adam" }))
        }
    };
    let rate = match speed {
        "fast" => 17_920,
        "slow" => 14_400,
        _ => return (pcm, source),
    };
    (crate::voice_regression::resample_to_pcm16k(&to_f32(&pcm), rate), format!("{source}:{speed}"))
}

/// Voice + speed of a case (explicit, else rotated over the manifest lists); `shift` = the alt take.
fn voice_of(m: &Value, case: &Value, i: usize, shift: usize) -> (String, String) {
    let voices = strs(&m["voices"]);
    let speeds = strs(&m["speeds"]);
    let v = case["voice"].as_str().filter(|_| shift == 0).map(String::from).unwrap_or_else(|| voices[(i + shift) % voices.len()].clone());
    let s = case["speed"]
        .as_str()
        .filter(|_| shift == 0)
        .map(String::from)
        .unwrap_or_else(|| speeds[(i / voices.len() + shift) % speeds.len()].clone());
    (v, s)
}

fn write_script(m: &Value, sources: &Value) {
    let mut md = String::from(
        "# Voice E2E script\n\nGenerated from `manifest.json` by `npm run test:e2e:synth` — edit the manifest, not this file.\n\
Voices/speeds rotate over the manifest lists (`fast`/`slow` = the take played 12 % faster / 10 % slower); variants: \
`noise` (12 dB SNR), `quiet` (−14 dB), `pad` (1.5 s + 2 s silence), `alt` (another voice/speed). TTS = where the base \
sample came from (`gemini:<model>`, or the offline `windows:<voice>` fallback once the free Gemini TTS quota — 10/day per \
model — is spent; the next synth run upgrades those).\n\n| # | id | phrase | voice | TTS | setup | expected | variants |\n|---|---|---|---|---|---|---|---|\n",
    );
    for (i, c) in m["cases"].as_array().unwrap().iter().enumerate() {
        let id = c["id"].as_str().unwrap_or("");
        let (v, s) = voice_of(m, c, i, 0);
        md.push_str(&format!(
            "| {} | `{id}` | „{}” | {v} / {s} | {} | {} | {} | {} |\n",
            i + 1,
            c["text"].as_str().unwrap_or(""),
            sources[format!("{id}.wav")].as_str().unwrap_or("—"),
            c.get("setup").map(|s| format!("`{s}`")).unwrap_or_default().replace('|', "\\|"),
            c["expect"].to_string().replace('|', "\\|"),
            strs(&c["variants"]).join(", ")
        ));
    }
    std::fs::write(suite_dir().join("SCRIPT.md"), md).expect("write SCRIPT.md");
}

#[test]
#[ignore]
fn voice_e2e_synth() {
    let m = manifest();
    let out = suite_dir().join("generated");
    std::fs::create_dir_all(&out).expect("generated/");
    let sources_path = out.join("sources.json");
    let mut sources: Value =
        std::fs::read_to_string(&sources_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    let force = std::env::var("SOUL_SYNTH_FORCE").is_ok_and(|v| v == "1");
    let delay = env_ms("SOUL_VOICE_DELAY_MS", 4000);
    let mut exhausted: Vec<&'static str> = Vec::new();
    let mut gemini_calls = 0;
    let mut synth = |file: String, text: &str, voice: &str, speed: &str| {
        let path = out.join(&file);
        let offline = sources[&file].as_str().is_some_and(|s| s.starts_with("windows"));
        let gemini_left = exhausted.len() < TTS_MODELS.len();
        if !force && path.exists() && !(offline && gemini_left) {
            return;
        }
        if gemini_left && gemini_calls > 0 {
            std::thread::sleep(delay); // free-tier pacing
        }
        gemini_calls += usize::from(gemini_left);
        let (pcm, source) = speak(text, voice, speed, &mut exhausted);
        if source.starts_with("windows") && offline && path.exists() {
            return; // still no Gemini quota: keep the offline take
        }
        std::fs::write(&path, pcm16k_to_wav(&pcm)).expect("write wav");
        println!("[e2e-synth] {file} ← {source}: {text:?}");
        sources[&file] = json!(source);
        std::fs::write(&sources_path, serde_json::to_string_pretty(&sources).unwrap()).expect("sources.json");
    };
    for (i, c) in m["cases"].as_array().unwrap().iter().enumerate() {
        let id = c["id"].as_str().expect("case id");
        let text = c["text"].as_str().expect("case text");
        let (voice, speed) = voice_of(&m, c, i, 0);
        synth(format!("{id}.wav"), text, &voice, &speed);
        let variants = strs(&c["variants"]);
        if variants.iter().any(|v| v == "alt") {
            let (voice, speed) = voice_of(&m, c, i, 1);
            synth(format!("{id}__alt.wav"), text, &voice, &speed);
        }
        let pcm = to_f32(&crate::voice_regression::load_pcm16k(&out.join(format!("{id}.wav"))).expect("base wav"));
        for v in variants.iter().filter(|v| DERIVED.contains(&v.as_str())) {
            std::fs::write(out.join(format!("{id}__{v}.wav")), pcm16k_to_wav(&to_pcm(&derive(&pcm, v)))).expect("write variant");
        }
    }
    let sources: Value = std::fs::read_to_string(&sources_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    write_script(&m, &sources);
    let offline = sources.as_object().map_or(0, |o| o.values().filter(|s| s.as_str().is_some_and(|s| s.starts_with("windows"))).count());
    println!("[e2e-synth] samples in {} — {offline} offline (Windows voice) take(s), upgraded on a later run", out.display());
}

// ───────────────────────────── local pages ─────────────────────────────

/// GET /<name>.html[?query] from tests/e2e/pages (plain names only).
fn serve_pages() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let dir = root().join("tests/e2e/pages");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/");
            let name = path.trim_start_matches('/');
            let file = (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') && !name.contains(".."))
                .then(|| std::fs::read(dir.join(name)).ok())
                .flatten();
            let mut s = reader.into_inner();
            let (status, ctype, body) = match file {
                Some(b) => ("200 OK", "text/html; charset=utf-8", b),
                None => ("404 Not Found", "text/plain", b"not found".to_vec()),
            };
            let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(&body);
        }
    });
    port
}

/// "notes?text=…" → http://127.0.0.1:<port>/notes.html?text=…
fn page_url(port: u16, page: &str) -> String {
    let (name, query) = page.split_once('?').map(|(n, q)| (n, format!("?{q}"))).unwrap_or((page, String::new()));
    format!("http://127.0.0.1:{port}/{name}.html{query}")
}

// ───────────────────────────── the app under test ─────────────────────────────

fn data_dir() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(a) => PathBuf::from(a).join("com.oskar.soul"),
        None => PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".local/share/com.oskar.soul"),
    }
}

/// The test drives a separate process on purpose (the real app, piped output) — not a user-facing spawn.
#[allow(clippy::disallowed_methods)]
fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    std::process::Command::new(program)
}

#[cfg(windows)]
fn process_running(image: &str) -> bool {
    command("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {image}"), "/NH"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains(&image.to_lowercase()))
}

#[cfg(not(windows))]
fn process_running(image: &str) -> bool {
    command("pgrep").args(["-x", image.trim_end_matches(".exe")]).status().is_ok_and(|s| s.success())
}

/// Settings + memory DB as they were before the run; put back when dropped (after the app exits).
struct Backup(Vec<(PathBuf, Option<Vec<u8>>)>);

impl Backup {
    fn take() -> Self {
        let dir = data_dir();
        Self(
            ["soul-settings.json", "soul-memory.db", "soul-memory.db-wal", "soul-memory.db-shm"]
                .iter()
                .map(|f| (dir.join(f), std::fs::read(dir.join(f)).ok()))
                .collect(),
        )
    }
}

impl Drop for Backup {
    fn drop(&mut self) {
        for (path, bytes) in &self.0 {
            let _ = match bytes {
                Some(b) => std::fs::write(path, b),
                None => std::fs::remove_file(path),
            };
        }
        eprintln!("[e2e] restored Soul's settings and memory DB");
    }
}

struct App {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl App {
    fn start(dry_run: &str) -> Self {
        let exe = root().join(if cfg!(windows) { "src-tauri/target/debug/soul.exe" } else { "src-tauri/target/debug/soul" });
        assert!(exe.exists(), "{} missing — `npm run test:e2e` builds it", exe.display());
        let mut child = command(&exe)
            .env("SOUL_E2E", "1")
            .env("SOUL_DRY_RUN_TOOLS", dry_run)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start soul");
        let lines = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::new(Mutex::new(std::fs::File::create(suite_dir().join("out/app.log")).expect("app.log")));
        let out: Vec<Box<dyn Read + Send>> =
            vec![Box::new(child.stdout.take().unwrap()), Box::new(child.stderr.take().unwrap())];
        for pipe in out {
            let (lines, log) = (Arc::clone(&lines), Arc::clone(&log));
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let _ = writeln!(log.lock(), "{line}");
                    lines.lock().push(line);
                }
            });
        }
        Self { child, lines }
    }

    fn len(&self) -> usize {
        self.lines.lock().len()
    }

    fn since(&self, from: usize) -> Vec<String> {
        self.lines.lock()[from..].to_vec()
    }

    fn wait_line(&self, from: usize, timeout: Duration, pred: impl Fn(&str) -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < timeout {
            if self.lines.lock()[from..].iter().any(|l| pred(l)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn events<'a>(ev: &'a [Value], kind: &'static str) -> impl Iterator<Item = &'a Value> {
    ev.iter().filter(move |e| e["ev"] == kind)
}

fn e2e_events(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .filter_map(|l| l.split_once("[soul][e2e] ").and_then(|(_, j)| serde_json::from_str(j).ok()))
        .collect()
}

// ───────────────────────────── Vio state ─────────────────────────────

struct VioState {
    tabs: Vec<Value>,
    active: Value,
    elements: Vec<Value>,
    page: String,
}

/// The page after a case. `tabs` = only the tabs opened during the case (never the user's own).
fn vio_state(c: &Client, baseline: &[String]) -> VioState {
    let mut tabs = c.call("listTabs", json!({})).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    tabs.retain(|t| t["active"] == json!(true) || !baseline.contains(&vio::tab_key(&t["tabId"]).unwrap_or_default()));
    let active = tabs.iter().find(|t| t["active"] == json!(true)).cloned().unwrap_or(Value::Null);
    let id = vio::tab_key(&active["tabId"]).unwrap_or_default();
    let elements = vio::elements_of(c, &id).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let page = c.call("getPage", json!({ "tabId": id })).ok().map(|p| vio::page_text(&p)).unwrap_or_default();
    VioState { tabs, active, elements, page }
}

/// Element whose label/name/placeholder equals (folded) `label`, else contains it.
fn element<'a>(els: &'a [Value], label: &str) -> Option<&'a Value> {
    let want = fold_text(label);
    let name = |e: &Value| fold_text(["label", "name", "placeholder"].iter().filter_map(|k| e[k].as_str()).find(|s| !s.trim().is_empty()).unwrap_or(""));
    els.iter().find(|e| name(e) == want).or_else(|| els.iter().find(|e| !want.is_empty() && name(e).contains(&want)))
}

fn field_ok(want: &Value, e: &Value) -> bool {
    let value = e["value"].as_str().unwrap_or("").trim().to_string();
    match want {
        Value::String(s) if s.is_empty() => value.is_empty() || e["empty"] == json!(true),
        Value::String(s) => fold_text(&value) == fold_text(s) || value.eq_ignore_ascii_case(s),
        _ => {
            let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
            want["contains"].as_str().is_none_or(|c| value.to_lowercase().contains(&c.to_lowercase()))
                && want["digits"].as_str().is_none_or(|d| digits(&value) == d)
        }
    }
}

// ───────────────────────────── one case ─────────────────────────────

struct Run {
    id: String,
    cap: String,
    wav: PathBuf,
    expect: Value,
    setup: Value,
    text: String,
    timeout: Duration,
    /// Why this sample cannot be judged (reported as SKIP).
    skip: Option<String>,
}

#[derive(Default)]
struct Outcome {
    status: &'static str,
    notes: Vec<String>,
    transcript: String,
    similarity: f64,
    stt: String,
    route: String,
    via: Vec<String>,
    tools: Vec<String>,
    agent: String,
    model_calls: u32,
    models: Vec<String>,
    http: BTreeMap<String, u32>,
    retries: u32,
    screenshots: u64,
    stt_ms: u64,
    total_ms: u64,
    island: Vec<String>,
}

/// Normalized Levenshtein similarity of the folded texts (1.0 = identical).
fn similarity(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (fold_text(a).chars().collect(), fold_text(b).chars().collect());
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    1.0 - prev[b.len()] as f64 / a.len().max(b.len()) as f64
}

/// Failures of one run against its expectations (empty = pass).
fn check(r: &Run, ev: &[Value], vio: Option<&VioState>, out: &Outcome) -> Vec<String> {
    let x = &r.expect;
    let mut bad = Vec::new();
    let of = |kind: &'static str| events(ev, kind);
    let tools: Vec<&Value> = of("tool").collect();
    let acting = |t: &&Value| t["name"] != "answer" && t["ok"] == json!(true);
    let pasted = of("paste").find(|p| p["ok"] == json!(true)).and_then(|p| p["text"].as_str()).unwrap_or("").to_string();
    let done = of("command_done").last();
    let agent = of("agent").last();
    let answer: String = done
        .iter()
        .flat_map(|d| [d["result"].as_str(), d["spoken"].as_str()])
        .chain(tools.iter().map(|t| t["result"].as_str()))
        .chain(agent.iter().map(|a| a["summary"].as_str()))
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");

    let min_sim = x["min_similarity"].as_f64().unwrap_or(0.7);
    if x["route"] != "rejected" && out.similarity < min_sim {
        bad.push(format!("transcript similarity {:.2} < {min_sim}", out.similarity));
    }
    match x["route"].as_str() {
        Some("rejected") => {
            if let Some(t) = tools.iter().copied().find(acting) {
                bad.push(format!("non-Polish utterance ran {}", t["name"]));
            }
            if !pasted.is_empty() {
                bad.push(format!("non-Polish utterance was pasted: „{pasted}”"));
            }
        }
        Some(route) if out.route.split(" (").next() != Some(route) => bad.push(format!("route {} ≠ {route}", out.route)),
        _ => {}
    }
    if let Some(via) = x["via"].as_str() {
        if !out.via.iter().any(|v| v == via) {
            bad.push(format!("via {:?} ≠ {via}", out.via));
        }
    }
    // A tool that ran and failed („run_scene ✗”) is not the expected outcome.
    let matches = |want: &Value, t: &Value| {
        t["name"] == want["name"]
            && t["ok"] == json!(true)
            && want["args"].as_object().into_iter().flatten().all(|(k, v)| value_matches(v, t["args"].get(k)))
    };
    let mut at = 0;
    for want in x["tools"].as_array().into_iter().flatten() {
        match tools[at..].iter().position(|t| matches(want, t)) {
            Some(i) => at += i + 1,
            None => bad.push(format!("missing tool {} {}", want["name"], want.get("args").unwrap_or(&json!({})))),
        }
    }
    for no in x["forbid_tools"].as_array().into_iter().flatten() {
        if tools.iter().any(|t| matches(no, t)) {
            bad.push(format!("forbidden tool ran: {} {}", no["name"], no.get("args").unwrap_or(&json!({}))));
        }
    }
    if x["no_action_tools"] == true {
        if let Some(t) = tools.iter().copied().find(acting) {
            bad.push(format!("unexpected tool {}", t["name"]));
        }
    }
    if x["agent"] == true && agent.is_none_or(|a| a["ok"] != json!(true)) {
        bad.push(match agent {
            Some(a) => format!("agent failed: {}", a["summary"].as_str().unwrap_or("")),
            None => "the computer agent did not run".into(),
        });
    }
    if x["answer_nonempty"] == true && answer.trim().is_empty() {
        bad.push("no answer".into());
    }
    for s in strs(&x["answer_contains"]) {
        if !fold_text(&answer).contains(&fold_text(&s)) {
            bad.push(format!("answer lacks „{s}”"));
        }
    }
    for s in strs(&x["pasted_contains"]) {
        if !pasted.to_lowercase().contains(&s.to_lowercase()) {
            bad.push(format!("pasted text lacks „{s}” (pasted „{pasted}”)"));
        }
    }
    for s in strs(&x["pasted_lacks"]) {
        if pasted.to_lowercase().contains(&s.to_lowercase()) {
            bad.push(format!("pasted text contains „{s}”"));
        }
    }
    if x["confirm"] == true && of("confirm").next().is_none() {
        bad.push("no confirmation card".into());
    }
    let v = &x["vio"];
    if v.is_object() {
        let Some(s) = vio else {
            bad.push("Vio state unavailable".into());
            return bad;
        };
        let url = s.active["url"].as_str().unwrap_or("");
        // A string, or alternatives: ["youtube.com/watch", "youtube.com/shorts"].
        let urls = match &v["active_url"] {
            Value::String(s) => vec![s.clone()],
            other => strs(other),
        };
        if !urls.is_empty() && !urls.iter().any(|w| url.to_lowercase().contains(&w.to_lowercase())) {
            bad.push(format!("active tab {url} lacks {}", urls.join(" | ")));
        }
        if let Some(want) = v["active_title"].as_str() {
            let title = s.active["title"].as_str().unwrap_or("");
            if !fold_text(title).contains(&fold_text(want)) {
                bad.push(format!("active tab „{title}” lacks „{want}”"));
            }
        }
        if let Some(want) = v["tab_url"].as_str() {
            if !s.tabs.iter().any(|t| t["url"].as_str().unwrap_or("").contains(want)) {
                bad.push(format!("no tab with {want}"));
            }
        }
        for (label, want) in v["fields"].as_object().into_iter().flatten() {
            match element(&s.elements, label) {
                Some(e) if field_ok(want, e) => {}
                Some(e) => bad.push(format!("{label} = {} (want {want})", e["value"])),
                None => bad.push(format!("field „{label}” not found")),
            }
        }
        for (label, want) in v["checked"].as_object().into_iter().flatten() {
            let got = element(&s.elements, label).map(|e| e["checked"] == json!(true));
            if got != want.as_bool() {
                bad.push(format!("{label} checked={got:?} (want {want})"));
            }
        }
        for p in strs(&v["page_contains"]) {
            if !fold_text(&s.page).contains(&fold_text(&p)) {
                bad.push(format!("page lacks „{p}”"));
            }
        }
        for p in strs(&v["page_lacks"]) {
            if fold_text(&s.page).contains(&fold_text(&p)) {
                bad.push(format!("page contains „{p}”"));
            }
        }
    }
    bad
}

/// Trace + log lines → report columns.
fn summarize(r: &Run, ev: &[Value], logs: &[String]) -> Outcome {
    let mut o = Outcome { status: "PASS", ..Default::default() };
    let of = |kind: &'static str| events(ev, kind);
    if let Some(h) = of("heard").last() {
        o.transcript = h["text"].as_str().or(h["error"].as_str()).unwrap_or("").to_string();
        o.stt = h["stt"].as_str().unwrap_or("error").to_string();
        o.stt_ms = h["ms"].as_u64().unwrap_or(0);
        if let Some(m) = h["model"].as_str() {
            o.models.push(m.to_string());
        }
    }
    o.similarity = similarity(&r.text, &o.transcript);
    o.route = of("route").last().and_then(|e| e["route"].as_str()).unwrap_or("—").to_string();
    if let Some(reason) = of("route").last().and_then(|e| e["reason"].as_str()) {
        o.route = format!("{} ({reason})", o.route);
    }
    for s in of("stage") {
        o.via.push(s["via"].as_str().unwrap_or("").to_string());
        if let Some(m) = s["model"].as_str() {
            o.models.push(m.to_string());
        }
    }
    for t in of("tool") {
        let mark = if t["dry_run"] == true { " (dry)" } else if t["ok"] == true { "" } else { " ✗" };
        o.tools.push(format!("{} {}{mark}", t["name"].as_str().unwrap_or(""), t["args"]));
    }
    if let Some(p) = of("paste").last() {
        o.tools.push(format!("paste „{}”", p["text"].as_str().unwrap_or("")));
    }
    if let Some(a) = of("agent").last() {
        o.agent = format!("{} steps, {} calls{}", a["steps"], a["model_calls"], if a["ok"] == true { "" } else { " ✗" });
        o.screenshots = a["screenshots"].as_u64().unwrap_or(0);
    }
    o.island = of("overlay").filter_map(|e| e["state"].as_str()).map(String::from).collect();
    o.island.dedup();
    o.total_ms = of("done").last().and_then(|d| d["ms"].as_u64()).unwrap_or(0);
    for l in logs {
        if let Some(rest) = l.split("stage=request_sent model=").nth(1) {
            o.model_calls += 1;
            o.models.push(rest.split_whitespace().next().unwrap_or("").to_string());
        }
        if l.contains("stage=llm_cleanup") {
            o.model_calls += 1;
        }
        if let Some(code) = l.split(" http=").nth(1).map(|c| c.trim().to_string()).filter(|c| c != "200") {
            *o.http.entry(code).or_default() += 1;
        }
        if l.contains("[soul][live]") && (l.contains("error") || l.contains("failed")) {
            *o.http.entry("live-err".into()).or_default() += 1;
        }
        let low = l.to_lowercase();
        if ["fallback", "retry", "retrying", "cooling", "step-down", "thinking_step_down"].iter().any(|k| low.contains(k)) {
            o.retries += 1;
        }
    }
    o.models.sort();
    o.models.dedup();
    o
}

/// Likely cause of a failure, from what the trace shows.
fn likely_cause(o: &Outcome, notes: &[String]) -> &'static str {
    let all = notes.join(" ");
    if o.status == "ERROR" || o.total_ms == 0 {
        "pipeline did not finish (timeout / stuck turn) — see out/app.log"
    } else if all.contains("All models failed") || all.contains("(503)") {
        "Gemini overloaded (503 „high demand” on every model in rotation) — provider-side, retry later"
    } else if o.http.contains_key("429") {
        "free-tier rate limit (429) — retry later or raise SOUL_E2E_DELAY_MS"
    } else if all.contains("similarity") {
        "speech recognition (Live dictation model) misheard the sample"
    } else if all.contains("route ") {
        "Smart routing (agent::classify_smart) picked the wrong path"
    } else if all.contains("Sos =") || all.contains("Godzina dostawy =") {
        "Vio API cannot set <select>/<input type=time> (fill_plan Skip) — needs a Vio change"
    } else if all.contains("missing tool") && o.via.iter().any(|v| v == "local") {
        "pre-router / chain / follow-up resolution chose a different tool"
    } else if all.contains("missing tool") || all.contains("unexpected tool") {
        "the commands model (Live/REST) chose a different tool or arguments"
    } else if all.contains("agent failed") || all.contains(" = ") || all.contains("checked=") || all.contains("active tab") {
        "Soul Agent did not reach the expected page state"
    } else {
        "see notes"
    }
}

// ───────────────────────────── per-case setup ─────────────────────────────

fn wait_page(c: &Client, tab: &str) {
    for _ in 0..25 {
        if c.call("getPage", json!({ "tabId": tab })).is_ok_and(|p| !p["text"].as_str().unwrap_or("").trim().is_empty()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(windows)]
fn vio_to_front() {
    let p = crate::platform::create_platform();
    match crate::platform::find_window_for_tests("Vio") {
        Some(id) => {
            let _ = p.activate_window(id);
            std::thread::sleep(Duration::from_millis(400));
        }
        None => eprintln!("[e2e] Vio window not found"),
    }
}

#[cfg(not(windows))]
fn vio_to_front() {}

/// Like a user clicking into the field: Vio's DOM focus alone can leave the window's keyboard
/// focus on the address bar, where Ctrl+V (dictation, text actions) would land.
#[cfg(windows)]
fn os_focus(label: &str) -> Result<(), String> {
    use crate::platform::ui::{ElementOp, Role};
    let p = crate::platform::create_platform();
    let snap = p.ui_snapshot(&Default::default())?;
    let want = fold_text(label);
    let field = snap
        .elements
        .iter()
        .find(|e| e.role == Role::Edit && fold_text(&e.name) == want)
        .ok_or_else(|| format!("UI Automation sees no field „{label}” in „{}”", snap.window.title))?;
    p.ui_element_op(snap.gen, field.id, &ElementOp::Focus)?;
    // A real click as well: UIA focus alone does not move Chromium's keyboard focus into a <webview>.
    let r = field.rect;
    let at = crate::platform::coords::Point { x: r.x + r.w / 2, y: r.y + r.h / 2 };
    let steps = crate::platform::input::plan_click(at, crate::platform::input::MouseButton::Left, 1);
    let _ = crate::platform::input::execute(&steps, &|| false, &mut |s| p.send_input_step(s));
    std::thread::sleep(Duration::from_millis(150));
    Ok(())
}

#[cfg(not(windows))]
fn os_focus(_: &str) -> Result<(), String> {
    Ok(())
}

fn keys(combo: &str) {
    use crate::platform::input::{self, Combo};
    let p = crate::platform::create_platform();
    let _ = input::execute(&input::plan_combo(&Combo::parse(combo).expect("combo")), &|| false, &mut |s| p.send_input_step(s));
}

/// Close the suite's tabs, open the case's pages (the last one active), focus a field, foreground app.
/// Returns whether the case launched the calculator (closed afterwards).
fn close_suite_tabs(c: &Client, baseline: &[String]) -> Result<(), String> {
    let tabs = c.call("listTabs", json!({}))?;
    for t in tabs.as_array().into_iter().flatten() {
        let id = vio::tab_key(&t["tabId"]).unwrap_or_default();
        if !baseline.contains(&id) {
            c.call("closeTab", json!({ "tabId": id }))?;
        }
    }
    Ok(())
}

fn setup_case(c: &Client, port: u16, baseline: &[String], setup: &Value) -> Result<bool, String> {
    close_suite_tabs(c, baseline)?;
    let pages = match strs(&setup["tabs"]) {
        p if p.is_empty() => vec!["home".to_string()],
        p => p,
    };
    let mut last = String::new();
    for p in &pages {
        let r = c.call("openTab", json!({ "url": page_url(port, p) }))?;
        last = vio::tab_key(&r["tabId"]).ok_or("openTab returned no tabId")?;
        wait_page(c, &last);
    }
    vio_to_front();
    if let Some(label) = setup["focus"].as_str() {
        let els = vio::elements_of(c, &last)?;
        let e = element(els.as_array().map(Vec::as_slice).unwrap_or_default(), label).ok_or(format!("no field {label}"))?.clone();
        // fill focuses the field (a DOM click would not); same text keeps the content.
        c.call("fill", json!({ "tabId": last, "ref": e["ref"], "text": e["value"].as_str().unwrap_or("") }))?;
        os_focus(label)?;
        if setup["select_all"] == true {
            std::thread::sleep(Duration::from_millis(200));
            keys("ctrl+a");
        }
    }
    let calc = setup["foreground"] == "calc" && !process_running("CalculatorApp.exe");
    if setup["foreground"] == "calc" {
        crate::platform::open_path_for_tests("calc.exe")?;
        std::thread::sleep(Duration::from_millis(2000));
    }
    Ok(calc)
}

fn runs(m: &Value) -> Vec<Run> {
    let filter = std::env::var("SOUL_E2E_FILTER").unwrap_or_default();
    let variants = std::env::var("SOUL_E2E_VARIANTS").map_or(true, |v| v != "0");
    let (gen, real) = (suite_dir().join("generated"), suite_dir().join("real"));
    let real_files: Vec<String> = std::fs::read_dir(&real)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|f| f.ends_with(".wav"))
        .collect();
    let sources: Value =
        std::fs::read_to_string(gen.join("sources.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut out = Vec::new();
    for c in m["cases"].as_array().unwrap() {
        let id = c["id"].as_str().unwrap().to_string();
        if !id.contains(filter.as_str()) {
            continue;
        }
        let run = |name: String, wav: PathBuf| Run {
            id: name,
            cap: c["cap"].as_str().unwrap_or("").to_string(),
            wav,
            expect: c["expect"].clone(),
            setup: c.get("setup").cloned().unwrap_or(json!({})),
            // `transcript` = how Live writes it (digits, symbols); defaults to the spoken text.
            text: c["transcript"].as_str().or(c["text"].as_str()).unwrap_or("").to_string(),
            timeout: Duration::from_secs(c["timeout_s"].as_u64().unwrap_or(120)),
            skip: None,
        };
        // The offline fallback voice is Polish: it cannot speak another language's sentence.
        let lang = c["lang"].as_str().unwrap_or("pl");
        let offline = |file: &str| sources[file].as_str().is_some_and(|s| s.starts_with("windows"));
        let skip = |file: &str| {
            (lang != "pl" && offline(file)).then(|| format!("needs a Gemini TTS take: the offline Polish voice cannot speak „{lang}”"))
        };
        let file = format!("{id}.wav");
        out.push(Run { skip: skip(&file), ..run(id.clone(), gen.join(&file)) });
        if variants {
            for v in strs(&c["variants"]) {
                let file = format!("{id}__{v}.wav");
                out.push(Run { skip: skip(&file).or(skip(&format!("{id}.wav"))), ..run(format!("{id}__{v}"), gen.join(&file)) });
            }
        }
        for f in real_files.iter().filter(|f| f.trim_end_matches(".wav") == id || f.starts_with(&format!("{id}__"))) {
            out.push(run(format!("{id} (real {f})"), real.join(f)));
        }
    }
    out
}

fn cell(s: &str, max: usize) -> String {
    let s = s.replace('|', "\\|").replace(['\n', '\r'], " ");
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    } else {
        s
    }
}

fn report(rows: &[(Run, Outcome)], wall: Duration) -> String {
    let n = |s: &str| rows.iter().filter(|(_, o)| o.status == s).count();
    let mut lat: Vec<u64> = rows.iter().map(|(_, o)| o.total_ms).filter(|m| *m > 0).collect();
    lat.sort_unstable();
    let mut md = format!(
        "# Voice E2E report\n\n{} · real Soul dev build, real Gemini (Live + REST), real Vio · risky tools dry-run · run took {} min\n\n\
**{} runs: {} PASS · {} FAIL · {} ERROR · {} SKIP** · median latency {} ms · model calls {} · screenshots {}\n\n\
Latency = talk key released → pipeline done (incl. spoken reply); stt = transcript ready. Sim = transcript similarity to the script.\n\n\
| case | capability | result | transcript | sim | route | via | tools / steps | agent | models | HTTP / Live errors | retries | shots | latency (stt / total) ms | Island |\n\
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M"),
        wall.as_secs() / 60,
        rows.len(),
        n("PASS"),
        n("FAIL"),
        n("ERROR"),
        n("SKIP"),
        lat.get(lat.len() / 2).copied().unwrap_or(0),
        rows.iter().map(|(_, o)| o.model_calls).sum::<u32>(),
        rows.iter().map(|(_, o)| o.screenshots).sum::<u64>(),
    );
    for (r, o) in rows {
        let status = match o.status {
            "PASS" => "✅ PASS",
            "FAIL" => "❌ FAIL",
            "ERROR" => "💥 ERROR",
            _ => "⏭️ SKIP",
        };
        let http: Vec<String> = o.http.iter().map(|(k, v)| format!("{k}×{v}")).collect();
        md.push_str(&format!(
            "| `{}` | {} | {status} | {} | {:.2} | {} | {} | {} | {} | {} | {} | {} | {} | {} / {} | {} |\n",
            r.id,
            cell(&r.cap, 50),
            cell(&o.transcript, 80),
            o.similarity,
            cell(&o.route, 30),
            o.via.join(", "),
            cell(&o.tools.join(" → "), 120),
            o.agent,
            cell(&o.models.join(", "), 80),
            http.join(" "),
            o.retries,
            o.screenshots,
            o.stt_ms,
            o.total_ms,
            cell(&o.island.join("→"), 50),
        ));
    }
    let failed: Vec<&(Run, Outcome)> = rows.iter().filter(|(_, o)| matches!(o.status, "FAIL" | "ERROR")).collect();
    if !failed.is_empty() {
        md.push_str("\n## Failures\n\n");
        for (r, o) in failed {
            md.push_str(&format!(
                "- **`{}`** — {} · *likely cause:* {}\n  - said „{}”, heard „{}”\n",
                r.id,
                cell(&o.notes.join("; "), 400),
                likely_cause(o, &o.notes),
                r.text,
                cell(&o.transcript, 300)
            ));
        }
    }
    let skipped: Vec<String> =
        rows.iter().filter(|(_, o)| o.status == "SKIP").map(|(r, o)| format!("`{}` ({})", r.id, o.notes.join("; "))).collect();
    if !skipped.is_empty() {
        md.push_str(&format!("\n**Skipped:** {}\n", skipped.join(", ")));
    }
    md
}

#[test]
#[ignore]
fn voice_e2e() {
    crate::platform::prefer_physical_pixels();
    let m = manifest();
    let runs = runs(&m);
    assert!(!runs.is_empty(), "no cases (SOUL_E2E_FILTER?)");
    std::fs::create_dir_all(suite_dir().join("out")).expect("out/");
    assert!(!process_running(if cfg!(windows) { "soul.exe" } else { "soul" }), "Soul is running — quit it first (the suite starts its own dev build)");

    let path = vio::discovery_path().expect("config dir");
    assert!(vio::discover(&path).is_some(), "Vio is not running or its automation API is off ({})", path.display());
    let vio_client = Client::start(path, true, Box::new(|s| eprintln!("[e2e] runner Vio status {:?}", s.state)));
    let port = serve_pages();

    let _backup = Backup::take();
    let app = App::start(&strs(&m["dry_run_tools"]).join(","));
    eprintln!("[e2e] Soul started — waiting for its Vio connection (click „Zezwól” in Vio if asked)");
    let t0 = Instant::now();
    while !(app.wait_line(0, Duration::from_secs(1), |l| l.contains("vio authorized")) && vio_client.status().state == State::Connected) {
        assert!(t0.elapsed() < Duration::from_secs(300), "Vio never authorized Soul / the runner (consent dialog?)");
        if t0.elapsed().as_secs().is_multiple_of(15) {
            eprintln!("[e2e] still waiting for Vio: runner {:?} — accept the consent dialog in Vio", vio_client.status().state);
        }
    }
    std::thread::sleep(Duration::from_secs(3)); // Live sessions warm up
    let baseline: Vec<String> = vio_client
        .call("listTabs", json!({}))
        .expect("listTabs")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| vio::tab_key(&t["tabId"]))
        .collect();
    let utterance = data_dir().join("dev-utterance.json");
    let delay = env_ms("SOUL_E2E_DELAY_MS", 5000);
    let started = Instant::now();
    let mut rows: Vec<(Run, Outcome)> = Vec::new();
    let mut results = Vec::new();

    for (i, r) in runs.into_iter().enumerate() {
        let skip = r.skip.clone().or_else(|| (!r.wav.exists()).then(|| "no WAV — run `npm run test:e2e:synth`".to_string()));
        if let Some(why) = skip {
            println!("[e2e] {}: SKIP ({why})", r.id);
            rows.push((r, Outcome { status: "SKIP", notes: vec![why], ..Default::default() }));
            continue;
        }
        if i > 0 {
            std::thread::sleep(delay); // free-tier pacing
        }
        println!("[e2e] ── {}: „{}”", r.id, r.text);
        let calc = match setup_case(&vio_client, port, &baseline, &r.setup) {
            Ok(c) => c,
            Err(e) => {
                rows.push((r, Outcome { status: "ERROR", notes: vec![format!("setup: {e}")], ..Default::default() }));
                continue;
            }
        };
        let from = app.len();
        let req = json!({
            "id": r.id, "wav": r.wav.canonicalize().unwrap_or(r.wav.clone()),
            "confirm": r.setup["confirm"].as_str().unwrap_or("deny"),
            "fresh_context": r.setup["fresh_context"] != false,
        });
        std::fs::write(&utterance, req.to_string()).expect("write dev-utterance.json");
        let started_ok = app.wait_line(from, Duration::from_secs(10), |l| l.contains("[soul][e2e]") && l.contains("\"ev\":\"start\""));
        if std::env::var_os("SOUL_E2E_FOCUS_DEBUG").is_some() {
            let p = crate::platform::create_platform();
            for _ in 0..12 {
                std::thread::sleep(Duration::from_millis(500));
                let fg = p.foreground_window().map(|w| format!("{} ({}, pid {})", w.title, w.process, w.pid));
                eprintln!("[e2e] foreground: {fg:?}");
            }
        }
        let done = started_ok && app.wait_line(from, r.timeout, |l| l.contains("[soul][e2e]") && l.contains("\"ev\":\"done\""));
        std::thread::sleep(Duration::from_millis(1500)); // late tool calls / page settle
        let logs = app.since(from);
        let ev = e2e_events(&logs);
        let state = vio_state(&vio_client, &baseline);
        let mut o = summarize(&r, &ev, &logs);
        o.notes = check(&r, &ev, Some(&state), &o);
        if !started_ok {
            o.status = "ERROR";
            o.notes.insert(0, "the app never picked up dev-utterance.json (is it a debug build?)".into());
        } else if !done {
            o.status = "ERROR";
            o.notes.insert(0, format!("no `done` within {} s", r.timeout.as_secs()));
        } else if !o.notes.is_empty() {
            o.status = "FAIL";
        }
        if calc {
            let _ = command("taskkill").args(["/IM", "CalculatorApp.exe", "/F"]).output();
        }
        println!(
            "[e2e]    {} heard={:?} sim={:.2} route={} tools={:?} {}",
            o.status,
            o.transcript,
            o.similarity,
            o.route,
            o.tools,
            o.notes.join("; ")
        );
        results.push(json!({ "id": r.id, "status": o.status, "notes": o.notes, "events": ev,
            "vio": { "active": state.active, "elements": state.elements.len(), "page": state.page.chars().take(400).collect::<String>() } }));
        rows.push((r, o));
    }
    // Leave only the user's own tabs.
    let _ = close_suite_tabs(&vio_client, &baseline);
    drop(app);
    let md = report(&rows, started.elapsed());
    std::fs::write(suite_dir().join("report.md"), &md).expect("write report.md");
    std::fs::write(suite_dir().join("out/results.json"), serde_json::to_string_pretty(&results).unwrap()).expect("results.json");
    println!("\n{md}\nreport → tests/e2e/voice/report.md");
    let bad: Vec<&str> = rows.iter().filter(|(_, o)| matches!(o.status, "FAIL" | "ERROR")).map(|(r, _)| r.id.as_str()).collect();
    assert!(bad.is_empty(), "voice E2E failed: {bad:?}");
}

/// Diagnostics: does `Platform::paste_text` land in a Vio textarea focused like the suite does?
/// `cargo test --lib voice_e2e::vio_paste_probe -- --ignored --nocapture` (Vio running).
#[test]
#[ignore]
fn vio_paste_probe() {
    crate::platform::prefer_physical_pixels();
    let c = Client::start(vio::discovery_path().unwrap(), true, Box::new(|_| {}));
    let t0 = Instant::now();
    while c.status().state != State::Connected && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let port = serve_pages();
    let baseline: Vec<String> =
        c.call("listTabs", json!({})).unwrap().as_array().into_iter().flatten().filter_map(|t| vio::tab_key(&t["tabId"])).collect();
    setup_case(&c, port, &baseline, &json!({ "tabs": ["notes"], "focus": "Notatka" })).unwrap();
    let p = crate::platform::create_platform();
    // Something else on the clipboard first: a too-early restore pastes „OLD” instead.
    let mut cb = arboard::Clipboard::new().unwrap();
    let saved = cb.get_text().ok();
    for i in 0..10 {
        let _ = cb.set_text("OLD ");
        std::thread::sleep(Duration::from_millis(300));
        let r = p.paste_text(&format!("p{i} "));
        std::thread::sleep(Duration::from_millis(900));
        let s = vio_state(&c, &baseline);
        println!("paste {i}: {r:?} → Notatka = {:?}", element(&s.elements, "Notatka").map(|e| e["value"].clone()));
    }
    if let Some(t) = saved {
        let _ = cb.set_text(t);
    }
    let _ = close_suite_tabs(&c, &baseline);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_change_level_and_length_as_specified() {
        let base: Vec<f32> = (0..16_000).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let n = derive(&base, "noise");
        let added: Vec<f32> = n.iter().zip(&base).map(|(a, b)| a - b).collect();
        let snr = 20.0 * (rms(&base) / rms(&added)).log10();
        assert!((snr - 12.0).abs() < 0.5, "snr {snr}");
        assert!((rms(&derive(&base, "quiet")) / rms(&base) - 0.2).abs() < 0.01);
        let p = derive(&base, "pad");
        assert_eq!(p.len(), base.len() + 56_000);
        assert!(rms(&p[..24_000]) < 0.002 && p[24_000..40_000] == base[..]);
        assert_eq!(derive(&base, "noise"), n, "deterministic");
    }

    #[test]
    fn similarity_ignores_case_punctuation_and_diacritics() {
        assert_eq!(similarity("Następna piosenka.", "nastepna PIOSENKA"), 1.0);
        assert!(similarity("Otwórz Spotify", "otwórz spotifaj") > 0.8);
        assert!(similarity("Pauza", "Zamknij Discorda") < 0.3);
    }

    #[test]
    fn expectations_check_tools_in_order_and_page_state() {
        let r = Run {
            id: "x".into(),
            cap: String::new(),
            wav: PathBuf::new(),
            expect: json!({ "route": "command", "tools": [{ "name": "close_app", "args": { "name": "spotify" } }, { "name": "open_app" }],
                "forbid_tools": [{ "name": "open_app", "args": { "name": "spotify" } }],
                "vio": { "fields": { "Imię i nazwisko": "Jan Kowalski", "Telefon": { "digits": "600700800" }, "Hasło do konta": "" },
                         "checked": { "Duża": true }, "active_url": "n=3" } }),
            setup: json!({}),
            text: "Zamknij Spotify i otwórz Discorda".into(),
            timeout: Duration::ZERO,
            skip: None,
        };
        let ev = vec![
            json!({ "ev": "tool", "name": "close_app", "args": { "name": "Spotify" }, "ok": true }),
            json!({ "ev": "tool", "name": "open_app", "args": { "name": "Discord" }, "ok": true }),
        ];
        let vio = VioState {
            tabs: vec![],
            active: json!({ "url": "http://127.0.0.1/target.html?n=3", "title": "t" }),
            elements: vec![
                json!({ "label": "Imię i nazwisko", "value": "jan kowalski" }),
                json!({ "label": "Telefon", "value": "600 700 800" }),
                json!({ "label": "Hasło do konta", "value": null, "empty": true }),
                json!({ "label": "Duża", "checked": true }),
            ],
            page: String::new(),
        };
        let mut o = summarize(&r, &ev, &[]);
        o.route = "command".into();
        o.similarity = 1.0;
        assert_eq!(check(&r, &ev, Some(&vio), &o), Vec::<String>::new());

        let swapped = vec![ev[1].clone(), ev[0].clone()];
        assert_eq!(check(&r, &swapped, Some(&vio), &o).len(), 1, "order matters");
        let spotify = vec![ev[0].clone(), json!({ "ev": "tool", "name": "open_app", "args": { "name": "spotify" }, "ok": true })];
        assert!(check(&r, &spotify, Some(&vio), &o).iter().any(|n| n.starts_with("forbidden")));
        let mut wrong = vio;
        wrong.elements[1]["value"] = json!("600 700 801");
        wrong.elements[3]["checked"] = json!(false);
        assert_eq!(check(&r, &ev, Some(&wrong), &o).len(), 2);
    }

    #[test]
    fn rejected_means_nothing_ran_and_nothing_was_pasted() {
        let r = Run {
            id: "f".into(),
            cap: String::new(),
            wav: PathBuf::new(),
            expect: json!({ "route": "rejected" }),
            setup: json!({}),
            text: String::new(),
            timeout: Duration::ZERO,
            skip: None,
        };
        let o = Outcome::default();
        assert!(check(&r, &[json!({ "ev": "command_done", "ok": false, "result": "Nie zrozumiałem" })], None, &o).is_empty());
        assert_eq!(check(&r, &[json!({ "ev": "paste", "ok": true, "text": "Por favor" })], None, &o).len(), 1);
        assert_eq!(check(&r, &[json!({ "ev": "tool", "name": "youtube_search", "ok": true })], None, &o).len(), 1);
    }

    #[test]
    fn metrics_come_from_the_app_log() {
        let r = Run { id: String::new(), cap: String::new(), wav: PathBuf::new(), expect: json!({}), setup: json!({}), text: "x".into(), timeout: Duration::ZERO, skip: None };
        let logs: Vec<String> = [
            "[soul][agent] stage=request_sent model=gemini-a url=…",
            "[soul][agent] stage=first_byte ms=300 http=503",
            "[soul][agent] model gemini-a failed (Some(503)): x — cooling for 60 s",
            "[soul][agent] stage=request_sent model=gemini-b url=…",
            "[soul][agent] stage=first_byte ms=300 http=200",
            "[soul][e2e] {\"ev\":\"done\",\"ms\":4200}",
        ]
        .map(String::from)
        .to_vec();
        let o = summarize(&r, &e2e_events(&logs), &logs);
        assert_eq!((o.model_calls, o.total_ms, o.retries), (2, 4200, 1));
        assert_eq!(o.models, vec!["gemini-a", "gemini-b"]);
        assert_eq!(o.http.get("503"), Some(&1));
        assert_eq!(page_url(8, "notes?text=a%20b"), "http://127.0.0.1:8/notes.html?text=a%20b");
    }
}
