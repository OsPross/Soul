//! Vio browser integration: the computer agent drives the user's Electron browser „Vio” through
//! its local automation API instead of UI Automation + screenshots, when it is available.
//!
//! Discovery: Vio writes `{port, token, pid}` to `<config>/moja-przegladarka/automation.json`
//! (Windows `%APPDATA%`, Linux `~/.config`) only while the API is enabled. A background actor
//! (Tauri's tokio runtime) polls the file, checks the pid, connects to `ws://127.0.0.1:port`,
//! authorizes with `{cmd:"auth", token}` (Vio may show a consent dialog: up to 30 s, the Island
//! shows „Zezwól w Vio”) and then serves `{id, cmd, args}` → `{id, ok, result|error}` requests
//! from the agent thread (`Client::call`, 10 s, abortable by the kill switch, ≤ 20 commands/s).
//! The connection drops when the file disappears or the pid dies, and comes back when it returns
//! (failed connects back off 2 → 30 s).
//!
//! API v2 (`docs/automation-api.md` in Vio): after auth Soul sends `hello`, keeps the capabilities
//! and subscribes to the events Vio announces (`vio_ctx.rs`: browser context, Soul icon / panel /
//! STOP signals) and mirrors its state with a debounced `setSoulState`. Every v2 feature is gated
//! on the capabilities, so an old Vio (no `hello`) keeps the v1 behaviour.
//!
//! A page becomes a [`UiSnapshot`] (`Source::Vio`, ref in `automation_id`), so local plans,
//! safety verdicts and step limits are the same as for any other window. „forbidden” errors
//! (private / Tor / internal tabs, password fields) end the task — never worked around.
//! The token is never logged; `gemini::redact_secrets` scrubs it from error text.

use super::action::FillValue;
use super::vio_ctx::{next_backoff, BrowserCtx, Caps, Signal, StateGate, TextRoute};
use crate::i18n::{tr, Lang};
use crate::platform::ui::{clip, Rect, Role, Source, UiElement, UiSnapshot, WindowInfo, MAX_ELEMENTS};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the actor checks for a `setSoulState` to send.
const STATE_TICK: Duration = Duration::from_millis(100);
/// Vio's answer for 3 s after the user pressed STOP: expected, never an error.
const STOPPED: &str = "stopped by user";

/// How a session ended.
enum End {
    /// Auth refused / consent not given: no automatic retry (no dialog spam).
    Refused(String),
    /// No connection at all (port closed): retry with backoff.
    ConnectFailed,
    /// A working session ended (Vio quit, file changed, agent switched off).
    Closed,
}

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
    /// From `hello`; `None` = v1 Vio (or not connected).
    caps: Option<Caps>,
    ctx: BrowserCtx,
    /// The Soul state Vio should show (state, tooltip); sent by the actor, debounced.
    want: (String, String),
}

type SignalFn = Arc<dyn Fn(Signal) + Send + Sync>;

