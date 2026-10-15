//! Opt-in end-to-end smoke test against the real Live API.
//!
//!   SOUL_TEST_KEY=<key> cargo test --lib live_smoke -- --ignored --nocapture
//!
//! Synthesizes Polish speech with a Gemini TTS model, streams it through `LiveSession` in
//! 100 ms chunks (as the PCM feeder does), then checks dictation + commands turns.

use super::live::*;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const TTS_MODEL: &str = "gemini-3.8-flash-lite-tts";

fn key() -> String {
    std::env::var("SOUL_TEST_KEY").expect("set SOUL_TEST_KEY")
}

/// Polish TTS → 16 kHz s16le mono PCM.
fn tts_pcm16k(text: &str) -> Vec<u8> {
    let body = json!({
        "contents": [{ "parts": [{ "text": format!("Powiedz naturalnie po polsku: {text}") }] }],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": { "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": "Kore" } } }
        }
    });
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{TTS_MODEL}:generateContent"
    );
    let resp: Value = reqwest::blocking::Client::new()
        .post(url)
        .timeout(Duration::from_secs(60))
        .header("x-goog-api-key", key())
        .json(&body)
        .send()
        .expect("tts request")
        .json()
        .expect("tts json");
    let part = resp
        .pointer("/candidates/0/content/parts/0/inlineData")
        .unwrap_or_else(|| panic!("no audio in TTS response: {}", resp.to_string().chars().take(300).collect::<String>()));
    let mime = part["mimeType"].as_str().unwrap_or("");
    let rate: u32 = mime
        .split("rate=")
        .nth(1)
        .and_then(|r| r.split(';').next())
        .and_then(|r| r.parse().ok())
        .unwrap_or(24_000);
    let raw = BASE64.decode(part["data"].as_str().unwrap()).unwrap();
    let samples: Vec<f32> = raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / i16::MAX as f32)
        .collect();
    let ratio = rate as f64 / 16_000.0;
    let n = (samples.len() as f64 / ratio) as usize;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let s = samples[((i as f64) * ratio) as usize];
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
    }
    println!("tts: {text:?} → {} ms of audio (src {rate} Hz)", out.len() / 32);
    out
}

/// Stream like a held hotkey: 100 ms chunks at ~real time (sped up 4× to keep the test short).
fn stream(session: &LiveSession, pcm: &[u8]) -> Instant {
    session.begin_turn();
    for chunk in pcm.chunks(3_200) {
        session.push_audio(chunk.to_vec());
        std::thread::sleep(Duration::from_millis(25));
    }
    Instant::now()
}

#[test]
#[ignore]
fn live_smoke() {
    let api_key = key();
    let phrase = "Spotkanie przenosimy na czwartek, nie, na piątek o dziesiątej. Zadzwoń też do Łukasza w sprawie faktury.";
    let pcm = tts_pcm16k(phrase);

    // --- Dictation (warm) ---
    let dict = LiveSession::spawn("dictation");
    let t0 = Instant::now();
    dict.configure(LiveSetup::dictation(&api_key, "gemini-3.5-transcribe-live", "auto"));
    std::thread::sleep(Duration::from_secs(3)); // let it warm
    println!("warm wait {} ms", t0.elapsed().as_millis());
    for round in 0..2 {
        let released = stream(&dict, &pcm);
        let rx = dict.end_turn(released).unwrap();
        let r = collect_dictation(rx, released);
        dict.finish_turn();
        println!("dictation[{round}] → {r:?}");
        let r = r.expect("dictation result");
        let cleaned = crate::ai::cleanup::local_cleanup(&r.text);
        println!(
            "  final_ms={} needs_llm_cleanup={}",
            r.final_ms,
            crate::ai::cleanup::needs_llm_cleanup(&cleaned)
        );
        assert!(r.text.to_lowercase().contains("piątek"), "Polish kept: {}", r.text);
    }

    // --- LLM cleanup latency (only used when a self-correction slips through SMART) ---
    let model = std::env::var("SOUL_TEST_CLEANUP_MODEL").unwrap_or("gemini-3.1-flash-lite".into());
    let client = crate::ai::gemini::GeminiClient::new(&api_key, &model);
    for input in ["Kup mleko, to znaczy kefir, i chleb.", "Wyślij to do Marka, a nie, do Kasi."] {
        let t = Instant::now();
        let out = client.cleanup_dictation_text(
            input,
            "auto",
            Some("gemini-3.5-flash-lite"),
            Duration::from_millis(1500),
        );
        println!("cleanup {input:?} → {out:?} in {} ms", t.elapsed().as_millis());
    }

    // --- Cold start: audio buffered while connecting ---
    let cold = LiveSession::spawn("dictation-cold");
    cold.configure(LiveSetup::dictation(&api_key, "gemini-3.5-transcribe-live", "pl"));
    let released = stream(&cold, &pcm);
    let r = collect_dictation(cold.end_turn(released).unwrap(), released);
    println!("dictation[cold] → {r:?}");
    assert!(r.is_ok());

    // --- Commands with tools ---
    let cmd = LiveSession::spawn("commands");
    let tools = json!([{ "functionDeclarations": crate::ai::gemini::agent_tool_declarations() }]);
    let system = crate::ai::gemini::agent_live_system_prompt("auto");
    cmd.configure(LiveSetup::commands(&api_key, "gemini-3.8-live", &system, Some(tools), ""));
    std::thread::sleep(Duration::from_secs(3));
    for phrase in ["Otwórz notatnik.", "Zamknij Spotify."] {
        let pcm = tts_pcm16k(phrase);
        let released = stream(&cmd, &pcm);
        let rx = cmd.end_turn(released).unwrap();
        let deadline = released + Duration::from_secs(8);
        let mut transcript = String::new();
        let mut got_tool = None;
        while let Ok(ev) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            let ms = ev.at.saturating_duration_since(released).as_millis();
            match ev.kind {
                TurnEvent::Transcript(t) => {
                    merge_transcript(&mut transcript, &t);
                    println!("  +{ms}ms transcript {t:?}");
                }
                TurnEvent::ToolCall(calls) => {
                    println!("  +{ms}ms toolCall {:?}", calls.iter().map(|c| (&c.call.name, &c.call.args, &c.id)).collect::<Vec<_>>());
                    let responses = calls
                        .iter()
                        .map(|c| json!({ "id": c.id, "name": c.call.name, "response": { "result": "ok (test)" } }))
                        .collect();
                    cmd.send_tool_responses(responses);
                    got_tool = Some(calls[0].call.name.clone());
                }
                TurnEvent::OutputTranscript(t) => println!("  +{ms}ms spoken {t:?}"),
                TurnEvent::Complete { after_end } => {
                    println!("  +{ms}ms complete after_end={after_end}");
                    if after_end && got_tool.is_some() {
                        break;
                    }
                }
                TurnEvent::Error(e) => panic!("commands error: {e}"),
                other => println!("  +{ms}ms {other:?}"),
            }
        }
        cmd.finish_turn();
        println!("commands {phrase:?} → transcript={transcript:?} tool={got_tool:?}");
        assert!(got_tool.is_some(), "expected a tool call for {phrase}");
    }
}
