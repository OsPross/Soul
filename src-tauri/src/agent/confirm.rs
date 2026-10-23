//! Confirmations for destructive tools (`close_app`). A pending confirmation has top priority:
//! no other overlay/Island state replaces the card until it is answered or times out
//! (`blocks_overlay`). Answers: the Island buttons, Esc, or a spoken „tak” / „nie” on the talk
//! key (or the dictation / command hotkey in separate mode) — recorded on the dictation session
//! while the card stays up.

use super::fold_word;
use crate::config::load_settings;
use crate::i18n::{tr, ui_lang, Lang};
use crate::{emit_overlay, AppState, OverlayPayload};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const CONFIRM_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Pending {
    tx: Sender<bool>,
    /// The question on the card („Zamknąć Discord?”), re-shown after an unclear spoken answer.
    message: String,
    /// A free-text question (computer agent `ask_user`): the spoken answer goes here instead of
    /// being parsed as yes / no.
    answer_tx: Option<Sender<String>>,
}

/// A spoken answer is being recorded.
static ANSWERING: AtomicBool = AtomicBool::new(false);

const YES: &[&str] = &[
    "tak", "potwierdz", "potwierdzam", "jasne", "pewnie", "oczywiscie", "ok", "okej", "okay", "dobra",
    "dawaj", "zgoda", "zamknij", "zamykaj", "yes", "yeah", "yep", "sure", "confirm", "confirmed", "zezwol",
    "zezwalaj", "allow",
];
const NO: &[&str] = &[
    "nie", "niee", "anuluj", "anulujemy", "anulowac", "cancel", "stop", "nope", "zostaw", "przestan",
    "czekaj", "wroc", "odrzuc", "deny",
];

/// „Tak” → Some(true), „nie, zostaw” → Some(false), anything else → None. A negative word wins
/// (the safe side); a lone English "no" is a no, while Polish „no tak” / „no dobra” are a yes.
pub fn parse_answer(text: &str) -> Option<bool> {
    let words: Vec<String> = text.split_whitespace().map(fold_word).filter(|w| !w.is_empty()).collect();
    if words.is_empty() || words.len() > 6 {
        return None;
    }
    if words.iter().any(|w| NO.contains(&w.as_str())) || (words.len() == 1 && words[0] == "no") {
        return Some(false);
    }
    words.iter().any(|w| YES.contains(&w.as_str())).then_some(true)
}

/// While a confirmation waits, only the confirm card itself may be shown.
pub fn overlay_allowed(pending: bool, state: &str) -> bool {
    !pending || state == "confirm" || state == "question"
}

/// Soul's own confirm / question card waits.
pub fn soul_pending(app: &AppHandle) -> bool {
    app.try_state::<AppState>()
        .is_some_and(|s| s.agent.pending_confirm.lock().is_some())
}

/// The talk key records a spoken answer: Soul's card or a coding agent approval on the Island.
pub fn is_pending(app: &AppHandle) -> bool {
    soul_pending(app) || crate::coding_agents::pipe::has_pending_approval(app)
}

/// `emit_overlay` / `schedule_hide` must leave the confirm card alone.
pub fn blocks_overlay(app: &AppHandle, state: &str) -> bool {
    !overlay_allowed(soul_pending(app), state)
}

/// Island button / Esc / voice: resolve the pending confirmation.
pub fn answer(app: &AppHandle, accepted: bool) -> Result<(), String> {
    let pending = app.state::<AppState>().agent.pending_confirm.lock().take();
    match pending {
        Some(p) => {
            let _ = p.tx.send(accepted);
            Ok(())
        }
        None => Err("No pending confirmation.".into()),
    }
}

/// Show `message` with Confirm/Cancel and wait for the answer (false on timeout).
pub fn request(app: &AppHandle, message: &str) -> Result<bool, String> {
    if let Some(answer) = crate::e2e::confirm_answer(message) {
        return Ok(answer);
    }
    let (tx, rx) = mpsc::channel();
    *app.state::<AppState>().agent.pending_confirm.lock() = Some(Pending {
        tx,
        message: message.to_string(),
        answer_tx: None,
    });
    let _ = emit_overlay(app, "confirm", Some(message.to_string()));
    if let Some(win) = app.get_webview_window("overlay") {
        let _ = win.set_size(tauri::Size::Physical(tauri::PhysicalSize::new(360, 100)));
    }

    let accepted = rx.recv_timeout(CONFIRM_TIMEOUT).unwrap_or_else(|_| {
        eprintln!("[soul][agent] confirm timed out");
        if let Some(state) = app.try_state::<AppState>() {
            *state.agent.pending_confirm.lock() = None;
        }
        false
    });

    let _ = app.emit(
        "overlay://state",
        OverlayPayload {
            state: "thinking".into(),
            message: None,
        },
    );
    Ok(accepted)
}

