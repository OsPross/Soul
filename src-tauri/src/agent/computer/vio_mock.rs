//! Mock Vio automation API (`tests/agent/vio-mock.json`) and the Vio cases of the agent harness:
//! auth failure, forbidden tab, expired ref, empty fields, Polish fill, page read, confirmation,
//! password refusal. The very same `run_task` loop runs against a fake desktop (Vio in front,
//! no UI Automation), so nothing on the real screen is touched.
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
use tokio_tungstenite::tungstenite::{accept, Message};

fn scenario() -> Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/agent/vio-mock.json");
    serde_json::from_str(&std::fs::read_to_string(p).expect("vio-mock.json")).expect("vio-mock.json is JSON")
}

/// Page state the mock serves and the cases check.
struct Page {
    tabs: Vec<Value>,
    /// Bumped by navigation / `expireOnce`: refs from older generations fail.
    gen: u64,
    clicked: Vec<String>,
}

fn tab_mut<'a>(p: &'a mut Page, id: &Value) -> Result<&'a mut Value, String> {
    p.tabs.iter_mut().find(|t| &t["tabId"] == id).ok_or_else(|| "no such tab".to_string())
}

fn handle(p: &mut Page, cmd: &str, a: &Value) -> Result<Value, String> {
    let gen = p.gen;
    let allowed = |t: &Value| if t["forbidden"] == json!(true) { Err("forbidden: private tab".to_string()) } else { Ok(()) };
    match cmd {
        "listTabs" => Ok(Value::Array(
            p.tabs.iter().map(|t| json!({ "tabId": t["tabId"], "title": t["title"], "url": t["url"], "active": t["active"] })).collect(),
        )),
        "openTab" => {
            let id = p.tabs.iter().filter_map(|t| t["tabId"].as_i64()).max().unwrap_or(0) + 1;
            for t in &mut p.tabs {
                t["active"] = json!(false);
            }
            p.tabs.push(json!({ "tabId": id, "title": a["url"], "url": a["url"], "active": true, "text": "", "elements": [] }));
            Ok(json!({ "tabId": id }))
        }
        "activateTab" => {
            for t in &mut p.tabs {
                t["active"] = json!(t["tabId"] == a["tabId"]);
            }
            Ok(Value::Null)
        }
        "navigate" => {
            let t = tab_mut(p, &a["tabId"])?;
            allowed(t)?;
            t["url"] = a["url"].clone();
            p.gen += 1;
            Ok(Value::Null)
        }
        "getPage" => {
            let t = tab_mut(p, &a["tabId"])?;
            allowed(t)?;
            Ok(json!({ "url": t["url"], "title": t["title"], "text": t["text"] }))
        }
        "getElements" => {
            let t = tab_mut(p, &a["tabId"])?;
            allowed(t)?;
            let els = t["elements"].as_array().cloned().unwrap_or_default();
            Ok(Value::Array(
                els.into_iter()
                    .map(|mut e| {
                        e["ref"] = json!(format!("{}.{gen}", e["ref"].as_str().unwrap_or("")));
                        if e["type"] == json!("password") {
                            e["value"] = json!("");
                        }
                        e
                    })
                    .collect(),
            ))
        }
        "click" | "fill" | "pressKey" | "scroll" => {
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
                let text = a["text"].as_str().unwrap_or("");
                e["value"] = json!(text);
                e["empty"] = json!(text.is_empty());
                return Ok(Value::Null);
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
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let page = Arc::new(Mutex::new(Page { tabs, gen: 1, clicked: Vec::new() }));
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
                    loop {
                        let msg = match ws.read() {
                            Ok(Message::Text(t)) => serde_json::from_str::<Value>(t.as_str()).unwrap_or(Value::Null),
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
    for p in case["expect_prompt"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if !prompts.iter().any(|t| t.contains(p)) {
            bad.push(format!("the observation lacks {p}"));
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
        if let Some(active) = case["active_tab"].as_i64() {
            for t in &mut tabs {
                t["active"] = json!(t["tabId"] == json!(active));
            }
        }
        let mock = Mock::start(token.clone(), tabs);
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
            seen.lock().push(body["contents"][0]["parts"][0]["text"].as_str().unwrap_or("").to_string());
            inner(body)
        });
        let host = VioHost { vio: client, model: recording, accept: case["confirm"] == json!("accept"), confirms: Mutex::new(Vec::new()) };
        let out = run_task(&host, case["task"].as_str().unwrap());
        let confirms = host.confirms.lock().clone();
        let notes = check(case, &mock.page.lock(), &out, &confirms, &prompts.lock());
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
