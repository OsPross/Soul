//! Synthetic input for the computer agent, planned portably and executed step by step through
//! `Platform::send_input_step`. Typing never depends on the keyboard layout: text becomes UTF-16
//! code units sent as Unicode key events (Windows `KEYEVENTF_UNICODE`), so Polish AltGr layouts
//! and ą/ć/ę/ł/ń/ó/ś/ź/ż (and emoji, as surrogate pairs) type correctly. [`execute`] checks the
//! abort flag between steps and always releases every key / button it pressed.

use super::coords::Point;
use super::ui::ScrollDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Layout-independent keys for combos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Ctrl,
    Shift,
    Alt,
    Win,
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    Space,
    /// F1–F24.
    F(u8),
    /// A–Z (uppercase) or 0–9: virtual-key codes, the same on every Latin layout.
    Char(char),
    /// Context-menu key.
    Menu,
}

impl Key {
    pub fn is_modifier(self) -> bool {
        matches!(self, Self::Ctrl | Self::Shift | Self::Alt | Self::Win)
    }

    pub fn name(self) -> String {
        match self {
            Self::Ctrl => "ctrl".into(),
            Self::Shift => "shift".into(),
            Self::Alt => "alt".into(),
            Self::Win => "win".into(),
            Self::Enter => "enter".into(),
            Self::Tab => "tab".into(),
            Self::Escape => "esc".into(),
            Self::Backspace => "backspace".into(),
            Self::Delete => "delete".into(),
            Self::Insert => "insert".into(),
            Self::Home => "home".into(),
            Self::End => "end".into(),
            Self::PageUp => "pageup".into(),
            Self::PageDown => "pagedown".into(),
            Self::Up => "up".into(),
            Self::Down => "down".into(),
            Self::Left => "left".into(),
            Self::Right => "right".into(),
            Self::Space => "space".into(),
            Self::F(n) => format!("f{n}"),
            Self::Char(c) => c.to_ascii_lowercase().to_string(),
            Self::Menu => "menu".into(),
        }
    }

    fn parse(token: &str) -> Option<Self> {
        let t = token.trim().to_lowercase();
        Some(match t.as_str() {
            "ctrl" | "control" | "ctl" | "strg" => Self::Ctrl,
            "shift" => Self::Shift,
            "alt" | "option" => Self::Alt,
            "win" | "windows" | "super" | "meta" | "cmd" | "command" => Self::Win,
            "enter" | "return" | "ret" => Self::Enter,
            "tab" | "tabulator" => Self::Tab,
            "esc" | "escape" => Self::Escape,
            "backspace" | "bksp" => Self::Backspace,
            "delete" | "del" | "usun" | "usuń" => Self::Delete,
            "insert" | "ins" => Self::Insert,
            "home" => Self::Home,
            "end" => Self::End,
            "pageup" | "pgup" | "page_up" => Self::PageUp,
            "pagedown" | "pgdn" | "page_down" => Self::PageDown,
            "up" | "arrowup" | "gora" | "góra" => Self::Up,
            "down" | "arrowdown" | "dol" | "dół" => Self::Down,
            "left" | "arrowleft" | "lewo" => Self::Left,
            "right" | "arrowright" | "prawo" => Self::Right,
            "space" | "spacja" | "spacebar" => Self::Space,
            "menu" | "apps" | "contextmenu" => Self::Menu,
            f if f.len() >= 2 && f.len() <= 3 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
                let n: u8 = f[1..].parse().ok()?;
                if !(1..=24).contains(&n) {
                    return None;
                }
                Self::F(n)
            }
            s if s.chars().count() == 1 => {
                let c = s.chars().next()?;
                if c.is_ascii_alphanumeric() {
                    Self::Char(c.to_ascii_uppercase())
                } else {
                    return None;
                }
            }
            _ => return None,
        })
    }
}

/// Modifiers + one key: "ctrl+s", "Ctrl + Shift + T", "alt+f4", "enter".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combo {
    pub modifiers: Vec<Key>,
    pub key: Key,
}

