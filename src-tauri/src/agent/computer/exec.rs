//! Executes one decided action: picks a strategy (UI Automation pattern first, synthetic input
//! as the fallback — or the other way round for web content), waits for the UI to settle,
//! verifies the expected change and retries once with the other strategy when nothing happened.
//! A confirmed or risky action is never retried automatically (no double „Wyślij”).

use super::action::{Action, BrowserOp, Decision, Target};
use super::{killswitch, vio, Host};
use crate::i18n::{tr, Lang};
use crate::platform::coords::{clamp_off_corners, norm_to_screen, Monitor, Point};
use crate::platform::input::{self, Combo, InputStep, Key, MouseButton};
use crate::platform::ui::{clip, ElementOp, QuickState, Rect, Role, SnapshotOptions, UiElement, UiSnapshot, STALE};
use crate::platform::Platform;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SETTLE_MAX: Duration = Duration::from_millis(2000);
const SETTLE_MIN: Duration = Duration::from_millis(300);
const SETTLE_STABLE: Duration = Duration::from_millis(240);
const SETTLE_POLL: Duration = Duration::from_millis(120);
const HIGHLIGHT_MS: u64 = 320;
const VIO_SETTLE: Duration = Duration::from_millis(350);

pub struct Cx<'a> {
    pub host: &'a dyn Host,
    pub platform: &'a Arc<dyn Platform>,
    pub snap: &'a UiSnapshot,
    /// Screen region of the window / screenshot (0–1000 coordinates).
    pub region: Rect,
    pub monitors: &'a [Monitor],
    pub highlight: bool,
    /// This step goes through the Vio browser API: client + active tab.
    pub vio: Option<(&'a vio::Client, &'a str)>,
}

