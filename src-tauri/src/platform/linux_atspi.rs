//! Linux X11 backend of the computer agent (best effort): AT-SPI over the accessibility D-Bus for
//! perception and actions, enigo (XTest) for synthetic input, x11rb for the active window,
//! monitors and the kill-switch probe. An app that doesn't expose AT-SPI yields an empty
//! snapshot, and the agent switches to vision (screenshot + coordinates).
//!
//! D-Bus calls go through `call_method` directly (no `Proxy`: a proxy per node would subscribe to
//! property signals for thousands of objects).

use super::coords::{Monitor, Point};
use super::input::{InputProbe, InputStep, Key, MouseButton};
use super::ui::{self, Caps, ElementOp, QuickState, Rect, Role, SnapshotOptions, Source, UiElement, UiSnapshot, WindowInfo};
use parking_lot::Mutex;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const ROOT_BUS: &str = "org.a11y.atspi.Registry";
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
/// Breadth-first walk limits.
const MAX_VISITED: usize = 1500;
const MAX_DEPTH: usize = 12;
const WALK_BUDGET: Duration = Duration::from_millis(3500);

static GEN: AtomicU64 = AtomicU64::new(0);

/// (bus name, object path) of an accessible.
type Ref = (String, OwnedObjectPath);

struct Cache {
    gen: u64,
    refs: Vec<Ref>,
    ids: Vec<usize>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
static BUS: Mutex<Option<Connection>> = Mutex::new(None);

fn a11y_bus() -> Result<Connection, String> {
    if let Some(c) = BUS.lock().as_ref() {
        return Ok(c.clone());
    }
    let session = Connection::session().map_err(|e| format!("D-Bus session bus unavailable: {e}"))?;
    let address: String = session
        .call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &())
        .map_err(|e| format!("Accessibility bus unavailable (is AT-SPI enabled?): {e}"))?
        .body()
        .deserialize()
        .map_err(|e| e.to_string())?;
    let conn = zbus::blocking::connection::Builder::address(address.as_str())
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| format!("Accessibility bus connection failed: {e}"))?;
    *BUS.lock() = Some(conn.clone());
    Ok(conn)
}

fn call<R>(conn: &Connection, r: &Ref, iface: &str, method: &str, body: &(impl serde::Serialize + zbus::zvariant::DynamicType)) -> Result<R, String>
where
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    conn.call_method(Some(r.0.as_str()), r.1.as_str(), Some(iface), method, body)
        .map_err(|e| e.to_string())?
        .body()
        .deserialize::<R>()
        .map_err(|e| e.to_string())
}

fn prop(conn: &Connection, r: &Ref, iface: &str, name: &str) -> Result<OwnedValue, String> {
    call::<OwnedValue>(conn, r, "org.freedesktop.DBus.Properties", "Get", &(iface, name))
}

fn name_of(conn: &Connection, r: &Ref) -> String {
    prop(conn, r, ACCESSIBLE, "Name").ok().and_then(|v| String::try_from(v).ok()).unwrap_or_default()
}

fn children(conn: &Connection, r: &Ref) -> Vec<Ref> {
    call::<Vec<(String, OwnedObjectPath)>>(conn, r, ACCESSIBLE, "GetChildren", &()).unwrap_or_default()
}

/// AT-SPI state bit set (two u32 words).
fn states(conn: &Connection, r: &Ref) -> u64 {
    let words: Vec<u32> = call(conn, r, ACCESSIBLE, "GetState", &()).unwrap_or_default();
    words.first().copied().unwrap_or(0) as u64 | (words.get(1).copied().unwrap_or(0) as u64) << 32
}

const ST_ACTIVE: u32 = 1;
const ST_CHECKED: u32 = 4;
const ST_EDITABLE: u32 = 7;
const ST_ENABLED: u32 = 8;
const ST_EXPANDABLE: u32 = 9;
const ST_EXPANDED: u32 = 10;
const ST_FOCUSED: u32 = 12;
const ST_SELECTED: u32 = 23;
const ST_SENSITIVE: u32 = 24;
const ST_SHOWING: u32 = 25;
const ST_VISIBLE: u32 = 30;
const ST_CHECKABLE: u32 = 41;
const ST_READ_ONLY: u32 = 43;

fn has(states: u64, bit: u32) -> bool {
    states & (1u64 << bit) != 0
}

