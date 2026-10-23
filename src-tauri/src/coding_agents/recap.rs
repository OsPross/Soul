//! Local coding activity history for the weekly recap. Every hook event `pipe.rs` receives goes
//! through `observe`: a turn (`Draft`, per session) runs from the prompt / first tool until `Stop` /
//! `StopFailure` / `Interrupt` / `SessionEnd` (a draft silent for > 2 h is closed at its last
//! event). Privacy: only counts, the agent id, the last folder name of `cwd` and an opaque session
//! hash reach `<app data>/recap.json` — never commands, prompts, file paths or contents.

use crate::island::plan::{fnv1a, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const FILE: &str = "recap.json";
const SCHEMA_VERSION: u32 = 1;
pub const WINDOW_SECS: i64 = 12 * 7 * 86_400;
const MAX_ROWS: usize = 10_000;
const STALE_SECS: i64 = 2 * 3600;
const RUN_TOOLS: &[&str] = &["Bash", "PowerShell", "run_command", "shell", "run_shell_command", "exec_command", "terminal"];

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub agent: String,
    pub project: String,
    /// Opaque hash of the agent's session id (sessions are counted, never identified).
    pub session: String,
    pub start: i64,
    pub end: i64,
    pub files_changed: u32,
    pub lines_added: u64,
    pub lines_removed: u64,
    pub commands_run: u32,
    pub questions: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub at: i64,
    pub agent: String,
    pub allowed: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RecapPrefs {
    pub enabled: bool,
}

impl Default for RecapPrefs {
    fn default() -> Self {
        RecapPrefs { enabled: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Store {
    pub schema_version: u32,
    pub turns: Vec<Turn>,
    pub decisions: Vec<Decision>,
    pub prefs: RecapPrefs,
}

/// A turn in progress (memory only; file paths live here just to count distinct files).
struct Draft {
    turn: Turn,
    files: HashSet<String>,
}

#[derive(Default)]
pub struct Recap {
    pub store: Store,
    drafts: HashMap<String, Draft>,
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub fn project_of(cwd: &str) -> String {
    cwd.rsplit(['/', '\\']).find(|p| !p.is_empty()).unwrap_or_default().to_string()
}

impl Recap {
    fn close(&mut self, key: &str, end: i64) -> bool {
        let Some(mut d) = self.drafts.remove(key) else { return false };
        d.turn.end = end.max(d.turn.start);
        d.turn.files_changed = d.files.len() as u32;
        self.store.turns.push(d.turn);
        true
    }

    fn close_stale(&mut self, now: i64) -> bool {
        let stale: Vec<(String, i64)> =
            self.drafts.iter().filter(|(_, d)| now - d.turn.end > STALE_SECS).map(|(k, d)| (k.clone(), d.turn.end)).collect();
        stale.iter().fold(false, |dirty, (k, at)| self.close(k, *at) | dirty)
    }

    /// One hook event; `true` when something was persisted-worthy (a turn closed).
    pub fn observe(&mut self, p: &Value, now: i64) -> bool {
        if !self.store.prefs.enabled {
            return false;
        }
        let mut dirty = self.close_stale(now);
        let event = s(p, "hook_event_name");
        let agent = match s(p, "soul_agent") {
            "" => "claude",
            a => a,
        };
        let key = match s(p, "session_id") {
            "" => format!("{agent}:{}", s(p, "cwd")),
            id => id.to_string(),
        };
        match event {
            "Stop" | "StopFailure" | "Interrupt" | "SessionEnd" => return self.close(&key, now) | dirty,
            "UserPromptSubmit" => dirty |= self.close(&key, now),
            "PreToolUse" | "PostToolUse" => {}
            _ => {
                if let Some(d) = self.drafts.get_mut(&key) {
                    d.turn.end = now;
                }
                return dirty;
            }
        }
        let d = self.drafts.entry(key.clone()).or_insert_with(|| Draft {
            turn: Turn {
                agent: agent.to_string(),
                project: project_of(s(p, "cwd")),
                session: fnv1a(key.as_bytes()),
                start: now,
                end: now,
                ..Default::default()
            },
            files: HashSet::new(),
        });
        d.turn.end = now;
        let tool = s(p, "tool_name");
        let input = p.get("tool_input").cloned().unwrap_or(Value::Null);
        match event {
            "PreToolUse" if tool == "AskUserQuestion" => d.turn.questions += 1,
            "PreToolUse" if RUN_TOOLS.contains(&tool) => d.turn.commands_run += 1,
            "PostToolUse" if matches!(tool, "Edit" | "MultiEdit" | "Write") => {
                let path = s(&input, "file_path");
                if !path.is_empty() {
                    d.files.insert(path.to_string());
                }
                let edits: Vec<(&str, &str)> = match tool {
                    "Write" => vec![("", s(&input, "content"))],
                    "MultiEdit" => input
                        .get("edits")
                        .and_then(Value::as_array)
                        .map(|e| e.iter().map(|e| (s(e, "old_string"), s(e, "new_string"))).collect())
                        .unwrap_or_default(),
                    _ => vec![(s(&input, "old_string"), s(&input, "new_string"))],
                };
                for (old, new) in edits {
                    let (added, removed) = super::line_counts(old, new);
                    d.turn.lines_added += added;
                    d.turn.lines_removed += removed;
                }
            }
            _ => {}
        }
        dirty
    }

    /// An Island answer to a permission request (`allow` / `always` / `deny`; answers aren't counted).
    pub fn decision(&mut self, agent: &str, decision: &str, now: i64) -> bool {
        let allowed = match decision {
            "allow" | "always" => true,
            "deny" => false,
            _ => return false,
        };
        if !self.store.prefs.enabled {
            return false;
        }
        let agent = if agent.is_empty() { "claude" } else { agent };
        self.store.decisions.push(Decision { at: now, agent: agent.to_string(), allowed });
        true
    }

    /// 12-week window, at most 10 000 rows each (oldest dropped).
    pub fn prune(&mut self, now: i64) {
        let since = now - WINDOW_SECS;
        self.store.turns.retain(|t| t.end >= since);
        self.store.decisions.retain(|d| d.at >= since);
        let over = self.store.turns.len().saturating_sub(MAX_ROWS);
        self.store.turns.drain(..over);
        let over = self.store.decisions.len().saturating_sub(MAX_ROWS);
        self.store.decisions.drain(..over);
    }

    /// Closed turns plus the ones in progress (as of their last event).
    pub fn view(&self, since: i64) -> HistoryView {
        let open = self.drafts.values().map(|d| Turn { files_changed: d.files.len() as u32, ..d.turn.clone() });
        HistoryView {
            enabled: self.store.prefs.enabled,
            turns: self.store.turns.iter().cloned().chain(open).filter(|t| t.end >= since).collect(),
            decisions: self.store.decisions.iter().filter(|d| d.at >= since).cloned().collect(),
        }
    }
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HistoryView {
    pub enabled: bool,
    pub turns: Vec<Turn>,
    pub decisions: Vec<Decision>,
}

/// Read the store; a corrupt file is moved aside (`recap.json.corrupt-YYYYMMDD-HHMMSS`), not lost.
pub fn load(path: &Path) -> Store {
    let Ok(bytes) = fs::read(path) else { return Store { schema_version: SCHEMA_VERSION, ..Default::default() } };
    match serde_json::from_slice::<Store>(crate::island::plan::strip_bom(&bytes)) {
        Ok(store) => store,
        Err(_) => {
            let aside = path.with_file_name(format!("{FILE}.corrupt-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
            eprintln!("[soul][agent] recap.json unreadable, moved aside");
            let _ = fs::rename(path, aside);
            Store { schema_version: SCHEMA_VERSION, ..Default::default() }
        }
    }
}

pub fn save(path: &Path, store: &Store) -> Result<(), String> {
    let mut store = store.clone();
    store.schema_version = SCHEMA_VERSION;
    write_atomic(path, serde_json::to_string(&store).map_err(|e| e.to_string())?.as_bytes())
}

static STATE: parking_lot::Mutex<Option<(PathBuf, Recap)>> = parking_lot::const_mutex(None);

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Run `f` on the loaded recap; saves (pruned) when it returns true.
fn with<R>(app: &AppHandle, f: impl FnOnce(&mut Recap) -> (bool, R)) -> Option<R> {
    let mut guard = STATE.lock();
    if guard.is_none() {
        let path = app.path().app_data_dir().ok()?.join(FILE);
        let store = load(&path);
        *guard = Some((path, Recap { store, drafts: HashMap::new() }));
    }
    let (path, recap) = guard.as_mut()?;
    let (dirty, r) = f(recap);
    if dirty {
        recap.prune(now_secs());
        if let Err(e) = save(path, &recap.store) {
            eprintln!("[soul][agent] recap save failed: {e}");
        }
    }
    Some(r)
}

pub fn observe(app: &AppHandle, payload: &Value) {
    with(app, |r| (r.observe(payload, now_secs()), ()));
}

pub fn record_decision(app: &AppHandle, agent: &str, decision: &str) {
    with(app, |r| (r.decision(agent, decision, now_secs()), ()));
}

#[tauri::command]
pub fn coding_recap_history(app: AppHandle, since: i64) -> Option<HistoryView> {
    with(&app, |r| (false, r.view(since)))
}

#[tauri::command]
pub fn coding_recap_set_enabled(app: AppHandle, enabled: bool) {
    with(&app, |r| {
        r.store.prefs.enabled = enabled;
        if !enabled {
            r.drafts.clear();
        }
        (true, ())
    });
}

#[tauri::command]
pub fn coding_recap_clear(app: AppHandle) {
    with(&app, |r| {
        r.store.turns.clear();
        r.store.decisions.clear();
        r.drafts.clear();
        (true, ())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(event: &str, extra: Value) -> Value {
        let mut v = json!({ "hook_event_name": event, "session_id": "sess-1", "cwd": "/home/u/secret-project/soul" });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    #[test]
    fn counts_a_turn_from_prompt_to_stop() {
        let mut r = Recap::default();
        r.store.prefs.enabled = true;
        assert!(!r.observe(&ev("UserPromptSubmit", json!({ "prompt": "fix the bug" })), 100));
        r.observe(&ev("PreToolUse", json!({ "tool_name": "Bash", "tool_input": { "command": "cargo test" } })), 110);
        r.observe(&ev("PreToolUse", json!({ "tool_name": "AskUserQuestion" })), 115);
        let edit = json!({ "tool_name": "Edit", "tool_input": { "file_path": "/x/a.rs", "old_string": "a\nb", "new_string": "a\nc\nd" } });
        r.observe(&ev("PostToolUse", edit.clone()), 120);
        r.observe(&ev("PostToolUse", edit), 125);
        r.observe(&ev("PostToolUse", json!({ "tool_name": "Write", "tool_input": { "file_path": "/x/b.rs", "content": "1\n2\n3" } })), 130);
        assert_eq!(r.view(0).turns.len(), 1, "the open turn shows in the view");
        assert!(r.observe(&ev("Stop", json!({})), 160));
        let t = &r.store.turns[0];
        assert_eq!((t.start, t.end, t.files_changed, t.commands_run, t.questions), (100, 160, 2, 1, 1));
        assert_eq!((t.lines_added, t.lines_removed), (7, 2));
        assert_eq!((t.agent.as_str(), t.project.as_str()), ("claude", "soul"));
        assert!(r.decision("codex", "allow", 170) && r.decision("", "deny", 171));
        assert!(!r.decision("", r#"{"answers":{}}"#, 172));
        assert_eq!(r.store.decisions.iter().filter(|d| d.allowed).count(), 1);
        // A draft silent for over 2 h closes at its last event.
        r.observe(&ev("PreToolUse", json!({ "session_id": "s2", "tool_name": "Read" })), 1_000);
        r.observe(&ev("Notification", json!({ "session_id": "s3" })), 1_000 + STALE_SECS + 1);
        assert_eq!(r.store.turns.last().map(|t| (t.start, t.end)), Some((1_000, 1_000)));
        // Disabled: nothing recorded.
        r.store.prefs.enabled = false;
        assert!(!r.observe(&ev("UserPromptSubmit", json!({})), 9_000) && !r.decision("", "allow", 9_000));
    }

    fn temp_file(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("soul-recap-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.join(FILE)
    }

    #[test]
    fn no_secrets_on_disk_and_pruning() {
        let mut r = Recap::default();
        r.store.prefs.enabled = true;
        r.observe(&ev("UserPromptSubmit", json!({ "prompt": "my password is hunter2" })), 100);
        r.observe(&ev("PreToolUse", json!({ "tool_name": "Bash", "tool_input": { "command": "curl -H TOKEN-abc123" } })), 101);
        r.observe(&ev("PostToolUse", json!({ "tool_name": "Write", "tool_input": { "file_path": "/home/u/private/keys.txt", "content": "API_KEY=sk-live-xyz" } })), 102);
        r.observe(&ev("Stop", json!({ "last_assistant_message": "done with hunter2" })), 103);
        let path = temp_file("privacy");
        save(&path, &r.store).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        for secret in ["hunter2", "TOKEN-abc123", "keys.txt", "private", "sk-live", "sess-1", "secret-project", "curl"] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
        assert!(text.contains("\"schema_version\":1") && text.contains("\"project\":\"soul\""));
        assert_eq!(load(&path).turns, r.store.turns);
        // 12-week window and the row cap.
        r.store.turns.push(Turn { end: 0, ..Default::default() });
        r.store.decisions.push(Decision { at: 5, agent: "x".into(), allowed: true });
        r.prune(WINDOW_SECS + 100);
        assert_eq!(r.store.turns.len(), 1);
        assert!(r.store.decisions.is_empty());
        r.store.turns = vec![Turn { end: WINDOW_SECS + 100, ..Default::default() }; MAX_ROWS + 5];
        r.prune(WINDOW_SECS + 100);
        assert_eq!(r.store.turns.len(), MAX_ROWS);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let path = temp_file("corrupt");
        fs::write(&path, b"{ broken").unwrap();
        let store = load(&path);
        assert!(store.turns.is_empty() && store.prefs.enabled);
        assert!(!path.exists());
        let aside: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("recap.json.corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(fs::read(aside[0].path()).unwrap(), b"{ broken");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
