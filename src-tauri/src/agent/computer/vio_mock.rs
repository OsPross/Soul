//! Mock Vio automation API (`tests/agent/vio-mock.json`) and the Vio cases of the agent harness:
//! auth failure, forbidden tab, expired ref, empty fields, Polish fill, page read, confirmation,
//! password refusal — and API v2 (`"v2": true`): `hello` / `subscribe`, pushed events, STOP,
//! `setSoulState`, highlight, article, screenshot, find, media, tabs, history. The very same
//! `run_task` loop runs against a fake desktop (Vio in front, no UI Automation), so nothing on
//! the real screen is touched.
//!
//! `cargo test vio_mock_cases` replays each case's scripted model calls (no key needed);
//! `npm run test:agent` runs them with the real model and adds them to `tests/agent/report.md`.

use super::harness::Row;
use super::vio::{Client, State};
use super::{run_task, Host, ModelFn, Outcome, Progress};
use crate::platform::{apps, coords, hook, input, ui, Platform, PowerState};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::{accept, Error as WsError, Message};

fn scenario() -> Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/agent/vio-mock.json");
    serde_json::from_str(&std::fs::read_to_string(p).expect("vio-mock.json")).expect("vio-mock.json is JSON")
}

/// Page state the mock serves and the cases check.
#[derive(Default)]
struct Page {
    tabs: Vec<Value>,
    /// Bumped by navigation / `expireOnce`: refs from older generations fail.
    gen: u64,
    clicked: Vec<String>,
    /// API v2 (`hello`, events, the new commands); v1 answers „unknown command”.
    v2: bool,
    /// Every command received (name, args), auth excluded.
    log: Vec<(String, Value)>,
    /// Events to push to the client.
    outbox: Vec<Value>,
    /// After STOP: commands fail with „stopped by user” until then.
    stopped_until: Option<Instant>,
    /// Close the connection (Vio restart).
    kick: bool,
    closed: Vec<Value>,
}

const V2_COMMANDS: &[&str] = &[
    "hello", "capabilities", "subscribe", "unsubscribe", "listTabs", "openTab", "closeTab", "activateTab", "navigate",
    "getPage", "getElements", "click", "fill", "selectOption", "insertText", "getSelection", "pressKey", "scroll",
    "getReadable", "screenshot", "findInPage", "stopFind", "scrollTo", "mediaControl", "muteTab", "pinTab",
    "reopenClosedTab", "goBack", "goForward", "searchHistory", "searchBookmarks", "highlight", "clearHighlight",
    "setSoulState", "panel",
];
/// Index of the first command v1 doesn't have (`getReadable`).
const V2_FIRST_NEW: usize = 18;
const V2_EVENTS: &[&str] = &[
    "tabActivated", "pageLoaded", "tabClosed", "windowFocus", "selectionChanged", "focusedField", "mediaState",
    "soulIconClicked", "panelAction", "stopRequested",
];
/// Commands Vio still takes during the 3 s after STOP.
const META_COMMANDS: &[&str] = &["hello", "capabilities", "subscribe", "unsubscribe", "setSoulState", "panel"];

/// A 2×2 PNG for `screenshot`.
fn tiny_png() -> String {
    use base64::Engine;
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 2, image::Rgba([120, 80, 200, 255])).write_to(&mut out, image::ImageFormat::Png).unwrap();
    base64::engine::general_purpose::STANDARD.encode(out.into_inner())
}