/// AT-SPI role number → Soul role (`None` = not worth showing).
fn role_of(role: u32) -> Option<Role> {
    Some(match role {
        43 | 62 => Role::Button,                    // push button, toggle button
        79 | 77 | 76 => Role::Edit,                 // entry, editbar, autocomplete
        40 => Role::Edit,                           // password text
        61 => Role::Edit,                           // text (multi-line)
        82 | 94 | 95 | 96 => Role::Document,        // document frame / text / web / email
        88 => Role::Link,
        7 | 8 => Role::CheckBox,                    // check box, check menu item
        44 | 45 => Role::Radio,
        11 => Role::ComboBox,
        32 => Role::ListItem,
        35 => Role::MenuItem,
        37 => Role::TabItem,
        91 => Role::TreeItem,
        51 => Role::Slider,
        52 => Role::Spinner,
        56 => Role::DataItem,
        10 | 57 | 58 | 47 => Role::Header,
        29 | 83 | 73 => Role::Text,                 // label, heading, paragraph
        31 | 98 | 65 | 66 | 55 => Role::List,
        49 => Role::Pane,                           // scroll pane
        _ => return None,
    })
}

fn active_frame(conn: &Connection) -> Result<Ref, String> {
    let root: Ref = (ROOT_BUS.to_string(), OwnedObjectPath::try_from(ROOT_PATH).map_err(|e| e.to_string())?);
    let own = std::process::id();
    for app in children(conn, &root) {
        // Never Soul itself (Island / Ask windows are AT-SPI apps too).
        let pid = conn
            .call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "GetConnectionUnixProcessID", &(app.0.as_str(),))
            .ok()
            .and_then(|m| m.body().deserialize::<u32>().ok());
        if pid == Some(own) {
            continue;
        }
        for win in children(conn, &app) {
            if has(states(conn, &win), ST_ACTIVE) {
                return Ok(win);
            }
        }
    }
    Err("No active window exposes accessibility (AT-SPI).".into())
}

fn interfaces(conn: &Connection, r: &Ref) -> Vec<String> {
    call(conn, r, ACCESSIBLE, "GetInterfaces", &()).unwrap_or_default()
}

fn read_node(conn: &Connection, r: &Ref, role: Role, st: u64, password: bool) -> Option<UiElement> {
    let ifaces = interfaces(conn, r);
    let iface = |n: &str| ifaces.iter().any(|i| i == n);
    if !iface("org.a11y.atspi.Component") {
        return None;
    }
    let (x, y, w, h): (i32, i32, i32, i32) = call(conn, r, "org.a11y.atspi.Component", "GetExtents", &(0u32,)).ok()?;
    let mut e = UiElement::new(role, name_of(conn, r), Rect::new(x, y, w, h));
    e.enabled = has(st, ST_ENABLED) || has(st, ST_SENSITIVE);
    e.focused = has(st, ST_FOCUSED);
    e.password = password;
    let action = iface("org.a11y.atspi.Action");
    let editable = iface("org.a11y.atspi.EditableText") && has(st, ST_EDITABLE) && !has(st, ST_READ_ONLY);
    e.caps = Caps {
        invoke: action,
        set_value: editable,
        toggle: action && has(st, ST_CHECKABLE),
        select: action && matches!(role, Role::ListItem | Role::TabItem | Role::TreeItem),
        expand: action && has(st, ST_EXPANDABLE),
        scroll: false,
    };
    if has(st, ST_CHECKABLE) || matches!(role, Role::CheckBox | Role::Radio) {
        e.checked = Some(has(st, ST_CHECKED));
    }
    if has(st, ST_EXPANDABLE) {
        e.expanded = Some(has(st, ST_EXPANDED));
    }
    if has(st, ST_SELECTED) {
        e.selected = Some(true);
    }
    if !password {
        if iface("org.a11y.atspi.Text") && role.is_editable() {
            e.value = call::<String>(conn, r, "org.a11y.atspi.Text", "GetText", &(0i32, 4000i32)).ok();
        } else if iface("org.a11y.atspi.Value") {
            e.value = prop(conn, r, "org.a11y.atspi.Value", "CurrentValue").ok().and_then(|v| f64::try_from(v).ok()).map(|v| format!("{v}"));
        }
    }
    Some(e)
}

