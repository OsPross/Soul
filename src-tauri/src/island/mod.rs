//! Dynamic Island — top-anchored always-on-top assistant bar.
//!
//! Frontend owns the expand state machine (`collapsed` | `peek` | `expanded`).
//! Rust only resizes the native window when the frontend reports a mode/size change
//! (debounced). While not collapsed, Rust also polls the cursor every 100 ms as a
//! safety net: if it stays outside the panel it asks the UI to collapse, and if the
//! UI still has not collapsed shortly after, Rust collapses the window itself.
//!
//! The native window is the panel plus a transparent margin (`PAD_X` left/right,
//! `PAD_B` bottom) so the CSS shadow is never clipped into a rectangle.

pub(crate) mod clipboard;
pub mod debug_log;
mod media;
mod watchdog;

use crate::config::{load_settings, save_settings, IslandPosition, Settings};
use crate::AppState;
use media::NowPlaying;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Size};
use watchdog::{time_fn, Watch};

pub use clipboard::ClipboardItem;

pub const ISLAND_LABEL: &str = "island";
pub const COLLAPSED_W: f64 = 220.0;
pub const COLLAPSED_H: f64 = 40.0;
pub const PEEK_W: f64 = 380.0;
pub const PEEK_H: f64 = 96.0;
pub const EXPANDED_W: f64 = 640.0;
pub const EXPANDED_H: f64 = 340.0;
const TOP_MARGIN: i32 = 10;
const RESIZE_DEBOUNCE_MS: u64 = 40;
const LEAVE_POLL_MS: u64 = 100;
const LEAVE_GRACE_MS: u64 = 400;
/// If the UI has not collapsed this long after the first `force-collapse`, Rust does it.
const HARD_COLLAPSE_MS: u64 = 800;
/// Transparent window margin around the panel (logical px). Keep in sync with Island.tsx.
pub const PAD_X: f64 = 20.0;
pub const PAD_B: f64 = 28.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum IslandMode {
    #[default]
    Collapsed,
    Peek,
    Expanded,
}

