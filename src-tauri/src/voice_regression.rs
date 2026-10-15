//! Voice regression harness: recorded WAVs through the real Smart talk-key pipeline, tools dry-run.
//!
//!   SOUL_TEST_KEY=<key> npm run test:voice
//!   SOUL_TEST_KEY=<key> cargo test --manifest-path src-tauri/Cargo.toml --lib voice_regression -- --ignored --nocapture
//!
//! Every case in `tests/voice/cases.json` names a WAV (any sample rate, mono or stereo) and what
//! Soul should do with it. Like `lib.rs::on_smart_released`: the audio is streamed into the Live
//! dictation session as if the talk key were held, the transcript goes through
//! `agent::classify_smart`; dictation gets the local (+ LLM, for self-corrections) cleanup,
//! commands go to the pre-router, then the Live commands session (text turn + tools), then REST
//! tools. Tool calls are answered with a dry-run result and never executed.
//! The table is printed and written to `tests/voice/report.md`; any FAIL fails the test.
//!
//! Case: `{ "file": "x.wav", "expect": { "route": "command", "tool": "media_control",
//! "args": { "action": "next" }, "text_contains": "faktur" }, "synthetic": false }`.
//! Every `expect` key is optional; string args match when the actual value contains the expected
//! one (case- and diacritic-insensitive), `text_contains` takes a string or a list (diacritic-
//! insensitive), `text_equals` compares the whole text ignoring case and punctuation but NOT
//! diacritics ("Zażółć gęślą jaźń" ≠ "Zazolc gesla jazn").
//! Scenes come from `tests/voice/scenes.json` (in-memory DB; the user's own DB is never touched).
//!
//! Env: `SOUL_VOICE_FILTER` (substring of the file name), `SOUL_VOICE_DELAY_MS` (pause between
//! cases, default 4000 for the free tier), `SOUL_VOICE_REPORT` (report file name, default
//! report.md), `SOUL_TEST_LANGUAGE`, `SOUL_TEST_WAKE_WORD`, `SOUL_TEST_DICTATION_MODEL`,
//! `SOUL_TEST_COMMANDS_MODEL`, `SOUL_TEST_DICTATION_PROMPT=1` (send
//! `live::POLISH_TRANSCRIPTION_HINT` to the transcription session).

use crate::agent::{self, Intent};
use crate::ai::gemini::{
    agent_tool_declarations, build_agent_body_from_text, extract_function_calls,
    extract_text_from_value, FunctionCall, GeminiClient,
};
use crate::ai::live::{self, collect_dictation, LiveSession, TurnEvent};
use crate::config::Settings;
use crate::dev_samples::{read_cases, voice_test_dir};
use serde_json::{json, Value};
use std::io::Cursor;
use std::path::Path;
use std::time::{Duration, Instant};

