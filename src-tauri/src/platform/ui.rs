//! Portable model of the focused window for the computer agent (Soul Agent, `docs/agent.md`).
//!
//! Backends (`windows_uia.rs` UI Automation, `linux_atspi.rs` AT-SPI) produce raw elements in
//! physical screen pixels; [`finalize`] filters, orders (reading order) and numbers them, so every
//! backend hands the agent the same shape. [`UiSnapshot::observation`] is the compact text the
//! model sees every step; passwords never carry a value.

#![allow(dead_code)] // TEMP(phase 1): wired up by the agent loop in phase 2

use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// At most this many elements reach the model (the focused one is always kept).
pub const MAX_ELEMENTS: usize = 300;
/// Of those, at most this many plain text labels (pages are full of them).
pub const MAX_TEXT_ELEMENTS: usize = 80;
/// Names / values are cut to this many characters in the observation.
pub const MAX_LABEL_CHARS: usize = 80;
/// Error of an element operation on an outdated snapshot: the caller falls back to coordinates.
pub const STALE: &str = "stale snapshot";
/// Rows closer than this (px) count as one line for reading order.
const ROW_BAND: i32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    pub fn area(&self) -> i64 {
        if self.is_empty() {
            0
        } else {
            self.w as i64 * self.h as i64
        }
    }

    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        (r > x && b > y).then(|| Rect::new(x, y, r - x, b - y))
    }
}

/// Element kind, normalized across UIA control types and AT-SPI roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Button,
    Edit,
    Document,
    Link,
    CheckBox,
    Radio,
    ComboBox,
    ListItem,
    MenuItem,
    TabItem,
    TreeItem,
    Slider,
    Spinner,
    DataItem,
    Header,
    List,
    Text,
    Pane,
    Other,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::Button => "button",
            Self::Edit => "edit",
            Self::Document => "document",
            Self::Link => "link",
            Self::CheckBox => "checkbox",
            Self::Radio => "radio",
            Self::ComboBox => "combobox",
            Self::ListItem => "listitem",
            Self::MenuItem => "menuitem",
            Self::TabItem => "tab",
            Self::TreeItem => "treeitem",
            Self::Slider => "slider",
            Self::Spinner => "spinner",
            Self::DataItem => "cell",
            Self::Header => "header",
            Self::List => "list",
            Self::Text => "text",
            Self::Pane => "pane",
            Self::Other => "element",
        }
    }

    /// Accepts typed text.
    pub fn is_editable(self) -> bool {
        matches!(self, Self::Edit | Self::Document | Self::ComboBox | Self::Spinner)
    }

    /// Plain labels / containers: context for the model, rarely a click target.
    pub fn is_passive(self) -> bool {
        matches!(self, Self::Text | Self::Pane | Self::List | Self::Other)
    }

    /// Acting on it changes a setting / value rather than navigating.
    pub fn changes_state(self) -> bool {
        matches!(
            self,
            Self::CheckBox | Self::Radio | Self::ComboBox | Self::Slider | Self::Spinner | Self::Edit
        )
    }
}

/// What the backend can do with an element without synthetic input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Hash)]
pub struct Caps {
    pub invoke: bool,
    /// Value pattern present and writable.
    pub set_value: bool,
    pub toggle: bool,
    pub select: bool,
    pub expand: bool,
    pub scroll: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiElement {
    /// 1-based, valid only within the snapshot's `gen`.
    pub id: u32,
    pub role: Role,
    pub name: String,
    /// Current value (edit text, combo selection, slider position). Never set for passwords.
    pub value: Option<String>,
    /// Physical screen pixels.
    pub rect: Rect,
    pub enabled: bool,
    pub focused: bool,
    pub automation_id: String,
    pub password: bool,
    pub caps: Caps,
    pub checked: Option<bool>,
    pub expanded: Option<bool>,
    pub selected: Option<bool>,
    /// Web content (Chromium / Electron): typing goes through the keyboard first.
    pub web: bool,
    /// Vertical scroll position 0–100 when scrollable.
    pub scroll_pct: Option<f64>,
    /// Backend index into its element cache (order before `finalize`).
    #[serde(skip)]
    pub backend_index: usize,
}

impl UiElement {
    pub fn new(role: Role, name: impl Into<String>, rect: Rect) -> Self {
        Self {
            id: 0,
            role,
            name: name.into(),
            value: None,
            rect,
            enabled: true,
            focused: false,
            automation_id: String::new(),
            password: false,
            caps: Caps::default(),
            checked: None,
            expanded: None,
            selected: None,
            web: false,
            scroll_pct: None,
            backend_index: 0,
        }
    }

