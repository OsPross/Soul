//! Voice-command agent mode: hold hotkey → transcript → pre-router / Gemini tools → Platform.
//! Replies are spoken through the Live session's native audio (see `voice.rs`).

mod chain;
pub(crate) mod context;
mod intent;
mod router;
pub(crate) mod text_action;

pub use intent::{classify_with, Intent};
pub(crate) use router::fold_word;

use crate::ai::gemini::{
    agent_tool_declarations, build_agent_body_with_context, extract_function_calls,
    extract_model_content, extract_text_from_value, GeminiClient, SOUL_DONE_MARKER,
};
use chain::MAX_ACTIONS;
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
    let tx = app.state::<AppState>().agent.pending_confirm.lock().take();
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

/// Default `context_timeout_secs`.
pub const CONTEXT_TIMEOUT_SECS: u32 = context::DEFAULT_TIMEOUT_SECS;

/// The conversation `[CONTEXT]` block while context is alive (Live audio preamble).
pub fn context_block(settings: &crate::config::Settings) -> Option<String> {
    context::active_block(settings)
}

pub fn with_context_block(context: &str, command: &str) -> String {
    context::with_block(context, command)
}

/// Whether follow-ups currently route to commands (Smart classification).
pub fn context_active(settings: &crate::config::Settings) -> bool {
    context::is_active(settings)
}

/// Smart talk key routing: a scene trigger phrase ("gramy w FiveM") is always a command,
/// everything else goes through the dictation/command classifier (`text_field_focused`: see
/// `intent::classify_with`). Then three extras that would otherwise be dictated: a selected-text
/// action („popraw to”), a context reset („nowy temat”), and — only while conversation context
/// is alive — a short follow-up („a teraz głośniej”, „a jutro?”).
pub fn classify_smart(
    transcript: &str,
    wake_word: &str,
    text_field_focused: &dyn Fn() -> Option<bool>,
    context_active: bool,
) -> Intent {
    let text = transcript.trim();
    if crate::scenes::match_trigger(text).is_some() {
        return Intent::Command {
            text: text.to_string(),
            reason: "scene-trigger",
        };
    }
    let intent = classify_with(text, wake_word, text_field_focused);
    if intent != Intent::Dictation {
        return intent;
    }
    let command = |reason| Intent::Command {
        text: text.to_string(),
        reason,
    };
    if text_action::parse(text).is_some() {
        return command("text-action");
    }
    if context::is_reset_phrase(text) {
        return command("context-reset");
    }
    // The text-field probe (~150 ms on Windows) only when it can change the answer.
    if context_active
        && context::looks_like_followup(text, false)
        && (context::looks_like_followup(text, true) || text_field_focused() != Some(true))
    {
        return command("followup");
    }
    Intent::Dictation
}

/// A chain part that stands on its own as a command (verb, pre-router or scene trigger).
fn part_is_command(part: &str, scenes: bool) -> bool {
    intent::starts_with_command_verb(part)
        || router::match_command(part).is_some()
        || (scenes && crate::scenes::match_trigger(part).is_some())
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
        drain_voice(&live, &rx, &mut voice, "Skipped: this was only a notification.", None);
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
        let turn = context::begin_turn();
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
        finish_command(&app_handle, &platform, &live, voice, outcome, released, turn);
    });
}

/// Smart talk key: the transcript was routed to commands. Local dispatch first (context reset,
/// text action, follow-up, chain, pre-router), then the Live commands session as a text turn
/// (spoken reply), then REST tools.
pub fn run_text_command(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    live: &Arc<LivePool>,
    text: &str,
    released: Instant,
) {
    let turn = context::begin_turn();
    let mut voice = Voice::new(app, released);
    let outcome = text_command(app, platform, live, text.trim(), released, &mut voice);
    finish_command(app, platform, live, voice, outcome, released, turn);
}

/// A command Soul handled without the model.
struct Local {
    result: Result<String, String>,
    /// What ran, for the Live voice confirmation (`[SOUL_DONE] …`).
    summary: String,
    /// Let the Live voice confirm it. Never for text actions: their text stays out of Live.
    speak: bool,
}

