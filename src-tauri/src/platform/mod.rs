//! OS-specific helpers for paste, capture, and voice-command tools.

use std::sync::Arc;

pub mod apps;
pub mod hook;

#[cfg(target_os = "linux")]
mod linux_wayland;
#[cfg(target_os = "linux")]
mod linux_x11;
#[cfg(target_os = "linux")]
mod x11_hook;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
mod windows_hook;

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
}

pub fn create_platform() -> Arc<dyn Platform> {
    #[cfg(target_os = "windows")]
    {
        Arc::new(windows::WindowsPlatform::new())
    }

    #[cfg(target_os = "linux")]
    {
        if is_wayland() {
            return Arc::new(linux_wayland::LinuxWaylandPlatform::new());
        }
        return Arc::new(linux_x11::LinuxX11Platform::new());
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

    #[allow(dead_code)] // used by Wayland paste fallback
    pub fn copy_only(text: &str) -> Result<(), String> {
        let mut clipboard =
            Clipboard::new().map_err(|e| format!("Clipboard unavailable: {e}"))?;
        clipboard
            .set_text(text)
            .map_err(|e| format!("Failed to set clipboard: {e}"))
    }
}