impl IslandMode {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "peek" => Self::Peek,
            "expanded" | "expand" | "open" => Self::Expanded,
            _ => Self::Collapsed,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Collapsed => "collapsed",
            Self::Peek => "peek",
            Self::Expanded => "expanded",
        }
    }

    pub fn default_size(self) -> (f64, f64) {
        match self {
            Self::Collapsed => (COLLAPSED_W, COLLAPSED_H),
            Self::Peek => (PEEK_W, PEEK_H),
            Self::Expanded => (EXPANDED_W, EXPANDED_H),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantBanner {
    pub state: String,
    pub message: Option<String>,
    /// Which feature is active: `dictation` | `command` (None = unknown / idle).
    /// Drives the Island stage and accent colour.
    #[serde(default)]
    pub kind: Option<String>,
}

/// Feature the current assistant activity belongs to (see `set_activity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Dictation,
    Command,
}

impl Activity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dictation => "dictation",
            Self::Command => "command",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolChipPayload {
    name: String,
    /// Display text (scene steps: app name, "40%"…); the UI falls back to a label per tool.
    label: Option<String>,
    /// `start` | `ok` | `error`
    phase: &'static str,
}

/// Button on an Island toast; the UI only invokes whitelisted commands.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToastAction {
    pub label: String,
    pub command: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToastPayload {
    /// `memory` | `note` | `timer` | `scene` | `info`
    kind: String,
    text: String,
    action: Option<ToastAction>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SfxPayload {
    cue: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptionPayload {
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IslandSnapshot {
    pub enabled: bool,
    pub position: String,
    pub sections: crate::config::IslandSections,
    pub pinned_apps: Vec<String>,
    pub assistant: Option<AssistantBanner>,
    pub now_playing: NowPlaying,
    pub clipboard: Vec<ClipboardItem>,
    pub plans: Vec<PlanItem>,
    pub timers: Vec<crate::timers::Timer>,
    pub clock: String,
    pub mode: IslandMode,
    /// Back-compat for older frontends.
    pub expanded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub id: String,
    pub title: String,
    pub time: String,
    pub done: bool,
}

struct IslandRuntime {
    mode: IslandMode,
    assistant: Option<AssistantBanner>,
    activity: Option<Activity>,
    last_w: f64,
    last_h: f64,
    last_x: i32,
    last_y: i32,
    pending_resize: Option<(f64, f64, Instant)>,
    /// Last collapsed panel size reported by the UI (used for the hard-collapse net).
    collapsed_size: (f64, f64),
}

fn runtime() -> &'static Mutex<IslandRuntime> {
    static RT: OnceLock<Mutex<IslandRuntime>> = OnceLock::new();
    RT.get_or_init(|| {
        Mutex::new(IslandRuntime {
            mode: IslandMode::Collapsed,
            assistant: None,
            activity: None,
            last_w: 0.0,
            last_h: 0.0,
            last_x: i32::MIN,
            last_y: i32::MIN,
            pending_resize: None,
            collapsed_size: (COLLAPSED_W, COLLAPSED_H),
        })
    })
}

pub fn init(app: &AppHandle) {
    clipboard::start_watcher(app.clone());
    media::start_worker(app.clone());
    start_leave_watcher(app.clone());
    start_resize_flusher(app.clone());

    let settings = load_settings(app).unwrap_or_default();
    apply_enabled(app, settings.island_enabled);
}

pub fn apply_enabled(app: &AppHandle, enabled: bool) {
    if let Some(win) = app.get_webview_window(ISLAND_LABEL) {
        if enabled {
            let settings = load_settings(app).unwrap_or_default();
            let mode = runtime().lock().mode;
            let (w, h) = mode.default_size();
            let _ = apply_geometry(app, &settings.island_position, w, h, true);
            prepare_transparent_window(&win);
            let _ = win.set_ignore_cursor_events(false);
            let _ = win.show();
        } else {
            let _ = win.hide();
        }
    }
}

/// Disable native rounded corners / chrome so only the CSS panel draws the shape.
fn prepare_transparent_window(win: &tauri::WebviewWindow) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_SYSTEMBACKDROP_TYPE,
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
            DWM_WINDOW_CORNER_PREFERENCE,
        };
        if let Ok(hwnd) = win.hwnd() {
            let hwnd = HWND(hwnd.0);
            let pref = DWMWCP_DONOTROUND;
            unsafe {
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    &pref as *const DWM_WINDOW_CORNER_PREFERENCE as *const _,
                    std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
                );
                // No 1px system border, and no Mica/Acrylic backdrop on the window itself.
                let no_border: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_BORDER_COLOR,
                    &no_border as *const u32 as *const _,
                    std::mem::size_of::<u32>() as u32,
                );
                let no_backdrop: i32 = 1; // DWMSBT_NONE
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_SYSTEMBACKDROP_TYPE,
                    &no_backdrop as *const i32 as *const _,
                    std::mem::size_of::<i32>() as u32,
                );
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = win;
    }
}

fn apply_geometry(
    app: &AppHandle,
    position: &IslandPosition,
    width: f64,
    height: f64,
    force: bool,
) -> Result<(), String> {
    let _w = Watch::start("island_apply_geometry");
    let win = app
        .get_webview_window(ISLAND_LABEL)
        .ok_or_else(|| "Island window missing".to_string())?;

    let monitor = app
        .primary_monitor()
        .map_err(|e| e.to_string())?
        .or_else(|| {
            app.available_monitors()
                .ok()
                .and_then(|m| m.into_iter().next())
        })
        .ok_or_else(|| "No monitor found".to_string())?;

    let scale = monitor.scale_factor();
    let mpos = monitor.position();
    let msize = monitor.size();
    // Window = panel + transparent margin; panel is top-centred inside it.
    let win_w_logical = width + 2.0 * PAD_X;
    let win_h_logical = height + PAD_B;
    let win_w = (win_w_logical * scale) as i32;
    let x = match position {
        IslandPosition::TopLeft => mpos.x + 24,
        IslandPosition::TopRight => mpos.x + msize.width as i32 - win_w - 24,
        IslandPosition::TopCenter => mpos.x + (msize.width as i32 - win_w) / 2,
    };
    let y = mpos.y + TOP_MARGIN;

    {
        let mut rt = runtime().lock();
        let size_changed = (rt.last_w - width).abs() > 0.5 || (rt.last_h - height).abs() > 0.5;
        let pos_changed = rt.last_x != x || rt.last_y != y;
        if !force && !size_changed && !pos_changed {
            return Ok(());
        }
        rt.last_w = width;
        rt.last_h = height;
        rt.last_x = x;
        rt.last_y = y;
        rt.pending_resize = None;
    }

    let _ = win.set_size(Size::Logical(LogicalSize::new(win_w_logical, win_h_logical)));
    win.set_position(tauri::Position::Physical(PhysicalPosition::new(x, y)))
        .map_err(|e| e.to_string())?;
    let _ = win.set_always_on_top(true);
    prepare_transparent_window(&win);
    Ok(())
}

