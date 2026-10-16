//! Rolling conversation context for voice follow-ups: the last `MAX_EXCHANGES` exchanges
//! (user transcript, tools + args, Soul's answer), forgotten after `context_timeout_secs` of
//! inactivity. Passed to the Live commands model and the REST paths as a `[CONTEXT]` block, and
//! used by the deterministic follow-up resolver („a teraz głośniej”, „i zamknij go”).
//!
//! Text actions are recorded without their text: a selection never reaches a later prompt.

use super::router::{self, fold_word, PreRoute};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

pub const MAX_EXCHANGES: usize = 3;
pub const DEFAULT_TIMEOUT_SECS: u32 = 90;
/// Follow-up rules only look at short utterances.
const MAX_FOLLOWUP_WORDS: usize = 6;
/// Tool name recorded for text actions (args never contain the text).
pub const TEXT_ACTION_TOOL: &str = "text_action";

#[derive(Debug, Clone, PartialEq)]
pub struct ToolUse {
    pub name: String,
    pub args: Value,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Exchange {
    pub user: String,
    pub tools: Vec<ToolUse>,
    pub answer: String,
}

#[derive(Debug)]
pub struct Context {
    exchanges: VecDeque<Exchange>,
    last: Option<Instant>,
}

impl Context {
    pub const fn new() -> Self {
        Self {
            exchanges: VecDeque::new(),
            last: None,
        }
    }

    pub fn push(&mut self, ex: Exchange, now: Instant) {
        while self.exchanges.len() >= MAX_EXCHANGES {
            self.exchanges.pop_front();
        }
        self.exchanges.push_back(ex);
        self.last = Some(now);
    }

    pub fn is_active(&self, timeout: Duration, now: Instant) -> bool {
        !self.exchanges.is_empty()
            && self
                .last
                .is_some_and(|t| now.saturating_duration_since(t) < timeout)
    }

    /// Drops everything once the timeout passed. True when something was dropped.
    pub fn expire(&mut self, timeout: Duration, now: Instant) -> bool {
        if !self.exchanges.is_empty() && !self.is_active(timeout, now) {
            self.clear();
            return true;
        }
        false
    }

    pub fn clear(&mut self) {
        self.exchanges.clear();
        self.last = None;
    }

    pub fn len(&self) -> usize {
        self.exchanges.len()
    }

    /// The most recent successful tool (newest exchange first).
    pub fn last_tool(&self) -> Option<&ToolUse> {
        self.exchanges
            .iter()
            .rev()
            .flat_map(|e| e.tools.iter().rev())
            .find(|t| t.ok && t.name != "answer")
    }