pub struct Client {
    path: PathBuf,
    enabled: AtomicBool,
    shared: Mutex<Shared>,
    on_status: Box<dyn Fn(&VioStatus) + Send + Sync>,
    on_signal: Mutex<Option<SignalFn>>,
    /// Bumped by [`Client::cancel_all`]: waiting calls return „aborted”.
    cancel_gen: AtomicU64,
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
                caps: None,
                ctx: BrowserCtx::default(),
                want: ("idle".into(), String::new()),
            }),
            on_status,
            on_signal: Mutex::new(None),
            cancel_gen: AtomicU64::new(0),
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

    /// Soul icon / panel / STOP from Vio.
    pub fn on_signal(&self, f: SignalFn) {
        *self.on_signal.lock() = Some(f);
    }

    /// Vio announced this (v2) command.
    pub fn has(&self, cmd: &str) -> bool {
        self.shared.lock().caps.as_ref().is_some_and(|c| c.has(cmd))
    }

    pub fn caps(&self) -> Option<Caps> {
        self.shared.lock().caps.clone()
    }

    /// The browser context from events (`live: false` without them).
    pub fn context(&self) -> BrowserCtx {
        self.shared.lock().ctx.clone()
    }

    /// Change the context Soul keeps itself (the playback rate it set).
    pub fn update_context(&self, f: impl FnOnce(&mut BrowserCtx)) {
        f(&mut self.shared.lock().ctx);
    }

    /// The state the Soul icon in Vio should show; the actor sends it (debounced) when Vio has
    /// `setSoulState`.
    pub fn set_soul_state(&self, state: &str, text: &str) {
        self.shared.lock().want = (state.to_string(), clip(text, 200));
    }

    /// Fail every waiting call now (STOP): they return „aborted”.
    pub fn cancel_all(&self) {
        self.cancel_gen.fetch_add(1, Ordering::SeqCst);
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

    /// One command, blocking (agent thread): ≤ 10 s, aborted by the kill switch, by
    /// [`Client::cancel_all`] and by Vio's „stopped by user” (the 3 s after STOP).
    pub fn call(&self, cmd: &str, args: Value) -> Result<Value, String> {
        let gen = self.cancel_gen.load(Ordering::SeqCst);
        let tx = self.shared.lock().tx.clone().ok_or("Vio is not connected.")?;
        let (reply, rx) = mpsc::channel();
        tx.send(Req { cmd: cmd.to_string(), args, reply }).map_err(|_| "Vio is not connected.")?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        let aborted = || Err(crate::platform::input::ABORTED.to_string());
        loop {
            // The kill switch only applies while a task runs (it stays set after one was stopped).
            if (super::killswitch::is_running() && super::killswitch::aborted()) || self.cancel_gen.load(Ordering::SeqCst) != gen {
                return aborted();
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Err(e)) if e.contains(STOPPED) => {
                    eprintln!("[soul][agent] vio {cmd}: stopped by the user (expected)");
                    return aborted();
                }
                Ok(r) => return r.map_err(|e| format!("Vio: {}", redact(&e))),
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
                Err(RecvTimeoutError::Timeout) => return Err(format!("Vio did not answer {cmd} within 10 s.")),
                Err(RecvTimeoutError::Disconnected) => return Err("The connection to Vio was closed.".into()),
            }
        }
    }

    async fn actor(self: Arc<Self>) {
        let mut backoff = POLL;
        loop {
            let found = if self.enabled.load(Ordering::SeqCst) { discover(&self.path) } else { None };
            let mut wait = POLL;
            match found {
                Some(d) => {
                    *KNOWN_TOKEN.lock() = Some(d.token.clone());
                    let refused = self.shared.lock().refused.as_deref() == Some(d.token.as_str());
                    if !refused {
                        let why = match self.session(&d).await {
                            End::Refused(why) => {
                                self.shared.lock().refused = Some(d.token.clone());
                                Some(why)
                            }
                            End::ConnectFailed => {
                                backoff = next_backoff(backoff);
                                wait = backoff;
                                None
                            }
                            End::Closed => {
                                backoff = POLL;
                                None
                            }
                        };
                        self.set_status(State::Unavailable, why);
                    }
                }
                None => {
                    backoff = POLL;
                    self.set_status(State::Unavailable, None);
                }
            }
            tokio::time::sleep(wait).await;
        }
    }

    /// One connection: auth, `hello` + `subscribe` (v2), then requests, events and state.
    async fn session(&self, d: &Discovery) -> End {
        let url = format!("ws://127.0.0.1:{}", d.port);
        let ws = match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url.as_str())).await {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => {
                eprintln!("[soul][agent] vio connect failed (port {}): {}", d.port, redact(&e.to_string()));
                return End::ConnectFailed;
            }
            Err(_) => {
                eprintln!("[soul][agent] vio connect timed out (port {})", d.port);
                return End::ConnectFailed;
            }
        };
        let (mut sink, mut stream) = ws.split();
        self.set_status(State::Authorizing, None);
        eprintln!("[soul][agent] vio connected (port {}, pid {}) — authorizing", d.port, d.pid);
        let auth = json!({ "id": 0, "cmd": "auth", "token": d.token }).to_string();
        if sink.send(Message::Text(auth.into())).await.is_err() {
            return End::Closed;
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
                return End::Refused(format!("Vio refused access: {}", redact(&e)));
            }
            Ok(None) => return End::Refused("Vio closed the connection during authorization (access denied?).".into()),
            Err(_) => return End::Refused("No answer from Vio within 30 s (consent dialog not accepted).".into()),
        }

        // v2 handshake. A v1 Vio answers „unknown command”: no capabilities, the old behaviour.
        let mut next_id = 0u64;
        let mut ask = |cmd: &str, args: Value| {
            next_id += 1;
            (next_id, json!({ "id": next_id, "cmd": cmd, "args": args }).to_string())
        };
        let (id, hello) = ask("hello", json!({ "client": "soul", "version": env!("CARGO_PKG_VERSION") }));
        let mut early = Vec::new();
        let caps = match handshake_call(&mut sink, &mut stream, id, hello, &mut early).await {
            Ok(v) => Caps::parse(&v),
            Err(e) => {
                eprintln!("[soul][agent] vio hello: {} — API v1", redact(&e));
                None
            }
        };
        let events = caps.as_ref().map(Caps::subscribable).unwrap_or_default();
        let mut live = false;
        if !events.is_empty() {
            let (id, sub) = ask("subscribe", json!({ "events": events }));
            match handshake_call(&mut sink, &mut stream, id, sub, &mut early).await {
                Ok(_) => live = true,
                Err(e) => eprintln!("[soul][agent] vio subscribe failed: {}", redact(&e)),
            }
        }
        if let Some(c) = &caps {
            eprintln!(
                "[soul][agent] vio hello: api={} vio={} commands={} events={}",
                c.api_version,
                c.vio_version,
                c.commands.len(),
                if live { events.len() } else { 0 }
            );
        }
        let can_state = caps.as_ref().is_some_and(|c| c.has("setSoulState"));
        let mut gate = StateGate::default();
        gate.reset();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Req>();
        {
            let mut sh = self.shared.lock();
            sh.tx = Some(tx);
            sh.pid = d.pid;
            sh.caps = caps;
            sh.ctx = BrowserCtx { live, ..Default::default() };
        }
        for v in early {
            self.event(v["event"].as_str().unwrap_or_default(), &v["data"]);
        }
        self.set_status(State::Connected, None);
        eprintln!("[soul][agent] vio authorized");
        let mut pending: HashMap<u64, mpsc::Sender<Result<Value, String>>> = HashMap::new();
        let mut last_send = Instant::now() - MIN_GAP;
        let mut check = tokio::time::interval(POLL);
        let mut state_tick = tokio::time::interval(STATE_TICK);
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
                        if v["type"] == json!("event") {
                            self.event(v["event"].as_str().unwrap_or_default(), &v["data"]);
                        } else if let Some(reply) = v["id"].as_u64().and_then(|id| pending.remove(&id)) {
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
                _ = state_tick.tick(), if can_state => {
                    let want = self.shared.lock().want.clone();
                    if gate.take(&want, Instant::now()) {
                        next_id += 1;
                        // Fire and forget: its reply finds no pending entry.
                        let msg = json!({ "id": next_id, "cmd": "setSoulState", "args": { "state": want.0, "text": want.1 } });
                        last_send = Instant::now();
                        if sink.send(Message::Text(msg.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                }
                _ = check.tick() => {
                    if !self.enabled.load(Ordering::SeqCst) || discover(&self.path).as_ref() != Some(d) {
                        break;
                    }
                }
            }
        }
        // Dropping `pending` fails the waiting calls ("connection closed"). Vio resets its Soul
        // icon to idle when the last client disconnects.
        {
            let mut sh = self.shared.lock();
            sh.tx = None;
            sh.caps = None;
            sh.ctx = BrowserCtx::default();
        }
        eprintln!("[soul][agent] vio disconnected");
        End::Closed
    }

    fn event(&self, name: &str, data: &Value) {
        let signal = self.shared.lock().ctx.apply(name, data);
        crate::soul_debug!("[soul][agent] vio event {name}");
        if let Some(s) = signal {
            eprintln!("[soul][agent] vio signal {s:?}");
            let f = self.on_signal.lock().clone();
            if let Some(f) = f {
                // Off the actor: the handler calls back into Vio.
                std::thread::spawn(move || f(s));
            }
        }
    }

    /// Send `idle` now and wait (≤ 400 ms) until it went out: Soul is quitting.
    pub fn shutdown(&self) {
        self.set_soul_state("idle", "");
        if self.has("setSoulState") {
            std::thread::sleep(STATE_TICK * 2 + Duration::from_millis(200));
        }
    }
}