/// Tool picked for a command (name + args), and which stage decided it.
type CommandDecision = (Option<(String, Value)>, &'static str);

const REPORT_FILE: &str = "report.md";
const SCENES_FILE: &str = "scenes.json";
/// 100 ms of 16 kHz s16le mono, as the PCM feeder sends it.
const CHUNK_BYTES: usize = 3_200;
/// Stream at 4× real time (the smoke test's pace) to keep runs short.
const CHUNK_PACE: Duration = Duration::from_millis(25);
/// Same budget as `agent::AGENT_LIVE_TIMEOUT` for the model to pick a tool.
const LIVE_DECISION_TIMEOUT: Duration = Duration::from_secs(6);
/// After the decision, let the spoken reply finish so the next turn starts on a quiet session.
const REPLY_DRAIN: Duration = Duration::from_secs(8);
const WARMUP: Duration = Duration::from_secs(3);

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn dictation_prompt() -> bool {
    std::env::var("SOUL_TEST_DICTATION_PROMPT").is_ok_and(|v| v == "1")
}

/// Fixture scenes → in-memory memory DB, so trigger phrases route like in the app.
fn load_scenes(dir: &Path) {
    crate::memory::init_in_memory();
    let Ok(raw) = std::fs::read_to_string(dir.join(SCENES_FILE)) else {
        return;
    };
    let scenes: Vec<crate::scenes::Scene> =
        serde_json::from_str(&raw).expect("tests/voice/scenes.json: list of scenes");
    for scene in scenes {
        crate::scenes::save(scene).expect("fixture scene");
    }
}

fn test_settings() -> Settings {
    let defaults = Settings::default();
    Settings {
        api_key: std::env::var("SOUL_TEST_KEY").expect("set SOUL_TEST_KEY"),
        language: env_or("SOUL_TEST_LANGUAGE", &defaults.language),
        wake_word: env_or("SOUL_TEST_WAKE_WORD", &defaults.wake_word),
        dictation_model: env_or("SOUL_TEST_DICTATION_MODEL", &defaults.dictation_model),
        commands_model: env_or("SOUL_TEST_COMMANDS_MODEL", &defaults.commands_model),
        ..defaults
    }
}

// ───────────────────────────── audio ─────────────────────────────

/// Any WAV → 16 kHz s16le mono PCM.
pub(crate) fn load_pcm16k(path: &Path) -> Result<Vec<u8>, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample.max(1) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        }
    };
    let channels = spec.channels.max(1) as usize;
    let mono: Vec<f32> = samples
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect();
    Ok(resample_to_pcm16k(&mono, spec.sample_rate))
}

/// Linear resample to 16 kHz, then s16le.
pub(crate) fn resample_to_pcm16k(mono: &[f32], rate: u32) -> Vec<u8> {
    let ratio = rate as f64 / 16_000.0;
    let n = (mono.len() as f64 / ratio) as usize;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let pos = i as f64 * ratio;
        let at = pos as usize;
        let frac = (pos - at as f64) as f32;
        let a = mono[at.min(mono.len() - 1)];
        let b = mono[(at + 1).min(mono.len() - 1)];
        let s = a + (b - a) * frac;
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
    }
    out
}

/// 16 kHz s16le mono PCM → WAV bytes (REST fallback, synthetic samples).
pub(crate) fn pcm16k_to_wav(pcm: &[u8]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = Cursor::new(Vec::new());
    {
        let mut w = hound::WavWriter::new(&mut buf, spec).expect("wav writer");
        for s in pcm.as_chunks::<2>().0 {
            w.write_sample(i16::from_le_bytes([s[0], s[1]])).expect("wav sample");
        }
        w.finalize().expect("wav finalize");
    }
    buf.into_inner()
}

/// Feed PCM like a held talk key; returns the "released" instant.
fn stream(session: &LiveSession, pcm: &[u8]) -> Instant {
    session.begin_turn();
    for chunk in pcm.chunks(CHUNK_BYTES) {
        session.push_audio(chunk.to_vec());
        std::thread::sleep(CHUNK_PACE);
    }
    Instant::now()
}

// ───────────────────────────── pipeline ─────────────────────────────

struct Run {
    transcript: String,
    /// "live" or "rest" (Live failed → WAV transcribed over REST, as in the app).
    stt: &'static str,
    stt_ms: u128,
    route: &'static str,
    reason: &'static str,
    /// Pasted text for dictation, command text for commands.
    text: String,
    tool: Option<(String, Value)>,
    /// pre-router | live | live-voice | rest
    via: &'static str,
    total_ms: u128,
}

struct Pipeline {
    settings: Settings,
    rest: GeminiClient,
    fallback: String,
    dictation: LiveSession,
    commands: LiveSession,
}