/// Result of a performed step.
pub struct Performed {
    pub verified: bool,
    /// " (window → «…»)", " — retried with the mouse", " — nothing changed"…
    pub detail: String,
    /// `read_text` result for the next observation.
    pub read: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strategy {
    Primary,
    Alternate,
}

/// What a strategy did; `has_alternate` = a different strategy exists for a retry.
struct Attempt {
    used: &'static str,
    has_alternate: bool,
}

fn send_steps(cx: &Cx, steps: &[InputStep]) -> Result<(), String> {
    let p = Arc::clone(cx.platform);
    input::execute(steps, &killswitch::aborted, &mut |s| p.send_input_step(s))
}

/// Keyboard input goes to the foreground window: make sure it is the task's window.
fn ensure_foreground(cx: &Cx) -> Result<(), String> {
    let want = cx.snap.window.id;
    if want == 0 || cx.platform.active_window() == Some(want) {
        return Ok(());
    }
    cx.platform
        .activate_window(want)
        .map_err(|e| format!("Couldn't bring the window back to the front: {e}"))?;
    killswitch::sleep(Duration::from_millis(150));
    Ok(())
}

fn point_of(cx: &Cx, t: &Target) -> Option<(Point, Rect)> {
    match t {
        Target::Id(id) => {
            let e = cx.snap.get(*id)?;
            let (x, y) = e.rect.center();
            Some((Point::new(x, y), e.rect))
        }
        Target::Point { x, y } => {
            let p = norm_to_screen(*x, *y, cx.region);
            Some((p, Rect::new(p.x - 18, p.y - 18, 36, 36)))
        }
    }
}

fn flash(cx: &Cx, rect: Rect) {
    if cx.highlight {
        cx.host.highlight(rect);
        killswitch::sleep(Duration::from_millis(HIGHLIGHT_MS));
    }
}

fn mouse(cx: &Cx, p: Point, area: Rect, button: MouseButton, count: u8) -> Result<(), String> {
    let p = clamp_off_corners(p, cx.monitors, 3);
    flash(cx, area);
    let restore = cx.host.pointer_guard(area);
    let r = send_steps(cx, &input::plan_click(p, button, count));
    if let Some(undo) = restore {
        undo();
    }
    r
}

fn element_op(cx: &Cx, id: u32, op: ElementOp) -> Result<(), String> {
    match cx.platform.ui_element_op(cx.snap.gen, id, &op) {
        Ok(()) => Ok(()),
        // A modal dialog opened by Invoke can keep the call busy past UIA's timeout: the action
        // most likely happened — let verification decide.
        Err(e) if e.contains("timed out") || e.contains("timeout") => {
            eprintln!("[soul][agent] element op {op:?} timed out — verifying");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn pattern_click(e: &UiElement) -> Option<ElementOp> {
    if e.caps.invoke {
        Some(ElementOp::Invoke)
    } else if e.caps.toggle {
        Some(ElementOp::Toggle)
    } else if e.caps.select {
        Some(ElementOp::Select)
    } else if e.caps.expand {
        Some(if e.expanded == Some(true) { ElementOp::Collapse } else { ElementOp::Expand })
    } else {
        None
    }
}

fn click(cx: &Cx, target: &Target, button: MouseButton, count: u8, strategy: Strategy) -> Result<Attempt, String> {
    let (p, area) = point_of(cx, target).ok_or("The target is gone.")?;
    let el = match target {
        Target::Id(id) => cx.snap.get(*id),
        Target::Point { .. } => None,
    };
    if let Some(e) = el {
        if !e.enabled {
            return Err(format!("\u{201e}{}\u{201d} is disabled.", clip(&e.display_name(), 40)));
        }
    }
    let pattern = el.filter(|_| button == MouseButton::Left && count == 1).and_then(pattern_click);
    let use_pattern = match (strategy, &pattern) {
        (Strategy::Primary, Some(_)) => true,
        (Strategy::Alternate, Some(_)) => false,
        (_, None) => false,
    };
    if use_pattern {
        let e = el.expect("pattern implies element");
        flash(cx, area);
        match element_op(cx, e.id, pattern.clone().expect("checked")) {
            Ok(()) => return Ok(Attempt { used: "pattern", has_alternate: true }),
            Err(err) if err == STALE => {} // fall through to the mouse
            Err(err) => {
                eprintln!("[soul][agent] pattern click failed: {err} — mouse");
            }
        }
    }
    mouse(cx, p, area, button, count)?;
    Ok(Attempt {
        used: "mouse",
        has_alternate: strategy == Strategy::Primary && pattern.is_some() && !use_pattern,
    })
}

/// Single-line native fields take ValuePattern.SetValue; documents and web fields the keyboard.
fn can_set_value(e: &UiElement, clear: bool) -> bool {
    e.caps.set_value
        && !e.web
        && !e.password
        && matches!(e.role, Role::Edit | Role::ComboBox | Role::Spinner)
        && (clear || e.value.as_deref().is_none_or(|v| v.trim().is_empty()))
}

/// How text gets into a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeMode {
    /// ValuePattern.SetValue (single-line native fields).
    SetValue,
    /// Clipboard + Ctrl+V, previous clipboard restored. Exact for native apps: Win11 Notepad's
    /// text input resolves queued Unicode key packets late and repeats characters.
    Paste,
    /// One Unicode key event per UTF-16 unit (KEYEVENTF_UNICODE): web content / Electron.
    Keys,
}

/// Strategy order for a target: native single-line field → SetValue, Paste, Keys;
/// other native targets → Paste, Keys; web → Keys, Paste.
fn type_modes(el: Option<&UiElement>, focused_web: bool, clear: bool) -> Vec<TypeMode> {
    let web = el.map(|e| e.web).unwrap_or(focused_web);
    let mut modes = Vec::with_capacity(3);
    if el.is_some_and(|e| can_set_value(e, clear)) {
        modes.push(TypeMode::SetValue);
    }
    if web {
        modes.extend([TypeMode::Keys, TypeMode::Paste]);
    } else {
        modes.extend([TypeMode::Paste, TypeMode::Keys]);
    }
    modes
}

fn type_text(cx: &Cx, target: Option<&Target>, text: &str, clear: bool, submit: bool, strategy: Strategy) -> Result<Attempt, String> {
    let el = match target {
        Some(Target::Id(id)) => Some(cx.snap.get(*id).ok_or("The target field is gone.")?),
        _ => None,
    };
    let modes = type_modes(el, cx.snap.focused().is_some_and(|f| f.web), clear);
    let mode = match strategy {
        Strategy::Primary => modes[0],
        Strategy::Alternate => modes[1],
    };
    let has_alternate = strategy == Strategy::Primary;
    if let Some(e) = el {
        flash(cx, e.rect);
    }
    let enter = || input::plan_combo(&Combo { modifiers: vec![], key: Key::Enter });
    if mode == TypeMode::SetValue {
        let e = el.expect("SetValue implies an element");
        element_op(cx, e.id, ElementOp::SetValue(text.to_string()))?;
        if submit {
            let _ = element_op(cx, e.id, ElementOp::Focus);
            ensure_foreground(cx)?;
            send_steps(cx, &enter())?;
        }
        return Ok(Attempt { used: "set_value", has_alternate });
    }
    // Keyboard / paste: focus the field, place the caret, insert, optionally Enter.
    ensure_foreground(cx)?;
    match (el, target) {
        (Some(e), _) => {
            let focused = cx.platform.ui_element_op(cx.snap.gen, e.id, &ElementOp::Focus).is_ok();
            if !focused || e.web {
                let (x, y) = e.rect.center();
                mouse(cx, Point::new(x, y), e.rect, MouseButton::Left, 1)?;
            }
        }
        (None, Some(t @ Target::Point { .. })) => {
            let (p, area) = point_of(cx, t).ok_or("Bad coordinates.")?;
            mouse(cx, p, area, MouseButton::Left, 1)?;
        }
        _ => {}
    }
    killswitch::sleep(Duration::from_millis(60));
    let mut caret = Vec::new();
    if clear {
        caret.extend(input::plan_combo(&Combo { modifiers: vec![Key::Ctrl], key: Key::Char('A') }));
        caret.push(InputStep::Pause(40));
    } else if el.is_some_and(|e| e.role == Role::Document) {
        // Append at the end of a document, never in the middle of the user's text.
        caret.extend(input::plan_combo(&Combo { modifiers: vec![Key::Ctrl], key: Key::End }));
    } else if el.is_some_and(|e| e.value.as_deref().is_some_and(|v| !v.is_empty())) {
        caret.extend(input::plan_combo(&Combo { modifiers: vec![], key: Key::End }));
    }
    send_steps(cx, &caret)?;
    match mode {
        TypeMode::Paste => {
            if killswitch::aborted() {
                return Err(input::ABORTED.into());
            }
            cx.platform.paste_text(text)?;
        }
        _ => send_steps(cx, &input::plan_type(text))?,
    }
    if submit {
        killswitch::sleep(Duration::from_millis(80));
        send_steps(cx, &enter())?;
    }
    Ok(Attempt {
        used: if mode == TypeMode::Paste { "paste" } else { "keys" },
        has_alternate,
    })
}

/// The biggest scrollable element (the page / list) when no target was named.
fn default_scroll_target(s: &UiSnapshot) -> Option<&UiElement> {
    s.elements.iter().filter(|e| e.caps.scroll).max_by_key(|e| e.rect.area())
}

fn scroll(cx: &Cx, target: Option<&Target>, dir: crate::platform::ui::ScrollDir, amount: u8, strategy: Strategy) -> Result<Attempt, String> {
    let el = match target {
        Some(Target::Id(id)) => cx.snap.get(*id),
        Some(Target::Point { .. }) => None,
        None => default_scroll_target(cx.snap),
    };
    if strategy == Strategy::Primary {
        if let Some(e) = el.filter(|e| e.caps.scroll) {
            if element_op(cx, e.id, ElementOp::Scroll { dir, amount }).is_ok() {
                return Ok(Attempt { used: "scroll_pattern", has_alternate: true });
            }
        }
    }
    let p = match (target, el) {
        (Some(t @ Target::Point { .. }), _) => point_of(cx, t).map(|(p, _)| p),
        (_, Some(e)) => {
            let (x, y) = e.rect.center();
            Some(Point::new(x, y))
        }
        _ => {
            let (x, y) = cx.region.center();
            Some(Point::new(x, y))
        }
    };
    let p = p.map(|p| clamp_off_corners(p, cx.monitors, 3));
    let area = p.map(|p| Rect::new(p.x - 40, p.y - 40, 80, 80)).unwrap_or(cx.region);
    let restore = cx.host.pointer_guard(area);
    let r = send_steps(cx, &input::plan_scroll(p, dir, amount));
    if let Some(undo) = restore {
        undo();
    }
    r?;
    // The wheel is already the fallback (or the only way): nothing left to retry with.
    Ok(Attempt { used: "wheel", has_alternate: false })
}

fn attempt(cx: &Cx, a: &Action, strategy: Strategy) -> Result<Attempt, String> {
    match a {
        Action::Click { target, button, count } => click(cx, target, *button, *count, strategy),
        Action::TypeText { target, text, clear, submit } => type_text(cx, target.as_ref(), text, *clear, *submit, strategy),
        Action::KeyCombo(c) => {
            ensure_foreground(cx)?;
            if c.has(Key::Escape) {
                killswitch::suppress_esc_for(600);
            }
            send_steps(cx, &input::plan_combo(c))?;
            Ok(Attempt { used: "keys", has_alternate: false })
        }
        Action::Scroll { target, dir, amount } => scroll(cx, target.as_ref(), *dir, *amount, strategy),
        Action::Drag { from, to } => {
            let (a, ra) = point_of(cx, from).ok_or("Drag source is gone.")?;
            let (b, _) = point_of(cx, to).ok_or("Drag target is gone.")?;
            flash(cx, ra);
            let area = Rect::new(a.x.min(b.x) - 20, a.y.min(b.y) - 20, (a.x - b.x).abs() + 40, (a.y - b.y).abs() + 40);
            let restore = cx.host.pointer_guard(area);
            let r = send_steps(cx, &input::plan_drag(clamp_off_corners(a, cx.monitors, 3), clamp_off_corners(b, cx.monitors, 3)));
            if let Some(undo) = restore {
                undo();
            }
            r?;
            Ok(Attempt { used: "drag", has_alternate: false })
        }
        Action::OpenApp(name) => cx.platform.open_app(name).map(|_| Attempt { used: "open_app", has_alternate: false }),
        Action::OpenSettings(page) => cx.platform.open_settings_page(page).map(|_| Attempt { used: "open_settings", has_alternate: false }),
        Action::FocusApp(name) => cx.platform.focus_app(name).map(|_| Attempt { used: "focus_app", has_alternate: false }),
        Action::OpenUrl(url) => vio::open_url(cx.platform.as_ref(), url)
            .unwrap_or_else(|| cx.platform.open_url(url))
            .map(|_| Attempt { used: "open_url", has_alternate: false }),
        Action::Wait(_) | Action::ReadText(_) | Action::Done(_) | Action::AskUser(_) => Ok(Attempt { used: "none", has_alternate: false }),
        Action::Browser(_) => Err("The Vio browser is not connected.".into()),
    }
}

/// A Vio step: the API call is the action and its `ok` the verification (no settle polling).
fn perform_vio(cx: &Cx, c: &vio::Client, tab: &str, d: &Decision) -> Result<Performed, String> {
    let vref = |t: &Target| match t {
        Target::Id(id) => cx.snap.get(*id).map(|e| e.automation_id.clone()).ok_or_else(|| "No such element.".to_string()),
        Target::Point { .. } => Err("Vio needs an element id, not coordinates.".to_string()),
    };
    let key = |k: &str| {
        // Vio wants its window focused for key events.
        let _ = ensure_foreground(cx);
        c.call("pressKey", json!({ "tabId": tab, "key": k })).map(|_| ())
    };
    let mut read = None;
    let mut detail = String::new();
    match &d.action {
        Action::Click { target, button: MouseButton::Left, count: 1 } => {
            c.call("click", json!({ "tabId": tab, "ref": vref(target)? }))?;
        }
        Action::TypeText { target: Some(t), text, submit, .. } => {
            let r = vref(t)?;
            let skipped = |why: &str| Ok(Performed { verified: true, detail: format!(" — NOT filled: {why}"), read: None });
            let plan = match t {
                Target::Id(id) => cx.snap.get(*id).map(|e| vio::fill_plan(e, text)).unwrap_or(vio::FillPlan::Text),
                Target::Point { .. } => vio::FillPlan::Text,
            };
            match plan {
                vio::FillPlan::Skip(why) => return skipped(&why),
                vio::FillPlan::AlreadySet => return Ok(Performed { verified: true, detail: " (already set)".into(), read: None }),
                vio::FillPlan::Click => {
                    c.call("click", json!({ "tabId": tab, "ref": r }))?;
                }
                vio::FillPlan::Text => {
                    if let Err(e) = c.call("fill", json!({ "tabId": tab, "ref": r, "text": text })) {
                        // Never end the task over a field type: the model decides what to do next.
                        if e.contains("not a text field") {
                            return skipped(&format!("{e}. It is not a text field; click it, or skip it and say so in done."));
                        }
                        return Err(e);
                    }
                }
            }
            if *submit {
                killswitch::sleep(Duration::from_millis(80));
                key("Enter")?;
            }
        }
        Action::KeyCombo(k) => key(&k.label())?,
        Action::Scroll { dir, .. } if dir.is_vertical() => {
            let direction = if *dir == crate::platform::ui::ScrollDir::Up { "up" } else { "down" };
            c.call("scroll", json!({ "tabId": tab, "direction": direction }))?;
        }
        Action::Browser(op) => match op {
            BrowserOp::ListTabs => read = Some(vio::tabs_text(&c.call("listTabs", json!({}))?)),
            BrowserOp::Open { url: Some(u), .. } => {
                vio::open_tab(c, cx.platform.as_ref(), u)?;
            }
            BrowserOp::Open { tab: Some(t), .. } => {
                c.call("activateTab", json!({ "tabId": t }))?;
            }
            BrowserOp::Open { .. } => return Err("browser_open: give a url or a tab_id".into()),
            BrowserOp::Navigate(u) => {
                c.call("navigate", json!({ "tabId": tab, "url": u }))?;
            }
            BrowserOp::ReadPage => read = Some(vio::page_text(&c.call("getPage", json!({ "tabId": tab }))?)),
            BrowserOp::GetElements => detail = " (the list is refreshed every turn)".into(),
        },
        Action::OpenUrl(u) => {
            vio::open_tab(c, cx.platform.as_ref(), u)?;
        }
        Action::OpenApp(_) | Action::OpenSettings(_) | Action::FocusApp(_) => {
            attempt(cx, &d.action, Strategy::Primary)?;
        }
        _ => return Err("Not possible through Vio: use browser_click / browser_fill / browser_scroll with an element id.".into()),
    }
    eprintln!("[soul][agent] computer exec {} via=vio", d.name);
    if d.action.acts() {
        // Let the page react before the next getElements (navigation renews the refs).
        killswitch::sleep(VIO_SETTLE);
    }
    Ok(Performed { verified: true, detail, read })
}

/// Poll the cheap state until it is stable (or 2 s). Returns the last state.
fn settle(cx: &Cx, target: Option<u32>) -> Option<QuickState> {
    let start = Instant::now();
    let mut last: Option<QuickState> = None;
    let mut stable_since = start;
    loop {
        if !killswitch::sleep(SETTLE_POLL) {
            return last;
        }
        let q = cx.platform.ui_quick_state(cx.snap.gen, target).ok();
        let now = Instant::now();
        if q != last {
            last = q;
            stable_since = now;
        } else if now - stable_since >= SETTLE_STABLE && now - start >= SETTLE_MIN {
            return last;
        }
        if now - start >= SETTLE_MAX {
            return last;
        }
    }
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The same element in a newer snapshot: same role and automation id, or same name nearby.
pub fn counterpart<'a>(e: &UiElement, s: &'a UiSnapshot) -> Option<&'a UiElement> {
    let (cx, cy) = e.rect.center();
    s.elements
        .iter()
        .filter(|o| o.role == e.role && ((!e.automation_id.is_empty() && o.automation_id == e.automation_id) || o.name == e.name))
        .map(|o| {
            let (ox, oy) = o.rect.center();
            ((((ox - cx).pow(2) + (oy - cy).pow(2)) as f64).sqrt(), o)
        })
        .filter(|(d, _)| *d < 60.0)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, o)| o)
}

