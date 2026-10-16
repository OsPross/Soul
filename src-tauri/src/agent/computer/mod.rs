//! Soul Agent: voice-driven computer control (design: `docs/agent.md`).
//!
//! `run_task` loops observe → decide (local plan or ONE model function call) → safety verdict →
//! (confirm) → execute → settle → verify (→ one retry with the other strategy), within
//! `max_steps` (≤ 20), 90 s in total and 10 s per model call. Everything app-specific (Island,
//! confirmations, the model client) goes through [`Host`], so the harness drives the very same
//! loop without Tauri.

pub mod action;
pub mod config;
mod exec;
#[cfg(test)]
mod harness;
pub mod host;
pub mod intent;
pub mod killswitch;
pub mod prompt;
pub mod safety;
pub mod vio;
#[cfg(test)]
mod vio_mock;

pub use intent::is_computer_task;

use crate::i18n::{tr, Lang};
use crate::platform::coords::{monitor_for_rect, Monitor};
use crate::platform::marks;
use crate::platform::ui::{clip, Rect, SnapshotOptions, Source, UiSnapshot};
use crate::platform::Platform;
use action::{Action, Decision};
use config::AgentConfig;
use safety::{ConfirmLevel, Verdict};
use serde_json::Value;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const TASK_TIMEOUT: Duration = Duration::from_secs(90);
/// One model request; a slower model is cooled and the pool tries the next one.
pub const MODEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(9);
/// One step's model call, room for a rotation after a slow model.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(20);
/// Consecutive failed steps before the task gives up.
const MAX_FAILS_IN_ROW: u32 = 3;
/// Context tool name for the conversation context („a teraz kliknij drugi”).
pub const TOOL_NAME: &str = "computer_task";
/// Click-through ring window flashed around the target (`src/agent/Highlight.tsx`).
pub const HIGHLIGHT_LABEL: &str = "agent-highlight";
const HIGHLIGHT_PAD: i32 = 10;
const HIGHLIGHT_VISIBLE: Duration = Duration::from_millis(650);

/// Blocking model call (primary + fallback), callable from a helper thread.
pub type ModelFn = Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Running,
    Confirm,
    Asking,
    Done,
    Failed,
    Aborted,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: Phase,
    pub step: u32,
    pub max: u32,
    pub text: String,
}

/// Everything the loop needs from the app (or the harness).
pub trait Host {
    fn platform(&self) -> Arc<dyn Platform>;
    fn config(&self) -> AgentConfig;
    /// Settings `language` (prompt reply rule).
    fn language(&self) -> String;
    fn ui_lang(&self) -> Lang;
    /// `[CONTEXT]` block of the conversation, if alive.
    fn context(&self) -> Option<String>;
    fn model(&self) -> ModelFn;
    /// How long until a model has a free request slot (`None` = now). The agent waits for it
    /// outside the per-call timeout.
    fn model_wait(&self) -> Option<Duration> {
        None
    }
    fn progress(&self, p: &Progress);
    /// Confirmation card (buttons / spoken yes-no). Blocking; false on timeout.
    fn confirm(&self, question: &str) -> bool;
    /// Ask a question and wait for a spoken answer (`None` = no answer / unsupported).
    fn ask(&self, question: &str) -> Option<String>;
    /// Flash a ring around `rect` (physical px). Non-blocking.
    fn highlight(&self, rect: Rect);
    /// A synthetic pointer action is about to hit `rect`'s area: make Soul's own windows there
    /// click-through. Returns how to undo it.
    fn pointer_guard(&self, at: Rect) -> Option<Box<dyn FnOnce()>>;
    /// Keep Soul's windows out of screenshots while the task runs.
    fn capture_exclusion(&self, on: bool);
    /// Debug dump directory (opt-in), cleared per task.
    fn debug_dir(&self) -> Option<std::path::PathBuf>;
    /// The Vio browser API client (browser tasks skip UI Automation when it is connected).
    fn vio(&self) -> Option<Arc<vio::Client>> {
        None
    }
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub result: Result<String, String>,
    /// One short sentence for the voice / Island.
    pub summary: String,
    pub steps: u32,
    pub model_calls: u32,
    pub screenshots: u32,
    pub ms: u64,
    pub aborted: bool,
    /// The task ended with a question to the user.
    pub question: Option<String>,
    /// Model input tokens are not tracked; request bytes are (rough cost indicator).
    pub request_bytes: usize,
}