impl Pipeline {
    fn new(settings: Settings) -> Self {
        let rest = GeminiClient::new(&settings.api_key, settings.effective_ask_model());
        let fallback = settings.fallback_model.clone();
        let dictation = LiveSession::spawn("dictation");
        let commands = LiveSession::spawn("commands");
        if settings.use_live_api {
            let mut setup = live::dictation_setup(&settings);
            if dictation_prompt() {
                setup = setup.with_instruction(live::POLISH_TRANSCRIPTION_HINT);
            }
            dictation.configure(setup);
            commands.configure(live::commands_setup(&settings));
            std::thread::sleep(WARMUP);
        }
        Self {
            settings,
            rest,
            fallback,
            dictation,
            commands,
        }
    }

    fn run(&self, pcm: &[u8]) -> Result<Run, String> {
        let released = stream(&self.dictation, pcm);
        let live = self
            .dictation
            .end_turn(released)
            .map(|rx| collect_dictation(rx, released));
        self.dictation.finish_turn();
        let (transcript, stt) = match live {
            Some(Ok(r)) if !r.text.trim().is_empty() => (r.text, "live"),
            other => {
                let why = match other {
                    Some(Err(e)) => e,
                    Some(Ok(_)) => "empty Live transcript".into(),
                    None => "Live API off".into(),
                };
                eprintln!("[soul][dictation] REST fallback ({why})");
                let text = self.rest.transcribe_and_clean_resilient(
                    &pcm16k_to_wav(pcm),
                    &self.settings.language,
                    Some(&self.fallback),
                    |_, _| {},
                    |_| {},
                )?;
                (text, "rest")
            }
        };
        let stt_ms = released.elapsed().as_millis();

        let mut run = Run {
            transcript: transcript.clone(),
            stt,
            stt_ms,
            route: "dictation",
            reason: "",
            text: String::new(),
            tool: None,
            via: "",
            total_ms: 0,
        };
        // No focused app in the harness: the text-field signal is unknown, as on Linux.
        match agent::classify_smart(&transcript, &self.settings.wake_word, &|| None) {
            Intent::Command { text, reason } => {
                run.route = "command";
                run.reason = reason;
                let (tool, via) = self.command(&text)?;
                run.text = text;
                run.tool = tool;
                run.via = via;
            }
            Intent::Dictation => run.text = self.cleanup(&transcript, stt),
        }
        run.total_ms = released.elapsed().as_millis();
        Ok(run)
    }

    /// `paste_dictation` without the paste.
    fn cleanup(&self, heard: &str, stt: &str) -> String {
        if stt != "live" {
            return heard.to_string();
        }
        let cleaned = crate::ai::cleanup::local_cleanup(heard);
        if !crate::ai::cleanup::needs_llm_cleanup(&cleaned) {
            return cleaned;
        }
        match self.rest.cleanup_dictation_text(
            &cleaned,
            &self.settings.language,
            Some(&self.fallback),
            Duration::from_millis(1500),
        ) {
            Ok(out) if crate::ai::cleanup::plausible_cleanup(&cleaned, &out) => out,
            _ => cleaned,
        }
    }

    /// `agent::text_command` decision: pre-router → Live tools → REST tools. Nothing executes.
    fn command(&self, text: &str) -> Result<CommandDecision, String> {
        if text.trim().is_empty() {
            return Ok((None, "wake-word-only"));
        }
        if let Some((tool, args, reason)) = agent::preview_route(text) {
            eprintln!("[soul][agent] router=pre-router reason={reason} tool={tool} args={args} (dry-run)");
            return Ok((Some((tool, args)), "pre-router"));
        }
        if self.settings.use_live_api {
            if let Some(decision) = self.live_command(text) {
                return Ok(decision);
            }
            eprintln!("[soul][agent] Live gave no decision for text command — REST tools");
        }
        self.rest_command(text)
    }

