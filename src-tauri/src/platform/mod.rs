//! OS-specific helpers for paste, capture, and voice-command tools.

use std::sync::Arc;

pub mod apps;
pub mod close;
pub mod coords;
pub mod hook;
pub mod input;
pub mod marks;
pub mod ui;

#[cfg(target_os = "linux")]
mod linux_atspi;
#[cfg(target_os = "linux")]
mod linux_wayland;
#[cfg(target_os = "linux")]
mod linux_x11;
#[cfg(target_os = "linux")]
pub mod x11_capture;
#[cfg(target_os = "linux")]
mod x11_hook;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
mod windows_hook;
#[cfg(target_os = "windows")]
mod windows_uia;

/// Hints for the animation battery saver. `None` = unknown on this platform.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerState {
    pub on_battery: Option<bool>,
    /// A fullscreen app / game / presentation is in the foreground.
    pub fullscreen: Option<bool>,
}

pub trait Platform: Send + Sync {
    /// Register a global hotkey. Default PTT uses the plugin; platforms may override.
    fn register_hotkey(&self, _hotkey: &str) -> Result<(), String> {
        Ok(())
    }

    /// Type text via simulated keystrokes.
    fn type_text(&self, text: &str) -> Result<(), String>;

    /// Paste text: clipboard write + Ctrl+V (or Wayland equivalent), then restore clipboard.
    fn paste_text(&self, text: &str) -> Result<(), String>;

    /// Capture the full screen (or a region later).
    fn capture_screen(&self) -> Result<Vec<u8>, String>;

    /// Fuzzy-find an installed app and launch it. Returns a short confirmation.
    fn open_app(&self, name: &str) -> Result<String, String>;

    /// Autocomplete suggestions from the local app index (Island Apps tab).
    fn search_apps(&self, query: &str, limit: usize) -> Vec<apps::AppSuggestion>;

    /// Close a running app by fuzzy name. Destructive — caller must confirm first.
    fn close_app(&self, name: &str) -> Result<String, String>;

    /// Bring a running app window to the foreground.
    fn focus_app(&self, name: &str) -> Result<String, String>;

    /// Open an http(s) URL in the default browser.
    fn open_url(&self, url: &str) -> Result<String, String>;

    /// General web search (Google).
    fn web_search(&self, query: &str) -> Result<String, String> {
        let url = apps::search_url(query, Some("google"))?;
        self.open_url(&url)
    }

    /// YouTube search results page.
    fn youtube_search(&self, query: &str) -> Result<String, String> {
        let url = apps::youtube_search_url(query)?;
        self.open_url(&url)
    }

    /// Open the first YouTube result for `query` (best-effort scrape; falls back to search).
    fn youtube_play(&self, query: &str) -> Result<String, String> {
        let url = apps::youtube_first_result_url(query)?;
        self.open_url(&url)
    }

    /// Set system output volume to 0–100%.
    fn set_volume(&self, percent: u8) -> Result<String, String>;

    /// Media transport: play | pause | next | prev.
    fn media_control(&self, action: &str) -> Result<String, String>;

    /// Install the low-level keyboard/mouse hook for the Smart talk key (idempotent).
    /// Runs on its own thread; events go to `sink` and must never block it.
    fn install_input_hook(&self, sink: hook::HookSink) -> Result<(), String>;

    /// Update the talk key / enabled flag of an installed hook. Cheap; any thread.
    fn configure_input_hook(&self, config: hook::HookConfig);

    /// Battery / fullscreen state for the animation battery saver. Cheap; call off the UI thread.
    fn power_state(&self) -> PowerState;

    /// Duck other apps' audio while Soul speaks: `Some(f)` scales every other audio session to
    /// `f` of its level (remembering the original), `None` restores. Blocking; worker threads only.
    fn duck_other_audio(&self, factor: Option<f32>) -> Result<(), String>;