    /// Display name: the name, else the automation id, else the role.
    pub fn display_name(&self) -> String {
        let n = self.name.trim();
        if !n.is_empty() {
            return clip(n, MAX_LABEL_CHARS);
        }
        let a = self.automation_id.trim();
        if !a.is_empty() {
            return clip(a, MAX_LABEL_CHARS);
        }
        self.role.label().to_string()
    }

    fn is_interactive(&self) -> bool {
        !self.role.is_passive()
            || self.caps.invoke
            || self.caps.toggle
            || self.caps.select
            || self.caps.set_value
            || self.caps.expand
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WindowInfo {
    /// Opaque native id (HWND / X window).
    pub id: u64,
    pub title: String,
    /// Executable name without the path („notepad.exe”, „Discord.exe”, „firefox”).
    pub process: String,
    pub pid: u32,
    /// Physical screen pixels.
    pub rect: Rect,
    /// The monitor the window is (mostly) on.
    pub monitor: Rect,
    /// Covers its whole monitor (game / presentation / F11).
    pub fullscreen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Uia,
    Atspi,
    /// No accessibility data (backend unavailable): vision only.
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiSnapshot {
    /// Increases with every snapshot; element ids are only valid for this `gen`.
    pub gen: u64,
    pub window: WindowInfo,
    pub elements: Vec<UiElement>,
    pub source: Source,
    pub took_ms: u64,
    /// More elements existed than `MAX_ELEMENTS`.
    pub truncated: bool,
}

/// Snapshot of the focused window's interactive elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotOptions {
    /// Prefer the last real app window when one of Soul's own windows has focus.
    pub skip_own_windows: bool,
    pub max_elements: usize,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            skip_own_windows: true,
            max_elements: MAX_ELEMENTS,
        }
    }
}

/// Pattern-level operations on one element of a snapshot (no synthetic input).
#[derive(Debug, Clone, PartialEq)]
pub enum ElementOp {
    Invoke,
    Toggle,
    Select,
    Expand,
    Collapse,
    Focus,
    SetValue(String),
    Scroll { dir: ScrollDir, amount: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDir {
    Up,
    Down,
    Left,
    Right,
}

impl ScrollDir {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "up" | "gora" | "góra" | "w gore" | "w górę" => Some(Self::Up),
            "down" | "dol" | "dół" | "w dol" | "w dół" => Some(Self::Down),
            "left" | "lewo" | "w lewo" => Some(Self::Left),
            "right" | "prawo" | "w prawo" => Some(Self::Right),
            _ => None,
        }
    }