/// API v2 commands (`None` = not a v2 command, fall through to v1).
fn handle_v2(p: &mut Page, cmd: &str, a: &Value) -> Option<Result<Value, String>> {
    if !V2_COMMANDS[..4].contains(&cmd) && !V2_COMMANDS[V2_FIRST_NEW..].contains(&cmd) {
        return None;
    }
    if !p.v2 {
        return Some(Err("unknown command".into()));
    }
    let active = p.tabs.iter().position(|t| t["active"] == json!(true)).unwrap_or(0);
    let tab_idx = |p: &Page| a["tabId"].as_str().and_then(|id| p.tabs.iter().position(|t| t["tabId"] == json!(id)));
    let text_of = |p: &Page, i: usize| p.tabs[i]["text"].as_str().unwrap_or("").to_string();
    Some(match cmd {
        "hello" | "capabilities" => Ok(json!({ "apiVersion": 2, "vioVersion": "mock", "capabilities": V2_COMMANDS, "events": V2_EVENTS })),
        "subscribe" | "unsubscribe" => Ok(json!({ "events": a["events"] })),
        "setSoulState" | "panel" | "highlight" | "clearHighlight" | "stopFind" => Ok(json!({})),
        "getReadable" => tab_idx(p).map(|i| Ok(json!({ "title": p.tabs[i]["title"], "byline": "Redakcja", "text": text_of(p, i), "lang": "pl" }))).unwrap_or(Err("forbidden".into())),
        "screenshot" => Ok(json!({ "pngBase64": tiny_png(), "width": 2, "height": 2 })),
        "findInPage" => tab_idx(p)
            .map(|i| {
                let n = text_of(p, i).to_lowercase().matches(&a["text"].as_str().unwrap_or("").to_lowercase()).count();
                Ok(json!({ "matches": n, "active": if n > 0 { 1 } else { 0 } }))
            })
            .unwrap_or(Err("forbidden".into())),
        "scrollTo" => match (tab_idx(p), a["text"].as_str()) {
            (Some(i), Some(t)) if text_of(p, i).to_lowercase().contains(&t.to_lowercase()) => Ok(json!({})),
            (Some(_), Some(_)) => Err("text not found".into()),
            (Some(_), None) => Ok(json!({})),
            (None, _) => Err("forbidden".into()),
        },
        "mediaControl" => {
            let i = tab_idx(p).unwrap_or(active);
            let m = &mut p.tabs[i]["media"];
            if m.is_null() {
                return Some(Err("no media element".into()));
            }
            let v = a["value"].as_f64().unwrap_or(0.0);
            match a["action"].as_str().unwrap_or("") {
                "play" => m["playing"] = json!(true),
                "pause" => m["playing"] = json!(false),
                "seekBy" => m["currentTime"] = json!((m["currentTime"].as_f64().unwrap_or(0.0) + v).max(0.0)),
                "rate" => m["rate"] = json!(v),
                "volume" => m["volume"] = json!(v),
                _ => {}
            }
            Ok(m.clone())
        }
        "muteTab" | "pinTab" => {
            let Some(i) = tab_idx(p) else { return Some(Err("forbidden".into())) };
            let (k, v) = if cmd == "muteTab" { ("muted", a["muted"].clone()) } else { ("pinned", a["pinned"].clone()) };
            p.tabs[i][k] = v;
            Ok(json!({}))
        }
        "goBack" | "goForward" => {
            let Some(i) = tab_idx(p) else { return Some(Err("forbidden".into())) };
            let key = if cmd == "goBack" { "back" } else { "forward" };
            match p.tabs[i][key].as_str().map(str::to_string) {
                Some(url) => {
                    p.tabs[i]["url"] = json!(url);
                    p.gen += 1;
                    Ok(json!({}))
                }
                None => Err(format!("cannot go {key}")),
            }
        }
        "reopenClosedTab" => match p.closed.pop() {
            Some(t) => {
                let id = t["tabId"].clone();
                p.tabs.push(t);
                Ok(json!({ "tabId": id }))
            }
            None => Err("nothing to reopen".into()),
        },
        "searchHistory" | "searchBookmarks" => {
            let q = a["query"].as_str().unwrap_or("").to_lowercase();
            let all = if cmd == "searchHistory" {
                json!([{ "url": "https://kuchnia.pl/bigos", "title": "Bigos staropolski — przepis", "lastVisit": 1 },
                       { "url": "https://example.com/bigos-2", "title": "Bigos z grzybami", "lastVisit": 0 }])
            } else {
                json!([{ "url": "https://bank.example.pl/", "title": "Mój Bank", "folder": "Finanse" }])
            };
            let hits: Vec<Value> = all.as_array().unwrap().iter().filter(|e| e.to_string().to_lowercase().contains(&q)).cloned().collect();
            Ok(Value::Array(hits))
        }
        other => Err(format!("unknown command {other}")),
    })
}

fn tab_mut<'a>(p: &'a mut Page, id: &Value) -> Result<&'a mut Value, String> {
    p.tabs.iter_mut().find(|t| &t["tabId"] == id).ok_or_else(|| "forbidden".to_string())
}

/// Like Vio: a discarded tab has no page until `activateTab`.
fn loaded(t: &Value) -> Result<(), String> {
    if t["asleep"] == json!(true) { Err("tab not loaded (activateTab first)".into()) } else { Ok(()) }
}