    /// "Do Not Disturb" (notification banners off / on) for scenes. Best effort per desktop.
    fn set_do_not_disturb(&self, on: bool) -> Result<String, String>;

    /// Whether the focused element is an editable text field (Smart routing keeps short
    /// questions as dictation there). `None` = unknown. Bounded (~150 ms); worker threads only.
    fn text_input_focused(&self) -> Option<bool> {
        None
    }

    /// Current system output volume, 0–100 (relative „ścisz o 20”). Blocking; worker threads only.
    fn get_volume(&self) -> Result<u8, String>;

    /// Text selected in the focused app, for an explicitly invoked text action only. Windows:
    /// simulated copy with the clipboard saved and restored; X11: the PRIMARY selection.
    /// `Ok(None)` = nothing selected. Blocking (≤ ~0.6 s); worker threads only.
    fn read_selection(&self) -> Result<Option<String>, String>;

    /// Opaque id of the foreground window (to return focus after the Island was clicked).
    fn active_window(&self) -> Option<u64>;

    /// Bring a window from `active_window` back to the foreground. Best effort.
    fn activate_window(&self, id: u64) -> Result<(), String>;

    /// Send the app's own undo (Ctrl+Z) to the focused window.
    fn send_undo(&self) -> Result<(), String>;

    // ── Computer agent (docs/agent.md). All blocking and bounded; worker threads only. ──

    /// Interactive elements of the focused app window (accessibility tree), ids numbered.
    fn ui_snapshot(&self, opts: &ui::SnapshotOptions) -> Result<ui::UiSnapshot, String>;

    /// Pattern-level operation on element `id` of snapshot `gen` (no synthetic input).
    /// `Err(ui STALE)` when `gen` is outdated — the caller falls back to coordinates.
    fn ui_element_op(&self, gen: u64, id: u32, op: &ui::ElementOp) -> Result<(), String>;

    /// Cheap read (foreground window, focused element, the target's value/state) for settling.
    fn ui_quick_state(&self, gen: u64, target: Option<u32>) -> Result<ui::QuickState, String>;

    /// The app window the agent would act on (no tree walk).
    fn foreground_window(&self) -> Option<ui::WindowInfo>;

    /// One synthetic input event; sequences go through `input::execute` (abort + release).
    fn send_input_step(&self, step: &input::InputStep) -> Result<(), String>;

    /// Kill switch: Escape held + cursor position. Called every ~25 ms; must be cheap.
    fn input_probe(&self) -> input::InputProbe;

    /// Monitors in physical pixels with their scale factors.
    fn monitors(&self) -> Vec<coords::Monitor>;

    /// Open an allowlisted system settings page (`apps::settings_page`: sound, display…).
    fn open_settings_page(&self, page: &str) -> Result<String, String> {
        let _ = page;
        Err("Opening settings pages is not supported here.".into())
    }

    /// Turn one of Soul's windows into a click-through overlay that never takes focus
    /// (agent highlight ring). Best effort.
    fn prepare_overlay_window(&self, _window_id: u64) {}

    /// Show / hide such an overlay without activating it. `false` = not supported here (the
    /// caller uses the toolkit's show/hide).
    fn show_overlay_window(&self, _window_id: u64, _show: bool) -> bool {
        false
    }

    /// Keep one of Soul's windows out of screen captures during a task. Best effort.
    fn exclude_from_capture(&self, _window_id: u64, _on: bool) -> Result<(), String> {
        Ok(())
    }
}

/// Native id of a Tauri window (HWND on Windows) for `exclude_from_capture` / `activate_window`.
pub fn native_window_id(win: &tauri::WebviewWindow) -> Option<u64> {
    #[cfg(target_os = "windows")]
    {
        win.hwnd().ok().map(|h| h.0 as usize as u64)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = win;
        None
    }
}