/// Position using last known size (settings changes).
pub fn ensure_window_geometry(
    app: &AppHandle,
    position: &IslandPosition,
) -> Result<(), String> {
    let (w, h) = {
        let rt = runtime().lock();
        if rt.last_w > 1.0 && rt.last_h > 1.0 {
            (rt.last_w, rt.last_h)
        } else {
            rt.mode.default_size()
        }
    };
    apply_geometry(app, position, w, h, false)
}

pub fn emit_assistant_state(app: &AppHandle, state: &str, message: Option<String>) {
    let settings = load_settings(app).unwrap_or_default();
    if !settings.island_enabled {
        return;
    }

    let banner = {
        let mut rt = runtime().lock();
        let banner = AssistantBanner {
            state: state.to_string(),
            message,
            kind: rt.activity.map(|a| a.as_str().to_string()),
        };
        rt.assistant = Some(banner.clone());
        banner
    };

    let _ = ensure_window_geometry(app, &settings.island_position);
    if let Some(win) = app.get_webview_window(ISLAND_LABEL) {
        let _ = win.show();
        let _ = win.set_ignore_cursor_events(false);
    }

    let _ = app.emit("island://assistant", banner);
}

/// Tag the following assistant states with the feature that produced them (accent + stage).
pub fn set_activity(activity: Activity) {
    runtime().lock().activity = Some(activity);
}

pub fn clear_assistant(app: &AppHandle) {
    {
        let mut rt = runtime().lock();
        rt.assistant = None;
        rt.activity = None;
    }
    let _ = app.emit(
        "island://assistant",
        AssistantBanner {
            state: "idle".into(),
            message: None,
            kind: None,
        },
    );
}

/// Agent tool lifecycle for the animated tool chips (`start` → `ok` | `error`).
pub fn emit_tool(app: &AppHandle, name: &str, phase: &'static str) {
    emit_tool_labeled(app, name, None, phase);
}

/// Tool chip with explicit text (scene progress: one chip per step).
pub fn emit_tool_labeled(app: &AppHandle, name: &str, label: Option<&str>, phase: &'static str) {
    let _ = app.emit_to(
        tauri::EventTarget::webview_window(ISLAND_LABEL),
        "island://tool",
        ToolChipPayload {
            name: name.to_string(),
            label: label.map(str::to_string),
            phase,
        },
    );
}

/// Transient Island card ("Zapamiętałem: …", timer finished, scene suggestion).
pub fn toast(app: &AppHandle, kind: &str, text: &str, action: Option<ToastAction>) {
    let settings = load_settings(app).unwrap_or_default();
    if !settings.island_enabled {
        return;
    }
    let _ = ensure_window_geometry(app, &settings.island_position);
    if let Some(win) = app.get_webview_window(ISLAND_LABEL) {
        let _ = win.show();
    }
    let _ = app.emit_to(
        tauri::EventTarget::webview_window(ISLAND_LABEL),
        "island://toast",
        ToastPayload {
            kind: kind.to_string(),
            text: text.chars().take(240).collect(),
            action,
        },
    );
}

/// UI sound effect, synthesized locally by the Island (`listen` | `listenEnd` | `success` |
/// `error` | `notify` | `timer`). The Island applies the sound pack, volume and quiet mode.
pub fn sfx(app: &AppHandle, cue: &str) {
    eprintln!("[soul][sfx] cue={cue}");
    let _ = app.emit_to(
        tauri::EventTarget::webview_window(ISLAND_LABEL),
        "soul://sfx",
        SfxPayload { cue: cue.to_string() },
    );
}

/// Current track for "co gra?": (title, artist, playing), or None when nothing is available.
pub fn now_playing_brief() -> Option<(String, String, bool)> {
    let np = media::cached_now_playing();
    (np.available && !np.title.trim().is_empty()).then(|| (np.title, np.artist, np.playing))
}

/// Push recent notes to the Plans tab.
pub fn refresh_plans(app: &AppHandle) {
    let _ = app.emit_to(
        tauri::EventTarget::webview_window(ISLAND_LABEL),
        "island://plans",
        current_plans(),
    );
}