fn handle(p: &mut Page, cmd: &str, a: &Value) -> Result<Value, String> {
    p.log.push((cmd.to_string(), a.clone()));
    if p.stopped_until.is_some_and(|t| Instant::now() < t) && !META_COMMANDS.contains(&cmd) {
        return Err("stopped by user".into());
    }
    if let Some(r) = handle_v2(p, cmd, a) {
        return r;
    }
    if cmd == "getPage" {
        // A slow page (cancel / STOP tests).
        if let Some(ms) = p.tabs.iter().find(|t| t["tabId"] == a["tabId"]).and_then(|t| t["slowMs"].as_u64()) {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
    let gen = p.gen;
    let allowed = |t: &Value| if t["forbidden"] == json!(true) { Err("forbidden: private tab".to_string()) } else { Ok(()) };
    match cmd {
        "listTabs" => Ok(Value::Array(
            p.tabs.iter().map(|t| json!({ "tabId": t["tabId"], "title": t["title"], "url": t["url"], "active": t["active"] })).collect(),
        )),
        "openTab" => {
            let id = format!("tab-new-{}", p.tabs.len());
            for t in &mut p.tabs {
                t["active"] = json!(false);
            }
            p.tabs.push(json!({ "tabId": id, "title": a["url"], "url": a["url"], "active": true, "text": "", "elements": [] }));
            Ok(json!({ "tabId": id }))
        }
        "activateTab" => {
            for t in &mut p.tabs {
                t["active"] = json!(t["tabId"] == a["tabId"]);
                if t["tabId"] == a["tabId"] {
                    t["asleep"] = json!(false);
                }
            }
            Ok(Value::Null)
        }
        "navigate" => {
            let t = tab_mut(p, &a["tabId"])?;
            allowed(t)?;
            loaded(t)?;
            t["url"] = a["url"].clone();
            p.gen += 1;
            Ok(Value::Null)
        }
        "getPage" => {
            let t = tab_mut(p, &a["tabId"])?;
            allowed(t)?;
            loaded(t)?;
            Ok(json!({ "url": t["url"], "title": t["title"], "text": t["text"] }))
        }
        "getElements" => {
            let t = tab_mut(p, &a["tabId"])?;
            allowed(t)?;
            loaded(t)?;
            let all = a["all"] == json!(true);
            let els = t["elements"].as_array().cloned().unwrap_or_default();
            Ok(Value::Array(
                els.into_iter()
                    // `belowFold`: outside the viewport — only listed with `all: true`, flagged `visible: false`.
                    .filter(|e| all || e["belowFold"] != json!(true))
                    .map(|mut e| {
                        if all {
                            e["visible"] = json!(e["belowFold"] != json!(true));
                        }
                        e["ref"] = json!(format!("{}.{gen}", e["ref"].as_str().unwrap_or("")));
                        if e["type"] == json!("password") {
                            e["value"] = json!("");
                        }
                        e
                    })
                    .collect(),
            ))
        }
        "click" | "fill" | "selectOption" | "pressKey" | "scroll" => {
            let t = tab_mut(p, &a["tabId"])?;
            allowed(t)?;
            if matches!(cmd, "pressKey" | "scroll") {
                return Ok(Value::Null);
            }
            let r = a["ref"].as_str().unwrap_or("");
            let (base, g) = r.rsplit_once('.').ok_or("bad ref")?;
            if g != gen.to_string() {
                return Err("ref expired — call getElements again".into());
            }
            let els = t["elements"].as_array_mut().ok_or("no elements")?;
            let e = els.iter_mut().find(|e| e["ref"] == json!(base)).ok_or("ref expired — call getElements again")?;
            if cmd == "fill" {
                if e["type"] == json!("password") {
                    return Err("forbidden: password field".into());
                }
                if matches!(e["type"].as_str(), Some("radio" | "checkbox")) || e["tag"] == json!("select") {
                    return Err("element is not a text field".into());
                }
                let text = a["text"].as_str().unwrap_or("");
                let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
                let valid = match e["type"].as_str() {
                    Some("time") => text.split_once(':').is_some_and(|(h, m)| h.len() == 2 && m.len() == 2 && digits(h) && digits(m)),
                    Some("date") => text.split('-').map(str::len).eq([4, 2, 2]) && digits(&text.replace('-', "")),
                    _ => true,
                };
                if !valid && !text.is_empty() {
                    return Err(format!("invalid {} value", e["type"].as_str().unwrap_or("")));
                }
                e["value"] = json!(text);
                e["empty"] = json!(text.is_empty());
                return Ok(Value::Null);
            }
            if cmd == "selectOption" {
                if e["tag"] != json!("select") {
                    return Err("element is not a select".into());
                }
                let want = a["label"].as_str().or(a["value"].as_str()).unwrap_or("").trim().to_lowercase();
                let by_value = a["label"].is_null();
                let opt = e["options"].as_array().and_then(|o| {
                    o.iter().find(|o| o[if by_value { "value" } else { "label" }].as_str().is_some_and(|s| s.trim().to_lowercase() == want))
                });
                let Some(opt) = opt.cloned() else { return Err(format!("no such option: {want}")) };
                e["value"] = opt["label"].clone();
                return Ok(json!({ "value": opt["value"], "label": opt["label"] }));
            }
            if e["expireOnce"] == json!(true) {
                // The page re-rendered right before the click.
                e["expireOnce"] = json!(false);
                p.gen += 1;
                return Err("ref expired — the page changed, call getElements again".into());
            }
            if e["type"] == json!("checkbox") {
                e["checked"] = json!(e["checked"] != json!(true));
            }
            let label = e["label"].as_str().unwrap_or("").to_string();
            p.clicked.push(label);
            Ok(Value::Null)
        }
        other => Err(format!("unknown command {other}")),
    }
}

struct Mock {
    port: u16,
    page: Arc<Mutex<Page>>,
}

impl Mock {
    fn start(token: String, tabs: Vec<Value>) -> Self {
        Self::start_with(token, tabs, false)
    }

    fn start_with(token: String, tabs: Vec<Value>, v2: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let page = Arc::new(Mutex::new(Page { tabs, gen: 1, v2, ..Default::default() }));
        let pg = Arc::clone(&page);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (token, pg) = (token.clone(), Arc::clone(&pg));
                std::thread::spawn(move || {
                    let Ok(mut ws) = accept(stream) else { return };
                    let mut read = || loop {
                        match ws.read() {
                            Ok(Message::Text(t)) => return serde_json::from_str::<Value>(t.as_str()).ok(),
                            Ok(Message::Close(_)) | Err(_) => return None,
                            Ok(_) => {}
                        }
                    };
                    let Some(auth) = read() else { return };
                    let ok = auth["cmd"] == json!("auth") && auth["token"] == json!(token);
                    drop(read);
                    let reply = if ok { json!({ "ok": true }) } else { json!({ "ok": false, "error": "unauthorized" }) };
                    if ws.send(Message::Text(reply.to_string().into())).is_err() || !ok {
                        let _ = ws.close(None);
                        return;
                    }
                    // Short reads so pushed events go out between requests.
                    let _ = ws.get_mut().set_read_timeout(Some(Duration::from_millis(15)));
                    loop {
                        let (events, kick) = {
                            let mut p = pg.lock();
                            (std::mem::take(&mut p.outbox), std::mem::take(&mut p.kick))
                        };
                        if kick {
                            let _ = ws.close(None);
                            let _ = ws.flush();
                            return;
                        }
                        for ev in events {
                            if ws.send(Message::Text(ev.to_string().into())).is_err() {
                                return;
                            }
                        }
                        let msg = match ws.read() {
                            Ok(Message::Text(t)) => serde_json::from_str::<Value>(t.as_str()).unwrap_or(Value::Null),
                            Err(WsError::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                            Ok(Message::Close(_)) | Err(_) => return,
                            Ok(_) => continue,
                        };
                        let r = handle(&mut pg.lock(), msg["cmd"].as_str().unwrap_or(""), &msg["args"]);
                        let out = match r {
                            Ok(v) => json!({ "id": msg["id"], "ok": true, "result": v }),
                            Err(e) => json!({ "id": msg["id"], "ok": false, "error": e }),
                        };
                        if ws.send(Message::Text(out.to_string().into())).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self { port, page }
    }

    /// Queue an event for the connected client.
    fn push(&self, event: &str, data: Value) {
        self.page.lock().outbox.push(json!({ "type": "event", "event": event, "data": data }));
    }

    fn sent(&self, cmd: &str) -> Vec<Value> {
        self.page.lock().log.iter().filter(|(c, _)| c == cmd).map(|(_, a)| a.clone()).collect()
    }
}

/// A desktop where Vio is in front and nothing else works (no UI Automation, no input).
struct FakeDesktop;

impl Platform for FakeDesktop {
    fn type_text(&self, _: &str) -> Result<(), String> {
        Err("fake desktop".into())
    }
    fn paste_text(&self, _: &str) -> Result<(), String> {
        Err("fake desktop".into())
    }
    fn capture_screen(&self) -> Result<Vec<u8>, String> {
        Err("fake desktop".into())
    }
    fn open_app(&self, _: &str) -> Result<String, String> {
        Err("fake desktop".into())
    }
    fn search_apps(&self, _: &str, _: usize) -> Vec<apps::AppSuggestion> {
        Vec::new()
    }
    fn close_app(&self, _: &str) -> Result<String, String> {
        Err("fake desktop".into())
    }
    fn focus_app(&self, _: &str) -> Result<String, String> {
        Err("fake desktop".into())
    }
    fn open_url(&self, _: &str) -> Result<String, String> {
        Err("fake desktop".into())
    }
    fn set_volume(&self, _: u8) -> Result<String, String> {
        Err("fake desktop".into())
    }
    fn media_control(&self, _: &str) -> Result<String, String> {
        Err("fake desktop".into())
    }
    fn install_input_hook(&self, _: hook::HookSink) -> Result<(), String> {
        Err("fake desktop".into())
    }
    fn configure_input_hook(&self, _: hook::HookConfig) {}
    fn power_state(&self) -> PowerState {
        PowerState::default()
    }
    fn duck_other_audio(&self, _: Option<f32>) -> Result<(), String> {
        Ok(())
    }
    fn set_do_not_disturb(&self, _: bool) -> Result<String, String> {
        Err("fake desktop".into())
    }
    fn get_volume(&self) -> Result<u8, String> {
        Err("fake desktop".into())
    }
    fn read_selection(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn active_window(&self) -> Option<u64> {
        None
    }
    fn activate_window(&self, _: u64) -> Result<(), String> {
        Ok(())
    }
    fn send_undo(&self) -> Result<(), String> {
        Err("fake desktop".into())
    }
    fn ui_snapshot(&self, _: &ui::SnapshotOptions) -> Result<ui::UiSnapshot, String> {
        Err("no UI Automation on the fake desktop (the task left the Vio path)".into())
    }
    fn ui_element_op(&self, _: u64, _: u32, _: &ui::ElementOp) -> Result<(), String> {
        Err("fake desktop".into())
    }
    fn ui_quick_state(&self, _: u64, _: Option<u32>) -> Result<ui::QuickState, String> {
        Err("fake desktop".into())
    }
    fn foreground_window(&self) -> Option<ui::WindowInfo> {
        Some(ui::WindowInfo { title: "Vio".into(), process: "Vio.exe".into(), pid: std::process::id(), ..Default::default() })
    }
    fn send_input_step(&self, _: &input::InputStep) -> Result<(), String> {
        Err("fake desktop".into())
    }
    fn input_probe(&self) -> input::InputProbe {
        input::InputProbe { esc_down: false, cursor: None }
    }
    fn monitors(&self) -> Vec<coords::Monitor> {
        Vec::new()
    }
}

struct VioHost {
    vio: Arc<Client>,
    model: ModelFn,
    accept: bool,
    confirms: Mutex<Vec<String>>,
}

impl Host for VioHost {
    fn platform(&self) -> Arc<dyn Platform> {
        Arc::new(FakeDesktop)
    }
    fn config(&self) -> super::config::AgentConfig {
        super::config::AgentConfig { highlight: false, ..Default::default() }
    }
    fn language(&self) -> String {
        "pl".into()
    }
    fn ui_lang(&self) -> crate::i18n::Lang {
        crate::i18n::Lang::Pl
    }
    fn context(&self) -> Option<String> {
        None
    }
    fn model(&self) -> ModelFn {
        Arc::clone(&self.model)
    }
    fn progress(&self, p: &Progress) {
        eprintln!("[vio-mock]   {:?} {}/{} {}", p.phase, p.step, p.max, p.text);
    }
    fn confirm(&self, question: &str) -> bool {
        self.confirms.lock().push(question.to_string());
        self.accept
    }
    fn ask(&self, _: &str) -> Option<String> {
        None
    }
    fn highlight(&self, _: ui::Rect) {}
    fn pointer_guard(&self, _: ui::Rect) -> Option<Box<dyn FnOnce()>> {
        None
    }
    fn capture_exclusion(&self, _: bool) {}
    fn debug_dir(&self) -> Option<PathBuf> {
        None
    }
    fn vio(&self) -> Option<Arc<Client>> {
        Some(Arc::clone(&self.vio))
    }
}

/// Replays `[name, args]` calls as Gemini responses.
fn scripted(script: &Value) -> ModelFn {
    let calls: Vec<Value> = script.as_array().cloned().unwrap_or_default();
    let next = Arc::new(Mutex::new(0usize));
    Arc::new(move |_body| {
        let mut i = next.lock();
        let c = calls.get(*i).ok_or("the script has no more calls")?;
        *i += 1;
        Ok(json!({ "candidates": [{ "content": { "parts": [{ "functionCall": { "name": c[0], "args": c[1] } }] } }] }))
    })
}

fn wait_for(c: &Client, done: impl Fn(&super::vio::VioStatus) -> bool) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(8) {
        if done(&c.status()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn empty_outcome(result: Result<String, String>) -> Outcome {
    Outcome {
        summary: match &result {
            Ok(s) | Err(s) => s.clone(),
        },
        result,
        steps: 0,
        model_calls: 0,
        screenshots: 0,
        ms: 0,
        aborted: false,
        question: None,
        request_bytes: 0,
    }
}

fn check(case: &Value, page: &Page, out: &Outcome, confirms: &[String], prompts: &[String]) -> Vec<String> {
    let mut bad = Vec::new();
    let el = |label: &str| page.tabs[0]["elements"].as_array().and_then(|a| a.iter().find(|e| e["label"] == json!(label)).cloned());
    let exp = &case["expect"];
    for want in exp["clicked"].as_array().into_iter().flatten() {
        if !page.clicked.iter().any(|c| json!(c) == *want) {
            bad.push(format!("{want} was not clicked"));
        }
    }
    for no in exp["not_clicked"].as_array().into_iter().flatten() {
        if page.clicked.iter().any(|c| json!(c) == *no) {
            bad.push(format!("{no} was clicked"));
        }
    }
    for (label, want) in exp["values"].as_object().into_iter().flatten() {
        let got = el(label).map(|e| e["value"].clone()).unwrap_or(Value::Null);
        if &got != want {
            bad.push(format!("{label}: want {want}, got {got}"));
        }
    }
    for (label, want) in exp["checked"].as_object().into_iter().flatten() {
        let got = el(label).map(|e| e["checked"].clone()).unwrap_or(Value::Null);
        if &got != want {
            bad.push(format!("{label}: want checked {want}, got {got}"));
        }
    }
    for url in exp["opened"].as_array().into_iter().flatten() {
        if !page.tabs.iter().any(|t| t["url"] == *url && t["active"] == json!(true)) {
            bad.push(format!("{url} was not opened as the active Vio tab"));
        }
    }
    for p in case["expect_prompt"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if !prompts.iter().any(|t| t.contains(p)) {
            bad.push(format!("the observation lacks {p}"));
        }
    }
    for p in case["expect_prompt_lacks"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if prompts.iter().any(|t| t.contains(p)) {
            bad.push(format!("the request has {p}"));
        }
    }
    for s in case["expect_summary"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if !out.summary.to_lowercase().contains(&s.to_lowercase()) {
            bad.push(format!("the answer doesn't mention {s}"));
        }
    }
    if case["expect_confirm"] == json!(true) && confirms.is_empty() {
        bad.push("expected a confirmation card, none was shown".into());
    }
    match &case["expect_error"] {
        Value::Bool(true) if out.result.is_ok() => bad.push("expected a refusal".into()),
        Value::String(s) if !out.result.as_ref().is_err_and(|e| e.to_lowercase().contains(s)) => {
            bad.push(format!("expected an error mentioning {s}, got {:?}", out.result))
        }
        Value::Null if case["expect_confirm"] != json!(true) => {
            if let Err(e) = &out.result {
                bad.push(format!("task error: {e}"));
            }
        }
        _ => {}
    }
    bad
}

/// Every case of `vio-mock.json`. `model` = the real model (harness); `None` = each case's script.
pub(super) fn run_cases(model: Option<ModelFn>, only: Option<&str>) -> Vec<Row> {
    let sc = scenario();
    let token = sc["token"].as_str().unwrap().to_string();
    let mut rows = Vec::new();
    for case in sc["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap().to_string();
        if only.is_some_and(|o| !id.contains(o)) {
            continue;
        }
        let mut tabs = sc["tabs"].as_array().unwrap().clone();
        if let Some(active) = case["active_tab"].as_str() {
            for t in &mut tabs {
                t["active"] = json!(t["tabId"] == json!(active));
            }
        }
        let mock = Mock::start_with(token.clone(), tabs, case["v2"] == json!(true));
        let dir = std::env::temp_dir().join(format!("soul-vio-mock-{}-{id}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("automation.json");
        let use_token = case["token"].as_str().unwrap_or(&token);
        std::fs::write(&path, json!({ "port": mock.port, "token": use_token, "pid": std::process::id() }).to_string()).unwrap();
        let client = Client::start(path, true, Box::new(|_| {}));
        eprintln!("[vio-mock] ── {id}: {}", case["task"]);

        if case["expect_auth_error"] == json!(true) {
            let refused = wait_for(&client, |s| s.state == State::Unavailable && s.error.is_some());
            let status = client.status();
            let mut notes = Vec::new();
            if !refused || !status.error.as_deref().unwrap_or("").contains("unauthorized") {
                notes.push(format!("expected a refused auth, status {status:?}"));
            }
            if client.wait_ready(Duration::from_millis(100)).is_ok() || client.call("listTabs", json!({})).is_ok() {
                notes.push("calls work without a valid token".into());
            }
            let out = empty_outcome(Err(status.error.unwrap_or_default()));
            rows.push(Row { id, ok: notes.is_empty(), outcome: out, notes });
            continue;
        }
        if !wait_for(&client, |s| s.state == State::Connected) {
            rows.push(Row { id, ok: false, outcome: empty_outcome(Err("not connected".into())), notes: vec!["mock Vio never connected".into()] });
            continue;
        }
        let prompts = Arc::new(Mutex::new(Vec::<String>::new()));
        let inner = model.clone().unwrap_or_else(|| scripted(&case["script"]));
        let seen = Arc::clone(&prompts);
        let recording: ModelFn = Arc::new(move |body: Value| {
            let mut text = body["contents"][0]["parts"][0]["text"].as_str().unwrap_or("").to_string();
            if !body["contents"][0]["parts"][1]["inline_data"].is_null() {
                text.push_str("\n[IMAGE]");
            }
            let tools: Vec<&str> = body["tools"][0]["function_declarations"].as_array().into_iter().flatten().filter_map(|d| d["name"].as_str()).collect();
            text.push_str(&format!("\n[TOOLS] {}", tools.join(",")));
            seen.lock().push(text);
            inner(body)
        });
        let host = VioHost { vio: client, model: recording, accept: case["confirm"] == json!("accept"), confirms: Mutex::new(Vec::new()) };
        let out = run_task(&host, case["task"].as_str().unwrap());
        let confirms = host.confirms.lock().clone();
        let mut notes = check(case, &mock.page.lock(), &out, &confirms, &prompts.lock());
        // `expect_cmds`: these Vio commands were sent, in this order (others may sit between).
        let log: Vec<String> = mock.page.lock().log.iter().map(|(c, _)| c.clone()).collect();
        let mut at = 0;
        for want in case["expect_cmds"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            match log[at..].iter().position(|c| c == want) {
                Some(i) => at += i + 1,
                None => notes.push(format!("Vio never got {want} (in order); log: {}", log.join(","))),
            }
        }
        for no in case["expect_no_cmds"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if log.iter().any(|c| c == no) {
                notes.push(format!("Vio got {no}"));
            }
        }
        eprintln!("[vio-mock]    → {:?} | {}", out.result, if notes.is_empty() { "PASS".into() } else { notes.join("; ") });
        let mut n = notes.clone();
        if !confirms.is_empty() {
            n.push(format!("confirm shown: {}", confirms.join(" / ")));
        }
        rows.push(Row { id, ok: notes.is_empty(), outcome: out, notes: n });
        let _ = std::fs::remove_dir_all(&dir);
    }
    rows
}

/// web_search / youtube_* land in a Vio tab while connected; without Vio they use the system browser.
#[test]
fn searches_open_in_vio_when_connected() {
    let sc = scenario();
    let token = sc["token"].as_str().unwrap().to_string();
    let mock = Mock::start(token.clone(), sc["tabs"].as_array().unwrap().clone());
    let dir = std::env::temp_dir().join(format!("soul-vio-mock-{}-browse", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("automation.json");
    std::fs::write(&path, json!({ "port": mock.port, "token": token, "pid": std::process::id() }).to_string()).unwrap();
    let client = Client::start(path, true, Box::new(|_| {}));
    assert!(wait_for(&client, |s| s.state == State::Connected), "mock Vio never connected");

    let desktop = FakeDesktop;
    let urls = [
        apps::search_url("pogoda w łodzi", Some("google")).unwrap(),
        apps::youtube_search_url("queen").unwrap(),
        "https://www.youtube.com/watch?v=abc".to_string(),
    ];
    for url in &urls {
        let r = super::vio::browse_with(Some(&client), &desktop, url);
        assert!(r.is_ok(), "{url}: {r:?}");
        let page = mock.page.lock();
        let active: Vec<_> = page.tabs.iter().filter(|t| t["active"] == json!(true)).collect();
        assert!(active.len() == 1 && active[0]["url"] == json!(url), "{url} is not the active Vio tab");
    }
    // Not connected: the system browser is asked (the fake desktop refuses, which proves the path).
    let r = super::vio::browse_with(None, &desktop, &urls[0]);
    assert_eq!(r, Err("fake desktop".to_string()));
    let before = mock.page.lock().tabs.len();
    let _ = super::vio::browse_with(None, &desktop, &urls[1]);
    assert_eq!(mock.page.lock().tabs.len(), before, "no Vio tab without a connection");
    let _ = std::fs::remove_dir_all(&dir);
}

fn mock_client(v2: bool, tabs: Vec<Value>) -> (Mock, Arc<Client>, PathBuf) {
    let token = "mock-token-v2".to_string();
    let mock = Mock::start_with(token.clone(), tabs, v2);
    let dir = std::env::temp_dir().join(format!("soul-vio-mock-{}-{}-{}", std::process::id(), v2, mock.port));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("automation.json");
    std::fs::write(&path, json!({ "port": mock.port, "token": token, "pid": std::process::id() }).to_string()).unwrap();
    let client = Client::start(path, true, Box::new(|_| {}));
    assert!(wait_for(&client, |s| s.state == State::Connected), "mock Vio never connected");
    (mock, client, dir)
}

fn until(what: &str, f: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(5), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn two_tabs() -> Vec<Value> {
    vec![
        json!({ "tabId": "t1", "title": "Artykuł", "url": "https://example.com/a", "active": true, "text": "Latarnia w Mirosławicach. Latarnia stoi od 1912.", "elements": [],
                "media": { "playing": true, "muted": false, "currentTime": 10.0, "duration": 100.0, "title": "Film" }, "back": "https://example.com/start" }),
        json!({ "tabId": "t2", "title": "Wolna", "url": "https://example.com/slow", "active": false, "text": "x", "elements": [], "slowMs": 3000 }),
    ]
}

/// v2: `hello` capabilities, `subscribe`, events → context, debounced `setSoulState`, STOP.
#[test]
fn vio_v2_handshake_events_state_and_stop() {
    let (mock, c, dir) = mock_client(true, two_tabs());
    let caps = c.caps().expect("hello capabilities");
    assert_eq!(caps.vio_version, "mock");
    assert!(c.has("getReadable") && c.has("setSoulState"));
    let sub = mock.sent("subscribe");
    assert_eq!(sub.len(), 1, "one subscribe per connection");
    assert_eq!(sub[0]["events"].as_array().unwrap().len(), V2_EVENTS.len());
    assert!(c.context().live);

    // Events → browser context.
    mock.push("windowFocus", json!({ "focused": true }));
    mock.push("tabActivated", json!({ "tabId": "t1", "url": "https://example.com/a", "title": "Artykuł" }));
    mock.push("focusedField", json!({ "tabId": "t1", "ref": "r-1", "type": "email", "label": "E-mail", "editable": true }));
    mock.push("mediaState", json!({ "tabId": "t1", "playing": true, "muted": false, "currentTime": 3.0, "duration": 60.0, "title": "Film" }));
    until("events", || c.context().media.is_some() && c.context().field.is_some());
    let ctx = c.context();
    assert_eq!((ctx.tab.as_deref(), ctx.focused), (Some("t1"), Some(true)));
    assert_eq!(ctx.text_route(), crate::agent::computer::vio_ctx::TextRoute::Insert("t1".into()));

    // setSoulState: a burst collapses, the last state always arrives.
    for s in ["listening", "thinking", "speaking", "thinking", "acting"] {
        c.set_soul_state(s, "");
    }
    until("final soul state", || mock.sent("setSoulState").last().is_some_and(|a| a["state"] == "acting"));
    let n = mock.sent("setSoulState").len();
    assert!(n <= 2, "debounced: {n} setSoulState calls for a 5-state burst");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(mock.sent("setSoulState").len(), n, "an unchanged state is not resent");

    // STOP: signal to Soul, then Vio rejects commands for a while — „aborted”, not an error.
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    c.on_signal(Arc::new(move |s| g.lock().push(s)));
    mock.push("stopRequested", json!({}));
    until("stop signal", || !got.lock().is_empty());
    assert_eq!(got.lock()[0], crate::agent::computer::vio_ctx::Signal::Stop);
    mock.page.lock().stopped_until = Some(Instant::now() + Duration::from_secs(3));
    assert_eq!(c.call("listTabs", json!({})), Err(crate::platform::input::ABORTED.to_string()));
    c.set_soul_state("idle", "");
    until("setSoulState during STOP", || mock.sent("setSoulState").last().is_some_and(|a| a["state"] == "idle"));
    mock.page.lock().stopped_until = None;

    // cancel_all: a waiting call returns at once.
    let c2 = Arc::clone(&c);
    let slow = std::thread::spawn(move || {
        let t0 = Instant::now();
        (c2.call("getPage", json!({ "tabId": "t2" })), t0.elapsed())
    });
    std::thread::sleep(Duration::from_millis(200));
    c.cancel_all();
    let (r, took) = slow.join().unwrap();
    assert_eq!(r, Err(crate::platform::input::ABORTED.to_string()));
    assert!(took < Duration::from_secs(2), "cancelled after {took:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// v1 Vio (no `hello`): no capabilities, no subscribe, the old commands work.
#[test]
fn vio_v1_still_works() {
    let (mock, c, dir) = mock_client(false, two_tabs());
    assert!(c.caps().is_none() && !c.has("getReadable") && !c.context().live);
    assert!(mock.sent("subscribe").is_empty(), "nothing to subscribe on v1");
    assert!(c.call("listTabs", json!({})).unwrap().as_array().unwrap().len() == 2);
    // The article read falls back to getPage.
    assert!(super::vio::read_page(&c, "t1").unwrap().contains("1912"));
    assert!(mock.sent("getReadable").is_empty());
    c.set_soul_state("listening", "");
    std::thread::sleep(Duration::from_millis(400));
    assert!(mock.sent("setSoulState").is_empty(), "no setSoulState for v1");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Vio restarts: reconnect, a new `hello` and `subscribe`.
#[test]
fn vio_reconnect_handshakes_again() {
    let (mock, c, dir) = mock_client(true, two_tabs());
    mock.page.lock().kick = true;
    until("second hello", || mock.sent("hello").len() >= 2 && c.status().state == State::Connected && c.has("getReadable"));
    until("second subscribe", || mock.sent("subscribe").len() >= 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Media / tab / search helpers the voice commands use.
#[test]
fn vio_v2_helpers() {
    let (mock, c, dir) = mock_client(true, two_tabs());
    let v = super::vio::media(&c, Some("t1"), "seekBy", Some(30.0)).unwrap();
    assert_eq!(v["currentTime"], json!(40.0));
    assert!(super::vio::media_text(&v).starts_with("playing 0:40/1:40"));
    let e = super::vio::media(&c, Some("t2"), "pause", None).unwrap_err();
    assert!(e.contains("another site"), "cross-origin explanation: {e}");
    super::vio::tab_op(&c, "t1", "back").unwrap();
    assert_eq!(mock.page.lock().tabs[0]["url"], json!("https://example.com/start"));
    assert!(super::vio::tab_op(&c, "t1", "forward").unwrap_err().contains("cannot go forward"));
    super::vio::tab_op(&c, "t1", "mute").unwrap();
    assert_eq!(mock.page.lock().tabs[0]["muted"], json!(true));
    let (hits, text) = super::vio::search(&c, false, "bigos").unwrap();
    assert_eq!(hits.len(), 2);
    assert!(text.contains("Bigos staropolski"));
    assert!(super::vio::read_page(&c, "t1").unwrap().starts_with("Artykuł — Redakcja"));
    super::vio::click(&c, "t1", "r1", "Wyślij").unwrap_err();
    let log: Vec<String> = mock.page.lock().log.iter().map(|(c, _)| c.clone()).collect();
    let hl = log.iter().position(|c| c == "highlight").expect("highlight before click");
    assert_eq!(&log[hl..hl + 3], ["highlight", "click", "clearHighlight"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vio_mock_cases() {
    let rows = run_cases(None, None);
    let failed: Vec<String> = rows.iter().filter(|r| !r.ok).map(|r| format!("{}: {}", r.id, r.notes.join("; "))).collect();
    assert!(failed.is_empty(), "{failed:#?}");
    assert!(rows.len() >= 8);
    let forbidden = rows.iter().find(|r| r.id == "vio_forbidden").unwrap();
    assert_eq!(forbidden.outcome.model_calls, 0, "a forbidden tab never reaches the model");
    let expired = rows.iter().find(|r| r.id == "vio_expired_ref").unwrap();
    assert_eq!(expired.outcome.steps, 2, "local click → ref expired → fresh list → click");
}