pub fn create_platform() -> Arc<dyn Platform> {
    #[cfg(target_os = "windows")]
    {
        Arc::new(windows::WindowsPlatform::new())
    }

    #[cfg(target_os = "linux")]
    {
        if is_wayland() {
            Arc::new(linux_wayland::LinuxWaylandPlatform::new()) as Arc<dyn Platform>
        } else {
            Arc::new(linux_x11::LinuxX11Platform::new())
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        compile_error!("Soul currently supports Windows and Linux only");
    }
}

/// Run the calling thread below normal priority (background audio analysis, e.g. the wake word).
/// Best effort: failures are ignored.
pub fn lower_current_thread_priority() {
    #[cfg(target_os = "windows")]
    windows::lower_current_thread_priority();
    #[cfg(target_os = "linux")]
    {
        // Linux applies nice() to the calling thread only.
        // SAFETY: plain libc call without pointers.
        let _ = unsafe { libc::nice(10) };
    }
}

#[cfg(target_os = "linux")]
fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
}

/// Shared clipboard helpers used by platform backends.
pub mod clipboard {
    use arboard::Clipboard;
    use std::thread;
    use std::time::Duration;

    pub fn with_temporary_text<F>(text: &str, paste_action: F) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let mut clipboard =
            Clipboard::new().map_err(|e| format!("Clipboard unavailable: {e}"))?;

        let previous = clipboard.get_text().ok();

        clipboard
            .set_text(text)
            .map_err(|e| format!("Failed to set clipboard: {e}"))?;

        thread::sleep(Duration::from_millis(40));

        let paste_result = paste_action();

        thread::sleep(Duration::from_millis(80));

        match previous {
            Some(old) => {
                let _ = clipboard.set_text(old);
            }
            None => {
                let _ = clipboard.clear();
            }
        }

        paste_result
    }

    /// What was on the clipboard before Soul borrowed it (text or image; other formats are lost).
    #[cfg_attr(not(windows), allow(dead_code))] // Windows read_selection only
    pub enum Saved {
        Text(String),
        Image(arboard::ImageData<'static>),
        Empty,
    }

    #[cfg_attr(not(windows), allow(dead_code))] // Windows read_selection only
    pub fn save(clipboard: &mut Clipboard) -> Saved {
        if let Ok(t) = clipboard.get_text() {
            return Saved::Text(t);
        }
        match clipboard.get_image() {
            Ok(img) => Saved::Image(img.to_owned_img()),
            Err(_) => Saved::Empty,
        }
    }

    #[cfg_attr(not(windows), allow(dead_code))] // Windows read_selection only
    pub fn restore(clipboard: &mut Clipboard, saved: Saved) {
        let _ = match saved {
            Saved::Text(t) => clipboard.set_text(t),
            Saved::Image(img) => clipboard.set_image(img),
            Saved::Empty => clipboard.clear(),
        };
    }

    /// Volume parsed from `pactl get-sink-volume` ("… / 65% / …") or `amixer sget` ("[65%]").
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn parse_volume_percent(output: &str) -> Option<u8> {
        output.split(|c: char| c.is_whitespace() || c == '[' || c == ']' || c == '/').find_map(|tok| {
            let n = tok.strip_suffix('%')?;
            n.parse::<u16>().ok().map(|v| v.min(100) as u8)
        })
    }

    pub fn copy_only(text: &str) -> Result<(), String> {
        let mut clipboard =
            Clipboard::new().map_err(|e| format!("Clipboard unavailable: {e}"))?;
        clipboard
            .set_text(text)
            .map_err(|e| format!("Failed to set clipboard: {e}"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_volume_from_pactl_and_amixer() {
        let pactl = "Volume: front-left: 42598 /  65% / -11.23 dB,   front-right: 42598 /  65% / -11.23 dB";
        assert_eq!(super::clipboard::parse_volume_percent(pactl), Some(65));
        let amixer = "  Front Left: Playback 52428 [80%] [on]";
        assert_eq!(super::clipboard::parse_volume_percent(amixer), Some(80));
        assert_eq!(super::clipboard::parse_volume_percent("no volume here"), None);
    }
}