impl Combo {
    pub fn parse(s: &str) -> Result<Self, String> {
        let raw = s.trim();
        if raw.is_empty() {
            return Err("Empty key combination.".into());
        }
        // "+" itself as the key ("ctrl++") is not supported; split on '+' and whitespace-only parts.
        let tokens: Vec<&str> = raw.split('+').map(str::trim).filter(|t| !t.is_empty()).collect();
        if tokens.is_empty() || tokens.len() > 4 {
            return Err(format!("Unsupported key combination '{raw}'."));
        }
        let mut keys = Vec::with_capacity(tokens.len());
        for t in &tokens {
            keys.push(Key::parse(t).ok_or_else(|| format!("Unknown key '{t}' in '{raw}'."))?);
        }
        let key = *keys.last().expect("non-empty");
        let mut modifiers: Vec<Key> = keys[..keys.len() - 1].to_vec();
        if modifiers.iter().any(|k| !k.is_modifier()) {
            return Err(format!("'{raw}': only ctrl/shift/alt/win may precede the key."));
        }
        modifiers.sort_by_key(|k| match k {
            Key::Ctrl => 0,
            Key::Alt => 1,
            Key::Shift => 2,
            _ => 3,
        });
        modifiers.dedup();
        Ok(Self { modifiers, key })
    }

    /// Canonical lowercase form: "ctrl+shift+t".
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = self.modifiers.iter().map(|k| k.name()).collect();
        parts.push(self.key.name());
        parts.join("+")
    }

    pub fn has(&self, k: Key) -> bool {
        self.modifiers.contains(&k) || self.key == k
    }
}

/// One low-level input event. Backends implement exactly these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputStep {
    /// Absolute move to a physical screen pixel.
    MoveTo(Point),
    ButtonDown(MouseButton),
    ButtonUp(MouseButton),
    /// Wheel notches (+ = up / right, − = down / left).
    Wheel { horizontal: bool, notches: i32 },
    KeyDown(Key),
    KeyUp(Key),
    /// One UTF-16 code unit, pressed and released as a Unicode key event.
    Unicode(u16),
    Pause(u32),
}

/// Typed character → what to send. `\n` is Enter, `\t` is Tab, `\r` is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeUnit {
    Unicode(u16),
    Key(Key),
}

pub fn type_units(text: &str) -> Vec<TypeUnit> {
    let mut out = Vec::with_capacity(text.len());
    let mut buf = [0u16; 2];
    for c in text.chars() {
        match c {
            '\r' => {}
            '\n' => out.push(TypeUnit::Key(Key::Enter)),
            '\t' => out.push(TypeUnit::Key(Key::Tab)),
            c => out.extend(c.encode_utf16(&mut buf).iter().map(|u| TypeUnit::Unicode(*u))),
        }
    }
    out
}

/// Units per burst; a short pause between bursts keeps slow apps from dropping characters.
const TYPE_BURST: usize = 16;
const TYPE_PAUSE_MS: u32 = 8;

pub fn plan_type(text: &str) -> Vec<InputStep> {
    let mut steps = Vec::new();
    for (i, u) in type_units(text).into_iter().enumerate() {
        if i > 0 && i % TYPE_BURST == 0 {
            steps.push(InputStep::Pause(TYPE_PAUSE_MS));
        }
        match u {
            TypeUnit::Unicode(cu) => steps.push(InputStep::Unicode(cu)),
            TypeUnit::Key(k) => {
                steps.push(InputStep::KeyDown(k));
                steps.push(InputStep::KeyUp(k));
            }
        }
    }
    steps
}

pub fn plan_combo(combo: &Combo) -> Vec<InputStep> {
    let mut steps: Vec<InputStep> = combo.modifiers.iter().map(|k| InputStep::KeyDown(*k)).collect();
    steps.push(InputStep::KeyDown(combo.key));
    steps.push(InputStep::Pause(15));
    steps.push(InputStep::KeyUp(combo.key));
    steps.extend(combo.modifiers.iter().rev().map(|k| InputStep::KeyUp(*k)));
    steps
}

pub fn plan_click(p: Point, button: MouseButton, count: u8) -> Vec<InputStep> {
    let mut steps = vec![InputStep::MoveTo(p), InputStep::Pause(30)];
    for i in 0..count.clamp(1, 3) {
        if i > 0 {
            steps.push(InputStep::Pause(60));
        }
        steps.push(InputStep::ButtonDown(button));
        steps.push(InputStep::Pause(20));
        steps.push(InputStep::ButtonUp(button));
    }
    steps
}

/// `amount` 1–10 wheel notches at `at` (or wherever the cursor is).
pub fn plan_scroll(at: Option<Point>, dir: ScrollDir, amount: u8) -> Vec<InputStep> {
    let mut steps = Vec::new();
    if let Some(p) = at {
        steps.push(InputStep::MoveTo(p));
        steps.push(InputStep::Pause(30));
    }
    let (horizontal, sign) = match dir {
        ScrollDir::Up => (false, 1),
        ScrollDir::Down => (false, -1),
        ScrollDir::Left => (true, -1),
        ScrollDir::Right => (true, 1),
    };
    for _ in 0..amount.clamp(1, 10) {
        steps.push(InputStep::Wheel { horizontal, notches: sign });
        steps.push(InputStep::Pause(25));
    }
    steps
}