struct Run<'a> {
    host: &'a dyn Host,
    platform: Arc<dyn Platform>,
    cfg: AgentConfig,
    lang: Lang,
    task: String,
    t0: Instant,
    step: u32,
    calls: u32,
    shots: u32,
    bytes: usize,
    history: Vec<String>,
    fails_in_row: u32,
    last_failed: bool,
    /// The previous model call (name + action) — an identical repeat is not executed.
    last_sig: Option<String>,
    want_look: bool,
    note: Option<String>,
    /// The latest READ (tab list, page text): shown again when the model repeats that read.
    last_read: Option<String>,
    monitors: Vec<Monitor>,
    debug: Option<std::path::PathBuf>,
    vio: Option<Arc<vio::Client>>,
}

/// Restores capture exclusion when the task ends (any path).
struct ExclusionGuard<'a>(&'a dyn Host);

impl Drop for ExclusionGuard<'_> {
    fn drop(&mut self) {
        self.0.capture_exclusion(false);
    }
}

pub fn run_task(host: &dyn Host, task: &str) -> Outcome {
    let cfg = host.config();
    let lang = host.ui_lang();
    let platform = host.platform();
    let mut run = Run {
        host,
        platform: Arc::clone(&platform),
        lang,
        task: task.trim().to_string(),
        t0: Instant::now(),
        step: 0,
        calls: 0,
        shots: 0,
        bytes: 0,
        history: Vec::new(),
        fails_in_row: 0,
        last_failed: false,
        last_sig: None,
        want_look: false,
        note: None,
        last_read: None,
        monitors: Vec::new(),
        debug: if cfg.debug_screenshots { host.debug_dir() } else { None },
        cfg,
        vio: host.vio(),
    };
    if !run.cfg.enabled {
        return run.finish(
            Err(tr(lang, "Sterowanie komputerem jest wyłączone (Ustawienia → Agent).", "Computer control is turned off (Settings → Agent).").into()),
            Phase::Failed,
        );
    }
    eprintln!("[soul][agent] computer task start chars={} max_steps={}", run.task.chars().count(), run.cfg.max_steps);
    crate::platform::prefer_physical_pixels();
    let _kill = killswitch::Guard::start(Arc::clone(&platform));
    host.capture_exclusion(true);
    let _exclusion = ExclusionGuard(host);
    run.monitors = platform.monitors();
    if let Some(dir) = &run.debug {
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::create_dir_all(dir);
    }
    run.main_loop()
}