/// Deterministic handling, in order: context reset („nowy temat”), selected-text action,
/// follow-up on the previous tool („a teraz głośniej”), chained commands where every part is
/// deterministic, then the single-command pre-router. `None` = the model decides.
fn local_dispatch(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    text: &str,
) -> Option<Local> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if context::is_reset_phrase(text) {
        context::clear(app);
        return Some(Local {
            result: Ok("Okej, nowy temat.".into()),
            summary: "conversation context cleared (new topic)".into(),
            speak: true,
        });
    }
    if let Some(action) = text_action::parse(text) {
        return Some(Local {
            result: text_action::run(app, platform, &action),
            summary: String::new(),
            speak: false,
        });
    }
    let settings = load_settings(app).unwrap_or_default();
    if let Some(last) = context::last_tool(&settings) {
        if let Some(route) = context::resolve_followup(text, &last) {
            let result = run_route(app, platform, &route);
            return Some(Local {
                summary: route_summary(&route, &result),
                result,
                speak: true,
            });
        }
    }
    if let Some(parts) = chain::split_with(text, &|p| part_is_command(p, true)) {
        let steps: Option<Vec<router::PreRoute>> = parts
            .iter()
            .take(MAX_ACTIONS)
            .map(|p| local_route(p).or_else(|| router::match_app_verb(p)))
            .collect();
        let Some(steps) = steps else {
            // Not every part is deterministic: the model gets the whole utterance (it may
            // return several function calls).
            eprintln!("[soul][agent] chain parts={} → model", parts.len());
            return None;
        };
        let skipped = parts.len().saturating_sub(MAX_ACTIONS);
        let result = run_chain(app, platform, &steps, skipped);
        let summary = result.clone().unwrap_or_else(|e| format!("Error: {e}"));
        return Some(Local {
            summary: format!("Soul ran {} actions → {summary}", steps.len()),
            result,
            speak: true,
        });
    }
    let route = local_route(text)?;
    let result = run_route(app, platform, &route);
    Some(Local {
        summary: route_summary(&route, &result),
        result,
        speak: true,
    })
}

fn route_summary(route: &router::PreRoute, result: &Result<String, String>) -> String {
    let outcome = result.clone().unwrap_or_else(|e| format!("Error: {e}"));
    format!("Soul ran {} {} → {outcome}", route.tool, route.args)
}

/// Run chain steps in order; stop at the first failure (or a cancelled close).
/// Returns the combined Island line „✓ Spotify zamknięty · ✓ Discord otwarty”.
fn run_chain(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    steps: &[router::PreRoute],
    skipped: usize,
) -> Result<String, String> {
    let tools: Vec<&str> = steps.iter().map(|s| s.tool.as_str()).collect();
    eprintln!("[soul][agent] router=chain steps={} tools={tools:?} skipped={skipped}", steps.len());
    let mut done = Vec::new();
    let mut left = skipped;
    let mut failed = false;
    for (i, step) in steps.iter().enumerate() {
        match run_route(app, platform, step) {
            Ok(msg) if step.tool == "close_app" && msg.starts_with("Cancelled") => {
                done.push((false, format!("Anulowano: {}", step.args["name"].as_str().unwrap_or("zamykanie"))));
                left += steps.len() - i - 1;
                break;
            }
            Ok(msg) => done.push((true, chain::step_label(&step.tool, &step.args, &msg))),
            Err(e) => {
                done.push((false, failed_label(&step.tool, &step.args, &e)));
                left += steps.len() - i - 1;
                failed = true;
                break;
            }
        }
    }
    let line = chain::combine(&done, left);
    if failed {
        Err(line)
    } else {
        Ok(line)
    }
}