    fn live_command(&self, text: &str) -> Option<(Option<(String, Value)>, &'static str)> {
        let (rx, id) = self.commands.begin_text_turn(text);
        let mut deadline = Instant::now() + LIVE_DECISION_TIMEOUT;
        let mut call: Option<FunctionCall> = None;
        let mut reply = String::new();
        while let Ok(ev) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            match ev.kind {
                TurnEvent::ToolCall(calls) => {
                    let responses = calls
                        .iter()
                        .map(|c| {
                            let mut r = json!({
                                "name": c.call.name,
                                "response": { "result": "Done (dry-run test, nothing was executed)." }
                            });
                            if let Some(id) = &c.id {
                                r["id"] = json!(id);
                            }
                            r
                        })
                        .collect();
                    self.commands.send_tool_responses(responses);
                    if call.is_none() {
                        eprintln!(
                            "[soul][agent] router=Live tool_call name={} args={} (dry-run)",
                            calls[0].call.name, calls[0].call.args
                        );
                        call = Some(calls[0].call.clone());
                        deadline = Instant::now() + REPLY_DRAIN;
                    }
                }
                TurnEvent::Text(t) | TurnEvent::OutputTranscript(t) => reply.push_str(&t),
                TurnEvent::Complete { after_end: true } if call.is_some() || !reply.trim().is_empty() => {
                    break
                }
                TurnEvent::Error(e) => {
                    eprintln!("[soul][live] commands error: {e}");
                    break;
                }
                _ => {}
            }
        }
        self.commands.finish_turn_id(id);
        match call {
            Some(c) => Some((Some((c.name, c.args)), "live")),
            None if !reply.trim().is_empty() => Some((
                Some(("answer".into(), json!({ "text": reply.trim() }))),
                "live-voice",
            )),
            None => None,
        }
    }

    fn rest_command(&self, text: &str) -> Result<CommandDecision, String> {
        let initial = build_agent_body_from_text(text, &self.settings.language)?;
        let mut body = json!({
            "contents": initial["contents"],
            "tools": [{ "function_declarations": agent_tool_declarations() }],
            "tool_config": { "function_calling_config": { "mode": "AUTO" } },
            "generation_config": { "temperature": 0.0, "thinkingConfig": { "thinkingBudget": 0 } }
        });
        if let Some(sys) = initial.get("system_instruction") {
            body["system_instruction"] = sys.clone();
        }
        let response =
            self.rest
                .agent_generate_resilient(body, Some(&self.fallback), |_, _| {}, |_| {})?;
        if let Some(c) = extract_function_calls(&response).into_iter().next() {
            eprintln!("[soul][agent] router=LLM(REST) tool_call name={} args={} (dry-run)", c.name, c.args);
            return Ok((Some((c.name, c.args)), "rest"));
        }
        let answer = extract_text_from_value(&response).unwrap_or_default();
        Ok((Some(("answer".into(), json!({ "text": answer }))), "rest"))
    }
}

// ───────────────────────────── expectations ─────────────────────────────