/// Cleaned dictation text for the transient caption strip under the Island.
pub fn emit_caption(app: &AppHandle, text: &str) {
    let text: String = text.trim().chars().take(240).collect();
    if text.is_empty() {
        return;
    }
    let _ = app.emit_to(
        tauri::EventTarget::webview_window(ISLAND_LABEL),
        "island://caption",
        CaptionPayload { text },
    );
}

/// Plans tab: the latest voice notes (timers are pushed separately as live countdowns).
fn current_plans() -> Vec<PlanItem> {
    crate::memory::list(Some(crate::memory::NOTE_CATEGORY))
        .unwrap_or_default()
        .into_iter()
        .take(8)
        .map(|n| PlanItem {
            id: n.id.to_string(),
            title: n.text,
            time: chrono::DateTime::from_timestamp_millis(n.created_at)
                .map(|t| t.with_timezone(&chrono::Local).format("%d.%m %H:%M").to_string())
                .unwrap_or_default(),
            done: false,
        })
        .collect()
}

fn clock_string() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

/// Debounced flusher for window resizes requested by the frontend.
fn start_resize_flusher(app: AppHandle) {
    thread::Builder::new()
        .name("soul-island-resize".into())
        .spawn(move || loop {
            thread::sleep(Duration::from_millis(16));
            let pending = {
                let rt = runtime().lock();
                rt.pending_resize
            };
            let Some((w, h, at)) = pending else {
                continue;
            };
            if at.elapsed() < Duration::from_millis(RESIZE_DEBOUNCE_MS) {
                continue;
            }
            let settings = load_settings(&app).unwrap_or_default();
            if !settings.island_enabled {
                continue;
            }
            let _ = apply_geometry(&app, &settings.island_position, w, h, true);
        })
        .ok();
}

/// Safety net: while peek/expanded, poll the cursor. Outside the panel for LEAVE_GRACE
/// => emit `island://force-collapse` (repeatedly, so a lost event can't strand the UI).
/// Still not collapsed HARD_COLLAPSE after the first request => collapse natively.
fn start_leave_watcher(app: AppHandle) {
    thread::Builder::new()
        .name("soul-island-leave".into())
        .spawn(move || {
            let mut outside_since: Option<Instant> = None;
            let mut asked_at: Option<Instant> = None;
            loop {
                thread::sleep(Duration::from_millis(LEAVE_POLL_MS));
                let mode = runtime().lock().mode;
                if mode == IslandMode::Collapsed {
                    outside_since = None;
                    asked_at = None;
                    continue;
                }
                if cursor_over_panel(&app).unwrap_or(true) {
                    outside_since = None;
                    asked_at = None;
                    continue;
                }
                let since = *outside_since.get_or_insert_with(Instant::now);
                if since.elapsed() < Duration::from_millis(LEAVE_GRACE_MS) {
                    continue;
                }
                let first = *asked_at.get_or_insert_with(Instant::now);
                if first.elapsed() >= Duration::from_millis(HARD_COLLAPSE_MS) {
                    hard_collapse(&app);
                    outside_since = None;
                    asked_at = None;
                }
                let _ = app.emit("island://force-collapse", true);
            }
        })
        .ok();
}

fn hard_collapse(app: &AppHandle) {
    let (w, h) = {
        let mut rt = runtime().lock();
        rt.mode = IslandMode::Collapsed;
        rt.collapsed_size
    };
    eprintln!("[soul][island] UI did not collapse in time — collapsing natively");
    let settings = load_settings(app).unwrap_or_default();
    let _ = apply_geometry(app, &settings.island_position, w, h, true);
    let _ = app.emit("island://mode", IslandMode::Collapsed.as_str());
}

/// Cursor inside the visible panel (window minus transparent margin), with a little slack.
fn cursor_over_panel(app: &AppHandle) -> Result<bool, String> {
    let Some((cx, cy)) = cursor_screen_pos() else {
        return Ok(true);
    };
    let win = app
        .get_webview_window(ISLAND_LABEL)
        .ok_or_else(|| "no island".to_string())?;
    let pos = win.outer_position().map_err(|e| e.to_string())?;
    let size = win.outer_size().map_err(|e| e.to_string())?;
    let scale = win.scale_factor().unwrap_or(1.0);
    let pad_x = (PAD_X * scale) as i32;
    let pad_b = (PAD_B * scale) as i32;
    let slack = (6.0 * scale) as i32;
    Ok(cx >= pos.x + pad_x - slack
        && cx <= pos.x + size.width as i32 - pad_x + slack
        && cy >= pos.y - slack
        && cy <= pos.y + size.height as i32 - pad_b + slack)
}

