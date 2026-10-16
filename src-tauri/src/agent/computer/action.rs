//! One agent step: the action the model (or a local plan) chose, parsed and validated from a
//! function call, plus the short Island text describing it („Klikam „Zapisz”…”).

use crate::i18n::{tr, Lang};
use crate::platform::input::{Combo, MouseButton};
use crate::platform::ui::{clip, ScrollDir, UiSnapshot};
use serde_json::Value;

/// Where an action points: an element id of the current snapshot, or 0–1000 coordinates of the
/// screenshot region (vision mode).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Id(u32),
    Point { x: f64, y: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Click { target: Target, button: MouseButton, count: u8 },
    TypeText { target: Option<Target>, text: String, clear: bool, submit: bool },
    KeyCombo(Combo),
    Scroll { target: Option<Target>, dir: ScrollDir, amount: u8 },
    Drag { from: Target, to: Target },
    OpenApp(String),
    OpenSettings(String),
    FocusApp(String),
    OpenUrl(String),
    Wait(u32),
    ReadText(u32),
    Done(String),
    AskUser(String),
}

/// A parsed function call.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub action: Action,
    /// The model flagged it (send / submit / buy / delete / settings…).
    pub risky: bool,
    /// This action completes the task (no separate `done` call).
    pub final_step: bool,
    /// Attach a screenshot to the next observation.
    pub look: bool,
    /// Function name as called (logs / history).
    pub name: String,
}

impl Action {
    /// Changes something on screen (vs. reading / waiting / finishing).
    pub fn acts(&self) -> bool {
        !matches!(self, Self::Wait(_) | Self::ReadText(_) | Self::Done(_) | Self::AskUser(_))
    }

    pub fn target_id(&self) -> Option<u32> {
        match self {
            Self::Click { target: Target::Id(id), .. }
            | Self::TypeText { target: Some(Target::Id(id)), .. }
            | Self::Scroll { target: Some(Target::Id(id)), .. }
            | Self::Drag { from: Target::Id(id), .. } => Some(*id),
            Self::ReadText(id) => Some(*id),
            _ => None,
        }
    }

    pub fn targets(&self) -> Vec<Target> {
        match self {
            Self::Click { target, .. } => vec![*target],
            Self::TypeText { target: Some(t), .. } | Self::Scroll { target: Some(t), .. } => vec![*t],
            Self::Drag { from, to } => vec![*from, *to],
            Self::ReadText(id) => vec![Target::Id(*id)],
            _ => Vec::new(),
        }
    }
}

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn arg_bool(args: &Value, key: &str) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1"),
        _ => false,
    }
}

fn arg_num(args: &Value, key: &str) -> Option<f64> {
    match args.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `id` (preferred) or `x` + `y`, optionally with a prefix ("from_", "to_").
fn target(args: &Value, prefix: &str) -> Option<Target> {
    if let Some(id) = arg_num(args, &format!("{prefix}id")).filter(|n| *n >= 1.0) {
        return Some(Target::Id(id.round() as u32));
    }
    let x = arg_num(args, &format!("{prefix}x"))?;
    let y = arg_num(args, &format!("{prefix}y"))?;
    (x.is_finite() && y.is_finite()).then_some(Target::Point { x, y })
}

/// Function call → decision. Errors are fed back to the model as the step result.
pub fn parse(name: &str, args: &Value) -> Result<Decision, String> {
    let need = |k: &str| arg_str(args, k).ok_or_else(|| format!("{name}: missing '{k}'"));
    let action = match name {
        "click" | "double_click" | "right_click" => Action::Click {
            target: target(args, "").ok_or_else(|| format!("{name}: give an element id (or x/y in vision mode)"))?,
            button: if name == "right_click" { MouseButton::Right } else { MouseButton::Left },
            count: if name == "double_click" { 2 } else { 1 },
        },
        "type_text" => {
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|t| !t.is_empty())
                .ok_or("type_text: missing 'text'")?;
            if text.chars().count() > 2000 {
                return Err("type_text: text longer than 2000 characters".into());
            }
            Action::TypeText {
                target: target(args, ""),
                text,
                clear: arg_bool(args, "clear"),
                submit: arg_bool(args, "submit"),
            }
        }
        "key_combo" => Action::KeyCombo(Combo::parse(&need("keys")?)?),
        "scroll" => Action::Scroll {
            target: target(args, ""),
            dir: ScrollDir::parse(&need("direction")?).ok_or("scroll: direction must be up/down/left/right")?,
            amount: arg_num(args, "amount").map(|a| a.round().clamp(1.0, 10.0) as u8).unwrap_or(5),
        },
        "drag" => Action::Drag {
            from: target(args, "from_").ok_or("drag: missing from_id or from_x/from_y")?,
            to: target(args, "to_").ok_or("drag: missing to_id or to_x/to_y")?,
        },
        "open_app" => Action::OpenApp(need("name")?),
        "open_settings" => Action::OpenSettings(need("page")?),
        "focus_app" => Action::FocusApp(need("name")?),
        "open_url" => Action::OpenUrl(need("url")?),
        "wait" => Action::Wait(arg_num(args, "ms").map(|m| m.clamp(100.0, 5000.0) as u32).unwrap_or(1000)),
        "read_text" => Action::ReadText(
            arg_num(args, "id").filter(|n| *n >= 1.0).map(|n| n.round() as u32).ok_or("read_text: missing 'id'")?,
        ),
        "done" => Action::Done(arg_str(args, "summary").unwrap_or_default()),
        "ask_user" => Action::AskUser(need("question")?),
        other => return Err(format!("Unknown function '{other}'.")),
    };
    Ok(Decision {
        action,
        risky: arg_bool(args, "risky"),
        final_step: arg_bool(args, "final"),
        look: arg_bool(args, "look"),
        name: name.to_string(),
    })
}