fn fold_text(s: &str) -> String {
    s.split_whitespace()
        .map(agent::fold_word)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lowercase words without punctuation; diacritics kept.
fn exact_text(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn value_matches(expected: &Value, actual: Option<&Value>) -> bool {
    let Some(actual) = actual else {
        return false;
    };
    match (expected, actual) {
        (Value::String(e), Value::String(a)) => fold_text(a).contains(&fold_text(e)),
        (Value::Number(e), Value::Number(a)) => e.as_f64() == a.as_f64(),
        // "30" vs 30: models are not always strict about types.
        (Value::Number(e), Value::String(a)) | (Value::String(a), Value::Number(e)) => {
            a.trim().parse::<f64>().ok() == e.as_f64()
        }
        (e, a) => e == a,
    }
}

/// Failure reasons (empty = pass).
fn check(expect: &Value, run: &Run) -> Vec<String> {
    let mut fails = Vec::new();
    if let Some(route) = expect["route"].as_str() {
        if route != run.route {
            fails.push(format!("route {} ≠ {route}", run.route));
        }
    }
    if let Some(tool) = expect["tool"].as_str() {
        let actual = run.tool.as_ref().map(|(n, _)| n.as_str()).unwrap_or("—");
        if actual != tool {
            fails.push(format!("tool {actual} ≠ {tool}"));
        }
    }
    if let Some(args) = expect["args"].as_object() {
        let actual = run.tool.as_ref().map(|(_, a)| a);
        for (k, v) in args {
            let got = actual.and_then(|a| a.get(k));
            if !value_matches(v, got) {
                fails.push(format!(
                    "args.{k} {} ≠ {v}",
                    got.map(|g| g.to_string()).unwrap_or("—".into())
                ));
            }
        }
    }
    let needles: Vec<&str> = match &expect["text_contains"] {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let text = if run.text.is_empty() { &run.transcript } else { &run.text };
    if let Some(want) = expect["text_equals"].as_str() {
        if exact_text(text) != exact_text(want) {
            fails.push(format!("text \u{201e}{text}\u{201d} ≠ \u{201e}{want}\u{201d}"));
        }
    }
    let hay = fold_text(text);
    for n in needles {
        if !hay.contains(&fold_text(n)) {
            fails.push(format!("text lacks \u{201e}{n}\u{201d}"));
        }
    }
    fails
}

fn has_expectations(expect: &Value) -> bool {
    expect.as_object().is_some_and(|o| !o.is_empty())
}

fn describe_expect(expect: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(r) = expect["route"].as_str() {
        parts.push(r.to_string());
    }
    if let Some(t) = expect["tool"].as_str() {
        let args = expect.get("args").map(|a| a.to_string()).unwrap_or_default();
        parts.push(format!("{t} {args}").trim().to_string());
    }
    match &expect["text_contains"] {
        Value::Null => {}
        v => parts.push(format!("text∋{v}")),
    }
    if let Some(t) = expect["text_equals"].as_str() {
        parts.push(format!("text=\u{201e}{t}\u{201d}"));
    }
    if parts.is_empty() {
        "—".into()
    } else {
        parts.join(" · ")
    }
}

// ───────────────────────────── report ─────────────────────────────

struct Row {
    file: String,
    synthetic: bool,
    transcript: String,
    route: String,
    tool: String,
    expected: String,
    status: &'static str,
    latency: String,
    notes: Vec<String>,
    total_ms: Option<u128>,
}

fn cell(s: &str, max: usize) -> String {
    let s: String = s.replace('|', "\\|").replace(['\n', '\r'], " ");
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    } else {
        s
    }
}

fn render(settings: &Settings, rows: &[Row], wall: Duration) -> String {
    let count = |s: &str| rows.iter().filter(|r| r.status == s).count();
    let mut lat: Vec<u128> = rows.iter().filter_map(|r| r.total_ms).collect();
    lat.sort_unstable();
    let median = lat.get(lat.len() / 2).map(|m| format!("{m} ms")).unwrap_or("—".into());
    let mut out = String::new();
    out.push_str("# Voice regression report\n\n");
    out.push_str(&format!(
        "{} · dictation `{}`{} · commands `{}` · REST `{}` → `{}` · language `{}` · wake word `{}` · tools **dry-run**\n\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M"),
        settings.dictation_model,
        if dictation_prompt() { " + Polish prompt" } else { "" },
        settings.commands_model,
        settings.effective_ask_model(),
        settings.fallback_model,
        settings.language,
        settings.wake_word,
    ));
    out.push_str(&format!(
        "**{} cases: {} PASS · {} FAIL · {} ERROR · {} SKIP** · median latency {median} · run took {} s\n\n",
        rows.len(),
        count("PASS"),
        count("FAIL"),
        count("ERROR"),
        count("SKIP"),
        wall.as_secs()
    ));
    out.push_str("Latency = talk key released → decision (tool chosen or dictation text ready); `stt` = transcript ready. *synthetic* = Gemini TTS sample.\n\n");
    out.push_str("| file | transcript | route | tool+args | expected | result | latency ms |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");
    for r in rows {
        let file = if r.synthetic {
            format!("{} *(synthetic)*", cell(&r.file, 60))
        } else {
            cell(&r.file, 60)
        };
        let status = match r.status {
            "PASS" => "✅ PASS",
            "FAIL" => "❌ FAIL",
            "ERROR" => "💥 ERROR",
            "SKIP" => "⏭️ SKIP",
            other => other,
        };
        out.push_str(&format!(
            "| {file} | {} | {} | {} | {} | {status} | {} |\n",
            cell(&r.transcript, 90),
            cell(&r.route, 40),
            cell(&r.tool, 90),
            cell(&r.expected, 70),
            cell(&r.latency, 30),
        ));
    }
    let noted: Vec<&Row> = rows.iter().filter(|r| !r.notes.is_empty()).collect();
    if !noted.is_empty() {
        out.push_str("\n## Details\n\n");
        for r in noted {
            out.push_str(&format!("- **{}** ({}): {}\n", r.file, r.status, r.notes.join("; ")));
        }
    }
    out
}

#[test]
#[ignore]
fn voice_regression() {
    let dir = voice_test_dir().expect("tests/voice");
    let cases = read_cases(&dir).expect("read tests/voice/cases.json");
    let filter = std::env::var("SOUL_VOICE_FILTER").unwrap_or_default();
    let delay = Duration::from_millis(env_or("SOUL_VOICE_DELAY_MS", "4000").parse().unwrap_or(4000));
    let cases: Vec<Value> = cases
        .into_iter()
        .filter(|c| c["file"].as_str().is_some_and(|f| f.contains(filter.as_str())))
        .collect();
    assert!(!cases.is_empty(), "no cases in {} (filter {filter:?})", dir.display());

    let settings = test_settings();
    std::env::set_var("SOUL_DRY_RUN", "1");
    load_scenes(&dir);
    let started = Instant::now();
    let pipeline = Pipeline::new(settings.clone());
    let mut rows = Vec::new();
    let mut ran_any = false;

    for case in &cases {
        let file = case["file"].as_str().unwrap_or_default().to_string();
        let expect = case.get("expect").cloned().unwrap_or(json!({}));
        let mut row = Row {
            file: file.clone(),
            synthetic: case["synthetic"].as_bool().unwrap_or(false),
            transcript: String::new(),
            route: String::new(),
            tool: "—".into(),
            expected: describe_expect(&expect),
            status: "SKIP",
            latency: String::new(),
            notes: Vec::new(),
            total_ms: None,
        };
        let path = dir.join(&file);
        if !path.exists() {
            row.notes.push("WAV not recorded yet".into());
            println!("[voice] {file}: SKIP (missing)");
            rows.push(row);
            continue;
        }
        if ran_any {
            std::thread::sleep(delay); // free-tier pacing
        }
        ran_any = true;
        println!("[voice] {file} …");
        let result = load_pcm16k(&path).and_then(|pcm| {
            if pcm.len() < CHUNK_BYTES {
                return Err("sample shorter than 100 ms".into());
            }
            pipeline.run(&pcm)
        });
        match result {
            Ok(run) => {
                row.transcript = run.transcript.clone();
                row.route = if run.reason.is_empty() {
                    run.route.to_string()
                } else {
                    format!("{} ({})", run.route, run.reason)
                };
                row.tool = match &run.tool {
                    Some((name, args)) => format!("{name} {args} [{}]", run.via),
                    None if run.route == "dictation" => format!("paste \u{201e}{}\u{201d}", run.text),
                    None => format!("— [{}]", run.via),
                };
                row.latency = format!("{} (stt {}{})", run.total_ms, run.stt_ms, if run.stt == "rest" { ", REST" } else { "" });
                row.total_ms = Some(run.total_ms);
                let fails = check(&expect, &run);
                row.status = if !has_expectations(&expect) {
                    row.notes.push("no expectations in cases.json".into());
                    "SKIP"
                } else if fails.is_empty() {
                    "PASS"
                } else {
                    row.notes.extend(fails);
                    "FAIL"
                };
            }
            Err(e) => {
                row.status = "ERROR";
                row.notes.push(e);
            }
        }
        println!(
            "[voice] {file}: {} transcript={:?} route={} tool={} latency={} {}",
            row.status,
            row.transcript,
            row.route,
            row.tool,
            row.latency,
            row.notes.join("; ")
        );
        rows.push(row);
    }

    let report = render(&settings, &rows, started.elapsed());
    let path = dir.join(env_or("SOUL_VOICE_REPORT", REPORT_FILE));
    std::fs::write(&path, &report).expect("write report.md");
    println!("\n{report}\nreport → {}", path.display());

    let bad: Vec<&str> = rows
        .iter()
        .filter(|r| matches!(r.status, "FAIL" | "ERROR"))
        .map(|r| r.file.as_str())
        .collect();
    assert!(bad.is_empty(), "voice regression failed: {bad:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(route: &'static str, text: &str, tool: Option<(&str, Value)>) -> Run {
        Run {
            transcript: text.into(),
            stt: "live",
            stt_ms: 0,
            route,
            reason: "",
            text: text.into(),
            tool: tool.map(|(n, a)| (n.to_string(), a)),
            via: "pre-router",
            total_ms: 0,
        }
    }

    #[test]
    fn expectations_match_loosely() {
        let r = run(
            "command",
            "Następna piosenka",
            Some(("media_control", json!({ "action": "next" }))),
        );
        let e = json!({ "route": "command", "tool": "media_control", "args": { "action": "next" } });
        assert!(check(&e, &r).is_empty());
        assert_eq!(check(&json!({ "route": "dictation" }), &r).len(), 1);
        assert_eq!(check(&json!({ "args": { "action": "previous" } }), &r).len(), 1);

        let r = run("command", "ścisz", Some(("set_volume", json!({ "level": "30", "q": "Lo-Fi Hip Hop" }))));
        assert!(check(&json!({ "args": { "level": 30, "q": "lofi" } }), &r).is_empty());
        assert!(check(&json!({ "args": { "level": 30, "q": "hip hop" } }), &r).is_empty());
        assert_eq!(check(&json!({ "args": { "q": "jazz" } }), &r).len(), 1);

        let r = run("dictation", "Zażółć, gęślą jaźń!", None);
        assert!(check(&json!({ "text_equals": "zażółć gęślą jaźń" }), &r).is_empty());
        assert_eq!(check(&json!({ "text_equals": "Zazolc gesla jazn" }), &r).len(), 1);

        let r = run("dictation", "Proszę o fakturę za październik.", None);
        assert!(check(&json!({ "text_contains": "faktur" }), &r).is_empty());
        assert!(check(&json!({ "text_contains": ["FAKTURE", "pazdziernik"] }), &r).is_empty());
        assert_eq!(check(&json!({ "text_contains": "rachunek" }), &r).len(), 1);
    }

    #[test]
    fn wav_roundtrip_resamples_to_16k_mono() {
        let dir = std::env::temp_dir().join(format!("soul-voice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stereo48k.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..48_000 {
            w.write_sample(8_000i16).unwrap();
            w.write_sample(8_000i16).unwrap();
        }
        w.finalize().unwrap();
        let pcm = load_pcm16k(&path).unwrap();
        assert_eq!(pcm.len(), 16_000 * 2);
        let first = i16::from_le_bytes([pcm[0], pcm[1]]);
        assert!((first - 8_000).abs() <= 1, "{first}");
        let wav = pcm16k_to_wav(&pcm);
        let back = hound::WavReader::new(Cursor::new(wav)).unwrap();
        assert_eq!(back.spec().sample_rate, 16_000);
        assert_eq!(back.len(), 16_000);
        let _ = std::fs::remove_dir_all(dir);
    }
}
