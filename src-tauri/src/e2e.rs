//! Voice E2E suite hooks (`voice_e2e.rs`, `npm run test:e2e`). With `SOUL_E2E=1` the app prints
//! one `[soul][e2e] {json}` line per pipeline milestone on stderr (transcript, route, tool call,
//! agent outcome, confirmation, done) and answers confirmation cards at once (deny unless a dev
//! build's test case asked for accept) instead of waiting 30 s for a click. Dev builds also play
//! `<app data>/dev-utterance.json` (`{id, wav, confirm, fresh_context}`) as a Smart talk-key hold.
//! Never prints the API key (nothing here sees it).

use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static ACCEPT: AtomicBool = AtomicBool::new(false);

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("SOUL_E2E").is_ok_and(|v| v == "1"))
}

pub fn trace(ev: &str, mut data: Value) {
    if !enabled() {
        return;
    }
    if !data.is_object() {
        data = serde_json::json!({ "value": data });
    }
    data["ev"] = ev.into();
    eprintln!("[soul][e2e] {data}");
}

#[cfg(debug_assertions)]
pub fn set_accept(accept: bool) {
    ACCEPT.store(accept, Ordering::SeqCst);
}

/// E2E mode: the case's answer to a confirmation card (traced). `None` = ask the user.
pub fn confirm_answer(question: &str) -> Option<bool> {
    if !enabled() {
        return None;
    }
    let accepted = ACCEPT.load(Ordering::SeqCst);
    trace("confirm", serde_json::json!({ "question": question, "accepted": accepted }));
    Some(accepted)
}