/// Did the action do what it should? `typed` = text that must now be in the target's value.
pub fn verified(a: &Action, changed: bool, before: Option<&QuickState>, after: Option<&QuickState>) -> bool {
    match a {
        Action::TypeText { text, submit, .. } => {
            let value = after.map(|q| norm(&q.target)).unwrap_or_default();
            let want = norm(text);
            let value_readable = after.is_some_and(|q| !q.target.is_empty()) || before.is_some_and(|q| !q.target.is_empty());
            (!want.is_empty() && value.contains(&want)) || (*submit && changed) || (!value_readable && changed)
        }
        Action::Click { .. } | Action::Scroll { .. } => changed,
        _ => true,
    }
}

pub fn perform(cx: &Cx, d: &Decision, confirmed: bool) -> Result<Performed, String> {
    match &d.action {
        Action::ReadText(id) => {
            let e = cx.snap.get(*id).ok_or("No such element.")?;
            let text = if e.password { String::new() } else { e.value.clone().filter(|v| !v.is_empty()).unwrap_or_else(|| e.name.clone()) };
            return Ok(Performed { verified: true, detail: String::new(), read: Some(text) });
        }
        Action::Wait(ms) => {
            killswitch::sleep(Duration::from_millis(*ms as u64));
            return Ok(Performed { verified: true, detail: String::new(), read: None });
        }
        _ => {}
    }
    if let Some((c, tab)) = cx.vio {
        return perform_vio(cx, c, tab, d);
    }
    let target = d.action.target_id();
    let before = cx.platform.ui_quick_state(cx.snap.gen, target).ok();
    let first = attempt(cx, &d.action, Strategy::Primary)?;
    let after = settle(cx, target);
    let mut changed = before.is_some() && after.is_some() && before != after;
    let mut fresh: Option<UiSnapshot> = None;
    if !changed {
        // The cheap state didn't move: compare the whole tree.
        if let Ok(s1) = cx.platform.ui_snapshot(&SnapshotOptions::default()) {
            changed = s1.fingerprint() != cx.snap.fingerprint() || s1.window.id != cx.snap.window.id;
            fresh = Some(s1);
        }
    }
    let window_note = |q: Option<&QuickState>| match (before.as_ref(), q) {
        (Some(b), Some(a)) if a.window_id != b.window_id || a.title != b.title => format!(" (window → \u{ab}{}\u{bb})", clip(&a.title, 60)),
        _ => String::new(),
    };
    let ok = verified(&d.action, changed, before.as_ref(), after.as_ref());
    eprintln!("[soul][agent] computer exec {} via={} changed={changed} verified={ok}", d.name, first.used);
    if ok {
        return Ok(Performed { verified: true, detail: window_note(after.as_ref()), read: None });
    }
    // Retry once with the other strategy — only when nothing changed at all, and never for an
    // action the user confirmed or the model flagged (it may have worked invisibly).
    if !changed && first.has_alternate && !confirmed && !d.risky {
        if let (Some(s1), Some(old)) = (fresh.as_ref(), target.and_then(|id| cx.snap.get(id))) {
            if let Some(new_el) = counterpart(old, s1) {
                let mut retry = d.action.clone();
                retarget(&mut retry, new_el.id);
                let cx2 = Cx { snap: s1, ..*cx };
                let before2 = cx.platform.ui_quick_state(s1.gen, Some(new_el.id)).ok();
                let second = attempt(&cx2, &retry, Strategy::Alternate)?;
                let after2 = settle(&cx2, Some(new_el.id));
                let mut changed2 = before2.is_some() && after2.is_some() && before2 != after2;
                if !changed2 {
                    if let Ok(s2) = cx.platform.ui_snapshot(&SnapshotOptions::default()) {
                        changed2 = s2.fingerprint() != s1.fingerprint() || s2.window.id != s1.window.id;
                    }
                }
                let ok2 = verified(&retry, changed2, before2.as_ref(), after2.as_ref());
                eprintln!("[soul][agent] computer retry {} via={} verified={ok2}", d.name, second.used);
                return Ok(Performed {
                    verified: ok2,
                    detail: if ok2 {
                        format!(" (retried via {}){}", second.used, window_note(after2.as_ref()))
                    } else {
                        " — nothing changed, also after a retry".into()
                    },
                    read: None,
                });
            }
        }
    }
    Ok(Performed {
        verified: false,
        detail: if changed { " — the screen changed but not as expected".into() } else { " — nothing changed".into() },
        read: None,
    })
}

