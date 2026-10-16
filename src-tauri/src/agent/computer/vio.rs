//! Vio browser integration: the computer agent drives the user's Electron browser „Vio” through
//! its local automation API instead of UI Automation + screenshots, when it is available.
//!
//! Discovery: Vio writes `{port, token, pid}` to `<config>/moja-przegladarka/automation.json`
//! (Windows `%APPDATA%`, Linux `~/.config`) only while the API is enabled. A background actor
//! (Tauri's tokio runtime) polls the file, checks the pid, connects to `ws://127.0.0.1:port`,
//! authorizes with `{cmd:"auth", token}` (Vio may show a consent dialog: up to 30 s, the Island
//! shows „Zezwól w Vio”) and then serves `{id, cmd, args}` → `{id, ok, result|error}` requests
//! from the agent thread (`Client::call`, 10 s, abortable by the kill switch, ≤ 20 commands/s).
//! The connection drops when the file disappears or the pid dies, and comes back when it returns.
//!
//! A page becomes a [`UiSnapshot`] (`Source::Vio`, ref in `automation_id`), so local plans,
//! safety verdicts and step limits are the same as for any other window. „forbidden” errors
//! (private / Tor / internal tabs, password fields) end the task — never worked around.
//! The token is never logged; `gemini::redact_secrets` scrubs it from error text.

use crate::i18n::{tr, Lang};
use crate::platform::ui::{clip, Rect, Role, Source, UiElement, UiSnapshot, WindowInfo, MAX_ELEMENTS};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

pub const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_secs(2);
/// Vio's rate limit is 20 commands/s.
const MIN_GAP: Duration = Duration::from_millis(50);

/// What Vio writes while its API is enabled. No `Debug`: it holds the token.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub struct Discovery {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

pub fn discovery_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("moja-przegladarka").join("automation.json"))
}

/// The file, parsed, with a live pid.
pub fn discover(path: &Path) -> Option<Discovery> {
    let d: Discovery = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    (d.port != 0 && !d.token.trim().is_empty() && crate::platform::pid_alive(d.pid)).then_some(d)
}

static KNOWN_TOKEN: Mutex<Option<String>> = Mutex::new(None);

/// Scrub the current Vio token from text that leaves the backend.
pub fn redact(text: &str) -> String {
    redact_with(text, KNOWN_TOKEN.lock().as_deref())
}

