//! Local Spotify client: installed / running, silent background launch, its own mixer volume.
//! No account needed; the Web API lives in `api.rs`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Spotify's executable: the installer build or the Microsoft Store alias (a zero-size reparse
/// point, so only `symlink_metadata` sees it).
#[cfg(windows)]
pub fn exe_path() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Spotify").join("Spotify.exe")),
        std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("Microsoft").join("WindowsApps").join("Spotify.exe")),
    ];
    candidates.into_iter().flatten().find(|p| std::fs::symlink_metadata(p).is_ok())
}

#[cfg(target_os = "linux")]
pub fn exe_path() -> Option<PathBuf> {
    ["/usr/bin/spotify", "/snap/bin/spotify", "/var/lib/flatpak/exports/bin/com.spotify.Client"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn exe_path() -> Option<PathBuf> {
    None
}

pub fn is_installed() -> bool {
    exe_path().is_some()
}

pub fn is_running() -> bool {
    #[cfg(windows)]
    {
        crate::platform::windows::process_running("Spotify.exe")
    }
    #[cfg(not(windows))]
    {
        std::fs::read_dir("/proc").is_ok_and(|dir| {
            dir.flatten().any(|e| {
                std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "spotify")
            })
        })
    }
}

/// Start Spotify in the tray / background: no foreground window, no focus steal.
#[allow(clippy::disallowed_methods)] // Linux spawn; Windows goes through `hidden_command`
pub fn launch_spotify_minimized() -> Result<(), String> {
    let exe = exe_path().ok_or("Spotify is not installed.")?;
    eprintln!("[soul][platform] starting Spotify in the background");
    #[cfg(windows)]
    let mut cmd = crate::platform::windows::hidden_command(&exe.to_string_lossy());
    #[cfg(not(windows))]
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--minimized")
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not start Spotify: {e}"))
}

/// Launch Spotify in the background unless it already runs. `Ok(true)` = it was started now.
pub fn ensure_spotify_running_background() -> Result<bool, String> {
    if is_running() {
        return Ok(false);
    }
    launch_spotify_minimized()?;
    Ok(true)
}

/// Poll `ready` every `step` until it holds or `timeout` passes.
pub fn wait_for(timeout: Duration, step: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if ready() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(step);
    }
}

/// Spotify's own mixer volume, 0–100 (`None` while it has no audio session).
pub fn get_spotify_volume() -> Option<u8> {
    #[cfg(windows)]
    let level = crate::platform::windows::app_volume("Spotify.exe", None).ok().flatten().map(f64::from);
    #[cfg(target_os = "linux")]
    let level = crate::island::media::spotify_mpris_volume(None).ok();
    #[cfg(not(any(windows, target_os = "linux")))]
    let level: Option<f64> = None;
    level.map(|v| (v * 100.0).round().clamp(0.0, 100.0) as u8)
}

/// Set Spotify's own volume (not the system one). Returns the new level.
pub fn set_spotify_volume(percent: u8) -> Result<u8, String> {
    let v = f64::from(percent.min(100)) / 100.0;
    #[cfg(windows)]
    let level = crate::platform::windows::app_volume("Spotify.exe", Some(v as f32))?
        .map(f64::from)
        .ok_or("Spotify has no audio session (start playback first).")?;
    #[cfg(target_os = "linux")]
    let level = crate::island::media::spotify_mpris_volume(Some(v))?;
    #[cfg(not(any(windows, target_os = "linux")))]
    let level: f64 = {
        let _ = v;
        return Err("Spotify volume is unsupported on this platform.".into());
    };
    Ok((level * 100.0).round().clamp(0.0, 100.0) as u8)
}