/// Show a question card and wait (≤ 30 s) for a spoken answer on the talk key. `None` = no
/// answer, cancelled (button / Esc) or nothing intelligible.
pub fn ask(app: &AppHandle, question: &str) -> Option<String> {
    if crate::e2e::enabled() {
        crate::e2e::trace("question", serde_json::json!({ "question": question }));
        return None;
    }
    let (tx, cancel_rx) = mpsc::channel();
    let (atx, arx) = mpsc::channel();
    *app.state::<AppState>().agent.pending_confirm.lock() = Some(Pending {
        tx,
        message: question.to_string(),
        answer_tx: Some(atx),
    });
    let _ = emit_overlay(app, "question", Some(question.to_string()));
    let deadline = Instant::now() + CONFIRM_TIMEOUT;
    let answer = loop {
        if let Ok(a) = arx.recv_timeout(Duration::from_millis(100)) {
            break Some(a);
        }
        // Cancel button / Esc resolve the pending entry through `answer(…, false)`.
        if cancel_rx.try_recv().is_ok() || Instant::now() >= deadline {
            break None;
        }
        if crate::agent::computer::killswitch::aborted() {
            break None;
        }
    };
    if let Some(state) = app.try_state::<AppState>() {
        *state.agent.pending_confirm.lock() = None;
    }
    let _ = app.emit(
        "overlay://state",
        OverlayPayload {
            state: "thinking".into(),
            message: None,
        },
    );
    answer.map(|a| a.trim().to_string()).filter(|a| !a.is_empty())
}

/// „Zamknąć Discord?”
pub fn close_question(lang: Lang, name: &str) -> String {
    tr(lang, format!("Zamknąć {name}?"), format!("Close {name}?"))
}

/// Result of a declined close — `is_cancelled` recognises it in both languages.
pub fn cancelled_message(lang: Lang, name: &str) -> String {
    tr(lang, format!("Anulowano zamykanie {name}"), format!("Cancelled closing {name}"))
}

pub fn is_cancelled(msg: &str) -> bool {
    msg.starts_with("Anulowano zamykanie") || msg.starts_with("Cancelled closing")
}

/// Talk key (gated capture, see `talk.rs`) or a hotkey pressed while the card is up.
pub fn mark_answering() {
    ANSWERING.store(true, Ordering::SeqCst);
}

/// The answer capture was dropped (chord, drag) before release.
pub fn cancel_answer() {
    ANSWERING.store(false, Ordering::SeqCst);
}

pub fn is_answering() -> bool {
    ANSWERING.load(Ordering::SeqCst)
}

/// Separate-mode hotkey pressed while a confirmation waits: record the spoken answer.
pub fn begin_answer(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.recorder.is_recording() {
        return;
    }
    match crate::begin_capture(app, crate::CaptureTarget::Dictation, false) {
        Ok(_) => mark_answering(),
        Err(e) => crate::report_error(app, e),
    }
}