#[cfg(windows)]
fn cursor_screen_pos() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut pt = POINT::default();
    unsafe {
        GetCursorPos(&mut pt).ok()?;
    }
    Some((pt.x, pt.y))
}

#[cfg(not(windows))]
fn cursor_screen_pos() -> Option<(i32, i32)> {
    None
}

#[tauri::command]
pub async fn island_get_snapshot(app: AppHandle) -> Result<IslandSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || {
        time_fn("island_get_snapshot", || {
            let settings = load_settings(&app)?;
            let rt = runtime().lock();
            Ok(IslandSnapshot {
                enabled: settings.island_enabled,
                position: settings.island_position.as_str().into(),
                sections: settings.island_sections.clone(),
                pinned_apps: settings.pinned_apps.clone(),
                assistant: rt.assistant.clone(),
                now_playing: media::cached_now_playing(),
                clipboard: clipboard::list(),
                plans: current_plans(),
                timers: crate::timers::list(),
                clock: clock_string(),
                mode: rt.mode,
                expanded: rt.mode == IslandMode::Expanded,
            })
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Frontend state machine → native window size (debounced).
#[tauri::command]
pub async fn island_set_mode(
    app: AppHandle,
    mode: String,
    width: Option<f64>,
    height: Option<f64>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        time_fn("island_set_mode", || {
            let next = IslandMode::parse(&mode);
            let (dw, dh) = next.default_size();
            let w = width.unwrap_or(dw).clamp(120.0, 900.0);
            let h = height.unwrap_or(dh).clamp(36.0, 480.0);

            {
                let mut rt = runtime().lock();
                rt.mode = next;
                if next == IslandMode::Collapsed {
                    rt.collapsed_size = (w, h);
                }
                rt.pending_resize = Some((w, h, Instant::now()));
            }

            let settings = load_settings(&app)?;
            if !settings.island_enabled {
                return Ok(());
            }
            if let Some(win) = app.get_webview_window(ISLAND_LABEL) {
                let _ = win.set_ignore_cursor_events(false);
            }
            let _ = app.emit("island://mode", next.as_str());
            Ok(())
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Back-compat shim.
#[tauri::command]
pub async fn island_set_expanded(app: AppHandle, expanded: bool) -> Result<(), String> {
    let mode = if expanded { "expanded" } else { "collapsed" };
    island_set_mode(app, mode.into(), None, None).await
}

#[tauri::command]
pub async fn island_media_action(action: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        time_fn("island_media_action", || media::media_action_async(action))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn island_get_now_playing() -> NowPlaying {
    media::cached_now_playing()
}

#[tauri::command]
pub async fn island_clipboard_list() -> Vec<ClipboardItem> {
    clipboard::list()
}

#[tauri::command]
pub async fn island_clipboard_clear(app: AppHandle) -> Result<(), String> {
    clipboard::clear();
    let _ = app.emit("island://clipboard", clipboard::list());
    Ok(())
}

#[tauri::command]
pub async fn island_clipboard_copy(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        time_fn("island_clipboard_copy", || clipboard::copy_item(&id))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn island_launch_pinned(app: AppHandle, name: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        time_fn("island_launch_pinned", || {
            let state = app.state::<AppState>();
            state.platform.open_app(&name)
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn island_set_volume(app: AppHandle, percent: u8) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        time_fn("island_set_volume", || {
            let state = app.state::<AppState>();
            state.platform.set_volume(percent)
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn island_list_apps(app: AppHandle) -> Result<Vec<String>, String> {
    let settings = load_settings(&app)?;
    Ok(settings.pinned_apps)
}

#[tauri::command]
pub async fn island_search_apps(
    app: AppHandle,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<crate::platform::apps::AppSuggestion>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        Ok(state
            .platform
            .search_apps(&query, limit.unwrap_or(8).clamp(1, 20)))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn island_update_pins(app: AppHandle, pinned: Vec<String>) -> Result<(), String> {
    let mut settings = load_settings(&app)?;
    settings.pinned_apps = pinned;
    save_settings(&app, &settings)?;
    Ok(())
}

pub fn on_settings_changed(app: &AppHandle, settings: &Settings) {
    apply_enabled(app, settings.island_enabled);
    if settings.island_enabled {
        let _ = ensure_window_geometry(app, &settings.island_position);
        let _ = app.emit("island://settings", settings.island_sections.clone());
    }
    let _ = app.emit("soul://appearance", settings.appearance.clone());
}