    /// `[CONTEXT]` block prepended to a follow-up command (oldest exchange first).
    pub fn prompt_block(&self) -> String {
        let mut out = String::from(
            "[CONTEXT] Recent exchanges with the user, oldest first. Use them only to resolve a follow-up \
(\u{201e}a jutro?\u{201d}, \u{201e}i zamknij go\u{201d}, \u{201e}głośniej\u{201d}, \u{201e}a ten drugi?\u{201d}); if the new command is unrelated, ignore them.\n",
        );
        for (i, ex) in self.exchanges.iter().enumerate() {
            out.push_str(&format!("{}. User: \u{201e}{}\u{201d}", i + 1, ex.user));
            for t in &ex.tools {
                out.push_str(&format!(
                    " → Soul ran {} {}{}",
                    t.name,
                    t.args,
                    if t.ok { "" } else { " (failed)" }
                ));
            }
            if !ex.answer.trim().is_empty() {
                out.push_str(&format!(" → Soul: \u{201e}{}\u{201d}", ex.answer));
            }
            out.push('\n');
        }
        out.push_str("[/CONTEXT]");
        out
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

/// Wraps a command with the context block (the model sees what „to”/„go”/„a jutro” refer to).
pub fn with_block(block: &str, command: &str) -> String {
    format!("{block}\nNew command: {command}")
}

// ───────────────────────────── utterance shape ─────────────────────────────

/// Relative words that only make sense after something („głośniej”, „jeszcze raz”).
const RELATIVE: &[&str] = &[
    "glosniej", "ciszej", "wiecej", "mniej", "szybciej", "wolniej", "ponownie", "louder", "quieter",
    "softer", "more", "less", "again",
];
const DEMONSTRATIVES: &[&str] = &[
    "ten", "ta", "to", "tamten", "tamta", "tamto", "drugi", "druga", "drugie", "pierwszy",
    "pierwsza", "trzeci", "trzecia", "ostatni", "ostatnia", "that", "one", "second", "first",
    "last", "other",
];
const LOUDER: &[&str] = &["glosniej", "podglosnij", "podglos", "louder"];
const QUIETER: &[&str] = &["ciszej", "scisz", "przycisz", "quieter", "softer"];
const PRONOUNS: &[&str] = &[
    "go", "ja", "je", "jego", "niego", "nia", "ich", "nich", "to", "tego", "it", "them", "this",
    "that",
];

fn folded_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(fold_word)
        .filter(|w| !w.is_empty())
        .collect()
}

/// Smart routing: does this short utterance read like a follow-up to the previous command?
/// Only asked while context is alive. Dictation traps („Odwiedziłem wczoraj…”, „Proszę o
/// przesłanie…”) have none of these markers. With a text field focused the connector opener
/// alone („A to dobry pomysł”) is not enough — a relative word or a short question is needed.
pub fn looks_like_followup(text: &str, text_field_focused: bool) -> bool {
    let raw = text.trim();
    let words = folded_words(raw);
    if words.is_empty() || words.len() > MAX_FOLLOWUP_WORDS {
        return false;
    }
    let relative = words.iter().any(|w| RELATIVE.contains(&w.as_str()))
        || words.windows(2).any(|p| p[0] == "jeszcze" && p[1] == "raz");
    let question = raw.ends_with('?') && words.len() <= 4;
    let demonstrative = words.len() <= 3
        && words.iter().any(|w| DEMONSTRATIVES.contains(&w.as_str()))
        && !words.iter().all(|w| w == "to");
    let opener = matches!(words[0].as_str(), "a" | "i" | "and" | "also" | "oraz")
        || words.starts_with(&["what".into(), "about".into()])
        || words.starts_with(&["how".into(), "about".into()])
        || words.starts_with(&["co".into(), "z".into()]);
    relative || question || demonstrative || (opener && !text_field_focused)
}

/// „zapomnij”, „nowy temat”, „new topic”: clear the conversation context.
pub fn is_reset_phrase(text: &str) -> bool {
    let words = folded_words(text);
    let (skip, _) = router::strip_filler(&words);
    let core = words[skip..].join(" ");
    matches!(
        core.as_str(),
        "zapomnij"
            | "zapomnij o tym"
            | "zapomnij to"
            | "nowy temat"
            | "zmien temat"
            | "zmiana tematu"
            | "inny temat"
            | "zacznijmy od nowa"
            | "new topic"
            | "change topic"
            | "change the topic"
            | "change the subject"
            | "forget it"
            | "forget that"
            | "forget this"
    )
}

/// Deterministic follow-up after `last` (the newest successful tool): „a teraz głośniej” after
/// set_voice_volume, „i zamknij go” after open_app, „anuluj go” after set_timer. `None` = let
/// the model resolve it with the `[CONTEXT]` block.
pub fn resolve_followup(text: &str, last: &ToolUse) -> Option<PreRoute> {
    let words = folded_words(text);
    if words.is_empty() || words.len() > MAX_FOLLOWUP_WORDS {
        return None;
    }
    let has = |set: &[&str]| words.iter().any(|w| set.contains(&w.as_str()));
    let dir: i64 = if has(LOUDER) {
        1
    } else if has(QUIETER) {
        -1
    } else {
        0
    };
    // „o 20” / „o 20 procent”: the step size.
    let step = words
        .windows(2)
        .find(|p| p[0] == "o" || p[0] == "by")
        .and_then(|p| p[1].trim_end_matches('%').parse::<i64>().ok())
        .filter(|n| (1..=100).contains(n));
    // „i zamknij go” / „zamknij” — but not „zamknij Spotify” (names another target).
    let only = |verbs: &[&str]| {
        words.iter().all(|w| {
            verbs.contains(&w.as_str())
                || PRONOUNS.contains(&w.as_str())
                || matches!(w.as_str(), "i" | "a" | "teraz" | "tez" | "juz" | "prosze" | "and" | "now" | "please")
        })
    };
    let pronoun = has(PRONOUNS);
    let again = has(&["ponownie", "again", "back"]) || text.contains("z powrotem");
    let name = || last.args.get("name").and_then(Value::as_str).map(str::to_string);
    let route = |tool: &str, args: Value| {
        Some(PreRoute {
            tool: tool.into(),
            args,
            reason: format!("followup:{}", last.name),
        })
    };
    match last.name.as_str() {
        "set_voice_volume" if dir != 0 => {
            route("set_voice_volume", json!({ "delta": dir * step.unwrap_or(20) }))
        }
        "set_volume" | "media_control" | "now_playing" | "youtube_play" | "run_scene" if dir != 0 => {
            route("set_volume", json!({ "delta": dir * step.unwrap_or(10) }))
        }
        "open_app" | "focus_app" => {
            let name = name()?;
            const CLOSE: &[&str] = &["zamknij", "close", "quit", "wylacz"];
            const FOCUS: &[&str] = &["pokaz", "przelacz", "przejdz", "focus", "show", "switch", "na", "to"];
            if has(CLOSE) && only(CLOSE) {
                route("close_app", json!({ "name": name }))
            } else if has(FOCUS) && pronoun && only(FOCUS) {
                route("focus_app", json!({ "name": name }))
            } else {
                None
            }
        }
        "close_app" => {
            let name = name()?;
            const OPEN: &[&str] = &[
                "otworz", "odpal", "uruchom", "wlacz", "open", "launch", "z", "powrotem", "ponownie",
                "again", "back", "it",
            ];
            let verb = has(&["otworz", "odpal", "uruchom", "wlacz", "open", "launch"]);
            (verb && (pronoun || again) && only(OPEN))
                .then(|| route("open_app", json!({ "name": name })))
                .flatten()
        }
        "set_timer" => {
            const CANCEL: &[&str] = &["anuluj", "usun", "wylacz", "cancel", "stop"];
            if has(CANCEL) && only(CANCEL) {
                let label = last.args.get("label").and_then(Value::as_str).unwrap_or_default();
                let args = if label.is_empty() {
                    json!({})
                } else {
                    json!({ "label": label })
                };
                route("cancel_timer", args)
            } else {
                None
            }
        }
        // „a teraz to samo na YouTube” / „to samo w Google”: the previous query, other engine.
        "web_search" | "youtube_search" | "youtube_play" if is_same_reference(&words) => {
            let query = last.args.get("query").and_then(Value::as_str)?;
            if has(&["youtube", "youtubie", "jutub", "jutubie", "yt"]) {
                route("youtube_search", json!({ "query": query }))
            } else if has(&["google", "googlu", "internecie", "necie", "web", "przegladarce"]) {
                route("web_search", json!({ "query": query }))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// „to samo”, „tego samego”, „the same”: the utterance points back at the previous request.
pub fn is_same_reference(words: &[String]) -> bool {
    words.iter().any(|w| matches!(w.as_str(), "samo" | "samego" | "same"))
}

// ───────────────────────────── shared state ─────────────────────────────

struct Pending {
    user: String,
    tools: Vec<ToolUse>,
    /// A text action ran: keep only the action, never the text or its result.
    private: bool,
}

struct Shared {
    ctx: Context,
    pending: Pending,
    /// Id of the command `pending` belongs to (a barge-in starts a new one mid-reply).
    turn: u64,
}

static STATE: Mutex<Shared> = Mutex::new(Shared {
    ctx: Context::new(),
    pending: Pending {
        user: String::new(),
        tools: Vec::new(),
        private: false,
    },
    turn: 0,
});

pub fn timeout(settings: &crate::config::Settings) -> Duration {
    Duration::from_secs(u64::from(settings.context_timeout_secs.clamp(15, 600)))
}

/// A new command starts: forget what the previous one noted. Returns its id for `commit`.
pub fn begin_turn() -> u64 {
    let mut st = STATE.lock();
    st.pending.user.clear();
    st.pending.tools.clear();
    st.pending.private = false;
    st.turn += 1;
    st.turn
}

/// The command's final transcript (first one wins; wake word already stripped).
pub fn note_user(text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let mut st = STATE.lock();
    if st.pending.user.is_empty() {
        st.pending.user = text.chars().take(200).collect();
    }
}

pub fn note_tool(name: &str, args: &Value, ok: bool) {
    let mut st = STATE.lock();
    if name == TEXT_ACTION_TOOL {
        st.pending.private = true;
    }
    st.pending.tools.push(ToolUse {
        name: name.to_string(),
        args: args.clone(),
        ok,
    });
}

/// Command `turn` finished: store the exchange (when context is on) and show the Island dot.
/// A no-op when a newer command already started.
pub fn commit(app: &AppHandle, turn: u64, answer: &str) {
    let settings = crate::config::load_settings(app).unwrap_or_default();
    let mut st = STATE.lock();
    if st.turn != turn {
        return;
    }
    let user = std::mem::take(&mut st.pending.user);
    let tools = std::mem::take(&mut st.pending.tools);
    let private = std::mem::replace(&mut st.pending.private, false);
    if !settings.context_enabled || user.is_empty() {
        return;
    }
    let answer = if private {
        "(wynik akcji na zaznaczonym tekście nie jest zapamiętywany)".to_string()
    } else {
        answer.trim().chars().take(300).collect()
    };
    let now = Instant::now();
    let timeout = timeout(&settings);
    st.ctx.expire(timeout, now);
    st.ctx.push(Exchange { user, tools, answer }, now);
    eprintln!("[soul][agent] context exchanges={}", st.ctx.len());
    drop(st);
    emit(app, Some(timeout));
}

/// „nowy temat”: forget the context; the reset command itself is not recorded either.
pub fn clear(app: &AppHandle) {
    {
        let mut st = STATE.lock();
        st.ctx.clear();
        st.pending.user.clear();
        st.pending.tools.clear();
    }
    eprintln!("[soul][agent] context cleared");
    emit(app, None);
}

/// Context is on and an exchange happened within the timeout.
pub fn is_active(settings: &crate::config::Settings) -> bool {
    settings.context_enabled && STATE.lock().ctx.is_active(timeout(settings), Instant::now())
}

/// The `[CONTEXT]` block while context is alive.
pub fn active_block(settings: &crate::config::Settings) -> Option<String> {
    if !settings.context_enabled {
        return None;
    }
    let mut st = STATE.lock();
    let timeout = timeout(settings);
    st.ctx.expire(timeout, Instant::now());
    st.ctx
        .is_active(timeout, Instant::now())
        .then(|| st.ctx.prompt_block())
}

pub fn last_tool(settings: &crate::config::Settings) -> Option<ToolUse> {
    if !is_active(settings) {
        return None;
    }
    STATE.lock().ctx.last_tool().cloned()
}

/// Test harness: seed the context with recorded exchanges.
#[cfg(test)]
pub fn seed(exchanges: Vec<Exchange>) {
    let mut st = STATE.lock();
    st.ctx.clear();
    for ex in exchanges {
        st.ctx.push(ex, Instant::now());
    }
}

#[cfg(test)]
pub fn reset_for_test() {
    STATE.lock().ctx.clear();
    let _ = begin_turn();
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ContextPayload {
    active: bool,
    /// Wall-clock expiry (ms since epoch) so the Island hides the dot on its own.
    expires_at_ms: i64,
}

fn emit(app: &AppHandle, alive_for: Option<Duration>) {
    let payload = ContextPayload {
        active: alive_for.is_some(),
        expires_at_ms: alive_for
            .map(|d| chrono::Utc::now().timestamp_millis() + d.as_millis() as i64)
            .unwrap_or(0),
    };
    let _ = app.emit_to(
        tauri::EventTarget::webview_window("island"),
        "island://context",
        payload,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, args: Value) -> ToolUse {
        ToolUse {
            name: name.into(),
            args,
            ok: true,
        }
    }

    fn ex(user: &str, t: ToolUse) -> Exchange {
        Exchange {
            user: user.into(),
            tools: vec![t],
            answer: "ok".into(),
        }
    }

    #[test]
    fn keeps_last_three_and_expires() {
        let mut c = Context::new();
        let t0 = Instant::now();
        for i in 0..5 {
            c.push(ex(&format!("u{i}"), tool("open_app", json!({ "name": "x" }))), t0);
        }
        assert_eq!(c.len(), 3);
        assert!(c.prompt_block().contains("u2") && !c.prompt_block().contains("u1"));
        let timeout = Duration::from_secs(90);
        assert!(c.is_active(timeout, t0 + Duration::from_secs(89)));
        assert!(!c.is_active(timeout, t0 + Duration::from_secs(91)));
        assert!(c.expire(timeout, t0 + Duration::from_secs(91)));
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn last_tool_skips_failures_and_answers() {
        let mut c = Context::new();
        let now = Instant::now();
        c.push(ex("otwórz Discord", tool("open_app", json!({ "name": "Discord" }))), now);
        c.push(
            Exchange {
                user: "jak się masz".into(),
                tools: vec![tool("answer", json!({ "text": "dobrze" }))],
                answer: "dobrze".into(),
            },
            now,
        );
        assert_eq!(c.last_tool().unwrap().name, "open_app");
    }

    #[test]
    fn followup_shapes() {
        for t in ["A teraz głośniej.", "a jutro?", "A ten drugi?", "i zamknij go", "jeszcze raz", "ciszej", "What about tomorrow?"] {
            assert!(looks_like_followup(t, false), "{t}");
        }
        // Recorded dictation traps and ordinary sentences stay dictation.
        for t in [
            "Odwiedziłem wczoraj nowy sklep kolegi.",
            "Proszę o przesłanie raportu do piątku.",
            "Spotkanie przenosimy na piątek o 10:00.",
            "Kup mleko, to znaczy kefir i chleb.",
            "Let's meet tomorrow at 5:00.",
            "Zażółć gęślą jaźń.",
            "Dalej nie wiem, co o tym myśleć.",
            "To jest bardzo długie zdanie które ktoś dyktuje do dokumentu.",
        ] {
            assert!(!looks_like_followup(t, false), "{t}");
        }
        // A connector opener alone is not enough while typing in a text field.
        assert!(looks_like_followup("A w Krakowie", false));
        assert!(!looks_like_followup("A w Krakowie", true));
        assert!(looks_like_followup("a jutro?", true));
    }

    #[test]
    fn deterministic_followups() {
        let voice = tool("set_voice_volume", json!({ "delta": -20 }));
        let r = resolve_followup("A teraz głośniej.", &voice).unwrap();
        assert_eq!((r.tool.as_str(), &r.args), ("set_voice_volume", &json!({ "delta": 20 })));

        let sys = tool("set_volume", json!({ "percent": 30 }));
        let r = resolve_followup("jeszcze ciszej", &sys).unwrap();
        assert_eq!((r.tool.as_str(), &r.args), ("set_volume", &json!({ "delta": -10 })));
        let r = resolve_followup("a teraz głośniej o 20", &sys).unwrap();
        assert_eq!(r.args, json!({ "delta": 20 }));

        let open = tool("open_app", json!({ "name": "Discord" }));
        let r = resolve_followup("i zamknij go", &open).unwrap();
        assert_eq!((r.tool.as_str(), &r.args), ("close_app", &json!({ "name": "Discord" })));
        assert!(resolve_followup("zamknij Spotify i Chrome teraz", &open).is_none());
        assert!(resolve_followup("zamknij Spotify", &open).is_none());

        let closed = tool("close_app", json!({ "name": "Spotify" }));
        assert_eq!(resolve_followup("otwórz go z powrotem", &closed).unwrap().tool, "open_app");
        assert!(resolve_followup("otwórz Discord z powrotem", &closed).is_none());

        let timer = tool("set_timer", json!({ "seconds": 600, "label": "makaron" }));
        let r = resolve_followup("anuluj go", &timer).unwrap();
        assert_eq!((r.tool.as_str(), &r.args), ("cancel_timer", &json!({ "label": "makaron" })));

        // Questions are for the model (with the [CONTEXT] block).
        let search = tool("web_search", json!({ "query": "pogoda Kraków" }));
        assert!(resolve_followup("a jutro?", &search).is_none());

        // „to samo na YouTube”: the previous query on the other engine (never a search for „samo”).
        let bigos = tool("web_search", json!({ "query": "przepis na bigos" }));
        let r = resolve_followup("A teraz to samo na YouTube.", &bigos).unwrap();
        assert_eq!((r.tool.as_str(), &r.args), ("youtube_search", &json!({ "query": "przepis na bigos" })));
        let yt = tool("youtube_search", json!({ "query": "queen" }));
        assert_eq!(resolve_followup("to samo w Google", &yt).unwrap().tool, "web_search");
        assert!(resolve_followup("to samo", &yt).is_none());
        assert!(router::match_command("A teraz to samo na YouTube.").is_none());
    }

    #[test]
    fn reset_phrases() {
        for t in ["Zapomnij.", "nowy temat", "Zmień temat", "new topic", "ej, zapomnij o tym"] {
            assert!(is_reset_phrase(t), "{t}");
        }
        assert!(!is_reset_phrase("zapomnij o serwerze"));
        assert!(!is_reset_phrase("nowy temat maila to faktura"));
    }

    #[test]
    fn prompt_block_mentions_tools_and_answers() {
        let mut c = Context::new();
        c.push(ex("mów ciszej", tool("set_voice_volume", json!({ "delta": -20 }))), Instant::now());
        let b = c.prompt_block();
        assert!(b.starts_with("[CONTEXT]") && b.ends_with("[/CONTEXT]"));
        assert!(b.contains("set_voice_volume") && b.contains("mów ciszej"));
        assert!(with_block(&b, "a teraz głośniej").ends_with("New command: a teraz głośniej"));
    }
}