/// Key released: transcribe, then answer the card (or ask again when it was neither yes nor no).
pub fn on_answer_released(app: &AppHandle) {
    ANSWERING.store(false, Ordering::SeqCst);
    let state = app.state::<AppState>();
    if !state.recorder.is_recording() {
        return;
    }
    let released = Instant::now();
    let wav = state.recorder.stop();
    let live_events = state.live.dictation.end_turn(released);
    let live = Arc::clone(&state.live);
    let app = app.clone();
    thread::spawn(move || {
        let settings = load_settings(&app).unwrap_or_default();
        let heard = crate::transcribe_turn(&app, &settings, live_events, wav, released);
        live.dictation.finish_turn();
        let text = heard.map(|h| h.text).unwrap_or_default();
        crate::island::set_activity(crate::island::Activity::Command);
        // A coding agent approval on the Island (Soul's own card, if any, goes first).
        if !soul_pending(&app) && crate::coding_agents::pipe::has_pending_approval(&app) {
            match parse_answer(&text) {
                Some(accepted) => {
                    eprintln!("[soul][agent] coding approval answered by voice accepted={accepted}");
                    crate::coding_agents::pipe::answer_active(&app, if accepted { "allow" } else { "deny" });
                }
                None => eprintln!("[soul][agent] coding approval: no yes/no in the spoken answer ({} chars)", text.chars().count()),
            }
            return;
        }
        // A free-text question takes the whole answer.
        let question = app
            .state::<AppState>()
            .agent
            .pending_confirm
            .lock()
            .as_ref()
            .and_then(|p| p.answer_tx.clone().map(|tx| (tx, p.message.clone())));
        if let Some((tx, message)) = question {
            if text.trim().is_empty() {
                let hint = tr(ui_lang(&settings), "Nie usłyszałem — przytrzymaj klawisz i odpowiedz.", "I didn't catch that — hold the key and answer.");
                let _ = emit_overlay(&app, "question", Some(format!("{message} {hint}")));
            } else {
                eprintln!("[soul][agent] question answered by voice ({} chars)", text.chars().count());
                let _ = tx.send(text);
            }
            return;
        }
        match parse_answer(&text) {
            Some(accepted) => {
                eprintln!(
                    "[soul][agent] confirm answered by voice accepted={accepted} ms={}",
                    released.elapsed().as_millis()
                );
                let _ = answer(&app, accepted);
            }
            None => {
                eprintln!(
                    "[soul][agent] confirm: no yes/no in the spoken answer ({} chars)",
                    text.chars().count()
                );
                let message = app
                    .state::<AppState>()
                    .agent
                    .pending_confirm
                    .lock()
                    .as_ref()
                    .map(|p| p.message.clone());
                if let Some(message) = message {
                    let hint = tr(ui_lang(&settings), "Powiedz „tak” albo „nie”.", "Say “yes” or “no”.");
                    let _ = emit_overlay(&app, "confirm", Some(format!("{message} {hint}")));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoken_answers() {
        for t in ["Zezwól", "zezwalaj", "allow"] {
            assert_eq!(parse_answer(t), Some(true), "{t}");
        }
        for t in ["Odrzuć", "deny"] {
            assert_eq!(parse_answer(t), Some(false), "{t}");
        }
        for t in ["Tak", "tak.", "Potwierdź", "potwierdzam", "No tak", "no dobra", "Okej", "Tak, zamknij go", "Soul, tak", "yes", "Confirm"] {
            assert_eq!(parse_answer(t), Some(true), "{t}");
        }
        for t in ["Nie", "nie.", "Anuluj", "nie zamykaj", "Nie, zostaw", "No", "cancel", "stop", "tak, nie", "Nie wiem"] {
            assert_eq!(parse_answer(t), Some(false), "{t}");
        }
        for t in ["", "  ", "hmm", "co to jest", "otwórz spotify", "a może jednak zrobimy coś zupełnie innego dzisiaj"] {
            assert_eq!(parse_answer(t), None, "{t}");
        }
    }

    #[test]
    fn pending_confirm_has_top_priority() {
        // Nothing replaces the card while it waits…
        for state in ["listening", "thinking", "speaking", "done", "error", "idle", "retrying"] {
            assert!(!overlay_allowed(true, state), "{state}");
            assert!(overlay_allowed(false, state), "{state}");
        }
        // …except the card itself (re-shown with a hint) and an agent question.
        assert!(overlay_allowed(true, "confirm"));
        assert!(overlay_allowed(true, "question"));
    }

    #[test]
    fn localized_close_strings() {
        assert_eq!(close_question(Lang::Pl, "Discord"), "Zamknąć Discord?");
        assert_eq!(close_question(Lang::En, "Discord"), "Close Discord?");
        assert!(is_cancelled(&cancelled_message(Lang::Pl, "Discord")));
        assert!(is_cancelled(&cancelled_message(Lang::En, "Discord")));
        assert!(!is_cancelled("Closed Discord"));
    }
}