pub(super) fn snapshot(opts: SnapshotOptions) -> Result<UiSnapshot, String> {
    let t0 = Instant::now();
    let window = foreground_window().unwrap_or_default();
    let conn = match a11y_bus() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[soul][platform] at-spi unavailable: {e}");
            return Ok(UiSnapshot::empty(window, Source::None));
        }
    };
    let frame = match active_frame(&conn) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[soul][platform] at-spi: {e}");
            return Ok(UiSnapshot::empty(window, Source::None));
        }
    };
    let mut raw: Vec<UiElement> = Vec::new();
    let mut refs: Vec<Ref> = Vec::new();
    let mut queue: VecDeque<(Ref, usize)> = VecDeque::from([(frame, 0)]);
    let mut visited = 0;
    while let Some((node, depth)) = queue.pop_front() {
        if visited >= MAX_VISITED || t0.elapsed() > WALK_BUDGET {
            break;
        }
        visited += 1;
        let st = states(&conn, &node);
        if !(has(st, ST_SHOWING) || has(st, ST_VISIBLE)) && depth > 0 {
            continue;
        }
        let role_num: u32 = call(&conn, &node, ACCESSIBLE, "GetRole", &()).unwrap_or(0);
        if let Some(role) = role_of(role_num) {
            if let Some(e) = read_node(&conn, &node, role, st, role_num == 40) {
                raw.push(e);
                refs.push(node.clone());
            }
        }
        if depth < MAX_DEPTH {
            for c in children(&conn, &node).into_iter().take(200) {
                queue.push_back((c, depth + 1));
            }
        }
    }
    let filter = if window.rect.is_empty() { window.monitor } else { window.rect };
    let (elements, truncated) = ui::finalize(raw, &filter, opts.max_elements);
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    *CACHE.lock() = Some(Cache {
        gen,
        ids: elements.iter().map(|e| e.backend_index).collect(),
        refs,
    });
    Ok(UiSnapshot {
        gen,
        window,
        elements,
        source: Source::Atspi,
        took_ms: t0.elapsed().as_millis() as u64,
        truncated,
    })
}

fn cached_ref(gen: u64, id: u32) -> Result<Ref, String> {
    let cache = CACHE.lock();
    let c = cache.as_ref().filter(|c| c.gen == gen).ok_or_else(|| ui::STALE.to_string())?;
    let idx = id.checked_sub(1).and_then(|i| c.ids.get(i as usize)).ok_or_else(|| format!("No element {id} in the current view."))?;
    c.refs.get(*idx).cloned().ok_or_else(|| ui::STALE.to_string())
}

pub(super) fn element_op(gen: u64, id: u32, op: &ElementOp) -> Result<(), String> {
    let conn = a11y_bus()?;
    let r = cached_ref(gen, id)?;
    let ok = |b: bool, what: &str| if b { Ok(()) } else { Err(format!("{what} was refused by the app.")) };
    match op {
        ElementOp::Invoke | ElementOp::Toggle | ElementOp::Select | ElementOp::Expand | ElementOp::Collapse => {
            let b: bool = call(&conn, &r, "org.a11y.atspi.Action", "DoAction", &(0i32,))?;
            ok(b, "The action")
        }
        ElementOp::Focus => {
            let b: bool = call(&conn, &r, "org.a11y.atspi.Component", "GrabFocus", &())?;
            ok(b, "Focus")
        }
        ElementOp::SetValue(text) => {
            let role: u32 = call(&conn, &r, ACCESSIBLE, "GetRole", &())?;
            if role == 40 {
                return Err("Refusing to write into a password field.".into());
            }
            let b: bool = call(&conn, &r, "org.a11y.atspi.EditableText", "SetTextContents", &(text.as_str(),))?;
            ok(b, "Setting the text")
        }
        ElementOp::Scroll { .. } => Err("Scrolling via AT-SPI is not supported; using the mouse wheel.".into()),
    }
}

pub(super) fn quick_state(gen: u64, target: Option<u32>) -> Result<QuickState, String> {
    let w = foreground_window().unwrap_or_default();
    let mut q = QuickState {
        window_id: w.id,
        title: w.title,
        ..Default::default()
    };
    if let (Some(id), Ok(conn)) = (target, a11y_bus()) {
        if let Ok(r) = cached_ref(gen, id) {
            let st = states(&conn, &r);
            let text: String = call(&conn, &r, "org.a11y.atspi.Text", "GetText", &(0i32, 4000i32)).unwrap_or_default();
            q.target = format!("{text}|{}|{}", has(st, ST_CHECKED), has(st, ST_EXPANDED));
            if has(st, ST_FOCUSED) {
                q.focused = name_of(&conn, &r);
            }
        }
    }
    Ok(q)
}