    pub fn is_vertical(self) -> bool {
        matches!(self, Self::Up | Self::Down)
    }
}

/// Cheap state read used while waiting for the UI to settle.
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub struct QuickState {
    pub window_id: u64,
    pub title: String,
    /// Role label + name of the focused element.
    pub focused: String,
    /// Value / toggle / expand state of the action's target, when it still exists.
    pub target: String,
}

/// Filter, order and number raw backend elements. Keeps elements that intersect `window`, drops
/// zero-size and duplicate (same rect + name + role) ones, keeps the focused element, interactive
/// elements before plain text, reading order (rows of `ROW_BAND` px, then x), at most `max`.
/// Returns the kept elements (ids 1..) and whether something was cut.
pub fn finalize(raw: Vec<UiElement>, window: &Rect, max: usize) -> (Vec<UiElement>, bool) {
    let mut seen = std::collections::HashSet::new();
    let mut items: Vec<UiElement> = Vec::with_capacity(raw.len().min(max));
    for (i, mut e) in raw.into_iter().enumerate() {
        e.backend_index = i;
        if e.rect.is_empty() {
            continue;
        }
        let Some(visible) = e.rect.intersect(window) else {
            continue;
        };
        // Mostly outside the window (off-screen list rows): not clickable where it claims.
        if visible.area() * 4 < e.rect.area() && !e.focused {
            continue;
        }
        if e.role == Role::Text && e.name.trim().is_empty() {
            continue;
        }
        if e.password {
            e.value = None;
        }
        if !seen.insert((e.rect, e.role, e.name.clone())) {
            continue;
        }
        items.push(e);
    }
    let total = items.len();
    // Priority: focused, interactive, scroll containers, text.
    let rank = |e: &UiElement| -> u8 {
        if e.focused {
            0
        } else if e.is_interactive() && !e.role.is_passive() {
            1
        } else if e.is_interactive() || e.caps.scroll {
            2
        } else {
            3
        }
    };
    items.sort_by_key(|e| rank(e));
    let mut kept = Vec::with_capacity(max.min(total));
    let mut texts = 0;
    for e in items {
        if kept.len() >= max {
            break;
        }
        if rank(&e) == 3 {
            if texts >= MAX_TEXT_ELEMENTS {
                continue;
            }
            texts += 1;
        }
        kept.push(e);
    }
    let truncated = kept.len() < total;
    kept.sort_by(|a, b| {
        let (ra, rb) = (a.rect.y / ROW_BAND, b.rect.y / ROW_BAND);
        ra.cmp(&rb).then(a.rect.x.cmp(&b.rect.x)).then(a.rect.y.cmp(&b.rect.y))
    });
    for (i, e) in kept.iter_mut().enumerate() {
        e.id = i as u32 + 1;
    }
    (kept, truncated)
}

impl UiSnapshot {
    pub fn empty(window: WindowInfo, source: Source) -> Self {
        Self {
            gen: 0,
            window,
            elements: Vec::new(),
            source,
            took_ms: 0,
            truncated: false,
        }
    }

    pub fn get(&self, id: u32) -> Option<&UiElement> {
        id.checked_sub(1).and_then(|i| self.elements.get(i as usize)).filter(|e| e.id == id)
    }

    pub fn focused(&self) -> Option<&UiElement> {
        self.elements.iter().find(|e| e.focused)
    }

    /// The tree can't drive this window (game, canvas, Electron without accessibility): fewer
    /// than three named interactive elements, or one document/pane covering ≥ 80 % of the window
    /// with almost nothing else.
    pub fn is_useless(&self) -> bool {
        let named = self
            .elements
            .iter()
            .filter(|e| e.is_interactive() && !e.name.trim().is_empty() && e.role != Role::Document)
            .count();
        if named < 3 {
            return true;
        }
        let win = self.window.rect.area().max(1);
        let big_opaque = self.elements.iter().any(|e| {
            matches!(e.role, Role::Document | Role::Pane | Role::Other)
                && e.rect.area() * 10 >= win * 8
                && e.name.trim().is_empty()
        });
        big_opaque && named < 6
    }

    /// Hash of what the user would see change: window, focus, names, values, states.
    pub fn fingerprint(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.window.id.hash(&mut h);
        self.window.title.hash(&mut h);
        for e in &self.elements {
            e.role.hash(&mut h);
            e.name.hash(&mut h);
            e.value.hash(&mut h);
            e.focused.hash(&mut h);
            e.checked.hash(&mut h);
            e.expanded.hash(&mut h);
            e.selected.hash(&mut h);
            e.enabled.hash(&mut h);
            e.rect.hash(&mut h);
            e.scroll_pct.map(|p| (p * 10.0) as i64).hash(&mut h);
        }
        h.finish()
    }

