// Linux spawns helpers (xdotool, pactl, xdg-open…) directly: no console windows to hide here.
#![allow(clippy::disallowed_methods)]

use super::apps::{
    collect_desktop_apps, letter_icon_data_url, pick_best, search_suggestions, validate_http_url,
    AppEntry, AppSuggestion,
};
use super::{clipboard, Platform};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use parking_lot::Mutex;
use std::process::Command;
use std::time::{Duration, Instant};

pub struct LinuxX11Platform {
    app_cache: Mutex<Option<(Instant, Vec<AppEntry>)>>,
}

impl LinuxX11Platform {
    pub fn new() -> Self {
        Self {
            app_cache: Mutex::new(None),
        }
    }

    fn paste_via_ctrl_v() -> Result<(), String> {
        if Command::new("xdotool")
            .args(["key", "--clearmodifiers", "ctrl+v"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Ok(());
        }

        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| format!("Failed to init input simulation: {e}"))?;
        enigo
            .key(Key::Control, Direction::Press)
            .map_err(|e| format!("Paste failed (Ctrl press): {e}"))?;
        enigo
            .key(Key::Unicode('v'), Direction::Click)
            .map_err(|e| format!("Paste failed (V): {e}"))?;
        enigo
            .key(Key::Control, Direction::Release)
            .map_err(|e| format!("Paste failed (Ctrl release): {e}"))?;
        Ok(())
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
        eprintln!("[soul][platform] indexed {} Linux .desktop apps", apps.len());
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

/// Sink inputs Soul ducked: (index, original volume %).
static DUCKED: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

/// `pactl list sink-inputs` (LC_ALL=C) → (index, application.process.id, first channel %).
pub(super) fn parse_sink_inputs(text: &str) -> Vec<(u32, String, u32)> {
    text.split("Sink Input #")
        .skip(1)
        .filter_map(|block| {
            let id: u32 = block.lines().next()?.trim().parse().ok()?;
            let pid = block
                .lines()
                .find_map(|l| l.trim().strip_prefix("application.process.id = "))
                .map(|v| v.trim_matches('"').to_string())
                .unwrap_or_default();
            let percent = block
                .lines()
                .find(|l| l.trim_start().starts_with("Volume:"))
                .and_then(|l| l.split_whitespace().find(|t| t.ends_with('%')))
                .and_then(|t| t.trim_end_matches('%').parse().ok())
                .unwrap_or(100);
            Some((id, pid, percent))
        })
        .collect()
}

/// Duck / restore every other app's PulseAudio / PipeWire stream (X11 and Wayland).
pub(super) fn pactl_duck(factor: Option<f32>) -> Result<(), String> {
    let mut saved = DUCKED.lock();
    let set = |id: u32, pct: u32| {
        Command::new("pactl")
            .args(["set-sink-input-volume", &id.to_string(), &format!("{pct}%")])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    match factor {
        Some(f) => {
            let out = Command::new("pactl")
                .env("LC_ALL", "C")
                .args(["list", "sink-inputs"])
                .output()
                .map_err(|e| format!("pactl unavailable ({e}) — install pulseaudio-utils."))?;
            if !out.status.success() {
                return Err("pactl list sink-inputs failed — is PulseAudio/PipeWire running?".into());
            }
            let own = std::process::id().to_string();
            for (id, pid, pct) in parse_sink_inputs(&String::from_utf8_lossy(&out.stdout)) {
                if pid == own || saved.iter().any(|(s, _)| *s == id) {
                    continue;
                }
                if set(id, ((pct as f32) * f).round() as u32) {
                    saved.push((id, pct));
                }
            }
        }
        None => {
            for (id, pct) in saved.drain(..) {
                set(id, pct);
            }
        }
    }
    Ok(())
}

/// GNOME banners via gsettings, else dunst.
pub(super) fn linux_do_not_disturb(on: bool) -> Result<String, String> {
    let gnome = Command::new("gsettings")
        .args([
            "set",
            "org.gnome.desktop.notifications",
            "show-banners",
            if on { "false" } else { "true" },
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let dunst = !gnome
        && Command::new("dunstctl")
            .args(["set-paused", if on { "true" } else { "false" }])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    if gnome || dunst {
        Ok(if on {
            "Powiadomienia wyciszone (Nie przeszkadzać)".into()
        } else {
            "Powiadomienia włączone".into()
        })
    } else {
        Err("Nie przeszkadzać działa z GNOME (gsettings) albo dunst.".into())
    }
}

impl Platform for LinuxX11Platform {
    fn power_state(&self) -> super::PowerState {
        super::PowerState {
            on_battery: sysfs_on_battery(),
            fullscreen: x11_fullscreen(),
        }
    }

    fn type_text(&self, text: &str) -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| format!("Failed to init input simulation: {e}"))?;
        enigo
            .text(text)
            .map_err(|e| format!("Failed to type text: {e}"))
    }

    fn paste_text(&self, text: &str) -> Result<(), String> {
        clipboard::with_temporary_text(text, Self::paste_via_ctrl_v)
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
        // Prefer wmctrl close-by-title; fall back to pkill -x on matched desktop binary.
        if Command::new("wmctrl")
            .args(["-c", name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Ok(format!("Closed {name}"));
        }
        let apps = self.indexed_apps();
        if let Some(entry) = pick_best(name, &apps) {
            let bin = entry
                .target
                .split_whitespace()
                .next()
                .unwrap_or(name)
                .rsplit('/')
                .next()
                .unwrap_or(name);
            let status = Command::new("pkill")
                .args(["-x", bin])
                .status()
                .map_err(|e| format!("Failed to close '{name}': {e}"))?;
            if status.success() {
                return Ok(format!("Closed {}", entry.name));
            }
        }
        Err(format!(
            "Could not close '{name}'. Install wmctrl or ensure the app is running."
        ))
    }

    fn focus_app(&self, name: &str) -> Result<String, String> {
        if Command::new("wmctrl")
            .args(["-a", name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Ok(format!("Focused {name}"));
        }
        Err(format!(
            "Could not focus '{name}'. Install wmctrl for window focus on X11."
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

    fn get_volume(&self) -> Result<u8, String> {
        read_volume()
    }

    fn read_selection(&self) -> Result<Option<String>, String> {
        use arboard::{GetExtLinux, LinuxClipboardKind};
        // X11 keeps the highlighted text in PRIMARY: no simulated copy, the clipboard is untouched.
        let mut cb =
            arboard::Clipboard::new().map_err(|e| format!("Clipboard unavailable: {e}"))?;
        match cb.get().clipboard(LinuxClipboardKind::Primary).text() {
            Ok(t) => Ok(Some(t).filter(|t| !t.trim().is_empty())),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(format!("Could not read the selection: {e}")),
        }
    }

    fn active_window(&self) -> Option<u64> {
        let out = Command::new("xdotool").arg("getactivewindow").output().ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }

    fn activate_window(&self, id: u64) -> Result<(), String> {
        let ok = Command::new("xdotool")
            .args(["windowactivate", "--sync", &id.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            Err("Could not focus the window (install xdotool).".into())
        }
    }

    fn send_undo(&self) -> Result<(), String> {
        if Command::new("xdotool")
            .args(["key", "--clearmodifiers", "ctrl+z"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Ok(());
        }
        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| format!("Failed to init input simulation: {e}"))?;
        enigo
            .key(Key::Control, Direction::Press)
            .map_err(|e| format!("Undo failed (Ctrl press): {e}"))?;
        let z = enigo.key(Key::Unicode('z'), Direction::Click);
        let up = enigo.key(Key::Control, Direction::Release);
        z.map_err(|e| format!("Undo failed (Z): {e}"))?;
        up.map_err(|e| format!("Undo failed (Ctrl release): {e}"))
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
        if Command::new("amixer")
            .args(["-q", "sset", "Master", &format!("{percent}%")])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Ok(format!("Volume set to {percent}%"));
        }
        Err("Could not set volume. Install pactl (pulseaudio/pipewire) or amixer.".into())
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
        pactl_duck(factor.map(|f| f.clamp(0.0, 1.0)))
    }

    fn set_do_not_disturb(&self, on: bool) -> Result<String, String> {
        linux_do_not_disturb(on)
    }

    fn install_input_hook(&self, sink: super::hook::HookSink) -> Result<(), String> {
        super::x11_hook::install(sink)
    }

    fn configure_input_hook(&self, config: super::hook::HookConfig) {
        super::x11_hook::configure(config);
    }
}

/// Cached X connection + the three EWMH atoms (connecting every poll is wasteful).
struct FullscreenProbe {
    conn: x11rb::rust_connection::RustConnection,
    root: u32,
    active: u32,
    state: u32,
    fullscreen: u32,
}

static FULLSCREEN_PROBE: once_cell::sync::Lazy<Mutex<Option<FullscreenProbe>>> = once_cell::sync::Lazy::new(|| Mutex::new(None));

impl FullscreenProbe {
    fn connect() -> Option<Self> {
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::ConnectionExt;
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let atom = |name: &[u8]| -> Option<u32> { Some(conn.intern_atom(false, name).ok()?.reply().ok()?.atom) };
        let active = atom(b"_NET_ACTIVE_WINDOW")?;
        let state = atom(b"_NET_WM_STATE")?;
        let fullscreen = atom(b"_NET_WM_STATE_FULLSCREEN")?;
        Some(Self { conn, root, active, state, fullscreen })
    }

    fn query(&self) -> Option<bool> {
        use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
        let win = self
            .conn
            .get_property(false, self.root, self.active, AtomEnum::WINDOW, 0, 1)
            .ok()?
            .reply()
            .ok()?
            .value32()?
            .next()
            .unwrap_or(0);
        if win == 0 {
            return Some(false);
        }
        let states = self
            .conn
            .get_property(false, win, self.state, AtomEnum::ATOM, 0, 64)
            .ok()?
            .reply()
            .ok()?;
        let fullscreen = states.value32()?.any(|a| a == self.fullscreen);
        Some(fullscreen)
    }
}

/// EWMH: the active window has `_NET_WM_STATE_FULLSCREEN`. None = no X / no EWMH WM.
fn x11_fullscreen() -> Option<bool> {
    let mut probe = FULLSCREEN_PROBE.lock();
    if probe.is_none() {
        *probe = FullscreenProbe::connect();
    }
    let result = probe.as_ref()?.query();
    if result.is_none() {
        // Connection broke (X restart…): reconnect next time.
        *probe = None;
    }
    result
}

/// True when a battery is present and no AC adapter reports `online` (Linux sysfs).
pub(super) fn sysfs_on_battery() -> Option<bool> {
    let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
    let mut has_battery = false;
    let mut ac_online = false;
    for entry in dir.flatten() {
        let path = entry.path();
        let kind = std::fs::read_to_string(path.join("type")).unwrap_or_default();
        match kind.trim() {
            "Battery" => has_battery = true,
            "Mains" | "USB"
                if std::fs::read_to_string(path.join("online")).map(|v| v.trim() == "1").unwrap_or(false) =>
            {
                ac_online = true;
            }
            _ => {}
        }
    }
    has_battery.then_some(!ac_online)
}

/// Default sink volume via `pactl` (PipeWire/PulseAudio), else ALSA `amixer`. Shared with Wayland.
pub(super) fn read_volume() -> Result<u8, String> {
    for (program, args) in [
        ("pactl", &["get-sink-volume", "@DEFAULT_SINK@"][..]),
        ("amixer", &["sget", "Master"][..]),
    ] {
        if let Ok(out) = Command::new(program).args(args).output() {
            if out.status.success() {
                if let Some(v) = clipboard::parse_volume_percent(&String::from_utf8_lossy(&out.stdout)) {
                    return Ok(v);
                }
            }
        }
    }
    Err("Could not read the volume. Install pactl (pulseaudio/pipewire) or amixer.".into())
}

#[cfg(test)]
mod tests {
    use super::parse_sink_inputs;

    /// Trimmed `LC_ALL=C pactl list sink-inputs` (PipeWire): Firefox, Soul itself (mono), Spotify.
    fn sample(own_pid: u32) -> String {
        format!(
            r#"Sink Input #42
	Driver: PipeWire
	Owner Module: n/a
	Client: 71
	Sink: 55
	Sample Specification: float32le 2ch 48000Hz
	Channel Map: front-left,front-right
	Format: pcm, format.sample_format = "\"float32le\""  format.rate = "48000"  format.channels = "2"  format.channel_map = "\"front-left,front-right\""
	Corked: no
	Mute: no
	Volume: front-left: 52429 /  80% / -5.81 dB,   front-right: 52429 /  80% / -5.81 dB
	        balance 0.00
	Buffer Latency: 0 usec
	Sink Latency: 0 usec
	Resample method: PipeWire
	Properties:
		media.name = "AudioStream"
		application.name = "Firefox"
		application.process.id = "2711"
		application.process.binary = "firefox"
		media.class = "Stream/Output/Audio"

Sink Input #57
	Driver: PipeWire
	Owner Module: n/a
	Client: 90
	Sink: 55
	Sample Specification: s16le 1ch 24000Hz
	Channel Map: mono
	Corked: no
	Mute: no
	Volume: mono: 65536 / 100% / 0.00 dB
	        balance 0.00
	Properties:
		media.name = "ALSA Playback"
		application.name = "ALSA plug-in [soul]"
		application.process.id = "{own_pid}"
		application.process.binary = "soul"

Sink Input #63
	Driver: PipeWire
	Owner Module: n/a
	Client: 94
	Sink: 55
	Sample Specification: float32le 2ch 44100Hz
	Channel Map: front-left,front-right
	Corked: no
	Mute: no
	Volume: front-left: 98304 / 150% / 10.57 dB,   front-right: 98304 / 150% / 10.57 dB
	        balance 0.00
	Properties:
		application.name = "spotify"
		application.process.id = "3302"
"#
        )
    }

    #[test]
    fn parses_index_pid_and_volume() {
        let parsed = parse_sink_inputs(&sample(4242));
        assert_eq!(
            parsed,
            vec![
                (42, "2711".to_string(), 80),
                (57, "4242".to_string(), 100),
                (63, "3302".to_string(), 150),
            ]
        );
    }

    #[test]
    fn own_stream_is_identifiable_for_skipping() {
        let own = std::process::id();
        let parsed = parse_sink_inputs(&sample(own));
        let others: Vec<u32> = parsed
            .iter()
            .filter(|(_, pid, _)| *pid != own.to_string())
            .map(|(id, _, _)| *id)
            .collect();
        assert_eq!(others, vec![42, 63]);
    }

    #[test]
    fn empty_or_garbage_output_yields_nothing() {
        assert!(parse_sink_inputs("").is_empty());
        assert!(parse_sink_inputs("Connection failure: Connection refused\n").is_empty());
    }
}