impl Run<'_> {
    fn progress(&self, phase: Phase, text: impl Into<String>) {
        self.host.progress(&Progress {
            phase,
            step: self.step,
            max: self.cfg.max_steps,
            text: text.into(),
        });
    }

    fn finish(&mut self, result: Result<String, String>, phase: Phase) -> Outcome {
        let aborted = phase == Phase::Aborted;
        let summary = match &result {
            Ok(s) => s.clone(),
            Err(e) => e.clone(),
        };
        self.progress(phase, clip(&summary, 160));
        crate::agent::context::note_tool(
            TOOL_NAME,
            &serde_json::json!({ "task": clip(&self.task, 160), "result": clip(&summary, 160) }),
            result.is_ok(),
        );
        eprintln!(
            "[soul][agent] computer task end ok={} aborted={aborted} steps={} model_calls={} screenshots={} request_kb={} ms={}",
            result.is_ok(),
            self.step,
            self.calls,
            self.shots,
            self.bytes / 1024,
            self.t0.elapsed().as_millis()
        );
        Outcome {
            result,
            summary,
            steps: self.step,
            model_calls: self.calls,
            screenshots: self.shots,
            ms: self.t0.elapsed().as_millis() as u64,
            aborted,
            question: None,
            request_bytes: self.bytes,
        }
    }

    fn aborted_outcome(&mut self) -> Outcome {
        let why = killswitch::reason();
        let msg = tr(
            self.lang,
            format!("Zatrzymałem ({why})."),
            format!("Stopped ({why})."),
        );
        self.finish(Ok(msg), Phase::Aborted)
    }

    fn log(&self, action: &str, result: &str, ok: bool, ms: u64, model: bool, shot: bool) {
        config::log_step(config::StepLog {
            at: config::unix_ms(),
            task: clip(&self.task, 80),
            step: self.step,
            action: action.to_string(),
            result: clip(result, 200),
            ok,
            ms,
            model,
            screenshot: shot,
        });
    }

    fn snapshot(&self) -> Result<UiSnapshot, String> {
        match self.platform.ui_snapshot(&SnapshotOptions::default()) {
            Ok(s) => Ok(s),
            Err(e) => {
                // The tree failed (hung app, no UIA): fall back to vision on the foreground window.
                eprintln!("[soul][agent] snapshot failed: {e} — vision fallback");
                let w = self.platform.foreground_window().ok_or(e)?;
                Ok(UiSnapshot::empty(w, Source::None))
            }
        }
    }

    /// This step goes to Vio: connected (after the consent wait) and Vio is in front or the task
    /// names the browser. Otherwise UI Automation as usual.
    fn vio_step(&self) -> Option<(Arc<vio::Client>, Option<crate::platform::ui::WindowInfo>)> {
        let c = self.vio.as_ref()?;
        let state = c.status().state;
        if state == vio::State::Unavailable {
            return None;
        }
        let fg = self.platform.foreground_window().filter(|w| (w.pid != 0 && w.pid == c.pid()) || vio::is_vio_process(&w.process));
        // A web task goes to Vio even when another browser is in front, unless the user points at the screen.
        let web = intent::names_browser(&self.task) || (intent::is_browser_phrase(&self.task) && !intent::refers_to_screen(&self.task));
        if fg.is_none() && !web {
            return None;
        }
        if state == vio::State::Authorizing {
            self.progress(Phase::Running, tr(self.lang, "Zezwól w Vio…", "Allow it in Vio…"));
            if let Err(e) = c.wait_ready(vio::AUTH_TIMEOUT) {
                eprintln!("[soul][agent] vio not ready: {e} — UI Automation");
                return None;
            }
        }
        if fg.is_none() {
            // Bring Vio to the front (best effort); its tab list is what the task works on.
            if let Err(e) = self.platform.focus_app("Vio") {
                eprintln!("[soul][agent] vio focus failed: {e}");
            }
        }
        Some((Arc::clone(c), fg))
    }

    /// Active tab + its elements as a snapshot. A sleeping (discarded) tab is woken first.
    fn vio_snapshot(&self, c: &vio::Client, window: Option<crate::platform::ui::WindowInfo>) -> Result<(UiSnapshot, String), String> {
        let tabs = c.call("listTabs", serde_json::json!({}))?;
        let (tab, id) = vio::active_tab(&tabs).ok_or("Vio has no open tab.")?;
        let elements = vio::elements_of(c, &id)?;
        Ok((vio::snapshot(&tab, &elements, window), id))
    }

    /// Region of the window the screenshot (and vision coordinates) refer to.
    fn region(&self, s: &UiSnapshot) -> Rect {
        let mon = if s.window.monitor.is_empty() {
            monitor_for_rect(&s.window.rect, &self.monitors).map(|m| m.rect).unwrap_or(s.window.rect)
        } else {
            s.window.monitor
        };
        s.window.rect.intersect(&mon).unwrap_or(s.window.rect)
    }

    fn screenshot(&mut self, s: &UiSnapshot, vision: bool) -> Option<marks::Shot> {
        let t = Instant::now();
        let (cx, cy) = s.window.rect.center();
        let shot = crate::capture::grab_monitor_at(cx, cy).and_then(|(mx, my, img)| {
            let marks = (!vision).then_some(s.elements.as_slice());
            marks::render(&img, (mx, my), s.window.rect, marks, marks::MAX_SIDE, marks::JPEG_QUALITY)
        });
        match shot {
            Ok(shot) => {
                self.shots += 1;
                eprintln!(
                    "[soul][agent] screenshot {}x{} kb={} ms={}",
                    shot.width,
                    shot.height,
                    shot.jpeg.len() / 1024,
                    t.elapsed().as_millis()
                );
                Some(shot)
            }
            Err(e) => {
                eprintln!("[soul][agent] screenshot failed: {e}");
                None
            }
        }
    }

    /// Model call on a helper thread: ≤ STEP_TIMEOUT, abortable.
    fn call_model(&mut self, body: Value) -> Result<Value, String> {
        self.bytes += body.to_string().len();
        self.calls += 1;
        // Waiting for a free RPM slot is not part of the call's STEP_TIMEOUT (the 90 s task limit still is).
        let wait_t0 = Instant::now();
        let mut said = false;
        while let Some(w) = self.host.model_wait() {
            if wait_t0.elapsed() >= crate::ai::pool::MAX_WAIT {
                break;
            }
            if !said {
                said = true;
                self.progress(Phase::Running, tr(self.lang, "Czekam na model…", "Waiting for a model…"));
            }
            if killswitch::aborted() {
                return Err(crate::platform::input::ABORTED.into());
            }
            std::thread::sleep(w.min(Duration::from_millis(100)));
        }
        let model = self.host.model();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(model(body));
        });
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if killswitch::aborted() {
                return Err(crate::platform::input::ABORTED.into());
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(r) => return r,
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
                Err(RecvTimeoutError::Timeout) => {
                    let s = STEP_TIMEOUT.as_secs();
                    return Err(tr(self.lang, format!("Model nie odpowiedział w {s} s."), format!("The model did not answer within {s} s.")));
                }
                Err(RecvTimeoutError::Disconnected) => return Err("Model call failed.".into()),
            }
        }
    }

    fn debug_dump(&self, s: &UiSnapshot, shot: Option<&marks::Shot>, decision: &str) {
        let Some(dir) = &self.debug else { return };
        let base = dir.join(format!("step{:02}", self.step));
        let text = format!("TASK: {}\nWINDOW: {} ({})\nDECISION: {decision}\n\n{}", self.task, s.window.title, s.window.process, s.observation());
        let _ = std::fs::write(base.with_extension("txt"), text);
        if let Some(shot) = shot {
            let _ = std::fs::write(base.with_extension("jpg"), &shot.jpeg);
        }
    }

    fn main_loop(&mut self) -> Outcome {
        let level = ConfirmLevel::parse(&self.cfg.confirm_level);
        let context = self.host.context();
        loop {
            if killswitch::aborted() {
                return self.aborted_outcome();
            }
            if self.step >= self.cfg.max_steps {
                let msg = tr(
                    self.lang,
                    format!("Przerwałem po {} krokach — zadanie nie zostało ukończone.", self.step),
                    format!("Stopped after {} steps — the task is not finished.", self.step),
                );
                return self.finish(Err(msg), Phase::Failed);
            }
            if self.t0.elapsed() >= TASK_TIMEOUT {
                let msg = tr(self.lang, "Przekroczyłem limit 90 s — przerywam.", "Hit the 90 s limit — stopping.");
                return self.finish(Err(msg.into()), Phase::Failed);
            }
            self.step += 1;
            let step_t0 = Instant::now();
            self.progress(Phase::Running, tr(self.lang, "Patrzę na ekran…", "Looking at the screen…"));

            let vio = self.vio_step();
            let (snap, tab) = match vio.as_ref().map(|(c, w)| self.vio_snapshot(c, w.clone())) {
                Some(Ok((s, tab))) => (s, Some(tab)),
                Some(Err(e)) if e == crate::platform::input::ABORTED => return self.aborted_outcome(),
                // Private / Tor / internal tab: tell the user, never fall back to the screen.
                Some(Err(e)) if vio::is_forbidden(&e) => {
                    self.log("vio", &e, false, 0, false, false);
                    return self.finish(Err(vio::forbidden_message(self.lang, &e)), Phase::Failed);
                }
                other => {
                    if let Some(Err(e)) = other {
                        eprintln!("[soul][agent] vio snapshot failed: {e} — UI Automation");
                    }
                    match self.snapshot() {
                        Ok(s) => (s, None),
                        Err(e) => return self.finish(Err(e), Phase::Failed),
                    }
                }
            };
            if killswitch::aborted() {
                return self.aborted_outcome();
            }
            if self.step == 1 {
                if let Verdict::Refuse(r) = safety::fullscreen_guard(&snap.window, &self.task) {
                    return self.finish(Err(r), Phase::Failed);
                }
            }
            if let Verdict::Refuse(r) = safety::check_window(&snap, &self.cfg.blocklist) {
                self.log("—", &r, false, 0, false, false);
                return self.finish(Err(r), Phase::Failed);
            }
            let vision = tab.is_none() && (snap.source == Source::None || snap.is_useless());
            // 0–1000 coordinates refer to what the model saw: the screenshot when there is one.
            let mut region = self.region(&snap);

            // ── decide ──
            let local = if self.step == 1 {
                intent::local_plan(&self.task, &snap)
                    // „streść tę stronę” in Vio: read the active tab right away, the model only answers.
                    .or_else(|| (tab.is_some() && intent::is_read_page_task(&self.task)).then(read_page_plan))
            } else {
                None
            };
            let from_model = local.is_none();
            let mut shot_taken = false;
            let decision: Decision = match local {
                Some(d) => {
                    eprintln!("[soul][agent] computer local plan: {}", d.name);
                    d
                }
                None => {
                    let need_image = tab.is_none() && (self.step == 1 || vision || self.last_failed || self.want_look);
                    let shot = if need_image { self.screenshot(&snap, vision) } else { None };
                    shot_taken = shot.is_some();
                    if let Some(sh) = &shot {
                        region = sh.region;
                    }
                    let note = self.note.take();
                    let obs = prompt::Observation {
                        task: &self.task,
                        context: context.as_deref(),
                        step: self.step,
                        max_steps: self.cfg.max_steps,
                        elapsed_s: self.t0.elapsed().as_secs(),
                        history: &self.history,
                        snapshot: &snap,
                        vision,
                        note: note.as_deref(),
                        jpeg: shot.as_ref().map(|s| s.jpeg.as_slice()),
                    };
                    let body = prompt::request_body(&obs, &self.host.language());
                    self.progress(Phase::Running, tr(self.lang, "Myślę…", "Thinking…"));
                    let response = self.call_model(body);
                    if killswitch::aborted() {
                        return self.aborted_outcome();
                    }
                    let parsed = response.and_then(|r| prompt::first_call(&r)).and_then(|(name, args)| {
                        eprintln!("[soul][agent] computer step={} call={name} args={}", self.step, clip(&args.to_string(), 300));
                        action::parse(&name, &args)
                    });
                    match parsed {
                        Ok(d) => {
                            self.debug_dump(&snap, shot.as_ref(), &format!("{} {:?}", d.name, d.action));
                            d
                        }
                        Err(e) => {
                            // The model never acted on this READ: keep it for the next try.
                            self.note = self.note.take().or(note);
                            self.fails_in_row += 1;
                            self.last_failed = true;
                            self.history.push(format!("(invalid step: {})", clip(&e, 160)));
                            self.log("model", &e, false, step_t0.elapsed().as_millis() as u64, true, shot_taken);
                            if self.fails_in_row >= MAX_FAILS_IN_ROW || e.contains("API key") || e.contains("quota") {
                                return self.finish(Err(e), Phase::Failed);
                            }
                            continue;
                        }
                    }
                }
            };
            self.want_look = decision.look;

            // ── loop guard: the same call twice in a row is not run again ──
            let sig = format!("{} {:?}", decision.name, decision.action);
            let repeatable = matches!(decision.action, Action::Scroll { .. } | Action::Wait(_) | Action::KeyCombo(_) | Action::Done(_) | Action::AskUser(_));
            if from_model && !repeatable && self.last_sig.as_deref() == Some(sig.as_str()) {
                let hint = repeat_hint(&self.task, tab.is_some());
                eprintln!("[soul][agent] computer repeated call {} — not executed", decision.name);
                self.fails_in_row += 1;
                self.last_failed = true;
                self.history.push(format!("{} → NOT RUN: identical to the previous call. {hint}", decision.name));
                // A repeated read gets its earlier result again („already above” must be true).
                let read_again = matches!(
                    decision.action,
                    Action::Browser(action::BrowserOp::ListTabs | action::BrowserOp::ReadPage | action::BrowserOp::GetElements)
                );
                self.note = match (&self.last_read, read_again) {
                    (Some(read), true) => Some(read.clone()),
                    _ => Some(hint),
                };
                if self.fails_in_row >= MAX_FAILS_IN_ROW {
                    let msg = tr(self.lang, "Zapętliłem się — przerywam.", "I'm going in circles — stopping.");
                    return self.finish(Err(msg.into()), Phase::Failed);
                }
                continue;
            }
            self.last_sig = Some(sig);

            // ── terminal actions ──
            match &decision.action {
                Action::Done(summary) => {
                    let summary = if summary.trim().is_empty() {
                        tr(self.lang, "Gotowe.", "Done.").to_string()
                    } else {
                        summary.clone()
                    };
                    self.log("done", &summary, true, step_t0.elapsed().as_millis() as u64, from_model, shot_taken);
                    return self.finish(Ok(summary), Phase::Done);
                }
                Action::AskUser(q) => {
                    self.progress(Phase::Asking, q.clone());
                    self.log("ask_user", q, true, 0, from_model, shot_taken);
                    match self.host.ask(q) {
                        Some(answer) if !answer.trim().is_empty() => {
                            self.history.push(format!("ask_user \"{}\" → user answered: \"{}\"", clip(q, 80), clip(&answer, 200)));
                            self.last_failed = false;
                            continue;
                        }
                        _ => {
                            let mut out = self.finish(Ok(q.clone()), Phase::Done);
                            out.question = Some(q.clone());
                            return out;
                        }
                    }
                }
                _ => {}
            }

            // ── validate targets ──
            if let Some(missing) = decision.action.targets().iter().find_map(|t| match t {
                action::Target::Id(id) if snap.get(*id).is_none() => Some(*id),
                _ => None,
            }) {
                self.fails_in_row += 1;
                self.last_failed = true;
                self.history.push(format!("{} → FAILED: no element #{missing} in the current list", action::history_line(&decision, &snap)));
                if self.fails_in_row >= MAX_FAILS_IN_ROW {
                    let msg = tr(self.lang, "Nie mogę znaleźć elementów, o które chodzi.", "I can't find the elements needed.");
                    return self.finish(Err(msg.into()), Phase::Failed);
                }
                continue;
            }

            // ── safety ──
            let describe = action::describe(&decision.action, &snap, self.lang);
            let mut confirmed = false;
            match safety::check_step(&decision, &snap, level) {
                Verdict::Refuse(r) => {
                    self.log(&action::history_line(&decision, &snap), &format!("REFUSED: {r}"), false, 0, from_model, shot_taken);
                    return self.finish(Err(r), Phase::Failed);
                }
                Verdict::Confirm(reason) => {
                    let question = format!("{} ({reason})", describe.trim_end_matches('…'));
                    self.progress(Phase::Confirm, question.clone());
                    eprintln!("[soul][agent] computer confirm step={} reason={reason}", self.step);
                    let t_wait = Instant::now();
                    let ok = self.host.confirm(&format!("{}?", question));
                    // Waiting for the user does not eat the task budget.
                    self.t0 += t_wait.elapsed();
                    if killswitch::aborted() {
                        return self.aborted_outcome();
                    }
                    if !ok {
                        let msg = tr(self.lang, "Anulowano — niczego nie zmieniłem w tym kroku.", "Cancelled — nothing was changed in this step.");
                        self.log(&action::history_line(&decision, &snap), "declined", false, 0, from_model, shot_taken);
                        return self.finish(Ok(msg.into()), Phase::Aborted);
                    }
                    confirmed = true;
                }
                Verdict::Allow => {}
            }

            // ── act ──
            self.progress(Phase::Running, describe.clone());
            let line = action::history_line(&decision, &snap);
            let outcome = exec::perform(
                &exec::Cx {
                    host: self.host,
                    platform: &self.platform,
                    snap: &snap,
                    region,
                    monitors: &self.monitors,
                    highlight: self.cfg.highlight,
                    vio: vio.as_ref().map(|(c, _)| c.as_ref()).zip(tab.as_deref()),
                },
                &decision,
                confirmed,
            );
            if killswitch::aborted() {
                return self.aborted_outcome();
            }
            let ms = step_t0.elapsed().as_millis() as u64;
            match outcome {
                Ok(done) => {
                    self.last_failed = !done.verified;
                    if done.verified {
                        self.fails_in_row = 0;
                    } else {
                        self.fails_in_row += 1;
                    }
                    if let Some(text) = &done.read {
                        self.note = Some(text.clone());
                        self.last_read = Some(text.clone());
                    }
                    let status = if done.verified { "ok" } else { "FAILED" };
                    self.history.push(format!("{line} → {status}{}", done.detail));
                    self.log(&line, &format!("{status}{}", done.detail), done.verified, ms, from_model, shot_taken);
                    if decision.final_step && done.verified {
                        let summary = exec::done_text(&decision.action, &snap, self.lang);
                        return self.finish(Ok(summary), Phase::Done);
                    }
                    if self.fails_in_row >= MAX_FAILS_IN_ROW {
                        let msg = tr(
                            self.lang,
                            format!("Nie udało się: {}", clip(&done.detail, 120)),
                            format!("It didn't work: {}", clip(&done.detail, 120)),
                        );
                        return self.finish(Err(msg), Phase::Failed);
                    }
                }
                Err(e) => {
                    if e == crate::platform::input::ABORTED {
                        return self.aborted_outcome();
                    }
                    if vio::is_forbidden(&e) {
                        self.log(&line, &e, false, ms, from_model, shot_taken);
                        return self.finish(Err(vio::forbidden_message(self.lang, &e)), Phase::Failed);
                    }
                    self.fails_in_row += 1;
                    self.last_failed = true;
                    self.history.push(format!("{line} → FAILED: {}", clip(&e, 160)));
                    self.log(&line, &e, false, ms, from_model, shot_taken);
                    if self.fails_in_row >= MAX_FAILS_IN_ROW {
                        return self.finish(Err(e), Phase::Failed);
                    }
                }
            }
        }
    }
}

