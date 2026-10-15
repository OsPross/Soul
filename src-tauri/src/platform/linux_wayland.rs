// Linux spawns helpers (xdotool, pactl, xdg-open…) directly: no console windows to hide here.
#![allow(clippy::disallowed_methods)]

use super::apps::{
    collect_desktop_apps, letter_icon_data_url, pick_best, search_suggestions, validate_http_url,
    AppEntry, AppSuggestion,
};
use super::{clipboard, Platform};
use parking_lot::Mutex;
use std::process::Command;
use std::time::{Duration, Instant};

pub struct LinuxWaylandPlatform {
    app_cache: Mutex<Option<(Instant, Vec<AppEntry>)>>,
}

impl LinuxWaylandPlatform {
    pub fn new() -> Self {
        Self {
            app_cache: Mutex::new(None),
        }
    }

    fn try_paste_tool() -> Result<(), String> {
        if Command::new("wtype")
            .args(["-M", "ctrl", "-k", "v", "-m", "ctrl"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Ok(());
        }

        if Command::new("ydotool")
            .args(["key", "29:1", "47:1", "47:0", "29:0"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Ok(());
        }

        Err(
            "Could not simulate paste on Wayland. Install `wtype` or `ydotool`, or paste manually from the clipboard."
                .into(),
        )
    }

    fn indexed_apps(&self) -> Vec<AppEntry> {
        const TTL: Duration = Duration::from_secs(10 * 60);
        {
            let guard = self.app_cache.lock();
            if let Some((at, apps)) = guard.as_ref() {
                if at.elapsed() < TTL {
                    return apps.clone();
                }
            }
        }
        let mut apps = Vec::new();
        collect_desktop_apps(&mut apps);
        *self.app_cache.lock() = Some((Instant::now(), apps.clone()));
        apps
    }

    fn launch_exec(exec_line: &str) -> Result<(), String> {
        let mut parts = exec_line.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| "Empty Exec line".to_string())?;
        let args: Vec<&str> = parts.collect();
        Command::new(program)
            .args(args)
            .spawn()
            .map_err(|e| format!("Failed to launch '{program}': {e}"))?;
        Ok(())
    }
}

impl Platform for LinuxWaylandPlatform {
    fn power_state(&self) -> super::PowerState {
        // Fullscreen detection is not available to clients on Wayland.
        super::PowerState {
            on_battery: super::linux_x11::sysfs_on_battery(),
            fullscreen: None,
        }
    }

    fn type_text(&self, text: &str) -> Result<(), String> {
        let status = Command::new("wtype")
            .arg(text)
            .status()
            .map_err(|e| {
                format!("wtype unavailable ({e}). Install wtype to type text on Wayland.")
            })?;
        if status.success() {
            Ok(())
        } else {
            Err("wtype failed to type text".into())
        }
    }

    fn paste_text(&self, text: &str) -> Result<(), String> {
        match clipboard::with_temporary_text(text, Self::try_paste_tool) {
            Ok(()) => Ok(()),
            Err(e) => {
                clipboard::copy_only(text)?;
                Err(format!(
                    "{e} Text was copied to the clipboard — paste with Ctrl+V."
                ))
            }
        }
    }

    fn capture_screen(&self) -> Result<Vec<u8>, String> {
        Ok(crate::capture::capture_monitor_under_cursor()?.png)
    }

    fn open_app(&self, name: &str) -> Result<String, String> {
        let apps = self.indexed_apps();
        let entry = pick_best(name, &apps)
            .ok_or_else(|| format!("No installed app matched '{name}'."))?;
        Self::launch_exec(&entry.target)?;
        Ok(format!("Opened {}", entry.name))
    }

    fn search_apps(&self, query: &str, limit: usize) -> Vec<AppSuggestion> {
        let apps = self.indexed_apps();
        let mut suggestions = search_suggestions(query, &apps, limit);
        if suggestions.is_empty() {
            if let Some(entry) = pick_best(query, &apps) {
                suggestions.push(AppSuggestion {
                    name: entry.name.clone(),
                    icon_data_url: letter_icon_data_url(&entry.name),
                });
            }
        }
        suggestions
    }

    fn close_app(&self, name: &str) -> Result<String, String> {
        Err(format!(
            "close_app is not supported on Wayland yet (asked to close '{name}')."
        ))
    }

    fn focus_app(&self, name: &str) -> Result<String, String> {
        Err(format!(
            "focus_app is not supported on Wayland yet (asked to focus '{name}')."
        ))
    }

    fn open_url(&self, url: &str) -> Result<String, String> {
        let url = validate_http_url(url)?;
        Command::new("xdg-open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("Failed to open URL (xdg-open): {e}"))?;
        Ok(format!("Opened {url}"))
    }

    fn set_volume(&self, percent: u8) -> Result<String, String> {
        let percent = percent.min(100);
        if Command::new("pactl")
            .args([
                "set-sink-volume",
                "@DEFAULT_SINK@",
                &format!("{percent}%"),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Ok(format!("Volume set to {percent}%"));
        }
        Err("Could not set volume on Wayland. Install pactl (pipewire/pulseaudio).".into())
    }

    fn media_control(&self, action: &str) -> Result<String, String> {
        let arg = match action.trim().to_ascii_lowercase().as_str() {
            "play" => "play",
            "pause" => "pause",
            "play_pause" | "playpause" => "play-pause",
            "next" => "next",
            "prev" | "previous" => "previous",
            other => {
                return Err(format!(
                    "Unknown media action '{other}'. Use play, pause, next, or prev."
                ));
            }
        };
        let status = Command::new("playerctl")
            .arg(arg)
            .status()
            .map_err(|e| format!("playerctl unavailable ({e}). Install playerctl for media keys."))?;
        if status.success() {
            Ok(match arg {
                "next" => "Next track".into(),
                "previous" => "Previous track".into(),
                "play" => "Playing".into(),
                "pause" => "Paused".into(),
                _ => "Play/Pause".into(),
            })
        } else {
            Err("playerctl failed — is a media player running?".into())
        }
    }

    fn duck_other_audio(&self, factor: Option<f32>) -> Result<(), String> {
        super::linux_x11::pactl_duck(factor.map(|f| f.clamp(0.0, 1.0)))
    }

    fn set_do_not_disturb(&self, on: bool) -> Result<String, String> {
        super::linux_x11::linux_do_not_disturb(on)
    }

    fn install_input_hook(&self, _sink: super::hook::HookSink) -> Result<(), String> {
        // Wayland has no global input snooping for unprivileged clients.
        Err("The single talk key needs X11 (Wayland blocks global key hooks). Using the separate hotkeys instead.".into())
    }

    fn configure_input_hook(&self, _config: super::hook::HookConfig) {}
}