// ───────────────────────────── X11 ─────────────────────────────

struct X {
    conn: RustConnection,
    root: u32,
    escape: Option<u8>,
}

static X11: Mutex<Option<X>> = Mutex::new(None);

fn with_x<T>(f: impl FnOnce(&X) -> Option<T>) -> Option<T> {
    let mut slot = X11.lock();
    if slot.is_none() {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let (min, max) = (conn.setup().min_keycode, conn.setup().max_keycode);
        let escape = conn.get_keyboard_mapping(min, max - min + 1).ok().and_then(|c| c.reply().ok()).and_then(|m| {
            let per = m.keysyms_per_keycode.max(1) as usize;
            m.keysyms.chunks(per).position(|syms| syms.contains(&0xff1b)).map(|i| min + i as u8)
        });
        *slot = Some(X { conn, root, escape });
    }
    let r = f(slot.as_ref()?);
    if r.is_none() {
        *slot = None; // reconnect next time
    }
    r
}

fn atom(x: &X, name: &[u8]) -> Option<u32> {
    Some(x.conn.intern_atom(false, name).ok()?.reply().ok()?.atom)
}

pub(super) fn foreground_window() -> Option<WindowInfo> {
    with_x(|x| {
        let active = atom(x, b"_NET_ACTIVE_WINDOW")?;
        let win = x.conn.get_property(false, x.root, active, AtomEnum::WINDOW, 0, 1).ok()?.reply().ok()?.value32()?.next()?;
        if win == 0 {
            return None;
        }
        let utf8 = atom(x, b"UTF8_STRING")?;
        let name_atom = atom(x, b"_NET_WM_NAME")?;
        let title = x
            .conn
            .get_property(false, win, name_atom, utf8, 0, 1024)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.value).to_string())
            .unwrap_or_default();
        let pid_atom = atom(x, b"_NET_WM_PID")?;
        let pid = x
            .conn
            .get_property(false, win, pid_atom, AtomEnum::CARDINAL, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut v| v.next()))
            .unwrap_or(0);
        let process = std::fs::read_to_string(format!("/proc/{pid}/comm")).map(|s| s.trim().to_string()).unwrap_or_default();
        let geo = x.conn.get_geometry(win).ok()?.reply().ok()?;
        let pos = x.conn.translate_coordinates(win, x.root, 0, 0).ok()?.reply().ok()?;
        let rect = Rect::new(pos.dst_x as i32, pos.dst_y as i32, geo.width as i32, geo.height as i32);
        let mons = monitors_with(x);
        let monitor = super::coords::monitor_for_rect(&rect, &mons).map(|m| m.rect).unwrap_or_default();
        Some(WindowInfo {
            id: win as u64,
            title,
            process,
            pid,
            rect,
            monitor,
            fullscreen: !monitor.is_empty() && rect.x <= monitor.x && rect.y <= monitor.y && rect.right() >= monitor.right() && rect.bottom() >= monitor.bottom(),
        })
    })
}

fn monitors_with(x: &X) -> Vec<Monitor> {
    use x11rb::protocol::randr::ConnectionExt as _;
    let mut out: Vec<Monitor> = x
        .conn
        .randr_get_monitors(x.root, true)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| {
            r.monitors
                .iter()
                .filter(|m| m.width > 0 && m.height > 0)
                .map(|m| Monitor {
                    rect: Rect::new(m.x as i32, m.y as i32, m.width as i32, m.height as i32),
                    scale: 1.0,
                })
                .collect()
        })
        .unwrap_or_default();
    if out.is_empty() {
        if let Ok(Ok(g)) = x.conn.get_geometry(x.root).map(|c| c.reply()) {
            out.push(Monitor {
                rect: Rect::new(0, 0, g.width as i32, g.height as i32),
                scale: 1.0,
            });
        }
    }
    out
}

pub(super) fn monitors() -> Vec<Monitor> {
    with_x(|x| Some(monitors_with(x))).unwrap_or_default()
}