/// „„Zapisz””, or „punkt 412,88” for coordinates.
pub fn target_label(t: &Target, snap: &UiSnapshot) -> String {
    match t {
        Target::Id(id) => match snap.get(*id) {
            Some(e) => format!("\u{201e}{}\u{201d}", clip(&e.display_name(), 40)),
            None => format!("#{id}"),
        },
        Target::Point { x, y } => format!("({:.0}, {:.0})", x, y),
    }
}

/// Island step text, present tense: „Klikam „Zapisz”…”, “Typing „FiveM”…”.
pub fn describe(a: &Action, snap: &UiSnapshot, lang: Lang) -> String {
    let t = |t: &Target| target_label(t, snap);
    match a {
        Action::Click { target, button: MouseButton::Right, .. } => {
            tr(lang, format!("Prawy klik: {}…", t(target)), format!("Right-clicking {}…", t(target)))
        }
        Action::Click { target, count: 2, .. } => {
            tr(lang, format!("Dwuklik: {}…", t(target)), format!("Double-clicking {}…", t(target)))
        }
        Action::Click { target, .. } => tr(lang, format!("Klikam {}…", t(target)), format!("Clicking {}…", t(target))),
        Action::TypeText { text, .. } => {
            let s = clip(text, 30);
            tr(lang, format!("Wpisuję \u{201e}{s}\u{201d}…"), format!("Typing \u{201e}{s}\u{201d}…"))
        }
        Action::KeyCombo(c) => tr(lang, format!("Naciskam {}…", c.label()), format!("Pressing {}…", c.label())),
        Action::Scroll { dir, .. } => {
            let (pl, en) = match dir {
                ScrollDir::Up => ("w górę", "up"),
                ScrollDir::Down => ("w dół", "down"),
                ScrollDir::Left => ("w lewo", "left"),
                ScrollDir::Right => ("w prawo", "right"),
            };
            tr(lang, format!("Przewijam {pl}…"), format!("Scrolling {en}…"))
        }
        Action::Drag { from, to } => tr(
            lang,
            format!("Przeciągam {} → {}…", t(from), t(to)),
            format!("Dragging {} → {}…", t(from), t(to)),
        ),
        Action::OpenApp(n) => tr(lang, format!("Otwieram {n}…"), format!("Opening {n}…")),
        Action::OpenSettings(p) => tr(lang, format!("Otwieram ustawienia: {p}…"), format!("Opening settings: {p}…")),
        Action::FocusApp(n) => tr(lang, format!("Przełączam na {n}…"), format!("Switching to {n}…")),
        Action::OpenUrl(u) => {
            let u = clip(u, 40);
            tr(lang, format!("Otwieram {u}…"), format!("Opening {u}…"))
        }
        Action::Wait(_) => tr(lang, "Czekam…", "Waiting…").to_string(),
        Action::ReadText(id) => {
            let l = t(&Target::Id(*id));
            tr(lang, format!("Czytam {l}…"), format!("Reading {l}…"))
        }
        Action::Done(_) => tr(lang, "Gotowe", "Done").to_string(),
        Action::AskUser(q) => clip(q, 80),
    }
}