fn retarget(a: &mut Action, id: u32) {
    match a {
        Action::Click { target, .. } => *target = Target::Id(id),
        Action::TypeText { target, .. } | Action::Scroll { target, .. } => *target = Some(Target::Id(id)),
        _ => {}
    }
}

/// Spoken / Island line after a final action: „Kliknąłem „Zapisz”.”
pub fn done_text(a: &Action, snap: &UiSnapshot, lang: Lang) -> String {
    let t = |t: &Target| super::action::target_label(t, snap);
    match a {
        Action::Click { target, .. } => tr(lang, format!("Kliknąłem {}.", t(target)), format!("Clicked {}.", t(target))),
        Action::TypeText { text, submit, .. } => {
            let s = clip(text, 40);
            if *submit {
                tr(lang, format!("Wpisałem \u{201e}{s}\u{201d} i zatwierdziłem."), format!("Typed \u{201e}{s}\u{201d} and pressed Enter."))
            } else {
                tr(lang, format!("Wpisałem \u{201e}{s}\u{201d}."), format!("Typed \u{201e}{s}\u{201d}."))
            }
        }
        Action::KeyCombo(c) => tr(lang, format!("Nacisnąłem {}.", c.label()), format!("Pressed {}.", c.label())),
        Action::Scroll { dir, .. } => {
            let (pl, en) = match dir {
                crate::platform::ui::ScrollDir::Up => ("w górę", "up"),
                crate::platform::ui::ScrollDir::Down => ("w dół", "down"),
                crate::platform::ui::ScrollDir::Left => ("w lewo", "left"),
                crate::platform::ui::ScrollDir::Right => ("w prawo", "right"),
            };
            tr(lang, format!("Przewinąłem {pl}."), format!("Scrolled {en}."))
        }
        Action::Drag { .. } => tr(lang, "Przeciągnąłem.", "Dragged.").to_string(),
        Action::OpenApp(n) => tr(lang, format!("Otworzyłem {n}."), format!("Opened {n}.")),
        Action::OpenSettings(p) => tr(lang, format!("Otworzyłem ustawienia: {p}."), format!("Opened settings: {p}.")),
        Action::FocusApp(n) => tr(lang, format!("Przełączyłem na {n}."), format!("Switched to {n}.")),
        Action::OpenUrl(u) | Action::Browser(BrowserOp::Open { url: Some(u), .. } | BrowserOp::Navigate(u)) => {
            tr(lang, format!("Otworzyłem {}.", clip(u, 40)), format!("Opened {}.", clip(u, 40)))
        }
        _ => tr(lang, "Gotowe.", "Done.").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::ui::{finalize, WindowInfo, MAX_ELEMENTS};

    fn q(title: &str, target: &str) -> QuickState {
        QuickState { window_id: 1, title: title.into(), focused: "edit Szukaj".into(), target: target.into() }
    }

    #[test]
    fn typing_is_verified_by_the_value() {
        let a = Action::TypeText { target: Some(Target::Id(1)), text: "FiveM  serwery".into(), clear: true, submit: false };
        assert!(verified(&a, true, Some(&q("Google", "")), Some(&q("Google", "fivem serwery"))));
        // Value didn't take the text although the screen changed.
        assert!(!verified(&a, true, Some(&q("Google", "x")), Some(&q("Google", "x y"))));
        // Submitted search: the box may be empty afterwards, the page changed.
        let s = Action::TypeText { target: Some(Target::Id(1)), text: "FiveM".into(), clear: true, submit: true };
        assert!(verified(&s, true, Some(&q("Google", "")), Some(&q("FiveM - Szukaj", ""))));
        // A field whose value can't be read (document without ValuePattern): any change counts.
        assert!(verified(&a, true, Some(&q("N", "")), Some(&q("N", ""))));
        assert!(!verified(&a, false, Some(&q("N", "")), Some(&q("N", ""))));
    }

    #[test]
    fn clicks_and_scrolls_need_a_change() {
        let c = Action::Click { target: Target::Id(1), button: MouseButton::Left, count: 1 };
        assert!(verified(&c, true, None, None));
        assert!(!verified(&c, false, None, None));
        assert!(verified(&Action::KeyCombo(Combo::parse("ctrl+s").unwrap()), false, None, None));
    }

    #[test]
    fn counterpart_survives_reordering() {
        let w = WindowInfo { rect: Rect::new(0, 0, 1000, 800), ..Default::default() };
        let mk = |els: Vec<UiElement>| {
            let (elements, truncated) = finalize(els, &w.rect, MAX_ELEMENTS);
            UiSnapshot { gen: 1, window: w.clone(), elements, source: crate::platform::ui::Source::Uia, took_ms: 0, truncated }
        };
        let old = mk(vec![UiElement::new(Role::Button, "Zapisz", Rect::new(400, 700, 80, 24))]);
        let new = mk(vec![
            UiElement::new(Role::Text, "Nowy baner", Rect::new(0, 0, 300, 20)),
            UiElement::new(Role::Button, "Zapisz", Rect::new(405, 702, 80, 24)),
            UiElement::new(Role::Button, "Zapisz", Rect::new(900, 10, 80, 24)),
        ]);
        let c = counterpart(old.get(1).unwrap(), &new).unwrap();
        assert_eq!(c.rect.x, 405);
        let far = mk(vec![UiElement::new(Role::Button, "Zapisz", Rect::new(10, 10, 80, 24))]);
        assert!(counterpart(old.get(1).unwrap(), &far).is_none());
    }

    #[test]
    fn typing_strategy_order() {
        let mut edit = UiElement::new(Role::Edit, "Nazwa pliku", Rect::new(0, 0, 10, 10));
        edit.caps.set_value = true;
        assert_eq!(type_modes(Some(&edit), false, true), [TypeMode::SetValue, TypeMode::Paste, TypeMode::Keys]);
        let doc = UiElement::new(Role::Document, "Edytor", Rect::new(0, 0, 10, 10));
        assert_eq!(type_modes(Some(&doc), false, false), [TypeMode::Paste, TypeMode::Keys]);
        let mut web = UiElement::new(Role::Edit, "Szukaj", Rect::new(0, 0, 10, 10));
        web.web = true;
        web.caps.set_value = true;
        assert_eq!(type_modes(Some(&web), false, true), [TypeMode::Keys, TypeMode::Paste]);
        assert_eq!(type_modes(None, true, false), [TypeMode::Keys, TypeMode::Paste]);
    }

    #[test]
    fn set_value_only_for_single_line_native_fields() {
        let mut e = UiElement::new(Role::Edit, "Nazwa pliku", Rect::new(0, 0, 10, 10));
        e.caps.set_value = true;
        assert!(can_set_value(&e, false));
        e.value = Some("stara".into());
        assert!(!can_set_value(&e, false));
        assert!(can_set_value(&e, true));
        e.web = true;
        assert!(!can_set_value(&e, true));
        let mut doc = UiElement::new(Role::Document, "Edytor", Rect::new(0, 0, 10, 10));
        doc.caps.set_value = true;
        assert!(!can_set_value(&doc, true), "never SetValue a whole document");
    }
}