/// A handshake request before the main loop (`hello`, `subscribe`): its reply within 5 s.
/// Events that arrive meanwhile (`windowFocus` right after `subscribe`) are kept in `early`.
async fn handshake_call<S, R>(sink: &mut S, stream: &mut R, id: u64, msg: String, early: &mut Vec<Value>) -> Result<Value, String>
where
    S: futures_util::Sink<Message> + Unpin,
    R: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    sink.send(Message::Text(msg.into())).await.map_err(|_| "connection lost".to_string())?;
    let wait = async {
        while let Some(m) = stream.next().await {
            match m {
                Ok(Message::Text(t)) => {
                    let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else { continue };
                    if v["type"] == json!("event") {
                        early.push(v);
                    } else if v["id"].as_u64() == Some(id) {
                        return if v["ok"] == json!(true) {
                            Ok(v["result"].clone())
                        } else {
                            Err(v["error"].as_str().unwrap_or("error").to_string())
                        };
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
        Err("connection closed".to_string())
    };
    tokio::time::timeout(HELLO_TIMEOUT, wait).await.unwrap_or_else(|_| Err("no answer".into()))
}

static GLOBAL: OnceLock<Arc<Client>> = OnceLock::new();

/// App startup: watch for Vio (connects only while the computer agent is enabled).
pub fn init(app: &tauri::AppHandle, enabled: bool) {
    let Some(path) = discovery_path() else { return };
    let handle = app.clone();
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
    client.on_signal(Arc::new(move |s| crate::agent::browser::on_signal(&handle, s)));
    let _ = GLOBAL.set(client);
}

pub fn global() -> Option<Arc<Client>> {
    GLOBAL.get().cloned()
}

/// The client while Vio is connected and authorized.
pub fn connected_client() -> Option<Arc<Client>> {
    global().filter(|c| c.status().state == State::Connected)
}

/// Soul's state for the icon in Vio (idle / listening / thinking / speaking / acting); debounced,
/// a no-op without Vio.
pub fn soul_state(state: &str, text: &str) {
    if let Some(c) = GLOBAL.get() {
        c.set_soul_state(state, text);
    }
}

/// Soul is quitting: the Vio icon goes back to idle.
pub fn shutdown() {
    if let Some(c) = connected_client() {
        c.shutdown();
    }
}

/// The page text for reading / summarizing: Vio's article extraction when it has it (main text
/// without menus), else the whole page.
pub fn read_page(c: &Client, tab: &str) -> Result<String, String> {
    if c.has("getReadable") {
        match c.call("getReadable", json!({ "tabId": tab })) {
            Ok(r) if !text(&r, "text").is_empty() => {
                let byline = text(&r, "byline");
                let head = if byline.is_empty() { text(&r, "title") } else { format!("{} — {byline}", text(&r, "title")) };
                return Ok(format!("{head}\n\n{}", text(&r, "text")));
            }
            Err(e) if is_forbidden(&e) || e == crate::platform::input::ABORTED => return Err(e),
            _ => {}
        }
    }
    Ok(page_text(&c.call("getPage", json!({ "tabId": tab }))?))
}

/// Purple frame before a click (`highlight`), a short pause so the user sees it, the click,
/// then `clearHighlight`. Without `highlight` (old Vio) just the click.
pub fn click(c: &Client, tab: &str, r: &str, label: &str) -> Result<(), String> {
    let lit = c.has("highlight") && c.call("highlight", json!({ "tabId": tab, "ref": r, "label": clip(label, 80) })).is_ok();
    if lit {
        super::killswitch::sleep(HIGHLIGHT_PAUSE);
    }
    let r = c.call("click", json!({ "tabId": tab, "ref": r })).map(|_| ());
    if lit {
        let _ = c.call("clearHighlight", json!({ "tabId": tab }));
    }
    r
}

const HIGHLIGHT_PAUSE: Duration = Duration::from_millis(350);

/// `mediaControl` (`tab` `None` = the playing tab, else the active one). „no media element” gets
/// an explanation: video in a cross-origin embed (an iframe from another site) is out of reach.
pub fn media(c: &Client, tab: Option<&str>, action: &str, value: Option<f64>) -> Result<Value, String> {
    let mut args = json!({ "action": action });
    if let Some(t) = tab {
        args["tabId"] = json!(t);
    }
    if let Some(v) = value {
        args["value"] = json!(v);
    }
    c.call("mediaControl", args).map_err(|e| {
        if e.contains("no media") {
            "No video or audio Soul can control on this page (players embedded from another site, e.g. a YouTube embed, can't be controlled)."
                .to_string()
        } else {
            e
        }
    })
}

/// `mediaControl`'s result in a few words.
pub fn media_text(v: &Value) -> String {
    let t = |s: f64| format!("{}:{:02}", (s.max(0.0) as u64) / 60, (s.max(0.0) as u64) % 60);
    let at = v["currentTime"].as_f64().unwrap_or(0.0);
    let dur = v["duration"].as_f64().unwrap_or(0.0);
    format!(
        "{} {}/{}{}",
        if v["playing"] == json!(true) { "playing" } else { "paused" },
        t(at),
        t(dur),
        if v["muted"] == json!(true) { ", muted" } else { "" }
    )
}

/// `browser_tab` / voice tab commands on `tab`.
pub fn tab_op(c: &Client, tab: &str, op: &str) -> Result<(), String> {
    let (cmd, args) = match op {
        "mute" | "unmute" => ("muteTab", json!({ "tabId": tab, "muted": op == "mute" })),
        "pin" | "unpin" => ("pinTab", json!({ "tabId": tab, "pinned": op == "pin" })),
        "back" => ("goBack", json!({ "tabId": tab })),
        "forward" => ("goForward", json!({ "tabId": tab })),
        "reopen" => ("reopenClosedTab", json!({})),
        other => return Err(format!("Unknown tab operation {other}.")),
    };
    if !c.has(cmd) {
        return Err("This Vio version can't do that (update Vio).".into());
    }
    c.call(cmd, args).map(|_| ())
}

/// History / bookmarks search: (entries, text for the model / Island).
pub fn search(c: &Client, bookmarks: bool, query: &str) -> Result<(Vec<Value>, String), String> {
    let cmd = if bookmarks { "searchBookmarks" } else { "searchHistory" };
    if !c.has(cmd) {
        return Err("This Vio version can't search history or bookmarks (update Vio).".into());
    }
    let list = c.call(cmd, json!({ "query": clip(query, 200), "limit": 10 }))?.as_array().cloned().unwrap_or_default();
    let text = list
        .iter()
        .map(|e| format!("\"{}\" {}", crate::platform::ui::sanitize(&clip(&text(e, "title"), 80)), clip(&text(e, "url"), 120)))
        .collect::<Vec<_>>()
        .join("\n");
    Ok((list, if text.is_empty() { "(nothing found)".into() } else { text }))
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
    // `visible` is only false for elements outside the viewport (`getElements {all:true}`).
    e.offscreen = v["visible"].as_bool() == Some(false);
    e.options = v["options"]
        .as_array()
        .map(|a| a.iter().map(|o| (text(o, "value"), text(o, "label"))).collect())
        .unwrap_or_default();
    e.kind = match (tag.as_str(), ty.as_str()) {
        ("select", _) => "select".into(),
        ("input", t @ ("time" | "date" | "datetime-local" | "month" | "week" | "color" | "range" | "file" | "number")) => t.into(),
        _ => String::new(),
    };
    Some(e)
}

/// How a form value is carried out for an element (Vio's `fill` types into text-like fields,
/// `selectOption` picks `<select>` options, radios / checkboxes are clicked).
#[derive(Debug, PartialEq)]
pub enum FillPlan {
    /// `fill` this text (also `time` / `date` / `datetime-local` / `number` / `range`).
    Text(String),
    /// `selectOption` with this option label or value.
    Select(String),
    /// Radio / checkbox: one click.
    Click,
    /// Checkbox already in the wanted state.
    AlreadySet,
    /// Cannot be done through the API: a note for the model (the task goes on).
    Skip(String),
}

fn wants_checked(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    !matches!(t.as_str(), "nie" | "no" | "false" | "off" | "0" | "odznacz" | "wyłącz" | "wylacz" | "uncheck")
}

pub fn fill_plan(e: &UiElement, v: &FillValue) -> FillPlan {
    let text = match v {
        FillValue::Text(t) | FillValue::Option(t) => Some(t.as_str()),
        FillValue::Checked(_) => None,
    };
    match (e.role, e.kind.as_str()) {
        (Role::Radio | Role::CheckBox, _) => {
            let want = match v {
                FillValue::Checked(c) => *c,
                FillValue::Text(t) | FillValue::Option(t) => wants_checked(t),
            };
            match (e.checked == Some(want), e.role, want) {
                (true, ..) => FillPlan::AlreadySet,
                (_, Role::Radio, false) => FillPlan::Skip("a radio button cannot be unchecked: choose another option of the group.".into()),
                _ => FillPlan::Click,
            }
        }
        (_, "select") => match text {
            Some(t) => FillPlan::Select(t.to_string()),
            None => FillPlan::Skip("this is a <select>: give the option (text or option), not checked.".into()),
        },
        (_, "month" | "week" | "color" | "file") => FillPlan::Skip(format!(
            "Vio cannot type into <input type={}> fields. Skip this field and say so in done, or ask_user for the value.",
            e.kind
        )),
        _ => match text {
            Some(t) => FillPlan::Text(t.to_string()),
            None => FillPlan::Skip("this is not a checkbox or radio button: give text.".into()),
        },
    }
}

/// `selectOption` arguments (`label` or `value`) for what the model asked for: the exact label,
/// else the exact value, else the only label containing it (spelling folded); else the text as label.
pub fn select_args(e: &UiElement, want: &str) -> Value {
    let fold = |s: &str| crate::agent::router::fold_word(s);
    let w = fold(want);
    let label = |l: &str| json!({ "label": l });
    if let Some((_, l)) = e.options.iter().find(|(_, l)| fold(l) == w) {
        return label(l);
    }
    if let Some((v, _)) = e.options.iter().find(|(v, _)| !v.is_empty() && fold(v) == w) {
        return json!({ "value": v });
    }
    let part: Vec<&(String, String)> = e.options.iter().filter(|(_, l)| !w.is_empty() && fold(l).contains(&w)).collect();
    match part.as_slice() {
        [(_, l)] => label(l),
        _ => label(want.trim()),
    }
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

/// A form field (not a button or a link): the page is worth listing completely.
fn has_form_fields(elements: &Value) -> bool {
    elements.as_array().is_some_and(|a| {
        a.iter().any(|v| {
            matches!(
                element(v).map(|e| e.role),
                Some(Role::Edit | Role::ComboBox | Role::CheckBox | Role::Radio)
            )
        })
    })
}

/// `getElements` of a tab. A page with form fields is listed whole (`all: true`, elements below
/// the fold carry `visible: false`) so a long form is filled in one or two calls instead of a
/// scroll per screen; any other page stays viewport-only (short prompts).
pub fn elements_of(c: &Client, tab: &str) -> Result<Value, String> {
    let viewport = elements_of_viewport(c, tab)?;
    if has_form_fields(&viewport) {
        if let Ok(all) = c.call("getElements", json!({ "tabId": tab, "all": true })) {
            return Ok(all);
        }
    }
    Ok(viewport)
}

/// A sleeping tab („tab not loaded (activateTab first)”) is activated and polled until its page
/// is up (≤ 3 s).
fn elements_of_viewport(c: &Client, tab: &str) -> Result<Value, String> {
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

/// A page for the user: a Vio tab while Vio is connected, else the system browser.
pub fn browse(platform: &dyn crate::platform::Platform, url: &str) -> Result<String, String> {
    browse_with(global().as_deref(), platform, url)
}

pub(super) fn browse_with(c: Option<&Client>, platform: &dyn crate::platform::Platform, url: &str) -> Result<String, String> {
    match c.filter(|c| c.status().state == State::Connected) {
        Some(c) => open_tab(c, platform, url),
        None => platform.open_url(url),
    }
}

pub fn web_search(platform: &dyn crate::platform::Platform, query: &str) -> Result<String, String> {
    browse(platform, &crate::platform::apps::search_url(query, Some("google"))?)
}

pub fn youtube_search(platform: &dyn crate::platform::Platform, query: &str) -> Result<String, String> {
    browse(platform, &crate::platform::apps::youtube_search_url(query)?)
}

/// First YouTube result (best-effort scrape, falls back to the search page).
pub fn youtube_play(platform: &dyn crate::platform::Platform, query: &str) -> Result<String, String> {
    browse(platform, &crate::platform::apps::youtube_first_result_url(query)?)
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

/// The active tab of Vio while Vio is connected and has the keyboard focus (where the user's
/// cursor is). `None` = the text is not going into Vio.
fn focused_tab(platform: &dyn crate::platform::Platform) -> Option<(Arc<Client>, String)> {
    let c = global().filter(|c| c.status().state == State::Connected)?;
    let w = platform.foreground_window()?;
    if !((w.pid != 0 && w.pid == c.pid()) || is_vio_process(&w.process)) {
        return None;
    }
    // The focused field from events names the tab: no listTabs round trip.
    if let TextRoute::Insert(tab) = c.context().text_route() {
        return Some((c, tab));
    }
    let (_, id) = active_tab(&c.call("listTabs", json!({})).ok()?)?;
    Some((c, id))
}

/// Vio's `insertText` limit.
const INSERT_MAX_CHARS: usize = 10_000;

/// Dictation / text-action output into the focused Vio field — no clipboard involved. `None` =
/// not applicable (Vio not connected or not in front, field not editable, too long): the caller
/// pastes through the clipboard. A password field is never worked around.
pub fn insert_text(platform: &dyn crate::platform::Platform, text: &str) -> Option<Result<(), String>> {
    if text.chars().count() > INSERT_MAX_CHARS {
        return None;
    }
    let (c, tab) = focused_tab(platform)?;
    if c.context().text_route() == TextRoute::Forbidden {
        return Some(Err(forbidden_message(Lang::En, "forbidden: password field")));
    }
    match c.call("insertText", json!({ "tabId": tab, "text": text })) {
        Ok(_) => {
            eprintln!("[soul][agent] text inserted via vio chars={}", text.chars().count());
            crate::e2e::trace("vio_text", json!({ "op": "insertText", "chars": text.chars().count() }));
            Some(Ok(()))
        }
        Err(e) if is_forbidden(&e) => Some(Err(forbidden_message(Lang::En, &e))),
        Err(e) => {
            eprintln!("[soul][agent] vio insertText failed: {e} — clipboard");
            None
        }
    }
}

/// The selection of the focused Vio field / page (`getSelection`). `None` = not applicable, the
/// caller reads it through the clipboard. `Some(Ok(None))` = nothing selected.
pub fn selection(platform: &dyn crate::platform::Platform) -> Option<Result<Option<String>, String>> {
    let (c, tab) = focused_tab(platform)?;
    match c.call("getSelection", json!({ "tabId": tab })) {
        Ok(v) => {
            let t = text(&v, "text");
            crate::e2e::trace("vio_text", json!({ "op": "getSelection", "chars": t.chars().count() }));
            Some(Ok(Some(t).filter(|t| !t.is_empty())))
        }
        Err(e) if is_forbidden(&e) => Some(Err(forbidden_message(Lang::En, &e))),
        Err(e) => {
            eprintln!("[soul][agent] vio getSelection failed: {e} — clipboard");
            None
        }
    }
}

/// Type text into the focused field: Vio's `insertText` when the field is in Vio, else the clipboard paste.
pub fn paste_text(platform: &dyn crate::platform::Platform, text: &str) -> Result<(), String> {
    insert_text(platform, text).unwrap_or_else(|| platform.paste_text(text))
}

/// The current selection: Vio's `getSelection` when the focus is in Vio, else Ctrl+C.
pub fn read_selection(platform: &dyn crate::platform::Platform) -> Result<Option<String>, String> {
    selection(platform).unwrap_or_else(|| platform.read_selection())
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

    #[test]
    fn form_values_get_a_plan_per_field_type() {
        let els = json!([
            { "ref": "a", "role": "combobox", "tag": "select", "type": "", "label": "Sos", "value": "— wybierz —",
              "options": [{ "value": "", "label": "— wybierz —", "selected": true }, { "value": "pomidorowy", "label": "Pomidorowy" },
                          { "value": "ostry", "label": "Ostry" }, { "value": "czosnkowy", "label": "Czosnkowy" }], "visible": true },
            { "ref": "b", "role": "textbox", "tag": "input", "type": "time", "label": "Czas dostawy", "empty": true },
            { "ref": "c", "role": "radio", "tag": "input", "type": "radio", "label": "Mały", "checked": false },
            { "ref": "d", "role": "checkbox", "tag": "input", "type": "checkbox", "label": "Ser", "checked": true },
            { "ref": "e", "role": "textbox", "tag": "input", "type": "text", "label": "Imię", "visible": false },
            { "ref": "f", "role": "textbox", "tag": "input", "type": "color", "label": "Kolor" },
            { "ref": "g", "role": "radio", "tag": "input", "type": "radio", "label": "Duży", "checked": true }
        ]);
        let s = snapshot(&json!({ "title": "t", "url": "https://x.pl" }), &els, None);
        let text = |t: &str| FillValue::Text(t.into());
        let plan = |i: u32, v: &FillValue| fill_plan(s.get(i).unwrap(), v);
        assert_eq!(plan(1, &text("ostry")), FillPlan::Select("ostry".into()));
        assert_eq!(plan(1, &FillValue::Option("Ostry".into())), FillPlan::Select("Ostry".into()));
        assert!(matches!(plan(1, &FillValue::Checked(true)), FillPlan::Skip(m) if m.contains("<select>")));
        assert_eq!(plan(2, &text("18:30")), FillPlan::Text("18:30".into()), "time is filled natively now");
        assert_eq!(plan(3, &text("tak")), FillPlan::Click);
        assert_eq!(plan(3, &FillValue::Checked(true)), FillPlan::Click);
        assert_eq!(plan(3, &FillValue::Checked(false)), FillPlan::AlreadySet);
        assert_eq!(plan(4, &text("tak")), FillPlan::AlreadySet);
        assert_eq!(plan(4, &text("nie")), FillPlan::Click);
        assert_eq!(plan(4, &FillValue::Checked(false)), FillPlan::Click);
        assert_eq!(plan(5, &text("Oskar")), FillPlan::Text("Oskar".into()));
        assert!(matches!(plan(5, &FillValue::Checked(true)), FillPlan::Skip(_)));
        assert!(matches!(plan(6, &text("#fff")), FillPlan::Skip(m) if m.contains("type=color")));
        assert!(matches!(plan(7, &FillValue::Checked(false)), FillPlan::Skip(m) if m.contains("radio")));
        // Observation: select options, current value, rows below the fold.
        let obs = s.observation();
        assert!(obs.contains("type=select options=\"— wybierz —|Pomidorowy|Ostry|Czosnkowy\""), "{obs}");
        assert!(obs.lines().nth(4).unwrap().ends_with(" offscreen"), "{obs}");
        assert!(!obs.lines().next().unwrap().contains("offscreen"), "{obs}");
        assert!(obs.contains("type=time"), "{obs}");
    }

    #[test]
    fn select_args_resolve_label_value_and_fragments() {
        let els = json!([{ "ref": "a", "role": "combobox", "tag": "select", "label": "Sos",
            "options": [{ "value": "", "label": "— wybierz —" }, { "value": "pomidorowy", "label": "Pomidorowy" },
                        { "value": "ostry", "label": "Ostry" }, { "value": "cz", "label": "Sos czosnkowy łagodny" }] }]);
        let s = snapshot(&json!({ "title": "t", "url": "https://x.pl" }), &els, None);
        let e = s.get(1).unwrap();
        assert_eq!(select_args(e, "ostry"), json!({ "label": "Ostry" }));
        assert_eq!(select_args(e, " POMIDOROWY "), json!({ "label": "Pomidorowy" }));
        assert_eq!(select_args(e, "cz"), json!({ "value": "cz" }));
        assert_eq!(select_args(e, "czosnkowy"), json!({ "label": "Sos czosnkowy łagodny" }));
        assert_eq!(select_args(e, "o"), json!({ "label": "o" }), "ambiguous fragment: Vio decides");
        assert_eq!(select_args(e, "Nieistniejący"), json!({ "label": "Nieistniejący" }));
    }

    #[test]
    fn long_pages_with_forms_are_listed_whole() {
        let form = json!([{ "ref": "1", "role": "link", "tag": "a", "label": "x" }, { "ref": "2", "role": "textbox", "tag": "input", "type": "text", "label": "Imię" }]);
        assert!(has_form_fields(&form));
        assert!(!has_form_fields(&json!([{ "ref": "1", "role": "link", "tag": "a", "label": "x" }, { "ref": "2", "role": "button", "tag": "button", "label": "Szukaj" }])));
        assert!(!has_form_fields(&json!([])));
    }

    #[test]
    fn search_urls_are_http_for_vio() {
        for u in [
            crate::platform::apps::search_url("pogoda łódź", Some("google")).unwrap(),
            crate::platform::apps::youtube_search_url("queen").unwrap(),
        ] {
            assert_eq!(normalize_url(&u).unwrap(), u);
        }
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

    /// Real running Vio, API v2: `cargo test --lib vio_v2_live_check -- --ignored --nocapture`.
    /// Read-only on the user's pages (no click / fill / key): hello, events, article, find,
    /// highlight, history, screenshot, the Soul icon states. Accept „Zezwól” in Vio if asked.
    #[test]
    #[ignore]
    fn vio_v2_live_check() {
        let path = discovery_path().expect("no config dir");
        assert!(discover(&path).is_some(), "Vio API is off or Vio is not running ({})", path.display());
        let c = Client::start(path, true, Box::new(|s| eprintln!("[live] status {:?}", s.state)));
        let end = Instant::now() + AUTH_TIMEOUT + Duration::from_secs(5);
        while c.status().state != State::Connected && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(100));
        }
        c.wait_ready(Duration::from_secs(1)).expect("not connected");
        let mut rows: Vec<(String, bool, String)> = Vec::new();
        let mut row = |name: &str, r: Result<String, String>| {
            let (ok, detail) = match r {
                Ok(d) => (true, d),
                Err(e) => (false, e),
            };
            rows.push((name.to_string(), ok, clip(&detail, 90)));
        };
        let caps = c.caps();
        row("hello", caps.as_ref().map(|c| format!("api {} vio {} · {} commands · {} events", c.api_version, c.vio_version, c.commands.len(), c.events.len())).ok_or_else(|| "no capabilities (v1?)".into()));
        std::thread::sleep(Duration::from_millis(800));
        let ctx = c.context();
        row("events subscribed", if ctx.live { Ok(format!("windowFocus={:?}", ctx.focused)) } else { Err("not subscribed".into()) });
        // Only internal pages open (they are hidden): a test tab, closed again at the end.
        let mut opened = None;
        if active_tab(&c.call("listTabs", json!({})).unwrap_or_default()).is_none() {
            opened = c.call("openTab", json!({ "url": "https://example.com" })).ok().and_then(|v| tab_key(&v["tabId"]));
            std::thread::sleep(Duration::from_secs(3));
        }
        let tabs = c.call("listTabs", json!({})).unwrap_or_default();
        let Some((_, tab)) = active_tab(&tabs) else {
            panic!("Vio has no normal tab open: listTabs={tabs}");
        };
        row("getReadable", read_page(&c, &tab).map(|t| format!("{} chars: {}", t.chars().count(), t.lines().next().unwrap_or(""))));
        row("findInPage", c.call("findInPage", json!({ "tabId": tab, "text": "a" })).map(|v| format!("matches {}", v["matches"])));
        row("stopFind", c.call("stopFind", json!({ "tabId": tab })).map(|_| "ok".into()));
        let els = c.call("getElements", json!({ "tabId": tab })).unwrap_or_default();
        let first = els.as_array().and_then(|a| a.iter().find_map(|e| tab_key(&e["ref"])));
        match first {
            Some(r) => {
                row("highlight", c.call("highlight", json!({ "tabId": tab, "ref": r, "label": "Soul test" })).map(|_| "purple frame shown".into()));
                std::thread::sleep(Duration::from_millis(700));
                row("clearHighlight", c.call("clearHighlight", json!({ "tabId": tab })).map(|_| "ok".into()));
            }
            None => row("highlight", Err("no element on the page".into())),
        }
        row("searchHistory", search(&c, false, "a").map(|(l, _)| format!("{} hits", l.len())));
        row("searchBookmarks", search(&c, true, "a").map(|(l, _)| format!("{} hits", l.len())));
        row("screenshot", c.call("screenshot", json!({ "tabId": tab })).map(|v| format!("{}x{}", v["width"], v["height"])));
        row("mediaControl pause", media(&c, Some(&tab), "pause", None).map(|v| media_text(&v)));
        for s in ["listening", "thinking", "speaking", "acting", "idle"] {
            c.set_soul_state(s, &format!("Soul test: {s}"));
            std::thread::sleep(Duration::from_millis(600));
        }
        row("setSoulState", Ok("listening → thinking → speaking → acting → idle (watch the icon)".into()));
        let ctx = c.context();
        row("context", Ok(ctx.observation().unwrap_or_else(|| "(no events yet)".into())));
        row("tabActivated / pageLoaded", ctx.tab.clone().map(|t| format!("active tab {t} \"{}\"", clip(&ctx.title, 40))).ok_or_else(|| "no tab event seen".into()));
        if let Some(t) = opened {
            row("closeTab", c.call("closeTab", json!({ "tabId": t })).map(|_| "test tab closed".into()));
            std::thread::sleep(Duration::from_millis(500));
            let back = c.call("reopenClosedTab", json!({}));
            row("reopenClosedTab", back.as_ref().map(|v| format!("reopened {}", v["tabId"])).map_err(|e| e.clone()));
            if let Some(again) = back.ok().and_then(|v| tab_key(&v["tabId"])) {
                std::thread::sleep(Duration::from_millis(800));
                let _ = c.call("closeTab", json!({ "tabId": again }));
            }
        }
        println!("\n| check | ok | detail |\n|---|---|---|");
        for (n, ok, d) in &rows {
            println!("| {n} | {} | {d} |", if *ok { "PASS" } else { "FAIL" });
        }
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