pub(super) fn probe() -> InputProbe {
    with_x(|x| {
        let p = x.conn.query_pointer(x.root).ok()?.reply().ok()?;
        let esc_down = x.escape.is_some_and(|kc| {
            x.conn
                .query_keymap()
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|k| k.keys[(kc / 8) as usize] & (1 << (kc % 8)) != 0)
        });
        Some(InputProbe {
            esc_down,
            cursor: Some(Point::new(p.root_x as i32, p.root_y as i32)),
        })
    })
    .unwrap_or_default()
}

// ───────────────────────────── input (enigo / XTest) ─────────────────────────────

thread_local! {
    static ENIGO: RefCell<Option<enigo::Enigo>> = const { RefCell::new(None) };
    /// High surrogate waiting for its pair (Unicode steps arrive as UTF-16 units).
    static HIGH: RefCell<Option<u16>> = const { RefCell::new(None) };
}

fn enigo_key(k: Key) -> enigo::Key {
    use enigo::Key as E;
    match k {
        Key::Ctrl => E::Control,
        Key::Shift => E::Shift,
        Key::Alt => E::Alt,
        Key::Win => E::Meta,
        Key::Enter => E::Return,
        Key::Tab => E::Tab,
        Key::Escape => E::Escape,
        Key::Backspace => E::Backspace,
        Key::Delete => E::Delete,
        Key::Insert => E::Insert,
        Key::Home => E::Home,
        Key::End => E::End,
        Key::PageUp => E::PageUp,
        Key::PageDown => E::PageDown,
        Key::Up => E::UpArrow,
        Key::Down => E::DownArrow,
        Key::Left => E::LeftArrow,
        Key::Right => E::RightArrow,
        Key::Space => E::Space,
        Key::Menu => E::Other(0xff67), // XK_Menu
        Key::F(n) => match n {
            1 => E::F1,
            2 => E::F2,
            3 => E::F3,
            4 => E::F4,
            5 => E::F5,
            6 => E::F6,
            7 => E::F7,
            8 => E::F8,
            9 => E::F9,
            10 => E::F10,
            11 => E::F11,
            _ => E::F12,
        },
        Key::Char(c) => E::Unicode(c.to_ascii_lowercase()),
    }
}

pub(super) fn send_step(step: &InputStep) -> Result<(), String> {
    use enigo::{Axis, Button, Coordinate, Direction, Keyboard, Mouse, Settings};
    ENIGO.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(enigo::Enigo::new(&Settings::default()).map_err(|e| format!("Input simulation unavailable: {e}"))?);
        }
        let en = slot.as_mut().expect("initialised");
        let err = |e: enigo::InputError| format!("Synthetic input failed: {e}");
        match *step {
            InputStep::MoveTo(p) => en.move_mouse(p.x, p.y, Coordinate::Abs).map_err(err),
            InputStep::ButtonDown(b) | InputStep::ButtonUp(b) => {
                let button = match b {
                    MouseButton::Left => Button::Left,
                    MouseButton::Right => Button::Right,
                    MouseButton::Middle => Button::Middle,
                };
                let dir = if matches!(step, InputStep::ButtonDown(_)) { Direction::Press } else { Direction::Release };
                en.button(button, dir).map_err(err)
            }
            // enigo: positive = down / right; Soul: positive = up / right.
            InputStep::Wheel { horizontal, notches } => {
                if horizontal {
                    en.scroll(notches, Axis::Horizontal).map_err(err)
                } else {
                    en.scroll(-notches, Axis::Vertical).map_err(err)
                }
            }
            InputStep::KeyDown(k) => en.key(enigo_key(k), Direction::Press).map_err(err),
            InputStep::KeyUp(k) => en.key(enigo_key(k), Direction::Release).map_err(err),
            InputStep::Unicode(u) => {
                let pending = HIGH.with(|h| h.borrow_mut().take());
                let units: Vec<u16> = match pending {
                    Some(hi) => vec![hi, u],
                    None if (0xD800..0xDC00).contains(&u) => {
                        HIGH.with(|h| *h.borrow_mut() = Some(u));
                        return Ok(());
                    }
                    None => vec![u],
                };
                let text: String = char::decode_utf16(units).filter_map(Result::ok).collect();
                if text.is_empty() {
                    Ok(())
                } else {
                    en.text(&text).map_err(err)
                }
            }
            InputStep::Pause(ms) => {
                std::thread::sleep(Duration::from_millis(ms as u64));
                Ok(())
            }
        }
    })
}