/// Short past-tense line for the step history sent back to the model.
pub fn history_line(d: &Decision, snap: &UiSnapshot) -> String {
    let t = |t: &Target| match t {
        Target::Id(id) => match snap.get(*id) {
            Some(e) => format!("#{id} \"{}\"", clip(&e.display_name(), 40)),
            None => format!("#{id}"),
        },
        Target::Point { x, y } => format!("({x:.0},{y:.0})"),
    };
    match &d.action {
        Action::Click { target, .. } => format!("{} {}", d.name, t(target)),
        Action::TypeText { target, text, clear, submit } => format!(
            "type_text{} \"{}\"{}{}",
            target.as_ref().map(|x| format!(" {}", t(x))).unwrap_or_default(),
            clip(text, 60),
            if *clear { " clear" } else { "" },
            if *submit { " +enter" } else { "" }
        ),
        Action::KeyCombo(c) => format!("key_combo {}", c.label()),
        Action::Scroll { target, dir, amount } => format!(
            "scroll {dir:?} x{amount}{}",
            target.as_ref().map(|x| format!(" {}", t(x))).unwrap_or_default()
        )
        .to_lowercase(),
        Action::Drag { from, to } => format!("drag {} → {}", t(from), t(to)),
        Action::OpenApp(n) => format!("open_app \"{n}\""),
        Action::OpenSettings(p) => format!("open_settings {p}"),
        Action::FocusApp(n) => format!("focus_app \"{n}\""),
        Action::OpenUrl(u) => format!("open_url {}", clip(u, 80)),
        Action::Wait(ms) => format!("wait {ms} ms"),
        Action::ReadText(id) => format!("read_text {}", t(&Target::Id(*id))),
        Action::Done(s) => format!("done \"{}\"", clip(s, 80)),
        Action::AskUser(q) => format!("ask_user \"{}\"", clip(q, 80)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_every_function() {
        let d = parse("click", &json!({ "id": 12, "risky": false, "final": true })).unwrap();
        assert_eq!(d.action, Action::Click { target: Target::Id(12), button: MouseButton::Left, count: 1 });
        assert!(d.final_step && !d.risky);
        let d = parse("double_click", &json!({ "x": 412.4, "y": "88" })).unwrap();
        assert_eq!(d.action, Action::Click { target: Target::Point { x: 412.4, y: 88.0 }, button: MouseButton::Left, count: 2 });
        assert!(matches!(parse("right_click", &json!({ "id": 3 })).unwrap().action, Action::Click { button: MouseButton::Right, .. }));
        let d = parse("type_text", &json!({ "id": 5, "text": "FiveM serwery", "submit": true, "clear": "true" })).unwrap();
        assert_eq!(
            d.action,
            Action::TypeText { target: Some(Target::Id(5)), text: "FiveM serwery".into(), clear: true, submit: true }
        );
        assert_eq!(parse("key_combo", &json!({ "keys": "Ctrl+S" })).unwrap().action, Action::KeyCombo(Combo::parse("ctrl+s").unwrap()));
        assert_eq!(
            parse("scroll", &json!({ "direction": "down", "amount": 40 })).unwrap().action,
            Action::Scroll { target: None, dir: ScrollDir::Down, amount: 10 }
        );
        assert_eq!(
            parse("drag", &json!({ "from_id": 2, "to_x": 500, "to_y": 900 })).unwrap().action,
            Action::Drag { from: Target::Id(2), to: Target::Point { x: 500.0, y: 900.0 } }
        );
        assert_eq!(parse("wait", &json!({ "ms": 99999 })).unwrap().action, Action::Wait(5000));
        assert_eq!(parse("read_text", &json!({ "id": 7 })).unwrap().action, Action::ReadText(7));
        assert_eq!(parse("done", &json!({ "summary": "Zapisano" })).unwrap().action, Action::Done("Zapisano".into()));
        assert_eq!(parse("ask_user", &json!({ "question": "Który Silver?" })).unwrap().action, Action::AskUser("Który Silver?".into()));
        assert_eq!(parse("open_settings", &json!({ "page": "sound" })).unwrap().action, Action::OpenSettings("sound".into()));
    }

    #[test]
    fn rejects_bad_calls() {
        assert!(parse("click", &json!({})).is_err());
        assert!(parse("click", &json!({ "id": 0 })).is_err());
        assert!(parse("type_text", &json!({ "id": 1 })).is_err());
        assert!(parse("type_text", &json!({ "text": "x".repeat(2001) })).is_err());
        assert!(parse("key_combo", &json!({ "keys": "ctrl+ż" })).is_err());
        assert!(parse("scroll", &json!({ "direction": "sideways" })).is_err());
        assert!(parse("run_shell", &json!({ "cmd": "rm -rf" })).is_err());
        assert!(parse("ask_user", &json!({ "question": "  " })).is_err());
    }

    #[test]
    fn acting_vs_reading() {
        assert!(parse("click", &json!({ "id": 1 })).unwrap().action.acts());
        assert!(!parse("read_text", &json!({ "id": 1 })).unwrap().action.acts());
        assert!(!parse("done", &json!({})).unwrap().action.acts());
        assert_eq!(parse("type_text", &json!({ "id": 4, "text": "a" })).unwrap().action.target_id(), Some(4));
    }
}
