//! Voice-command agent mode: hold hotkey → transcript → pre-router / Gemini tools → Platform.
//! Replies are spoken through the Live session's native audio (see `voice.rs`).

mod intent;
mod router;

pub use intent::{classify_with, Intent};
pub(crate) use router::fold_word;

use crate::ai::gemini::{
    agent_tool_declarations, build_agent_body_from_text, extract_function_calls,
    extract_model_content, extract_text_from_value, GeminiClient, SOUL_DONE_MARKER,
};
use crate::ai::live::{self, merge_transcript, LiveEvent, LiveFunctionCall, LivePool, TurnEvent};
use crate::config::load_settings;
use crate::voice::VoicePlayer;
use crate::{
    emit_overlay, report_error, resolve_fallback_model, retire_model_and_heal, schedule_hide,
    AppState, OverlayPayload,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const MAX_AGENT_ROUNDS: u32 = 6;
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(30);
/// Spoken reply: give up when the session is silent this long…
const VOICE_IDLE: Duration = Duration::from_secs(6);
/// …or when one reply runs longer than this.
const VOICE_MAX: Duration = Duration::from_secs(60);

pub struct AgentState {
    /// When set, overlay Confirm/Cancel should send true/false here.
    pub pending_confirm: Mutex<Option<Sender<bool>>>,
    /// Commands Live turn whose spoken reply is still streaming/playing (barge-in cancels it).
    pub speaking_turn: Mutex<Option<u64>>,
}

pub fn new_agent_state() -> AgentState {
    AgentState {
        pending_confirm: Mutex::new(None),
        speaking_turn: Mutex::new(None),
    }
}

#[tauri::command]
pub fn agent_confirm(app: AppHandle, accepted: bool) -> Result<(), String> {
    let state = app.state::<AppState>();
    let tx = state.agent.pending_confirm.lock().take();
    drop(state);
    match tx {
        Some(tx) => {
            let _ = tx.send(accepted);
            Ok(())
        }
        None => Err("No pending confirmation.".into()),
    }
}

/// Talk key pressed again while Soul is speaking: stop the audio now and drop the rest of the
/// reply (the Live turn is cancelled, so its late output is ignored).
pub fn barge_in(app: &AppHandle) {
    let state = app.state::<AppState>();
    let was_speaking = state.voice.stop();
    let turn = state.agent.speaking_turn.lock().take();
    if let Some(id) = turn {
        state.live.commands.cancel_turn_id(id);
    }
    if was_speaking || turn.is_some() {
        eprintln!("[soul][voice] barge-in was_speaking={was_speaking} turn={turn:?}");
    }
}

/// Smart talk key routing: a scene trigger phrase ("gramy w FiveM") is always a command,
/// everything else goes through the dictation/command classifier (`text_field_focused`: see
/// `intent::classify_with`).
pub fn classify_smart(
    transcript: &str,
    wake_word: &str,
    text_field_focused: &dyn Fn() -> Option<bool>,
) -> Intent {
    if crate::scenes::match_trigger(transcript).is_some() {
        return Intent::Command {
            text: transcript.trim().to_string(),
            reason: "scene-trigger",
        };
    }
    classify_with(transcript, wake_word, text_field_focused)
}

/// Deterministic routing: user scene triggers first, then the Polish/English pre-router.
fn local_route(text: &str) -> Option<router::PreRoute> {
    if let Some(scene) = crate::scenes::match_trigger(text) {
        return Some(router::PreRoute {
            tool: "run_scene".into(),
            args: json!({ "name": scene.name }),
            reason: "scene-trigger".into(),
        });
    }
    router::match_command(text)
}

/// Auto-remember: a durable fact stated in a command is saved with an undo chip on the Island.
fn auto_remember(app: &AppHandle, text: &str) {
    let first = text.split_whitespace().next().map(fold_word).unwrap_or_default();
    if matches!(first.as_str(), "zapamietaj" | "zanotuj" | "remember" | "zapomnij" | "forget") {
        return; // explicit memory commands are handled by their own tools
    }
    if !load_settings(app).map(|s| s.auto_remember).unwrap_or(false) {
        return;
    }
    let Some((fact, category)) = crate::memory::extract_fact(text) else {
        return;
    };
    match crate::memory::add_fact(&fact, category, "auto") {
        Ok((f, true)) => {
            eprintln!("[soul][memory] auto-remembered id={} category={category}", f.id);
            crate::memory_changed(app);
            crate::island::toast(
                app,
                "memory",
                &format!("Zapamiętałem: {}", f.text),
                Some(crate::island::ToastAction {
                    label: "Cofnij".into(),
                    command: "memory_delete".into(),
                    args: json!({ "id": f.id }),
                }),
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("[soul][memory] auto-remember skipped: {e}"),
    }
}

/// Speak a short notice (timer finished…) through the Live voice when Soul is idle.
pub fn speak_notice(app: &AppHandle, text: &str) {
    let app = app.clone();
    let text = text.to_string();
    thread::spawn(move || {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let Ok(settings) = load_settings(&app) else {
            return;
        };
        if !settings.voice_enabled
            || !crate::live_enabled_for(&settings, &settings.commands_model)
            || *state.busy.lock()
            || state.recorder.is_recording()
        {
            return;
        }
        let live = Arc::clone(&state.live);
        live.commands.configure(live::commands_setup(&settings));
        let (rx, id) = live
            .commands
            .begin_text_turn(&format!("{SOUL_DONE_MARKER} notification for the user: {text}"));
        let mut voice = Voice::new(&app, Instant::now());
        voice.fallback_text = text.clone();
        *state.agent.speaking_turn.lock() = Some(id);
        drain_voice(&live, &rx, &mut voice, "Skipped: this was only a notification.");
        {
            let mut slot = state.agent.speaking_turn.lock();
            if *slot == Some(id) {
                *slot = None;
            }
        }
        live.commands.finish_turn_id(id);
        if voice.started {
            schedule_hide(&app, 1_200);
        }
    });
}

pub fn on_agent_pressed(app: &AppHandle) {
    let state = app.state::<AppState>();
    barge_in(app);
    if *state.busy.lock() {
        return;
    }
    if crate::ask::is_active(&state.ask) {
        return;
    }
    if state.recorder.is_recording() {
        return;
    }
    match crate::begin_capture(app, crate::CaptureTarget::Commands, false) {
        Ok(_) => {}
        Err(e) => {
            report_error(app, e);
            schedule_hide(app, 8000);
        }
    }
}

pub fn on_agent_released(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.recorder.is_recording() {
        return;
    }
    {
        let mut busy = state.busy.lock();
        if *busy {
            return;
        }
        *busy = true;
    }

    let released = Instant::now();
    let wav = state.recorder.stop();
    let live_events = state.live.commands.end_turn(released);
    let turn_id = state.live.commands.last_turn_id();
    eprintln!(
        "[soul][agent] stage=recorder_stopped ms={} live={}",
        released.elapsed().as_millis(),
        live_events.is_some()
    );
    if live_events.is_none() {
        if let Err(e) = &wav {
            *state.busy.lock() = false;
            report_error(app, e.clone());
            schedule_hide(app, 8000);
            return;
        }
    }

    let _ = emit_overlay(app, "thinking", None);

    let app_handle = app.clone();
    let platform = Arc::clone(&state.platform);
    let live = Arc::clone(&state.live);

    thread::spawn(move || {
        let mut voice = Voice::new(&app_handle, released);
        let outcome = run_agent_session(
            &app_handle,
            &platform,
            &live,
            live_events.map(|rx| (rx, turn_id)),
            wav,
            released,
            &mut voice,
        );
        finish_command(&app_handle, &live, voice, outcome, released);
    });
}

/// Smart talk key: the transcript was routed to commands. Pre-router first, then the Live
/// commands session as a text turn (spoken reply), then REST tools.
pub fn run_text_command(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    live: &Arc<LivePool>,
    text: &str,
    released: Instant,
) {
    let mut voice = Voice::new(app, released);
    let outcome = text_command(app, platform, live, text.trim(), released, &mut voice);
    finish_command(app, live, voice, outcome, released);
}

fn text_command(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    live: &Arc<LivePool>,
    text: &str,
    released: Instant,
    voice: &mut Voice,
) -> CommandOutcome {
    if text.is_empty() {
        return CommandOutcome::done(Err(
            "I heard the wake word but no command. Try \u{201e}Soul, otwórz Spotify\u{201d}.".into(),
        ));
    }
    let settings = match load_settings(app) {
        Ok(s) => s,
        Err(e) => return CommandOutcome::done(Err(e)),
    };
    let live_ok = crate::live_enabled_for(&settings, &settings.commands_model);
    let _ = emit_overlay(app, "thinking", Some(shorten(text, 80)));
    auto_remember(app, text);

    if let Some(route) = local_route(text) {
        let result = run_route(app, platform, &route);
        if !(voice.enabled && live_ok) {
            return CommandOutcome::done(result);
        }
        // Executed locally; let the Live voice confirm it in the chosen personality.
        let summary = result.clone().unwrap_or_else(|e| format!("Error: {e}"));
        live.commands.configure(live::commands_setup(&settings));
        let (rx, id) = live.commands.begin_text_turn(&format!(
            "{SOUL_DONE_MARKER} user said: \u{201e}{text}\u{201d}; Soul ran {} {} → {summary}",
            route.tool, route.args
        ));
        return CommandOutcome {
            result,
            tail: Some((rx, id)),
            handled: format!("Handled by Soul's local router: {summary}"),
        };
    }

    if live_ok {
        live.commands.configure(live::commands_setup(&settings));
        let (rx, id) = live.commands.begin_text_turn(text);
        match run_agent_live(app, platform, live, &rx, released, text, voice) {
            LiveAgentOutcome::Done(result) => {
                let handled = result
                    .clone()
                    .map(|r| format!("Skipped: the command was already handled ({r})."))
                    .unwrap_or_else(|e| format!("Skipped: the command already failed ({e})."));
                return CommandOutcome {
                    result,
                    tail: Some((rx, id)),
                    handled,
                };
            }
            LiveAgentOutcome::TranscriptOnly(_) | LiveAgentOutcome::Failed(_) => {
                live.commands.finish_turn_id(id);
                eprintln!("[soul][agent] Live gave no decision for text command — REST tools");
            }
        }
    }

    let client = GeminiClient::new(&settings.api_key, settings.effective_ask_model());
    let fallback = resolve_fallback_model(&settings, &crate::ai::gemini::cached_models());
    eprintln!("[soul][agent] router=LLM(REST) transcript={text:?}");
    CommandOutcome::done(run_agent_rest(app, platform, &client, &fallback, text, released))
}

/// Result of one command + the Live turn that may still be speaking its reply.
struct CommandOutcome {
    result: Result<String, String>,
    tail: Option<(Receiver<LiveEvent>, u64)>,
    /// toolResponse text for tool calls that arrive while we only wait for speech.
    handled: String,
}

impl CommandOutcome {
    fn done(result: Result<String, String>) -> Self {
        Self {
            result,
            tail: None,
            handled: String::new(),
        }
    }
}

/// Show the result, release `busy`, then let the spoken reply play out (barge-in aware).
fn finish_command(
    app: &AppHandle,
    live: &Arc<LivePool>,
    mut voice: Voice,
    outcome: CommandOutcome,
    released: Instant,
) {
    eprintln!(
        "[soul][live] commands total ms={} ok={}",
        released.elapsed().as_millis(),
        outcome.result.is_ok()
    );
    match &outcome.result {
        Ok(msg) if voice.started => {
            let shown = if voice.spoken.trim().is_empty() {
                msg.clone()
            } else {
                voice.spoken.clone()
            };
            let _ = emit_overlay(app, "speaking", Some(shorten(&shown, 400)));
        }
        Ok(msg) => {
            let _ = emit_overlay(app, "done", Some(msg.clone()));
        }
        Err(e) => report_error(app, e.clone()),
    }
    voice.fallback_text = outcome.result.clone().unwrap_or_default();
    if let Some(state) = app.try_state::<AppState>() {
        *state.busy.lock() = false;
        *state.agent.pending_confirm.lock() = None;
    }

    if let Some((rx, id)) = outcome.tail {
        if voice.enabled && !voice.barged() {
            if let Some(state) = app.try_state::<AppState>() {
                *state.agent.speaking_turn.lock() = Some(id);
            }
            drain_voice(live, &rx, &mut voice, &outcome.handled);
            if let Some(state) = app.try_state::<AppState>() {
                let mut slot = state.agent.speaking_turn.lock();
                if *slot == Some(id) {
                    *slot = None;
                }
            }
        }
        live.commands.finish_turn_id(id);
    }

    let hide_ms = match (&outcome.result, voice.started) {
        (Err(_), _) => 10_000,
        (Ok(_), true) => 1_200,
        (Ok(_), false) => 2_500,
    };
    schedule_hide(app, hide_ms);
}

/// Plays the spoken reply of one Live turn and mirrors its transcript on the Island.
pub(crate) struct Voice {
    app: AppHandle,
    player: Arc<VoicePlayer>,
    generation: u64,
    enabled: bool,
    started: bool,
    spoken: String,
    /// Shown while speaking when the output transcription is empty.
    fallback_text: String,
    released: Instant,
    bytes: usize,
}

impl Voice {
    fn new(app: &AppHandle, released: Instant) -> Self {
        let state = app.state::<AppState>();
        let player = Arc::clone(&state.voice);
        let enabled = load_settings(app).map(|s| s.voice_enabled).unwrap_or(true);
        Self {
            app: app.clone(),
            generation: player.generation(),
            player,
            enabled,
            started: false,
            spoken: String::new(),
            fallback_text: String::new(),
            released,
            bytes: 0,
        }
    }

    fn audio(&mut self, pcm: Vec<u8>) {
        if !self.enabled || self.barged() {
            return;
        }
        if !self.started {
            self.started = true;
            eprintln!(
                "[soul][voice] first_audio ms={} (since release)",
                self.released.elapsed().as_millis()
            );
            self.show();
        }
        self.bytes += pcm.len();
        self.player.push(self.generation, pcm);
    }

    fn text(&mut self, piece: &str) {
        merge_transcript(&mut self.spoken, piece);
        if self.started && !self.barged() {
            self.show();
        }
    }

    fn show(&self) {
        let text = if self.spoken.trim().is_empty() {
            &self.fallback_text
        } else {
            &self.spoken
        };
        let msg = (!text.trim().is_empty()).then(|| shorten(text, 400));
        let _ = emit_overlay(&self.app, "speaking", msg);
    }

    fn barged(&self) -> bool {
        self.player.generation() != self.generation
    }
}

/// Keep reading the turn after the command finished: play audio, answer late tool calls, stop on
/// turn end / error / silence / barge-in, then wait until the queued audio has played.
fn drain_voice(live: &Arc<LivePool>, rx: &Receiver<LiveEvent>, voice: &mut Voice, handled: &str) {
    let t0 = Instant::now();
    let mut last_event = Instant::now();
    let mut why = "turn_end";
    loop {
        if voice.barged() {
            why = "barge_in";
            break;
        }
        if t0.elapsed() > VOICE_MAX {
            why = "max_duration";
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ev) => {
                last_event = Instant::now();
                match ev.kind {
                    TurnEvent::Audio(pcm) => voice.audio(pcm),
                    TurnEvent::OutputTranscript(t) => voice.text(&t),
                    TurnEvent::ToolCall(calls) => {
                        let names: Vec<&str> = calls.iter().map(|c| c.call.name.as_str()).collect();
                        eprintln!("[soul][agent] late tool_call {names:?} answered as handled");
                        live.commands
                            .send_tool_responses(skipped_responses(&calls, handled));
                    }
                    TurnEvent::Error(_) => {
                        why = "turn_error";
                        break;
                    }
                    _ => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if last_event.elapsed() > VOICE_IDLE {
                    why = "idle";
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    while voice.player.is_speaking() && !voice.barged() && t0.elapsed() < VOICE_MAX {
        thread::sleep(Duration::from_millis(50));
    }
    eprintln!(
        "[soul][voice] reply done why={why} audio_ms={} total_ms={} since_release_ms={}",
        voice.bytes / 48, // 24 kHz × 2 bytes = 48 bytes per ms
        t0.elapsed().as_millis(),
        voice.released.elapsed().as_millis()
    );
}

/// Why the Live path stopped; decides whether REST may retry the command.
enum LiveAgentOutcome {
    /// Finished (tool executed or answer shown) — never re-run on REST.
    Done(Result<String, String>),
    /// Live produced a transcript but no decision in time — run REST tools on the transcript.
    TranscriptOnly(String),
    /// Nothing usable from Live — transcribe the WAV over REST.
    Failed(String),
}

const AGENT_LIVE_TIMEOUT: Duration = Duration::from_secs(6);
/// After a toolCall, how long to wait for a still-missing input transcript (pre-router needs it).
const TRANSCRIPT_GRACE: Duration = Duration::from_millis(400);
/// After generationComplete without a toolCall, wait this long for a racing toolCall.
const COMPLETE_GRACE: Duration = Duration::from_millis(250);

fn run_agent_session(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    live: &Arc<LivePool>,
    live_turn: Option<(Receiver<LiveEvent>, u64)>,
    wav: Result<Vec<u8>, String>,
    released: Instant,
    voice: &mut Voice,
) -> CommandOutcome {
    let settings = match load_settings(app) {
        Ok(s) => s,
        Err(e) => return CommandOutcome::done(Err(e)),
    };
    let rest_primary = settings.effective_ask_model().to_string();
    let client = GeminiClient::new(&settings.api_key, &rest_primary);
    let fallback = resolve_fallback_model(&settings, &crate::ai::gemini::cached_models());

    let transcript = match live_turn {
        Some((rx, id)) => match run_agent_live(app, platform, live, &rx, released, "", voice) {
            LiveAgentOutcome::Done(result) => {
                let handled = result
                    .clone()
                    .map(|r| format!("Skipped: the command was already handled ({r})."))
                    .unwrap_or_else(|e| format!("Skipped: the command already failed ({e})."));
                return CommandOutcome {
                    result,
                    tail: Some((rx, id)),
                    handled,
                };
            }
            LiveAgentOutcome::TranscriptOnly(t) => {
                live.commands.finish_turn_id(id);
                eprintln!("[soul][agent] Live gave no decision — REST tools on transcript");
                t
            }
            LiveAgentOutcome::Failed(e) => {
                live.commands.finish_turn_id(id);
                eprintln!("[soul][agent] Live failed: {e} — REST transcribe");
                if crate::live_failure_is_model_gone(&e) {
                    let app = app.clone();
                    thread::spawn(move || crate::revalidate_models(&app));
                }
                match rest_transcribe(app, &client, &fallback, &settings.language, wav) {
                    Ok(t) => t,
                    Err(e) => return CommandOutcome::done(Err(e)),
                }
            }
        },
        None => match rest_transcribe(app, &client, &fallback, &settings.language, wav) {
            Ok(t) => t,
            Err(e) => return CommandOutcome::done(Err(e)),
        },
    };

    let transcript = transcript.trim().to_string();
    eprintln!(
        "[soul][agent] transcript={:?} ms={}",
        transcript,
        released.elapsed().as_millis()
    );
    if transcript.is_empty() {
        return CommandOutcome::done(Err("I didn't catch that. Try again.".into()));
    }
    auto_remember(app, &transcript);
    if let Some(result) = try_pre_router(app, platform, &transcript) {
        return CommandOutcome::done(result);
    }
    eprintln!("[soul][agent] router=LLM(REST) transcript={transcript:?}");
    CommandOutcome::done(run_agent_rest(
        app,
        platform,
        &client,
        &fallback,
        &transcript,
        released,
    ))
}

fn rest_transcribe(
    app: &AppHandle,
    client: &GeminiClient,
    fallback: &str,
    language: &str,
    wav: Result<Vec<u8>, String>,
) -> Result<String, String> {
    let wav = wav?;
    let _ = emit_overlay(app, "processing", None);
    let app_retire = app.clone();
    client.transcribe_and_clean_resilient(
        &wav,
        language,
        Some(fallback),
        |st, msg| {
            let _ = emit_overlay(app, st, msg);
        },
        move |retired| {
            retire_model_and_heal(&app_retire, retired);
        },
    )
}

/// Deterministic Polish/English pre-router. Some(result) when it handled the command.
fn try_pre_router(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    transcript: &str,
) -> Option<Result<String, String>> {
    let route = local_route(transcript)?;
    Some(run_route(app, platform, &route))
}

fn run_route(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    route: &router::PreRoute,
) -> Result<String, String> {
    let args_preview = serde_json::to_string(&route.args).unwrap_or_default();
    eprintln!(
        "[soul][agent] router=pre-router reason={} tool={} args={}",
        route.reason, route.tool, args_preview
    );
    let _ = emit_overlay(app, "thinking", None);
    let result = execute_tool(app, platform, &route.tool, &route.args);
    match &result {
        Ok(r) => eprintln!("[soul][agent] tool_result name={} result={}", route.tool, r),
        Err(e) => eprintln!("[soul][agent] tool_result name={} error={}", route.tool, e),
    }
    result.map(|r| shorten(&r, 500))
}

/// Drive one Live command turn: pre-router first (on the final transcript), else execute the
/// model's tool call and send the toolResponse. Reply audio is played as it arrives.
/// `known_text` pre-fills the transcript for text turns (no input transcription comes back).
fn run_agent_live(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    live: &Arc<LivePool>,
    rx: &Receiver<LiveEvent>,
    released: Instant,
    known_text: &str,
    voice: &mut Voice,
) -> LiveAgentOutcome {
    let deadline = released + AGENT_LIVE_TIMEOUT;
    let mut transcript = known_text.to_string();
    let mut interim = String::new();
    let mut answer = String::new();

    let heard = |transcript: &str, interim: &str| -> String {
        let mut t = transcript.to_string();
        if !interim.trim().is_empty() {
            merge_transcript(&mut t, interim);
        }
        t.trim().to_string()
    };

    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        let ev = match rx.recv_timeout(wait) {
            Ok(ev) => ev,
            Err(RecvTimeoutError::Timeout) => {
                let t = heard(&transcript, &interim);
                return if t.is_empty() {
                    LiveAgentOutcome::Failed("Live command timed out".into())
                } else {
                    LiveAgentOutcome::TranscriptOnly(t)
                };
            }
            Err(RecvTimeoutError::Disconnected) => {
                let t = heard(&transcript, &interim);
                if voice.started && !voice.spoken.trim().is_empty() {
                    // The model answered by voice and closed the turn.
                    return LiveAgentOutcome::Done(Ok(shorten(&voice.spoken, 200)));
                }
                return if t.is_empty() {
                    LiveAgentOutcome::Failed("Live session ended".into())
                } else {
                    LiveAgentOutcome::TranscriptOnly(t)
                };
            }
        };
        match ev.kind {
            TurnEvent::Interim(t) => interim = t,
            TurnEvent::Transcript(t) => {
                merge_transcript(&mut transcript, &t);
                interim.clear();
                // Pre-router BEFORE the model: a final post-release segment with nothing pending
                // is the complete utterance — route media/volume/search without waiting.
                if ev.at >= released {
                    if let Some(result) = try_pre_router(app, platform, transcript.trim()) {
                        log_live_final(app, released, &transcript, "pre-router");
                        return LiveAgentOutcome::Done(result);
                    }
                }
            }
            TurnEvent::Text(t) => answer.push_str(&t),
            TurnEvent::OutputTranscript(t) => voice.text(&t),
            TurnEvent::Audio(pcm) => voice.audio(pcm),
            TurnEvent::ToolCall(calls) => {
                // Make sure the pre-router sees the transcript before trusting the model.
                if transcript.trim().is_empty() {
                    let grace = Instant::now() + TRANSCRIPT_GRACE;
                    while let Ok(ev) =
                        rx.recv_timeout(grace.saturating_duration_since(Instant::now()))
                    {
                        match ev.kind {
                            TurnEvent::Transcript(t) => {
                                merge_transcript(&mut transcript, &t);
                                interim.clear();
                                break;
                            }
                            TurnEvent::Interim(t) => interim = t,
                            TurnEvent::Audio(pcm) => voice.audio(pcm),
                            TurnEvent::OutputTranscript(t) => voice.text(&t),
                            _ => {}
                        }
                    }
                }
                let text = heard(&transcript, &interim);
                let try_router = known_text.is_empty();
                return handle_live_calls(app, platform, live, &calls, &text, released, try_router);
            }
            TurnEvent::ToolCancel(ids) => {
                eprintln!("[soul][agent] Live cancelled tool calls {ids:?}");
            }
            TurnEvent::Complete { after_end: true } => {
                // generationComplete can race the toolCall message — give it a moment.
                let grace = Instant::now() + COMPLETE_GRACE;
                while let Ok(ev) =
                    rx.recv_timeout(grace.saturating_duration_since(Instant::now()))
                {
                    match ev.kind {
                        TurnEvent::ToolCall(calls) => {
                            let text = heard(&transcript, &interim);
                            let try_router = known_text.is_empty();
                            return handle_live_calls(
                                app, platform, live, &calls, &text, released, try_router,
                            );
                        }
                        TurnEvent::Transcript(t) => {
                            merge_transcript(&mut transcript, &t);
                            interim.clear();
                        }
                        TurnEvent::Text(t) => answer.push_str(&t),
                        TurnEvent::OutputTranscript(t) => voice.text(&t),
                        TurnEvent::Audio(pcm) => voice.audio(pcm),
                        _ => {}
                    }
                }
                let text = heard(&transcript, &interim);
                log_live_final(app, released, &text, "turn_complete");
                if !text.is_empty() && known_text.is_empty() {
                    if let Some(result) = try_pre_router(app, platform, &text) {
                        return LiveAgentOutcome::Done(result);
                    }
                }
                let reply = if !answer.trim().is_empty() {
                    answer
                } else {
                    voice.spoken.clone()
                };
                if !reply.trim().is_empty() {
                    return LiveAgentOutcome::Done(Ok(shorten(&reply, 200)));
                }
                return if text.is_empty() {
                    LiveAgentOutcome::Failed("Live returned nothing".into())
                } else {
                    LiveAgentOutcome::TranscriptOnly(text)
                };
            }
            TurnEvent::Complete { after_end: false } => {}
            TurnEvent::Error(e) => {
                let text = heard(&transcript, &interim);
                // Audio already reached the model and was transcribed: don't re-transcribe.
                return if text.is_empty() {
                    LiveAgentOutcome::Failed(e)
                } else {
                    LiveAgentOutcome::TranscriptOnly(text)
                };
            }
        }
    }
}

/// Pre-router first (unless the caller already tried it), then the model's call. Every call id
/// gets a toolResponse.
fn handle_live_calls(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    live: &Arc<LivePool>,
    calls: &[LiveFunctionCall],
    heard: &str,
    released: Instant,
    try_router: bool,
) -> LiveAgentOutcome {
    log_live_final(app, released, heard, "tool_call");
    if try_router && !heard.is_empty() {
        if let Some(result) = try_pre_router(app, platform, heard) {
            let summary = result.clone().unwrap_or_else(|e| format!("Error: {e}"));
            live.commands.send_tool_responses(skipped_responses(
                calls,
                &format!("Handled by Soul's local router: {summary}"),
            ));
            return LiveAgentOutcome::Done(result);
        }
    }
    LiveAgentOutcome::Done(execute_live_calls(app, platform, live, calls))
}

fn log_live_final(app: &AppHandle, released: Instant, transcript: &str, via: &str) {
    eprintln!(
        "[soul][live] commands final ms={} via={via} transcript={:?}",
        released.elapsed().as_millis(),
        transcript
    );
    if !transcript.trim().is_empty() {
        auto_remember(app, transcript);
    }
}

/// Execute the model's tool call(s) — one action per command — and answer every call id.
fn execute_live_calls(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    live: &Arc<LivePool>,
    calls: &[LiveFunctionCall],
) -> Result<String, String> {
    let Some(first) = calls.first() else {
        return Err("Live returned an empty tool call.".into());
    };
    if calls.len() > 1 {
        eprintln!(
            "[soul][agent] warning: Live returned {} tools — executing only the first ({})",
            calls.len(),
            first.call.name
        );
    }
    let args_preview = serde_json::to_string(&first.call.args).unwrap_or_default();
    eprintln!(
        "[soul][agent] router=Live tool_call name={} args={}",
        first.call.name, args_preview
    );
    let result = execute_tool(app, platform, &first.call.name, &first.call.args);
    match &result {
        Ok(r) => eprintln!("[soul][agent] tool_result name={} result={}", first.call.name, r),
        Err(e) => eprintln!("[soul][agent] tool_result name={} error={}", first.call.name, e),
    }

    let mut responses = vec![function_response(
        first,
        match &result {
            Ok(r) => json!({ "result": r }),
            Err(e) => json!({ "error": e }),
        },
    )];
    responses.extend(skipped_responses(
        &calls[1..],
        "Skipped: only one tool is allowed per command.",
    ));
    live.commands.send_tool_responses(responses);
    result.map(|r| shorten(&r, 500))
}

fn function_response(call: &LiveFunctionCall, response: Value) -> Value {
    let mut r = json!({ "name": call.call.name, "response": response });
    if let Some(id) = &call.id {
        r["id"] = json!(id);
    }
    r
}

fn skipped_responses(calls: &[LiveFunctionCall], why: &str) -> Vec<Value> {
    calls
        .iter()
        .map(|c| function_response(c, json!({ "result": why })))
        .collect()
}

fn run_agent_rest(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    client: &GeminiClient,
    fallback: &str,
    transcript: &str,
    released: Instant,
) -> Result<String, String> {
    let settings = load_settings(app)?;
    let initial = build_agent_body_from_text(transcript, &settings.language)?;
    let system_instruction = initial.get("system_instruction").cloned();
    let mut contents = initial
        .get("contents")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut last_confirmation = String::from("Done");

    for round in 0..MAX_AGENT_ROUNDS {
        eprintln!(
            "[soul][agent] stage=gemini_round round={} ms={}",
            round,
            released.elapsed().as_millis()
        );
        let _ = emit_overlay(app, "thinking", None);

        let mut body = json!({
            "contents": contents,
            "tools": [{ "function_declarations": agent_tool_declarations() }],
            "tool_config": {
                "function_calling_config": { "mode": "AUTO" }
            },
            "generation_config": {
                "temperature": 0.0,
                "thinkingConfig": { "thinkingBudget": 0 }
            }
        });
        if let Some(sys) = &system_instruction {
            body["system_instruction"] = sys.clone();
        }

        let app_retire = app.clone();
        let response = client.agent_generate_resilient(
            body,
            Some(fallback),
            |st, msg| {
                let _ = emit_overlay(app, st, msg);
            },
            move |retired| {
                retire_model_and_heal(&app_retire, retired);
            },
        )?;

        let calls = extract_function_calls(&response);
        if calls.is_empty() {
            if let Ok(text) = extract_text_from_value(&response) {
                return Ok(shorten(&text, 160));
            }
            return Ok(last_confirmation);
        }

        let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
        eprintln!(
            "[soul][agent] router=LLM tool_calls={:?} round={}",
            names, round
        );

        let model_content = extract_model_content(&response)
            .ok_or_else(|| "Gemini returned function calls without content.".to_string())?;
        contents.push(model_content);

        let mut function_parts = Vec::new();
        let mut only_answers = true;

        let calls_to_run: Vec<_> = if calls.len() > 1 {
            eprintln!(
                "[soul][agent] warning: model returned {} tools — executing only the first ({})",
                calls.len(),
                calls[0].name
            );
            calls.iter().take(1).collect()
        } else {
            calls.iter().collect()
        };

        for call in calls_to_run {
            let args_preview = serde_json::to_string(&call.args).unwrap_or_default();
            eprintln!(
                "[soul][agent] tool_call name={} args={}",
                call.name, args_preview
            );

            let result = execute_tool(app, platform, &call.name, &call.args)?;
            eprintln!(
                "[soul][agent] tool_result name={} result={}",
                call.name, result
            );

            if call.name == "answer" {
                last_confirmation = result.clone();
            } else {
                only_answers = false;
                last_confirmation = result.clone();
            }

            function_parts.push(json!({
                "functionResponse": {
                    "name": call.name,
                    "response": { "result": result }
                }
            }));
        }

        for call in calls.iter().skip(1) {
            function_parts.push(json!({
                "functionResponse": {
                    "name": call.name,
                    "response": {
                        "result": "Skipped: only one tool is allowed per command."
                    }
                }
            }));
        }

        contents.push(json!({
            "role": "user",
            "parts": function_parts
        }));

        if only_answers {
            return Ok(shorten(&last_confirmation, 200));
        }
    }

    Ok(last_confirmation)
}

fn shorten(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

/// `SOUL_DRY_RUN=1`: tools are logged, never executed (pipeline checks without side effects).
pub(crate) fn dry_run() -> bool {
    static DRY_RUN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DRY_RUN.get_or_init(|| {
        std::env::var("SOUL_DRY_RUN").is_ok_and(|v| !v.is_empty() && v != "0")
    })
}

/// Deterministic route (scene trigger / pre-router) for `text` without executing it:
/// `(tool, args, reason)`. Used by the voice regression harness.
#[cfg(test)]
pub(crate) fn preview_route(text: &str) -> Option<(String, Value, String)> {
    local_route(text).map(|r| (r.tool, r.args, r.reason))
}

/// Runs a registered tool and mirrors its lifecycle as an Island tool chip.
fn execute_tool(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    if dry_run() {
        eprintln!("[soul][agent] dry_run tool={name} args={args} (not executed)");
        return Ok(format!("dry-run: {name} was not executed"));
    }
    // `answer` is a spoken reply, not an action — no chip.
    let chip = name != "answer";
    if chip {
        crate::island::emit_tool(app, name, "start");
    }
    let result = run_tool(app, platform, name, args);
    if chip {
        crate::island::emit_tool(app, name, if result.is_ok() { "ok" } else { "error" });
    }
    result
}

fn run_tool(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    match name {
        "open_app" => {
            let app_name = arg_str(args, "name")?;
            let result = platform.open_app(&app_name);
            if result.is_ok() {
                crate::scenes::note_app_opened(app, &app_name);
            }
            result
        }
        "open_url" => {
            let url = arg_str(args, "url")?;
            platform.open_url(&url)
        }
        "web_search" => {
            let query = arg_str(args, "query")?;
            web_search_with_answer(app, platform, &query)
        }
        "youtube_search" => {
            let query = arg_str(args, "query")?;
            platform.youtube_search(&query)
        }
        "youtube_play" => {
            let query = arg_str(args, "query")?;
            platform.youtube_play(&query)
        }
        "close_app" => {
            let app_name = arg_str(args, "name")?;
            if !request_confirm(app, &format!("Close {app_name}?"))? {
                return Ok(format!("Cancelled closing {app_name}"));
            }
            let _ = emit_overlay(app, "thinking", Some("Closing…".into()));
            platform.close_app(&app_name)
        }
        "focus_app" => {
            let app_name = arg_str(args, "name")?;
            platform.focus_app(&app_name)
        }
        "set_volume" => {
            let percent = arg_u8(args, "percent")?;
            platform.set_volume(percent)
        }
        "media_control" => {
            let action = arg_str(args, "action")?;
            platform.media_control(&action)
        }
        "type_text" => {
            let text = arg_str(args, "text")?;
            platform.paste_text(&text)?;
            Ok("Typed text".into())
        }
        "answer" => arg_str(args, "text"),
        "set_voice_volume" => set_voice_volume(app, args),
        "remember" => {
            let fact = arg_str(args, "fact")?;
            let category = args
                .get("category")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| crate::memory::guess_category(&fact).to_string());
            let (f, new) = crate::memory::add_fact(&fact, &category, "voice")?;
            crate::memory_changed(app);
            crate::island::toast(
                app,
                "memory",
                &format!("Zapamiętałem: {}", f.text),
                Some(crate::island::ToastAction {
                    label: "Cofnij".into(),
                    command: "memory_delete".into(),
                    args: json!({ "id": f.id }),
                }),
            );
            Ok(if new {
                format!("Zapamiętano: {}", f.text)
            } else {
                format!("Już to pamiętam: {}", f.text)
            })
        }
        "forget" => {
            let query = arg_str(args, "query")?;
            let gone = crate::memory::forget(&query)?;
            crate::island::refresh_plans(app);
            crate::memory_changed(app);
            Ok(if gone.is_empty() {
                format!("Nie pamiętam nic o „{query}”.")
            } else {
                format!(
                    "Zapomniałem: {}",
                    gone.iter().map(|f| f.text.as_str()).collect::<Vec<_>>().join("; ")
                )
            })
        }
        "recall" => {
            let query = args.get("query").and_then(Value::as_str).unwrap_or_default();
            let facts = crate::memory::search(query, 8)?;
            Ok(if facts.is_empty() {
                "Nic o tym nie pamiętam.".into()
            } else {
                format!(
                    "Pamiętam: {}",
                    facts.iter().map(|f| f.text.as_str()).collect::<Vec<_>>().join("; ")
                )
            })
        }
        "add_note" => {
            let text = arg_str(args, "text")?;
            let (note, _) = crate::memory::add_fact(&text, crate::memory::NOTE_CATEGORY, "voice")?;
            crate::island::refresh_plans(app);
            crate::memory_changed(app);
            crate::island::toast(
                app,
                "note",
                &format!("Notatka: {}", note.text),
                Some(crate::island::ToastAction {
                    label: "Cofnij".into(),
                    command: "memory_delete".into(),
                    args: json!({ "id": note.id }),
                }),
            );
            Ok(format!("Zanotowano: {}", note.text))
        }
        "set_timer" => {
            let seconds = args
                .get("seconds")
                .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
                .or_else(|| args.get("seconds").and_then(Value::as_str).and_then(|s| s.trim().parse().ok()))
                .ok_or_else(|| "Missing tool argument 'seconds'".to_string())?;
            let label = args.get("label").and_then(Value::as_str).unwrap_or_default();
            let t = crate::timers::add(app, seconds, label)?;
            let dur = crate::timers::human_duration(t.total_ms);
            Ok(if t.label.is_empty() {
                format!("Timer na {dur} ustawiony.")
            } else {
                format!("Timer „{}” na {dur} ustawiony.", t.label)
            })
        }
        "cancel_timer" => {
            let label = args.get("label").and_then(Value::as_str);
            let n = crate::timers::cancel(app, None, label);
            Ok(if n == 0 {
                "Nie ma aktywnych timerów.".into()
            } else {
                format!("Anulowano timery: {n}.")
            })
        }
        "run_scene" => {
            let name = arg_str(args, "name")?;
            crate::scenes::run_by_name(app, platform, &name)
        }
        "create_scene" => {
            let scene = crate::scenes::save(crate::scenes::from_args(args)?)?;
            let _ = app.emit("soul://scenes-changed", ());
            Ok(format!(
                "Utworzono scenę „{}” ({} kroków). Powiedz „uruchom scenę {}”.",
                scene.name,
                scene.steps.len(),
                scene.name
            ))
        }
        "now_playing" => Ok(now_playing_text()),
        "morning_briefing" => Ok(morning_briefing()),
        other => Err(format!("Unknown tool '{other}' (not allowed).")),
    }
}

/// Question searches ("jak…", "co to…") get a short spoken + Island answer (Gemini with Google
/// Search grounding); the browser opens too when "Otwieraj przeglądarkę przy wyszukiwaniu" is on.
fn web_search_with_answer(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    query: &str,
) -> Result<String, String> {
    let settings = load_settings(app)?;
    if !router::is_question(query) || settings.api_key.trim().is_empty() {
        return platform.web_search(query);
    }
    let _ = emit_overlay(app, "thinking", Some(format!("Szukam: {}", shorten(query, 60))));
    let opened = settings.search_open_browser && platform.web_search(query).is_ok();
    let client = GeminiClient::new(&settings.api_key, settings.effective_ask_model());
    let fallback = resolve_fallback_model(&settings, &crate::ai::gemini::cached_models());
    let t0 = Instant::now();
    match client.search_answer(query, &settings.language, Some(&fallback)) {
        Ok(answer) => {
            eprintln!(
                "[soul][agent] search_answer ms={} chars={} browser={opened}",
                t0.elapsed().as_millis(),
                answer.chars().count()
            );
            Ok(answer)
        }
        Err(e) => {
            eprintln!("[soul][agent] search_answer failed: {e}");
            if opened {
                Ok(format!("Otworzyłem wyniki wyszukiwania: {query}"))
            } else {
                platform.web_search(query)
            }
        }
    }
}

/// "mów ciszej / głośniej", "wycisz się": Soul's own voice volume, persisted in settings.
fn set_voice_volume(app: &AppHandle, args: &Value) -> Result<String, String> {
    let mut settings = load_settings(app)?;
    let current = settings.voice_volume as i64;
    let next = if let Some(p) = args.get("percent").filter(|v| !v.is_null()) {
        p.as_i64()
            .or_else(|| p.as_f64().map(|f| f as i64))
            .or_else(|| p.as_str().and_then(|s| s.trim().trim_end_matches('%').parse().ok()))
            .ok_or_else(|| "Invalid 'percent' value".to_string())?
    } else {
        let delta = args.get("delta").and_then(Value::as_i64).unwrap_or(-20);
        current + delta
    }
    .clamp(0, 100) as u8;
    settings.voice_volume = next;
    crate::config::save_settings(app, &settings)?;
    if let Some(state) = app.try_state::<AppState>() {
        state.voice.set_volume(next);
    }
    let _ = app.emit("soul://voice-volume", next);
    eprintln!("[soul][voice] volume {current} -> {next}");
    Ok(if next == 0 {
        "Wyciszyłem swój głos — odpowiedzi zobaczysz na wyspie.".into()
    } else {
        format!("Głośność mojego głosu: {next}%.")
    })
}

fn now_playing_text() -> String {
    match crate::island::now_playing_brief() {
        Some((title, artist, playing)) => {
            let track = if artist.trim().is_empty() {
                title
            } else {
                format!("{title} – {artist}")
            };
            if playing {
                format!("Teraz gra: {track}.")
            } else {
                format!("Wstrzymane: {track}.")
            }
        }
        None => "Teraz nic nie gra.".into(),
    }
}

const WEEKDAYS: [&str; 7] = ["poniedziałek", "wtorek", "środa", "czwartek", "piątek", "sobota", "niedziela"];
const MONTHS: [&str; 12] = [
    "stycznia", "lutego", "marca", "kwietnia", "maja", "czerwca", "lipca", "sierpnia", "września",
    "października", "listopada", "grudnia",
];

/// "dzień dobry": time, date, running timers, recent notes and the current track (Polish).
fn morning_briefing() -> String {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let mut out = format!(
        "Dzień dobry! Jest {}:{:02}, {}, {} {}.",
        now.hour(),
        now.minute(),
        WEEKDAYS[now.weekday().num_days_from_monday() as usize],
        now.day(),
        MONTHS[now.month0() as usize]
    );
    let timers = crate::timers::list();
    let now_ms = chrono::Utc::now().timestamp_millis();
    if !timers.is_empty() {
        let list: Vec<String> = timers
            .iter()
            .take(3)
            .map(|t| {
                let left = crate::timers::human_duration(t.ends_at_ms - now_ms);
                if t.label.is_empty() {
                    format!("timer za {left}")
                } else {
                    format!("{} za {left}", t.label)
                }
            })
            .collect();
        out.push_str(&format!(" Timery: {}.", list.join(", ")));
    }
    let notes = crate::memory::list(Some(crate::memory::NOTE_CATEGORY)).unwrap_or_default();
    if !notes.is_empty() {
        let list: Vec<&str> = notes.iter().take(3).map(|n| n.text.as_str()).collect();
        out.push_str(&format!(" Twoje plany i notatki: {}.", list.join("; ")));
    }
    if timers.is_empty() && notes.is_empty() {
        out.push_str(" Nie masz żadnych timerów ani notatek.");
    }
    if crate::island::now_playing_brief().is_some() {
        out.push(' ');
        out.push_str(&now_playing_text());
    }
    out
}

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("Missing tool argument '{key}'"))
}

fn arg_u8(args: &Value, key: &str) -> Result<u8, String> {
    let v = args
        .get(key)
        .ok_or_else(|| format!("Missing tool argument '{key}'"))?;
    if let Some(n) = v.as_u64() {
        return Ok(n.min(100) as u8);
    }
    if let Some(n) = v.as_i64() {
        return Ok(n.clamp(0, 100) as u8);
    }
    if let Some(n) = v.as_f64() {
        return Ok(n.clamp(0.0, 100.0) as u8);
    }
    if let Some(s) = v.as_str() {
        let n: u8 = s
            .trim()
            .trim_end_matches('%')
            .parse()
            .map_err(|_| format!("Invalid '{key}' value"))?;
        return Ok(n.min(100));
    }
    Err(format!("Invalid '{key}' value"))
}

fn request_confirm(app: &AppHandle, message: &str) -> Result<bool, String> {
    let (tx, rx): (Sender<bool>, Receiver<bool>) = mpsc::channel();
    {
        let state = app.state::<AppState>();
        *state.agent.pending_confirm.lock() = Some(tx);
    }
    let _ = emit_overlay(app, "confirm", Some(message.to_string()));
    if let Some(win) = app.get_webview_window("overlay") {
        let _ = win.set_size(tauri::Size::Physical(tauri::PhysicalSize::new(360, 100)));
    }

    let accepted = match rx.recv_timeout(CONFIRM_TIMEOUT) {
        Ok(v) => v,
        Err(_) => {
            if let Some(state) = app.try_state::<AppState>() {
                *state.agent.pending_confirm.lock() = None;
            }
            false
        }
    };

    let _ = app.emit(
        "overlay://state",
        OverlayPayload {
            state: "thinking".into(),
            message: None,
        },
    );
    Ok(accepted)
}
