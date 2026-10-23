//! Local clipboard history (last 20 text items). Skips password-like content.

use arboard::Clipboard;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, EventTarget};

use crate::soul_debug;

const HISTORY_LIMIT: usize = 20;
const POLL_MS: u64 = 500;
/// How long Soul's own paste texts stay ignored by the watcher.
const INTERNAL_TTL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardItem {
    pub id: String,
    pub text: String,
    pub preview: String,
    pub ts: u64,
}

struct ClipboardState {
    items: Vec<ClipboardItem>,
    last_text: String,
    seeded: bool,
    /// Texts Soul itself put on the clipboard (dictation paste, restored previous text).
    internal: Vec<(String, Instant)>,
}

fn normalized(text: &str) -> String {
    text.replace('\0', "").trim().to_string()
}

/// Soul is about to paste `temp` and later restore `restored`: neither is a new history item.
pub fn note_internal_paste(temp: &str, restored: Option<&str>) {
    let mut st = state().lock();
    st.internal.retain(|(_, at)| at.elapsed() < INTERNAL_TTL);
    let now = Instant::now();
    st.internal.push((normalized(temp), now));
    if let Some(r) = restored {
        st.internal.push((normalized(r), now));
    }
}

fn state() -> &'static Mutex<ClipboardState> {
    static STATE: OnceLock<Mutex<ClipboardState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(ClipboardState {
            items: Vec::new(),
            last_text: String::new(),
            seeded: false,
            internal: Vec::new(),
        })
    })
}

fn emit_clipboard(app: &AppHandle, items: &[ClipboardItem]) {
    let _ = app.emit("island://clipboard", items);
    let _ = app.emit_to(EventTarget::webview_window("island"), "island://clipboard", items);
}

pub fn start_watcher(app: AppHandle) {
    thread::Builder::new()
        .name("soul-clipboard".into())
        .spawn(move || {
            eprintln!("[soul][island][clipboard] watcher started (poll {}ms)", POLL_MS);
            // Reuse one clipboard handle when possible; recreate on error.
            let mut clipboard = Clipboard::new().ok();
            let mut fail_streak = 0u32;

            loop {
                thread::sleep(Duration::from_millis(POLL_MS));

                if clipboard.is_none() {
                    clipboard = Clipboard::new().ok();
                    if clipboard.is_none() {
                        fail_streak += 1;
                        if fail_streak == 1 || fail_streak.is_multiple_of(20) {
                            eprintln!(
                                "[soul][island][clipboard] Clipboard::new failed (x{fail_streak})"
                            );
                        }
                        continue;
                    }
                }

                let text = {
                    let clip = clipboard.as_mut().unwrap();
                    match clip.get_text() {
                        Ok(t) => {
                            fail_streak = 0;
                            t
                        }
                        Err(e) => {
                            soul_debug!("[soul][island][clipboard] get_text: {e}");
                            // Recreate handle next loop — common after other apps lock the clipboard.
                            clipboard = None;
                            continue;
                        }
                    }
                };

                let text = normalized(&text);
                if text.is_empty() {
                    continue;
                }

                let mut st = state().lock();

                // First successful read only seeds baseline (don't treat existing clip as "new").
                if !st.seeded {
                    st.last_text = text;
                    st.seeded = true;
                    soul_debug!(
                        "[soul][island][clipboard] seeded baseline ({} chars)",
                        st.last_text.len()
                    );
                    continue;
                }

                if text == st.last_text {
                    continue;
                }

                st.internal.retain(|(_, at)| at.elapsed() < INTERNAL_TTL);
                if st.internal.iter().any(|(t, _)| *t == text) {
                    st.last_text = text;
                    continue;
                }

                if looks_like_secret(&text) {
                    eprintln!(
                        "[soul][island][clipboard] skipped secret-like text ({} chars)",
                        text.len()
                    );
                    st.last_text = text;
                    continue;
                }

                st.last_text = text.clone();
                st.items.retain(|i| i.text != text);
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let char_count = text.chars().count();
                let preview: String = text.chars().take(80).collect();
                let preview = if preview.chars().count() < char_count {
                    format!("{preview}…")
                } else {
                    preview
                };
                let id = format!("{ts}");
                st.items.insert(
                    0,
                    ClipboardItem {
                        id,
                        text: text.clone(),
                        preview,
                        ts,
                    },
                );
                st.items.truncate(HISTORY_LIMIT);
                let items = st.items.clone();
                drop(st);

                eprintln!(
                    "[soul][island][clipboard] +1 item ({} chars), history={}",
                    text.chars().count(),
                    items.len()
                );
                emit_clipboard(&app, &items);
            }
        })
        .ok();
}

pub fn list() -> Vec<ClipboardItem> {
    state().lock().items.clone()
}

pub fn clear() {
    let mut st = state().lock();
    st.items.clear();
    // Keep last_text so we don't immediately re-add the same clip.
}

pub fn copy_item(id: &str) -> Result<(), String> {
    let text = {
        let st = state().lock();
        st.items
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.text.clone())
            .ok_or_else(|| "Clipboard item not found.".to_string())?
    };
    let mut clipboard = Clipboard::new().map_err(|e| format!("Clipboard unavailable: {e}"))?;
    clipboard
        .set_text(&text)
        .map_err(|e| format!("Failed to copy: {e}"))?;
    // Avoid treating our own re-copy as a new history entry.
    state().lock().last_text = text;
    Ok(())
}

/// Heuristic: skip high-entropy / password-shaped clipboard text.
pub fn looks_like_secret(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    if lower.contains("password=")
        || lower.contains("passwd=")
        || lower.contains("secret=")
        || lower.contains("api_key=")
        || lower.contains("apikey=")
        || lower.starts_with("sk-")
        || lower.starts_with("ghp_")
        || lower.starts_with("xoxb-")
    {
        return true;
    }

    // Only flag long single-token high-entropy strings (avoid skipping normal words).
    if !t.chars().any(|c| c.is_whitespace()) && (20..128).contains(&t.len()) {
        let has_lower = t.chars().any(|c| c.is_ascii_lowercase());
        let has_upper = t.chars().any(|c| c.is_ascii_uppercase());
        let has_digit = t.chars().any(|c| c.is_ascii_digit());
        let has_special = t.chars().any(|c| !c.is_ascii_alphanumeric());
        let classes = [has_lower, has_upper, has_digit, has_special]
            .iter()
            .filter(|&&b| b)
            .count();
        if classes >= 3 {
            return true;
        }
    }
    false
}