/// A page read as the first (model-free) step of „streść / przeczytaj tę stronę”.
fn read_page_plan() -> Decision {
    Decision { action: Action::Browser(action::BrowserOp::ReadPage), risky: false, final_step: false, look: false, name: "browser_read_page".into() }
}

/// What to tell the model after it repeated an identical call.
fn repeat_hint(task: &str, vio: bool) -> String {
    if vio && intent::is_read_page_task(task) {
        "You already have this. Call browser_read_page once (not browser_list_tabs) and then answer with done(summary).".into()
    } else {
        "Do not repeat it: its result is already above. Pick a different action, or finish with done / ask_user.".into()
    }
}

/// Highlight flashes; a hide only applies to the flash that scheduled it.
static HIGHLIGHT_TICKET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Flash the ring around `rect` (physical px) for ~0.65 s. The window (`tauri.conf.json`,
/// created hidden) is click-through, never activated and excluded from screen capture.
pub(crate) fn show_highlight(app: &tauri::AppHandle, rect: Rect) {
    use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize};
    let Some(state) = app.try_state::<crate::AppState>() else { return };
    let Some(win) = app.get_webview_window(HIGHLIGHT_LABEL) else { return };
    let platform = Arc::clone(&state.platform);
    let id = crate::platform::native_window_id(&win);
    // Before the style bits: tao recomputes the extended style on this call.
    let _ = win.set_ignore_cursor_events(true);
    if let Some(id) = id {
        platform.prepare_overlay_window(id);
    }
    let pad = HIGHLIGHT_PAD;
    let _ = win.set_position(PhysicalPosition::new(rect.x - pad, rect.y - pad));
    let _ = win.set_size(PhysicalSize::new((rect.w + 2 * pad).max(24) as u32, (rect.h + 2 * pad).max(24) as u32));
    if !id.is_some_and(|id| platform.show_overlay_window(id, true)) {
        let _ = win.show();
    }
    // Only once visible (see `exclude_from_capture`).
    if let Some(id) = id {
        let _ = platform.exclude_from_capture(id, true);
    }
    let _ = win.emit("agent://highlight", ());
    let ticket = HIGHLIGHT_TICKET.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(HIGHLIGHT_VISIBLE);
        if HIGHLIGHT_TICKET.load(std::sync::atomic::Ordering::SeqCst) == ticket {
            hide_highlight(&app);
        }
    });
}

/// Hide the ring now (flash timeout, task end / abort).
pub(crate) fn hide_highlight(app: &tauri::AppHandle) {
    use tauri::Manager;
    HIGHLIGHT_TICKET.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let Some(win) = app.get_webview_window(HIGHLIGHT_LABEL) else { return };
    let Some(state) = app.try_state::<crate::AppState>() else { return };
    if !crate::platform::native_window_id(&win).is_some_and(|id| state.platform.show_overlay_window(id, false)) {
        let _ = win.hide();
    }
}