fn failed_label(tool: &str, args: &Value, error: &str) -> String {
    let what = args
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| tool.to_string());
    format!("{what}: {}", shorten(error, 80))
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
    context::note_user(text);
    auto_remember(app, text);

    // The previous exchanges (taken before this command adds its own).
    let ctx = context::active_block(&settings);
    if let Some(local) = local_dispatch(app, platform, text) {
        if !(voice.enabled && live_ok && local.speak) {
            return CommandOutcome::done(local.result);
        }
        // Executed locally; let the Live voice confirm it in the chosen personality.
        live.commands.configure(live::commands_setup(&settings));
        let (rx, id) = live.commands.begin_text_turn(&format!(
            "{SOUL_DONE_MARKER} user said: \u{201e}{text}\u{201d}; {}",
            local.summary
        ));
        return CommandOutcome {
            handled: format!("Handled by Soul's local router: {}", local.summary),
            result: local.result,
            tail: Some((rx, id)),
            late: None,
        };
    }

    if live_ok {
        live.commands.configure(live::commands_setup(&settings));
        let turn_text = match &ctx {
            Some(block) => context::with_block(block, text),
            None => text.to_string(),
        };
        let (rx, id) = live.commands.begin_text_turn(&turn_text);
        match run_agent_live(app, platform, live, &rx, released, text, voice) {
            LiveAgentOutcome::Done(result, late) => {
                return CommandOutcome::live(result, (rx, id), late);
            }
            LiveAgentOutcome::Quiet(result) => {
                live.commands.cancel_turn_id(id);
                return CommandOutcome::done(result);
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
    /// The model ran these calls (name, args): its further calls in the same turn are the
    /// rest of a chained command and still run (up to `MAX_ACTIONS` in all).
    late: Option<Vec<(String, Value)>>,
}

impl CommandOutcome {
    fn done(result: Result<String, String>) -> Self {
        Self {
            result,
            tail: None,
            handled: String::new(),
            late: None,
        }
    }

    /// A Live turn decided the command; its reply may still be streaming.
    fn live(
        result: Result<String, String>,
        tail: (Receiver<LiveEvent>, u64),
        late: Option<Vec<(String, Value)>>,
    ) -> Self {
        let handled = result
            .clone()
            .map(|r| format!("Skipped: the command was already handled ({r})."))
            .unwrap_or_else(|e| format!("Skipped: the command already failed ({e})."));
        Self {
            late: late.filter(|_| result.is_ok()),
            result,
            tail: Some(tail),
            handled,
        }
    }
}

/// Model tool calls that arrive after the first batch (sequential multi-action turns).
struct LateTools<'a> {
    app: &'a AppHandle,
    platform: &'a Arc<dyn crate::platform::Platform>,
    seen: Vec<(String, Value)>,
    /// An action failed: answer any further calls as skipped.
    stopped: bool,
}

/// Show the result, release `busy`, then let the spoken reply play out (barge-in aware), and
/// finally store the exchange in the conversation context (`turn` from `context::begin_turn`).
fn finish_command(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    live: &Arc<LivePool>,
    mut voice: Voice,
    outcome: CommandOutcome,
    released: Instant,
    turn: u64,
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
            let mut late = outcome.late.map(|seen| LateTools {
                app,
                platform,
                seen,
                stopped: false,
            });
            drain_voice(live, &rx, &mut voice, &outcome.handled, late.as_mut());
            if let Some(state) = app.try_state::<AppState>() {
                let mut slot = state.agent.speaking_turn.lock();
                if *slot == Some(id) {
                    *slot = None;
                }
            }
        }
        live.commands.finish_turn_id(id);
    }

    let answer = if !voice.spoken.trim().is_empty() {
        voice.spoken.clone()
    } else {
        match &outcome.result {
            Ok(msg) => msg.clone(),
            Err(e) => format!("Error: {e}"),
        }
    };
    context::commit(app, turn, &answer);

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
fn drain_voice(
    live: &Arc<LivePool>,
    rx: &Receiver<LiveEvent>,
    voice: &mut Voice,
    handled: &str,
    mut late: Option<&mut LateTools>,
) {
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
                        match late.as_deref_mut() {
                            Some(l) if !l.stopped && l.seen.len() < MAX_ACTIONS => {
                                eprintln!("[soul][agent] late tool_call {names:?} (next action)");
                                let (steps, responses) =
                                    run_live_calls(l.app, l.platform, &calls, &mut l.seen);
                                l.stopped = steps.iter().any(|s| !s.ok);
                                live.commands.send_tool_responses(responses);
                            }
                            _ => {
                                eprintln!("[soul][agent] late tool_call {names:?} answered as handled");
                                live.commands
                                    .send_tool_responses(skipped_responses(&calls, handled));
                            }
                        }
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
    /// Finished (tool executed or answer shown) — never re-run on REST. `Some(calls)` = the
    /// model ran these calls (later calls in the turn may continue a chained command).
    Done(Result<String, String>, Option<Vec<(String, Value)>>),
    /// Handled locally without a spoken reply (text action): cancel the Live turn.
    Quiet(Result<String, String>),
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
            LiveAgentOutcome::Done(result, late) => {
                return CommandOutcome::live(result, (rx, id), late);
            }
            LiveAgentOutcome::Quiet(result) => {
                live.commands.cancel_turn_id(id);
                return CommandOutcome::done(result);
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
    context::note_user(&transcript);
    auto_remember(app, &transcript);
    if let Some(local) = local_dispatch(app, platform, &transcript) {
        return CommandOutcome::done(local.result);
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

/// `local_dispatch` inside a Live turn: Some(outcome) when Soul handled the command itself.
fn try_pre_router(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    transcript: &str,
) -> Option<LiveAgentOutcome> {
    let local = local_dispatch(app, platform, transcript)?;
    Some(if local.speak {
        LiveAgentOutcome::Done(local.result, None)
    } else {
        LiveAgentOutcome::Quiet(local.result)
    })
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
                    return LiveAgentOutcome::Done(Ok(shorten(&voice.spoken, 200)), None);
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
                    if let Some(outcome) = try_pre_router(app, platform, transcript.trim()) {
                        log_live_final(app, released, &transcript, "pre-router");
                        return outcome;
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
                    if let Some(outcome) = try_pre_router(app, platform, &text) {
                        return outcome;
                    }
                }
                let reply = if !answer.trim().is_empty() {
                    answer
                } else {
                    voice.spoken.clone()
                };
                if !reply.trim().is_empty() {
                    return LiveAgentOutcome::Done(Ok(shorten(&reply, 200)), None);
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

/// Pre-router first (unless the caller already tried it), then the model's calls. Every call
/// id gets a toolResponse.
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
        if let Some(local) = local_dispatch(app, platform, heard) {
            let summary = if local.speak {
                local.summary.clone()
            } else {
                "a text action (nothing to say)".to_string()
            };
            live.commands.send_tool_responses(skipped_responses(
                calls,
                &format!("Handled by Soul's local router: {summary}"),
            ));
            return if local.speak {
                LiveAgentOutcome::Done(local.result, None)
            } else {
                LiveAgentOutcome::Quiet(local.result)
            };
        }
    }
    let mut seen = Vec::new();
    let result = execute_live_calls(app, platform, live, calls, &mut seen);
    LiveAgentOutcome::Done(result, Some(seen))
}

fn log_live_final(app: &AppHandle, released: Instant, transcript: &str, via: &str) {
    eprintln!(
        "[soul][live] commands final ms={} via={via} transcript={:?}",
        released.elapsed().as_millis(),
        transcript
    );
    if !transcript.trim().is_empty() {
        context::note_user(transcript);
        auto_remember(app, transcript);
    }
}

/// One executed model call.
struct Step {
    ok: bool,
    label: String,
    result: Result<String, String>,
}

/// Execute model calls in order — at most `MAX_ACTIONS` per command counting `seen`, exact
/// duplicates skipped — and stop at the first failure. Returns the executed steps and one
/// functionResponse per call id.
fn run_live_calls(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    calls: &[LiveFunctionCall],
    seen: &mut Vec<(String, Value)>,
) -> (Vec<Step>, Vec<Value>) {
    let mut steps: Vec<Step> = Vec::new();
    let mut responses = Vec::new();
    for c in calls {
        let key = (c.call.name.clone(), c.call.args.clone());
        let skip = if steps.iter().any(|s| !s.ok) {
            Some("Skipped: an earlier action failed.")
        } else if seen.contains(&key) {
            Some("Skipped: duplicate of an action that already ran.")
        } else if seen.len() >= MAX_ACTIONS {
            Some("Skipped: at most four actions per command.")
        } else {
            None
        };
        if let Some(why) = skip {
            responses.push(function_response(c, json!({ "result": why })));
            continue;
        }
        seen.push(key);
        let args_preview = serde_json::to_string(&c.call.args).unwrap_or_default();
        eprintln!(
            "[soul][agent] router=Live tool_call name={} args={}",
            c.call.name, args_preview
        );
        let result = execute_tool(app, platform, &c.call.name, &c.call.args);
        match &result {
            Ok(r) => eprintln!("[soul][agent] tool_result name={} result={}", c.call.name, r),
            Err(e) => eprintln!("[soul][agent] tool_result name={} error={}", c.call.name, e),
        }
        responses.push(function_response(
            c,
            match &result {
                Ok(r) => json!({ "result": r }),
                Err(e) => json!({ "error": e }),
            },
        ));
        steps.push(Step {
            ok: result.is_ok(),
            label: match &result {
                Ok(r) => chain::step_label(&c.call.name, &c.call.args, r),
                Err(e) => failed_label(&c.call.name, &c.call.args, e),
            },
            result,
        });
    }
    (steps, responses)
}

/// Result shown for the executed steps: the tool's own message for one action, the combined
/// „✓ … · ✓ …” line for several.
fn steps_result(steps: Vec<Step>, total: usize) -> Result<String, String> {
    if steps.len() == 1 && total == 1 {
        let step = steps.into_iter().next().expect("one step");
        return step.result.map(|r| shorten(&r, 500));
    }
    if steps.is_empty() {
        return Err("Nothing to do.".into());
    }
    let failed = steps.iter().any(|s| !s.ok);
    let labels: Vec<(bool, String)> = steps.iter().map(|s| (s.ok, s.label.clone())).collect();
    let line = chain::combine(&labels, total.saturating_sub(steps.len()));
    if failed {
        Err(line)
    } else {
        Ok(line)
    }
}

/// Execute the model's tool call(s) — a chained command may bring several — and answer every id.
fn execute_live_calls(
    app: &AppHandle,
    platform: &Arc<dyn crate::platform::Platform>,
    live: &Arc<LivePool>,
    calls: &[LiveFunctionCall],
    seen: &mut Vec<(String, Value)>,
) -> Result<String, String> {
    if calls.is_empty() {
        return Err("Live returned an empty tool call.".into());
    }
    if calls.len() > 1 {
        let names: Vec<&str> = calls.iter().map(|c| c.call.name.as_str()).collect();
        eprintln!("[soul][agent] Live returned {} tools {names:?} — running in order", calls.len());
    }
    let (steps, responses) = run_live_calls(app, platform, calls, seen);
    live.commands.send_tool_responses(responses);
    steps_result(steps, calls.len())
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
    let ctx = context::active_block(&settings);
    let initial = build_agent_body_with_context(transcript, &settings.language, ctx.as_deref())?;
    let system_instruction = initial.get("system_instruction").cloned();
    let mut contents = initial
        .get("contents")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut last_confirmation = String::from("Done");
    // Every action of this command (chained commands: several, across rounds).
    let mut seen: Vec<(String, Value)> = Vec::new();
    let mut labels: Vec<(bool, String)> = Vec::new();
    let finish = |labels: &[(bool, String)], last: String| {
        if labels.len() > 1 {
            chain::combine(labels, 0)
        } else {
            last
        }
    };

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
            if labels.len() > 1 {
                return Ok(finish(&labels, last_confirmation));
            }
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
        let mut failed: Option<String> = None;

        // Chained commands: run in order, at most MAX_ACTIONS, stop at the first failure.
        for call in &calls {
            let key = (call.name.clone(), call.args.clone());
            let skip = if failed.is_some() {
                Some("Skipped: an earlier action failed.")
            } else if seen.contains(&key) {
                Some("Skipped: duplicate of an action that already ran.")
            } else if seen.len() >= MAX_ACTIONS {
                Some("Skipped: at most four actions per command.")
            } else {
                None
            };
            if let Some(why) = skip {
                function_parts.push(json!({
                    "functionResponse": { "name": call.name, "response": { "result": why } }
                }));
                continue;
            }
            seen.push(key);
            let args_preview = serde_json::to_string(&call.args).unwrap_or_default();
            eprintln!(
                "[soul][agent] tool_call name={} args={}",
                call.name, args_preview
            );
            match execute_tool(app, platform, &call.name, &call.args) {
                Ok(result) => {
                    eprintln!(
                        "[soul][agent] tool_result name={} result={}",
                        call.name, result
                    );
                    if call.name != "answer" {
                        only_answers = false;
                    }
                    labels.push((true, chain::step_label(&call.name, &call.args, &result)));
                    last_confirmation = result.clone();
                    function_parts.push(json!({
                        "functionResponse": { "name": call.name, "response": { "result": result } }
                    }));
                }
                Err(e) => {
                    eprintln!("[soul][agent] tool_result name={} error={}", call.name, e);
                    labels.push((false, failed_label(&call.name, &call.args, &e)));
                    function_parts.push(json!({
                        "functionResponse": { "name": call.name, "response": { "error": e } }
                    }));
                    failed = Some(e);
                }
            }
        }

        if let Some(e) = failed {
            return Err(if labels.len() > 1 {
                chain::combine(&labels, calls.len().saturating_sub(labels.len()))
            } else {
                e
            });
        }

        contents.push(json!({
            "role": "user",
            "parts": function_parts
        }));

        if only_answers {
            return Ok(shorten(&finish(&labels, last_confirmation), 200));
        }
        if seen.len() >= MAX_ACTIONS {
            break;
        }
    }

    Ok(finish(&labels, last_confirmation))
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

/// What `local_dispatch` would run for `text`, without running it: the steps (tool, args) and
/// the reason. `None` = the model decides. Used by the voice regression harness.
#[cfg(test)]
pub(crate) fn preview_plan(
    text: &str,
    settings: &crate::config::Settings,
) -> Option<(Vec<(String, Value)>, String)> {
    let text = text.trim();
    if context::is_reset_phrase(text) {
        return Some((vec![("context_reset".into(), json!({}))], "context-reset".into()));
    }
    if let Some(action) = text_action::parse(text) {
        return Some((
            vec![(context::TEXT_ACTION_TOOL.into(), action.args())],
            "text-action".into(),
        ));
    }
    if let Some(last) = context::last_tool(settings) {
        if let Some(r) = context::resolve_followup(text, &last) {
            return Some((vec![(r.tool, r.args)], r.reason));
        }
    }
    if let Some(parts) = chain::split_with(text, &|p| part_is_command(p, true)) {
        let steps: Option<Vec<router::PreRoute>> = parts
            .iter()
            .take(MAX_ACTIONS)
            .map(|p| local_route(p).or_else(|| router::match_app_verb(p)))
            .collect();
        return steps.map(|s| (s.into_iter().map(|r| (r.tool, r.args)).collect(), "chain".into()));
    }
    local_route(text).map(|r| (vec![(r.tool, r.args)], r.reason))
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
        context::note_tool(name, args, true);
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
    context::note_tool(name, args, result.is_ok());
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
            let percent = match args.get("delta").and_then(Value::as_i64) {
                // „ścisz o 20”, „głośniej”: relative to the current level.
                Some(delta) if args.get("percent").is_none_or(Value::is_null) => {
                    let now = i64::from(platform.get_volume()?);
                    (now + delta).clamp(0, 100) as u8
                }
                _ => arg_u8(args, "percent")?,
            };
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
    let ctx = context::active_block(&settings);
    match client.search_answer(query, &settings.language, Some(&fallback), ctx.as_deref()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn smart(text: &str, context_active: bool) -> Intent {
        classify_smart(text, "Soul", &|| None, context_active)
    }

    fn is_cmd(i: &Intent) -> bool {
        matches!(i, Intent::Command { .. })
    }

    #[test]
    fn followups_route_to_commands_only_with_context() {
        for t in ["A teraz głośniej.", "a jutro?", "A ten drugi?", "jeszcze ciszej"] {
            assert_eq!(smart(t, false), Intent::Dictation, "{t} without context");
            assert!(
                matches!(smart(t, true), Intent::Command { reason: "followup", .. }),
                "{t} with context"
            );
        }
        // „i zamknij go” has a command verb anyway.
        assert!(is_cmd(&smart("i zamknij go", false)));
        // Typing in a text field: a bare connector opener stays dictation, a question does not.
        let typing = || Some(true);
        assert_eq!(classify_smart("A w Krakowie", "Soul", &typing, true), Intent::Dictation);
        assert!(is_cmd(&classify_smart("a jutro?", "Soul", &typing, true)));
    }

    #[test]
    fn dictation_traps_stay_dictation_even_with_context() {
        for t in [
            "Odwiedziłem wczoraj nowy sklep kolegi.",
            "Proszę o przesłanie raportu do piątku.",
            "Spotkanie przenosimy na piątek o 10:00.",
            "Kup mleko, to znaczy kefir i chleb.",
            "Let's meet tomorrow at 5:00.",
            "Zażółć gęślą jaźń.",
            "Dalej nie wiem, co o tym myśleć.",
            "Dzień dobry, przesyłam w załączniku fakturę za październik.",
        ] {
            assert_eq!(smart(t, true), Intent::Dictation, "{t}");
        }
    }

    #[test]
    fn text_actions_and_resets_are_commands() {
        for t in ["Przetłumacz to na angielski.", "Popraw to.", "Streść to", "Zrób to bardziej formalne", "Wyjaśnij to"] {
            assert!(matches!(smart(t, false), Intent::Command { .. }), "{t}");
        }
        assert!(matches!(smart("Nowy temat.", false), Intent::Command { reason: "context-reset", .. }));
        assert_eq!(smart("Popraw mi humor tym filmem", false), Intent::Dictation);
    }

    #[test]
    fn chain_parts_are_commands() {
        assert!(part_is_command("otwórz Discord", false));
        assert!(part_is_command("ścisz o 20", false));
        assert!(part_is_command("ustaw timer na 30 minut", false));
        assert!(!part_is_command("Spotify", false));
        assert!(!part_is_command("20 minut", false));
    }

    /// Plans that use the shared conversation context — one test, so nothing races on it.
    #[test]
    fn plans_with_and_without_context() {
        let settings = crate::config::Settings::default();
        context::reset_for_test();

        let (steps, reason) = preview_plan("Zamknij Spotify i otwórz Discord.", &settings).unwrap();
        assert_eq!(reason, "chain");
        assert_eq!(
            steps,
            vec![
                ("close_app".to_string(), json!({ "name": "Spotify" })),
                ("open_app".to_string(), json!({ "name": "Discord" })),
            ]
        );
        let (steps, _) = preview_plan("Pauza i ścisz o 20.", &settings).unwrap();
        assert_eq!(
            steps,
            vec![
                ("media_control".to_string(), json!({ "action": "pause" })),
                ("set_volume".to_string(), json!({ "delta": -20 })),
            ]
        );
        let (steps, _) =
            preview_plan("Włącz scenę granie, a potem ustaw timer na 30 minut.", &settings).unwrap();
        assert_eq!(steps[0], ("run_scene".to_string(), json!({ "name": "granie" })));
        assert_eq!(steps[1].0, "set_timer");
        assert_eq!(steps[1].1["seconds"], 1800);
        // Five actions: only the first four run.
        let (steps, _) = preview_plan("pauza i następna i ścisz o 10 i mów ciszej i wycisz", &settings).unwrap();
        assert_eq!(steps.len(), MAX_ACTIONS);

        let (steps, reason) = preview_plan("Przetłumacz to na angielski.", &settings).unwrap();
        assert_eq!(reason, "text-action");
        assert_eq!(steps[0].1, json!({ "action": "translate", "language": "English" }));

        // No context: a bare follow-up has nothing to resolve.
        assert!(preview_plan("A teraz głośniej.", &settings).is_none());
        context::seed(vec![context::Exchange {
            user: "mów ciszej".into(),
            tools: vec![context::ToolUse {
                name: "set_voice_volume".into(),
                args: json!({ "delta": -20 }),
                ok: true,
            }],
            answer: "Głośność mojego głosu: 60%.".into(),
        }]);
        assert!(context_active(&settings));
        let (steps, reason) = preview_plan("A teraz głośniej.", &settings).unwrap();
        assert_eq!(reason, "followup:set_voice_volume");
        assert_eq!(steps, vec![("set_voice_volume".to_string(), json!({ "delta": 20 }))]);
        assert!(context_block(&settings).unwrap().contains("mów ciszej"));

        // Context off in Settings: no follow-ups, no block.
        let off = crate::config::Settings {
            context_enabled: false,
            ..settings.clone()
        };
        assert!(!context_active(&off) && context_block(&off).is_none());
        assert!(preview_plan("A teraz głośniej.", &off).is_none());

        context::reset_for_test();
        assert!(!context_active(&settings));
    }
}