    /// Element centre in 0–1000 window-normalized coordinates (same frame as the screenshot).
    pub fn norm_center(&self, e: &UiElement) -> (i32, i32) {
        let (cx, cy) = e.rect.center();
        let r = &self.window.rect;
        let nx = ((cx - r.x) as f64 * 1000.0 / r.w.max(1) as f64).round() as i32;
        let ny = ((cy - r.y) as f64 * 1000.0 / r.h.max(1) as f64).round() as i32;
        (nx.clamp(0, 1000), ny.clamp(0, 1000))
    }

    /// Compact text for the model, one element per line:
    /// `12 button "Zapisz" @410,88` · `5 edit "Szukaj" value="FiveM" focused @500,40`.
    /// Quotes and newlines inside names are neutralized (on-screen text is data, never syntax).
    pub fn observation(&self) -> String {
        let mut out = String::with_capacity(self.elements.len() * 48);
        for e in &self.elements {
            let (nx, ny) = self.norm_center(e);
            out.push_str(&format!("{} {} \"{}\"", e.id, e.role.label(), sanitize(&e.display_name())));
            if e.password {
                out.push_str(" password");
            } else if let Some(v) = &e.value {
                if !v.trim().is_empty() && v.trim() != e.name.trim() {
                    out.push_str(&format!(" value=\"{}\"", sanitize(&clip(v, MAX_LABEL_CHARS))));
                }
            }
            if e.focused {
                out.push_str(" focused");
            }
            if !e.enabled {
                out.push_str(" disabled");
            }
            match e.checked {
                Some(true) => out.push_str(" checked"),
                Some(false) => out.push_str(" unchecked"),
                None => {}
            }
            match e.expanded {
                Some(true) => out.push_str(" expanded"),
                Some(false) => out.push_str(" collapsed"),
                None => {}
            }
            if e.selected == Some(true) {
                out.push_str(" selected");
            }
            if e.caps.scroll {
                match e.scroll_pct {
                    Some(p) => out.push_str(&format!(" scroll={}%", p.round() as i64)),
                    None => out.push_str(" scrollable"),
                }
            }
            out.push_str(&format!(" @{nx},{ny}\n"));
        }
        out
    }
}

/// Cut to `max` characters (adds „…”).
pub fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// One line, no double quotes, no control characters.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '"' => '\'',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(role: Role, name: &str, x: i32, y: i32) -> UiElement {
        let mut e = UiElement::new(role, name, Rect::new(x, y, 80, 24));
        e.caps.invoke = role == Role::Button;
        e
    }

    fn snap(elements: Vec<UiElement>) -> UiSnapshot {
        let window = WindowInfo {
            id: 1,
            title: "Test".into(),
            rect: Rect::new(0, 0, 1000, 800),
            ..Default::default()
        };
        let (elements, truncated) = finalize(elements, &window.rect, MAX_ELEMENTS);
        UiSnapshot {
            gen: 1,
            window,
            elements,
            source: Source::Uia,
            took_ms: 0,
            truncated,
        }
    }

    #[test]
    fn finalize_orders_numbers_and_filters() {
        let mut pw = el(Role::Edit, "Hasło", 100, 300);
        pw.password = true;
        pw.value = Some("tajne123".into());
        let raw = vec![
            el(Role::Button, "Anuluj", 500, 702),
            el(Role::Button, "Zapisz", 400, 700),
            el(Role::Text, "", 10, 10),                                   // unnamed text
            UiElement::new(Role::Button, "zero", Rect::new(5, 5, 0, 10)), // empty rect
            el(Role::Button, "Poza", 2000, 10),                          // outside the window
            el(Role::Button, "Zapisz", 400, 700),                        // duplicate
            el(Role::Text, "Nagłówek", 10, 20),
            pw,
        ];
        let s = snap(raw);
        let names: Vec<&str> = s.elements.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Nagłówek", "Hasło", "Zapisz", "Anuluj"]);
        assert_eq!(s.elements.iter().map(|e| e.id).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert_eq!(s.get(3).unwrap().name, "Zapisz");
        assert!(s.get(0).is_none() && s.get(9).is_none());
        // Password value never survives.
        assert_eq!(s.get(2).unwrap().value, None);
        assert!(!s.truncated);
    }

    #[test]
    fn cap_keeps_focused_and_limits_text() {
        let mut raw: Vec<UiElement> = (0..400).map(|i| el(Role::Text, &format!("t{i}"), 0, i)).collect();
        let mut f = el(Role::Edit, "Szukaj", 10, 790);
        f.focused = true;
        raw.push(f);
        raw.extend((0..50).map(|i| el(Role::Button, &format!("b{i}"), 100 + i, 400)));
        let s = snap(raw);
        assert!(s.truncated);
        assert!(s.focused().is_some());
        assert_eq!(s.elements.iter().filter(|e| e.role == Role::Text).count(), MAX_TEXT_ELEMENTS);
        assert_eq!(s.elements.iter().filter(|e| e.role == Role::Button).count(), 50);
    }

    #[test]
    fn observation_is_compact_and_safe() {
        let mut edit = el(Role::Edit, "Szukaj \"tu\"\nteraz", 450, 28);
        edit.value = Some("FiveM serwery".into());
        edit.focused = true;
        let mut pw = el(Role::Edit, "Hasło", 100, 300);
        pw.password = true;
        let mut cb = el(Role::CheckBox, "Pamiętaj mnie", 100, 340);
        cb.checked = Some(false);
        let s = snap(vec![edit, pw, cb, el(Role::Button, "Zapisz", 400, 700)]);
        let obs = s.observation();
        assert!(obs.contains("1 edit \"Szukaj 'tu' teraz\" value=\"FiveM serwery\" focused @490,50"), "{obs}");
        assert!(obs.contains("2 edit \"Hasło\" password @140,390"), "{obs}");
        assert!(obs.contains("3 checkbox \"Pamiętaj mnie\" unchecked"), "{obs}");
        assert!(obs.contains("4 button \"Zapisz\" @440,890"), "{obs}");
        assert_eq!(obs.lines().count(), 4);
    }

    #[test]
    fn useless_tree_detection() {
        // A game: one big unnamed pane.
        let mut pane = UiElement::new(Role::Pane, "", Rect::new(0, 0, 1000, 800));
        pane.caps.scroll = true;
        assert!(snap(vec![pane.clone()]).is_useless());
        // Electron without a11y: a document and two window buttons.
        let doc = UiElement::new(Role::Document, "", Rect::new(0, 30, 1000, 770));
        assert!(snap(vec![doc.clone(), el(Role::Button, "Minimize", 900, 0), el(Role::Button, "Close", 950, 0)]).is_useless());
        // A real dialog.
        let dialog = vec![
            el(Role::Button, "Zapisz", 400, 700),
            el(Role::Button, "Anuluj", 500, 700),
            el(Role::Edit, "Nazwa pliku", 100, 600),
            el(Role::ComboBox, "Typ", 100, 640),
        ];
        assert!(!snap(dialog).is_useless());
    }

    #[test]
    fn fingerprint_tracks_visible_changes() {
        let a = snap(vec![el(Role::Button, "Zapisz", 400, 700), el(Role::Edit, "Nazwa", 10, 10)]);
        let mut b = a.clone();
        assert_eq!(a.fingerprint(), b.fingerprint());
        b.elements[0].value = Some("x".into());
        assert_ne!(a.fingerprint(), b.fingerprint());
        let mut c = a.clone();
        c.window.title = "Inny".into();
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    #[test]
    fn rect_math() {
        let a = Rect::new(0, 0, 100, 100);
        assert_eq!(a.intersect(&Rect::new(50, 50, 100, 100)), Some(Rect::new(50, 50, 50, 50)));
        assert_eq!(a.intersect(&Rect::new(100, 0, 10, 10)), None);
        assert!(a.contains(99, 99) && !a.contains(100, 50));
        assert_eq!(Rect::new(10, 20, 31, 41).center(), (25, 40));
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(ScrollDir::parse("w dół"), Some(ScrollDir::Down));
    }
}