fn redact_with(text: &str, token: Option<&str>) -> String {
    match token {
        Some(t) if t.len() >= 4 && text.contains(t) => text.replace(t, "[redacted-vio-token]"),
        _ => text.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Unavailable,
    /// Waiting for the auth reply (Vio's consent dialog).
    Authorizing,
    Connected,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VioStatus {
    pub state: State,
    /// Why the last attempt failed (auth refused…).
    pub error: Option<String>,
}

struct Req {
    cmd: String,
    args: Value,
    reply: mpsc::Sender<Result<Value, String>>,
}

struct Shared {
    status: VioStatus,
    tx: Option<tokio::sync::mpsc::UnboundedSender<Req>>,
    pid: u32,
    /// Token Vio refused (or whose consent dialog timed out): no retry — no dialog spam — until
    /// the file changes or the agent is switched off and on.
    refused: Option<String>,
}

pub struct Client {
    path: PathBuf,
    enabled: AtomicBool,
    shared: Mutex<Shared>,
    on_status: Box<dyn Fn(&VioStatus) + Send + Sync>,
}

impl Client {
    /// Spawn the discovery / connection actor.
    pub fn start(path: PathBuf, enabled: bool, on_status: Box<dyn Fn(&VioStatus) + Send + Sync>) -> Arc<Self> {
        let c = Arc::new(Self {
            path,
            enabled: AtomicBool::new(enabled),
            shared: Mutex::new(Shared {
                status: VioStatus { state: State::Unavailable, error: None },
                tx: None,
                pid: 0,
                refused: None,
            }),
            on_status,
        });
        tauri::async_runtime::spawn(Arc::clone(&c).actor());
        c
    }

    pub fn status(&self) -> VioStatus {
        self.shared.lock().status.clone()
    }

    /// Vio's main process (foreground-window match).
    pub fn pid(&self) -> u32 {
        self.shared.lock().pid
    }

    pub fn set_enabled(&self, on: bool) {
        if !self.enabled.swap(on, Ordering::SeqCst) && on {
            self.shared.lock().refused = None;
        }
    }

    fn set_status(&self, state: State, error: Option<String>) {
        let s = VioStatus { state, error };
        {
            let mut sh = self.shared.lock();
            if sh.status == s {
                return;
            }
            sh.status = s.clone();
        }
        (self.on_status)(&s);
    }

    /// Wait while the auth is pending. Err = not connected (with the reason).
    pub fn wait_ready(&self, timeout: Duration) -> Result<(), String> {
        let end = Instant::now() + timeout;
        loop {
            let s = self.status();
            match s.state {
                State::Connected => return Ok(()),
                State::Unavailable => return Err(s.error.unwrap_or_else(|| "Vio is not available.".into())),
                State::Authorizing if Instant::now() < end && !super::killswitch::aborted() => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                State::Authorizing => return Err("Vio did not answer the authorization.".into()),
            }
        }
    }

    /// One command, blocking (agent thread): ≤ 10 s, aborted by the kill switch.
    pub fn call(&self, cmd: &str, args: Value) -> Result<Value, String> {
        let tx = self.shared.lock().tx.clone().ok_or("Vio is not connected.")?;
        let (reply, rx) = mpsc::channel();
        tx.send(Req { cmd: cmd.to_string(), args, reply }).map_err(|_| "Vio is not connected.")?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        loop {
            // The kill switch only applies while a task runs (it stays set after one was stopped).
            if super::killswitch::is_running() && super::killswitch::aborted() {
                return Err(crate::platform::input::ABORTED.into());
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(r) => return r.map_err(|e| format!("Vio: {}", redact(&e))),
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
                Err(RecvTimeoutError::Timeout) => return Err(format!("Vio did not answer {cmd} within 10 s.")),
                Err(RecvTimeoutError::Disconnected) => return Err("The connection to Vio was closed.".into()),
            }
        }
    }

    async fn actor(self: Arc<Self>) {
        loop {
            let found = if self.enabled.load(Ordering::SeqCst) { discover(&self.path) } else { None };
            match found {
                Some(d) => {
                    *KNOWN_TOKEN.lock() = Some(d.token.clone());
                    let refused = self.shared.lock().refused.as_deref() == Some(d.token.as_str());
                    if !refused {
                        let why = self.session(&d).await;
                        if why.is_some() {
                            self.shared.lock().refused = Some(d.token.clone());
                        }
                        self.set_status(State::Unavailable, why);
                    }
                }
                None => self.set_status(State::Unavailable, None),
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// One connection. Returns why the auth was refused (no automatic retry), else `None`.
    async fn session(&self, d: &Discovery) -> Option<String> {
        let url = format!("ws://127.0.0.1:{}", d.port);
        let ws = match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url.as_str())).await {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => {
                eprintln!("[soul][agent] vio connect failed (port {}): {}", d.port, redact(&e.to_string()));
                return None;
            }
            Err(_) => {
                eprintln!("[soul][agent] vio connect timed out (port {})", d.port);
                return None;
            }
        };
        let (mut sink, mut stream) = ws.split();
        self.set_status(State::Authorizing, None);
        eprintln!("[soul][agent] vio connected (port {}, pid {}) — authorizing", d.port, d.pid);
        let auth = json!({ "id": 0, "cmd": "auth", "token": d.token }).to_string();
        if sink.send(Message::Text(auth.into())).await.is_err() {
            return None;
        }
        let reply = tokio::time::timeout(AUTH_TIMEOUT, async {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(Message::Text(t)) => return serde_json::from_str::<Value>(t.as_str()).ok(),
                    Ok(Message::Close(_)) | Err(_) => return None,
                    Ok(_) => {}
                }
            }
            None
        })
        .await;
        match reply {
            Ok(Some(v)) if v["ok"] == json!(true) => {}
            Ok(Some(v)) => {
                let e = v["error"].as_str().map(str::to_string).unwrap_or_else(|| "auth refused".into());
                eprintln!("[soul][agent] vio auth refused: {}", redact(&e));
                return Some(format!("Vio refused access: {}", redact(&e)));
            }
            Ok(None) => return Some("Vio closed the connection during authorization (access denied?).".into()),
            Err(_) => return Some("No answer from Vio within 30 s (consent dialog not accepted).".into()),
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Req>();
        {
            let mut sh = self.shared.lock();
            sh.tx = Some(tx);
            sh.pid = d.pid;
        }
        self.set_status(State::Connected, None);
        eprintln!("[soul][agent] vio authorized");
        let mut pending: HashMap<u64, mpsc::Sender<Result<Value, String>>> = HashMap::new();
        let mut next_id = 0u64;
        let mut last_send = Instant::now() - MIN_GAP;
        let mut check = tokio::time::interval(POLL);
        loop {
            tokio::select! {
                Some(req) = rx.recv() => {
                    let wait = (last_send + MIN_GAP).saturating_duration_since(Instant::now());
                    if !wait.is_zero() {
                        tokio::time::sleep(wait).await;
                    }
                    next_id += 1;
                    let msg = json!({ "id": next_id, "cmd": req.cmd, "args": req.args }).to_string();
                    last_send = Instant::now();
                    if sink.send(Message::Text(msg.into())).await.is_err() {
                        let _ = req.reply.send(Err("connection lost".into()));
                        break;
                    }
                    pending.insert(next_id, req.reply);
                }
                msg = stream.next() => match msg {
                    Some(Ok(Message::Text(t))) => {
                        let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else { continue };
                        if let Some(reply) = v["id"].as_u64().and_then(|id| pending.remove(&id)) {
                            let r = if v["ok"] == json!(true) {
                                Ok(v["result"].clone())
                            } else {
                                Err(v["error"].as_str().map(str::to_string).unwrap_or_else(|| v["error"].to_string()))
                            };
                            let _ = reply.send(r);
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
                _ = check.tick() => {
                    if !self.enabled.load(Ordering::SeqCst) || discover(&self.path).as_ref() != Some(d) {
                        break;
                    }
                }
            }
        }
        // Dropping `pending` fails the waiting calls ("connection closed").
        self.shared.lock().tx = None;
        eprintln!("[soul][agent] vio disconnected");
        None
    }
}

static GLOBAL: OnceLock<Arc<Client>> = OnceLock::new();

/// App startup: watch for Vio (connects only while the computer agent is enabled).
pub fn init(app: &tauri::AppHandle, enabled: bool) {
    let Some(path) = discovery_path() else { return };
    let app = app.clone();
    let client = Client::start(
        path,
        enabled,
        Box::new(move |s: &VioStatus| {
            eprintln!("[soul][agent] vio status={:?}", s.state);
            if s.state == State::Authorizing {
                let lang = crate::i18n::ui_lang(&crate::config::load_settings(&app).unwrap_or_default());
                crate::island::toast(
                    &app,
                    "info",
                    tr(lang, "Zezwól w Vio — Soul prosi o dostęp do przeglądarki.", "Allow it in Vio — Soul asks for browser access."),
                    None,
                );
            }
        }),
    );
    let _ = GLOBAL.set(client);
}

pub fn global() -> Option<Arc<Client>> {
    GLOBAL.get().cloned()
}

/// Vio is connected and authorized (voice routing of browser phrases).
pub fn connected() -> bool {
    GLOBAL.get().is_some_and(|c| c.status().state == State::Connected)
}

pub fn status() -> VioStatus {
    GLOBAL.get().map(|c| c.status()).unwrap_or(VioStatus { state: State::Unavailable, error: None })
}

pub fn is_vio_process(process: &str) -> bool {
    let p = process.to_ascii_lowercase();
    p.trim_end_matches(".exe") == "vio"
}

pub fn is_forbidden(err: &str) -> bool {
    err.to_lowercase().contains("forbidden")
}

pub fn forbidden_message(lang: Lang, err: &str) -> String {
    let e = clip(err, 80);
    tr(
        lang,
        format!("Vio na to nie pozwala ({e}): prywatne, Tor i wewnętrzne karty oraz pola haseł są zablokowane. Zrób to sam(a)."),
        format!("Vio doesn't allow this ({e}): private, Tor and internal tabs and password fields are blocked. Please do it yourself."),
    )
}

/// http(s) only; a bare domain gets `https://`. No javascript:, file:, data:, about:…
pub fn normalize_url(url: &str) -> Result<String, String> {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Ok(u.to_string());
    }
    let scheme = lower.split(':').next().unwrap_or("");
    let has_scheme = lower.contains(':') && !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
    if has_scheme || u.contains(char::is_whitespace) || !u.contains('.') {
        return Err(format!("Only http(s) URLs can be opened, not \u{201e}{}\u{201d}.", clip(u, 60)));
    }
    Ok(format!("https://{u}"))
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().map(str::trim).unwrap_or_default().to_string()
}

/// One `getElements` entry → element (`automation_id` = Vio ref).
fn element(v: &Value) -> Option<UiElement> {
    let r = match &v["ref"] {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let (role, tag, ty) = (text(v, "role").to_lowercase(), text(v, "tag").to_lowercase(), text(v, "type").to_lowercase());
    let role = match (role.as_str(), tag.as_str(), ty.as_str()) {
        ("checkbox" | "switch", _, _) | (_, _, "checkbox") => Role::CheckBox,
        ("radio", _, _) | (_, _, "radio") => Role::Radio,
        ("link", _, _) | (_, "a", _) => Role::Link,
        ("button", _, _) | (_, "button", _) | (_, "input", "submit" | "button" | "reset" | "image") => Role::Button,
        ("combobox" | "listbox", _, _) | (_, "select", _) => Role::ComboBox,
        ("textbox" | "searchbox" | "spinbutton", _, _) | (_, "input" | "textarea", _) => Role::Edit,
        ("tab", _, _) => Role::TabItem,
        ("menuitem", _, _) => Role::MenuItem,
        ("option", _, _) => Role::ListItem,
        _ => Role::Other,
    };
    let name = ["label", "name", "placeholder"].iter().map(|k| text(v, k)).find(|s| !s.is_empty()).unwrap_or_default();
    let mut e = UiElement::new(role, name, Rect::default());
    e.automation_id = r;
    e.web = true;
    e.password = ty == "password";
    e.value = Some(text(v, "value")).filter(|s| !s.is_empty() && !e.password);
    e.checked = v["checked"].as_bool();
    e.empty = v["empty"].as_bool() == Some(true);
    Some(e)
}

/// The active tab as a snapshot. `window` = Vio's window when it is in front (pressKey focus).
pub fn snapshot(tab: &Value, elements: &Value, window: Option<WindowInfo>) -> UiSnapshot {
    let mut w = window.unwrap_or_default();
    let (title, url) = (text(tab, "title"), text(tab, "url"));
    // The URL in the title: the blocklist (bank domains) sees it.
    w.title = if url.is_empty() { title } else { format!("{title} — {url}") };
    if w.process.is_empty() {
        w.process = "Vio".into();
    }
    w.fullscreen = false;
    let all: Vec<UiElement> = elements.as_array().map(|a| a.iter().filter_map(element).collect()).unwrap_or_default();
    let truncated = all.len() > MAX_ELEMENTS;
    let elements = all
        .into_iter()
        .take(MAX_ELEMENTS)
        .enumerate()
        .map(|(i, mut e)| {
            e.id = i as u32 + 1;
            e
        })
        .collect();
    UiSnapshot { gen: 0, window: w, elements, source: Source::Vio, took_ms: 0, truncated }
}

/// A tab id as Vio uses it: a string (numbers accepted from the model / old mocks).
pub fn tab_key(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `listTabs` (`[{tabId, title, url, active}]`) → the active tab (else the first).
pub fn active_tab(tabs: &Value) -> Option<(Value, String)> {
    let list = tabs.as_array()?;
    let tab = list.iter().find(|t| t["active"] == json!(true)).or_else(|| list.first())?;
    Some((tab.clone(), tab_key(&tab["tabId"])?))
}

/// `getElements` of a tab; a sleeping tab („tab not loaded (activateTab first)”) is activated and
/// polled until its page is up (≤ 3 s).
pub fn elements_of(c: &Client, tab: &str) -> Result<Value, String> {
    match c.call("getElements", json!({ "tabId": tab })) {
        Err(e) if e.contains("not loaded") || e.contains("not ready") => {
            c.call("activateTab", json!({ "tabId": tab }))?;
            let mut last = e;
            for _ in 0..10 {
                super::killswitch::sleep(Duration::from_millis(300));
                match c.call("getElements", json!({ "tabId": tab })) {
                    Err(e) if e.contains("not loaded") || e.contains("not ready") => last = e,
                    r => return r,
                }
            }
            Err(last)
        }
        r => r,
    }
}

/// `open_url` through Vio while it is connected (Live tool, router, agent): see [`open_tab`].
/// `None` = not connected, the caller uses the system browser.
pub fn open_url(platform: &dyn crate::platform::Platform, url: &str) -> Option<Result<String, String>> {
    let c = global().filter(|c| c.status().state == State::Connected)?;
    Some(open_tab(&c, platform, url))
}

/// New foreground tab in Vio, then the Vio window in front.
pub fn open_tab(c: &Client, platform: &dyn crate::platform::Platform, url: &str) -> Result<String, String> {
    let u = normalize_url(url)?;
    c.call("openTab", json!({ "url": u }))?;
    if let Err(e) = platform.focus_app("Vio") {
        eprintln!("[soul][agent] vio focus failed: {e}");
    }
    eprintln!("[soul][agent] open_url via vio");
    Ok(format!("Opened {u} in Vio"))
}

/// `listTabs` for the model (READ).
pub fn tabs_text(tabs: &Value) -> String {
    tabs.as_array()
        .map(|a| {
            a.iter()
                .map(|t| {
                    format!(
                        "tab_id={} \"{}\" {}{}",
                        tab_key(&t["tabId"]).unwrap_or_default(),
                        crate::platform::ui::sanitize(&clip(&text(t, "title"), 80)),
                        clip(&text(t, "url"), 120),
                        if t["active"] == json!(true) { " (active)" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// `getPage` for the model (READ).
pub fn page_text(page: &Value) -> String {
    format!("{}\n{}\n\n{}", text(page, "title"), text(page, "url"), text(page, "text"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_http_only() {
        assert_eq!(normalize_url("https://example.com/a b").unwrap(), "https://example.com/a b");
        assert_eq!(normalize_url(" youtube.com ").unwrap(), "https://youtube.com");
        for bad in ["javascript:alert(1)", "file:///C:/x", "data:text/html,x", "about:blank", "chrome://settings", "kot", "dwa słowa.pl"] {
            assert!(normalize_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn elements_map_to_a_snapshot_with_empty_flags() {
        let tab = json!({ "tabId": 4, "title": "Formularz", "url": "https://example.com/f", "active": true });
        let els = json!([
            { "ref": "r1", "role": "textbox", "tag": "input", "type": "text", "label": "Imię", "value": "Oskar", "empty": false },
            { "ref": "r2", "role": "", "tag": "input", "type": "email", "label": "", "placeholder": "E-mail", "value": "", "empty": true },
            { "ref": "r3", "role": "", "tag": "input", "type": "password", "label": "Hasło", "value": "sekret", "empty": false },
            { "ref": "r4", "role": "checkbox", "tag": "input", "type": "checkbox", "label": "Newsletter", "checked": true },
            { "ref": "r5", "role": "button", "tag": "button", "label": "Wyślij" },
            { "role": "button", "label": "bez ref" }
        ]);
        let s = snapshot(&tab, &els, None);
        assert_eq!(s.source, Source::Vio);
        assert_eq!(s.window.title, "Formularz — https://example.com/f");
        assert_eq!(s.elements.len(), 5);
        assert_eq!(s.get(2).unwrap().automation_id, "r2");
        let pw = s.get(3).unwrap();
        assert!(pw.password && pw.value.is_none(), "password values never reach the model");
        let obs = s.observation();
        assert!(obs.contains("1 edit \"Imię\" value=\"Oskar\"\n"), "{obs}");
        assert!(obs.contains("2 edit \"E-mail\" empty\n"), "{obs}");
        assert!(obs.contains("4 checkbox \"Newsletter\" checked\n"), "{obs}");
        assert!(obs.contains("5 button \"Wyślij\"\n"), "{obs}");
        assert!(!obs.contains('@'), "no coordinates for Vio");
        assert_eq!(active_tab(&json!([{ "tabId": 1 }, tab])).unwrap().1, "4");
        // Real Vio: string ids.
        let real = json!([{ "tabId": "t-a", "title": "httpbin", "url": "https://httpbin.org/forms/post", "active": false },
                          { "tabId": "t-b", "title": "Wiki", "url": "https://pl.wikipedia.org/", "active": true }]);
        assert_eq!(active_tab(&real).unwrap().1, "t-b");
        assert!(tabs_text(&real).contains("tab_id=t-b \"Wiki\""), "{}", tabs_text(&real));
    }

    /// Real running Vio (API toggle on): `cargo test --lib vio_live_probe -- --ignored --nocapture`.
    /// Prints listTabs + getElements of the active tab; accept „Zezwól” in Vio if asked.
    #[test]
    #[ignore]
    fn vio_live_probe() {
        let path = discovery_path().expect("no config dir");
        assert!(discover(&path).is_some(), "Vio API is off or Vio is not running ({})", path.display());
        let c = Client::start(path, true, Box::new(|s| eprintln!("[probe] status {:?}", s.state)));
        let end = Instant::now() + AUTH_TIMEOUT + Duration::from_secs(5);
        while c.status().state != State::Connected && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(100));
        }
        c.wait_ready(Duration::from_secs(1)).expect("not connected");
        if std::env::var_os("SOUL_VIO_PROBE_OPEN").is_some() {
            // Diagnoses an empty listTabs: a fresh tab (no webContents yet) vs. after it loaded.
            println!("openTab: {:?}", c.call("openTab", json!({ "url": "https://example.com" })));
            println!("listTabs right after: {}", c.call("listTabs", json!({})).unwrap_or_default());
            std::thread::sleep(Duration::from_secs(3));
            println!("listTabs after 3 s: {}", c.call("listTabs", json!({})).unwrap_or_default());
        }
        let tabs = c.call("listTabs", json!({})).expect("listTabs");
        println!("listTabs raw:
{}", serde_json::to_string_pretty(&tabs).unwrap());
        println!("tabs_text:
{}", tabs_text(&tabs));
        let (tab, id) = active_tab(&tabs).expect("Vio has no open tab");
        let els = elements_of(&c, &id).expect("getElements");
        let n = els.as_array().map_or(0, Vec::len);
        println!("getElements tab={id}: {n} elements, first 15:");
        for e in els.as_array().into_iter().flatten().take(15) {
            println!("  {e}");
        }
        println!("observation:
{}", snapshot(&tab, &els, None).observation());
    }

    #[test]
    fn token_is_redacted_and_forbidden_detected() {
        assert_eq!(redact_with("auth tok-123456 failed", Some("tok-123456")), "auth [redacted-vio-token] failed");
        assert_eq!(redact_with("nothing here", Some("tok-123456")), "nothing here");
        assert!(is_forbidden("Vio: Forbidden: private tab"));
        assert!(!is_forbidden("Vio: ref expired"));
        assert!(is_vio_process("Vio.exe") && is_vio_process("vio") && !is_vio_process("vivaldi.exe"));
    }
}
