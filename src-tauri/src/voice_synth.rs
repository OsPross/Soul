//! Synthetic voice regression samples from Gemini TTS (opt-in, uses quota):
//!
//!   SOUL_TEST_KEY=<key> npm run test:voice:synth
//!
//! Reads `tests/voice/synth.json` — `[{ "id": "nastepna", "text": "Następna piosenka",
//! "expect": { … }, "voices": ["Kore", "Puck"], "speeds": ["normal", "fast"] }]` — and writes
//! one `synth_<id>_<voice>_<speed>.wav` (16 kHz mono) per voice × speed, then adds/updates the
//! matching `cases.json` entries with `"synthetic": true`. Existing WAVs are kept unless
//! `SOUL_SYNTH_FORCE=1`, so a run interrupted by a 429 can simply be repeated.

use crate::dev_samples::{read_cases, voice_test_dir, write_cases};
use crate::voice_e2e::speak;
use crate::voice_regression::pcm16k_to_wav;
use serde_json::{json, Value};
use std::time::Duration;

const SYNTH_FILE: &str = "synth.json";
const DEFAULT_VOICES: &[&str] = &["Kore", "Puck"];
const DEFAULT_SPEEDS: &[&str] = &["normal", "fast"];

fn strings(v: &Value, default: &[&str]) -> Vec<String> {
    match v.as_array() {
        Some(a) if !a.is_empty() => a.iter().filter_map(Value::as_str).map(String::from).collect(),
        _ => default.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
#[ignore]
fn voice_synth() {
    let dir = voice_test_dir().expect("tests/voice");
    let raw = std::fs::read_to_string(dir.join(SYNTH_FILE)).expect("read tests/voice/synth.json");
    let phrases: Vec<Value> = serde_json::from_str(&raw).expect("synth.json must be a JSON array");
    let force = std::env::var("SOUL_SYNTH_FORCE").is_ok_and(|v| v == "1");
    let delay = Duration::from_millis(
        std::env::var("SOUL_VOICE_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4000),
    );
    let mut cases = read_cases(&dir).expect("read cases.json");
    let mut generated = 0;
    let mut exhausted = Vec::new();

    for p in &phrases {
        let id = p["id"].as_str().expect("synth entry needs an id");
        let text = p["text"].as_str().expect("synth entry needs a text");
        for voice in strings(&p["voices"], DEFAULT_VOICES) {
            for speed in strings(&p["speeds"], DEFAULT_SPEEDS) {
                let file = format!(
                    "synth_{id}_{}_{}.wav",
                    voice.to_ascii_lowercase(),
                    speed.to_ascii_lowercase()
                );
                let path = dir.join(&file);
                if force || !path.exists() {
                    if generated > 0 {
                        std::thread::sleep(delay); // free-tier pacing
                    }
                    let (pcm, source) = speak(text, &voice, &speed, &mut exhausted);
                    std::fs::write(&path, pcm16k_to_wav(&pcm)).expect("write wav");
                    generated += 1;
                    println!("[synth] {file} ← {source}: {text:?}");
                }
                let case = json!({
                    "file": file,
                    "expect": p.get("expect").cloned().unwrap_or(json!({})),
                    "synthetic": true,
                    "note": format!("Gemini TTS voice {voice}, speed {speed}: {text}"),
                });
                match cases.iter_mut().find(|c| c["file"] == json!(file)) {
                    Some(existing) => *existing = case,
                    None => cases.push(case),
                }
            }
        }
    }
    write_cases(&dir, &cases).expect("write cases.json");
    println!("[synth] generated {generated} new sample(s); cases.json now has {} case(s)", cases.len());
}