/// Press at `from`, glide to `to` in 12 steps, release.
pub fn plan_drag(from: Point, to: Point) -> Vec<InputStep> {
    let mut steps = vec![InputStep::MoveTo(from), InputStep::Pause(40), InputStep::ButtonDown(MouseButton::Left), InputStep::Pause(60)];
    const N: i32 = 12;
    for i in 1..=N {
        steps.push(InputStep::MoveTo(Point::new(
            from.x + (to.x - from.x) * i / N,
            from.y + (to.y - from.y) * i / N,
        )));
        steps.push(InputStep::Pause(16));
    }
    steps.push(InputStep::Pause(60));
    steps.push(InputStep::ButtonUp(MouseButton::Left));
    steps
}

/// Run `steps` through `send`, checking `aborted` before each one. Whatever ends the run
/// (done, abort, error), every key and button still held is released (reverse order).
/// Returns `Err("aborted")` when the abort flag stopped it.
pub fn execute(
    steps: &[InputStep],
    aborted: &dyn Fn() -> bool,
    send: &mut dyn FnMut(&InputStep) -> Result<(), String>,
) -> Result<(), String> {
    let mut held_keys: Vec<Key> = Vec::new();
    let mut held_buttons: Vec<MouseButton> = Vec::new();
    let mut result = Ok(());
    for step in steps {
        if aborted() {
            result = Err(ABORTED.into());
            break;
        }
        if let InputStep::Pause(ms) = step {
            // Sleep in slices so an abort is noticed mid-pause.
            let mut left = *ms;
            while left > 0 {
                let d = left.min(10);
                std::thread::sleep(std::time::Duration::from_millis(d as u64));
                left -= d;
            }
            continue;
        }
        if let Err(e) = send(step) {
            result = Err(e);
            break;
        }
        match *step {
            InputStep::KeyDown(k) => held_keys.push(k),
            InputStep::KeyUp(k) => held_keys.retain(|h| *h != k),
            InputStep::ButtonDown(b) => held_buttons.push(b),
            InputStep::ButtonUp(b) => held_buttons.retain(|h| *h != b),
            _ => {}
        }
    }
    for b in held_buttons.into_iter().rev() {
        let _ = send(&InputStep::ButtonUp(b));
    }
    for k in held_keys.into_iter().rev() {
        let _ = send(&InputStep::KeyUp(k));
    }
    result
}

pub const ABORTED: &str = "aborted";

/// Kill-switch inputs, read every ~25 ms while an agent task runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputProbe {
    /// Escape is physically held right now.
    pub esc_down: bool,
    /// Cursor in physical screen pixels.
    pub cursor: Option<Point>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polish_text_is_unicode_units_not_layout_keys() {
        let units = type_units("Zażółć gęślą jaźń");
        assert_eq!(units.len(), "Zażółć gęślą jaźń".chars().count());
        let expected: Vec<u16> = "Zażółć gęślą jaźń".encode_utf16().collect();
        let got: Vec<u16> = units
            .iter()
            .map(|u| match u {
                TypeUnit::Unicode(c) => *c,
                TypeUnit::Key(k) => panic!("unexpected key {k:?}"),
            })
            .collect();
        assert_eq!(got, expected);
        // Every Polish letter, both cases, round-trips.
        let pl = "ąćęłńóśźżĄĆĘŁŃÓŚŹŻ";
        let back: String = char::decode_utf16(type_units(pl).iter().map(|u| match u {
            TypeUnit::Unicode(c) => *c,
            TypeUnit::Key(_) => 0,
        }))
        .map(|r| r.unwrap())
        .collect();
        assert_eq!(back, pl);
        // „Będę za 10 minut” — a typical message, with Polish quotes.
        assert!(type_units("„Będę za 10 minut”").iter().all(|u| matches!(u, TypeUnit::Unicode(_))));
    }

    #[test]
    fn emoji_become_surrogate_pairs_and_newlines_are_enter() {
        let units = type_units("ok 👍\r\nnext\tx");
        let unicode: Vec<u16> = units
            .iter()
            .filter_map(|u| match u {
                TypeUnit::Unicode(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert!(unicode.contains(&0xD83D) && unicode.contains(&0xDC4D), "{unicode:x?}");
        assert!(units.contains(&TypeUnit::Key(Key::Enter)));
        assert!(units.contains(&TypeUnit::Key(Key::Tab)));
        assert_eq!(units.iter().filter(|u| **u == TypeUnit::Unicode(b'\r' as u16)).count(), 0);
    }

    #[test]
    fn typing_plan_paces_bursts() {
        let steps = plan_type(&"a".repeat(40));
        assert_eq!(steps.iter().filter(|s| matches!(s, InputStep::Unicode(_))).count(), 40);
        assert_eq!(steps.iter().filter(|s| matches!(s, InputStep::Pause(_))).count(), 2);
    }

    #[test]
    fn combos_parse_and_normalize() {
        let c = Combo::parse("Shift + Ctrl + t").unwrap();
        assert_eq!(c.label(), "ctrl+shift+t");
        assert_eq!(Combo::parse("ctrl+s").unwrap().key, Key::Char('S'));
        assert_eq!(Combo::parse("Alt+F4").unwrap().label(), "alt+f4");
        assert_eq!(Combo::parse("enter").unwrap().modifiers, vec![]);
        assert_eq!(Combo::parse("win+r").unwrap().label(), "win+r");
        assert_eq!(Combo::parse("ctrl+shift+esc").unwrap().label(), "ctrl+shift+esc");
        assert_eq!(Combo::parse("spacja").unwrap().key, Key::Space);
        assert!(Combo::parse("").is_err());
        assert!(Combo::parse("ctrl+ż").is_err());
        assert!(Combo::parse("s+ctrl").is_err());
        assert!(Combo::parse("f25").is_err());
        assert!(Combo::parse("ctrl+alt+shift+win+x").is_err());
    }

    #[test]
    fn combo_plan_releases_in_reverse() {
        let steps = plan_combo(&Combo::parse("ctrl+shift+t").unwrap());
        let keys: Vec<String> = steps
            .iter()
            .filter_map(|s| match s {
                InputStep::KeyDown(k) => Some(format!("+{}", k.name())),
                InputStep::KeyUp(k) => Some(format!("-{}", k.name())),
                _ => None,
            })
            .collect();
        assert_eq!(keys, ["+ctrl", "+shift", "+t", "-t", "-shift", "-ctrl"]);
    }

    #[test]
    fn click_scroll_drag_plans() {
        let p = Point::new(10, 20);
        let dbl = plan_click(p, MouseButton::Left, 2);
        assert_eq!(dbl.first(), Some(&InputStep::MoveTo(p)));
        assert_eq!(dbl.iter().filter(|s| **s == InputStep::ButtonDown(MouseButton::Left)).count(), 2);
        let sc = plan_scroll(Some(p), ScrollDir::Down, 3);
        assert_eq!(sc.iter().filter(|s| **s == InputStep::Wheel { horizontal: false, notches: -1 }).count(), 3);
        assert_eq!(plan_scroll(None, ScrollDir::Right, 50).iter().filter(|s| matches!(s, InputStep::Wheel { .. })).count(), 10);
        let dr = plan_drag(p, Point::new(130, 260));
        assert_eq!(dr.iter().rev().find(|s| matches!(s, InputStep::MoveTo(_))), Some(&InputStep::MoveTo(Point::new(130, 260))));
        assert_eq!(dr.last(), Some(&InputStep::ButtonUp(MouseButton::Left)));
    }

    #[test]
    fn execute_releases_everything_on_abort_and_error() {
        use std::cell::Cell;
        let plan = plan_combo(&Combo::parse("ctrl+shift+t").unwrap());
        // Abort right after both modifiers went down.
        let sent = std::cell::RefCell::new(Vec::new());
        let n = Cell::new(0);
        let r = execute(
            &plan,
            &|| n.get() >= 2,
            &mut |s| {
                n.set(n.get() + 1);
                sent.borrow_mut().push(*s);
                Ok(())
            },
        );
        assert_eq!(r, Err(ABORTED.to_string()));
        let sent = sent.into_inner();
        assert_eq!(
            sent,
            vec![InputStep::KeyDown(Key::Ctrl), InputStep::KeyDown(Key::Shift), InputStep::KeyUp(Key::Shift), InputStep::KeyUp(Key::Ctrl)]
        );
        // A failing step mid-drag still releases the mouse button.
        let drag = plan_drag(Point::new(0, 0), Point::new(100, 100));
        let mut log = Vec::new();
        let mut moves = 0;
        let r = execute(&drag, &|| false, &mut |s| {
            if matches!(s, InputStep::MoveTo(_)) {
                moves += 1;
                if moves == 5 {
                    return Err("boom".into());
                }
            }
            log.push(*s);
            Ok(())
        });
        assert_eq!(r, Err("boom".into()));
        assert_eq!(log.last(), Some(&InputStep::ButtonUp(MouseButton::Left)));
    }
}
