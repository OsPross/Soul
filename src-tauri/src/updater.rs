//! Auto-update: signed releases from GitHub (tauri-plugin-updater + `latest.json`).
//!
//! - Checks 30 s after startup and every 6 h while `auto_update` is on (manual checks always work).
//! - Channel `stable` = `releases/latest/download/latest.json` (GitHub's "latest" skips prereleases
//!   and drafts); `beta` = the newest published release *including* prereleases (GitHub API).
//! - `auto` installs (NSIS/MSI on Windows, AppImage on Linux) download in the background and install
//!   only on the user's click and only while Soul is idle (no recording, speaking, Ask or busy turn).
//!   `manual` installs (.deb / .rpm / dev builds) only link to the release page.
//! - State is mirrored to every window as `soul://update` (`UpdateStatus`).
use crate::config::{load_settings, save_settings};
use crate::AppState;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

const REPO: &str = "OsPross/Soul";
const STABLE_ENDPOINT: &str = "https://github.com/OsPross/Soul/releases/latest/download/latest.json";
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

pub const CHANNEL_STABLE: &str = "stable";
pub const CHANNEL_BETA: &str = "beta";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Ready,
    /// Install requested; waiting for Soul to go idle.
    Waiting,
    Installing,
    /// Installed (Linux AppImage) — restart to run the new version.
    Installed,
    Error,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub phase: Phase,
    pub current_version: String,
    pub version: Option<String>,
    pub notes: Option<String>,
    pub date: Option<String>,
    pub release_url: Option<String>,
    /// `auto` = Soul installs it itself; `manual` = open the release page (.deb / .rpm / dev build).
    pub install: &'static str,
    /// `nsis` | `msi` | `appimage` | `deb` | `rpm` | `dev`
    pub bundle: &'static str,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
    /// Unix ms of the last finished check.
    pub last_check: Option<i64>,
    pub auto_update: bool,
    pub channel: String,
}

struct Inner {
    status: UpdateStatus,
    update: Option<Update>,
    bytes: Option<Vec<u8>>,
}

static STATE: Lazy<Mutex<Option<Inner>>> = Lazy::new(|| Mutex::new(None));
static CHECKING: AtomicBool = AtomicBool::new(false);
static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static INSTALLING: AtomicBool = AtomicBool::new(false);

fn bundle_name() -> &'static str {
    use tauri::utils::{config::BundleType, platform::bundle_type};
    match bundle_type() {
        Some(BundleType::Nsis) => "nsis",
        Some(BundleType::Msi) => "msi",
        Some(BundleType::AppImage) => "appimage",
        Some(BundleType::Deb) => "deb",
        Some(BundleType::Rpm) => "rpm",
        _ if cfg!(target_os = "linux") && std::env::var_os("APPIMAGE").is_some() => "appimage",
        _ => "dev",
    }
}

/// Only Windows installers and the AppImage update themselves; packages from a package manager
/// (.deb / .rpm) get a download link, as does a dev build (nothing to replace).
fn install_kind(bundle: &str) -> &'static str {
    match bundle {
        "nsis" | "msi" | "appimage" => "auto",
        _ => "manual",
    }
}

pub fn normalize_channel(channel: &str) -> String {
    if channel.trim().eq_ignore_ascii_case(CHANNEL_BETA) {
        CHANNEL_BETA.into()
    } else {
        CHANNEL_STABLE.into()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn with_state<T>(f: impl FnOnce(&mut Inner) -> T) -> Option<T> {
    STATE.lock().as_mut().map(f)
}

fn emit(app: &AppHandle) {
    if let Some(status) = with_state(|s| s.status.clone()) {
        let _ = app.emit("soul://update", status);
    }
}

fn set_phase(app: &AppHandle, phase: Phase, error: Option<String>) {
    with_state(|s| {
        s.status.phase = phase;
        s.status.error = error;
    });
    emit(app);
}

/// Recording, speaking, Circle-to-Ask or a running turn — never interrupt those with an install.
pub fn soul_is_busy(app: &AppHandle) -> bool {
    let Some(state) = app.try_state::<AppState>() else {
        return false;
    };
    *state.busy.lock()
        || state.recorder.is_capturing()
        || state.voice.is_speaking()
        || crate::ask::is_active(&state.ask)
}

pub fn init(app: &AppHandle) {
    let settings = load_settings(app).unwrap_or_default();
    let bundle = bundle_name();
    *STATE.lock() = Some(Inner {
        status: UpdateStatus {
            phase: Phase::Idle,
            current_version: app.package_info().version.to_string(),
            version: None,
            notes: None,
            date: None,
            release_url: None,
            install: install_kind(bundle),
            bundle,
            downloaded: 0,
            total: None,
            error: None,
            last_check: None,
            auto_update: settings.auto_update,
            channel: normalize_channel(&settings.update_channel),
        },
        update: None,
        bytes: None,
    });
    eprintln!(
        "[soul][update] v{} bundle={bundle} install={} auto={} channel={}",
        app.package_info().version,
        install_kind(bundle),
        settings.auto_update,
        settings.update_channel
    );

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            let auto = load_settings(&app).map(|s| s.auto_update).unwrap_or(true);
            if auto {
                if let Err(e) = check(&app, false).await {
                    eprintln!("[soul][update] background check failed: {e}");
                }
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

/// `latest.json` URL for the channel. Beta asks the GitHub API for the newest published release
/// (prereleases included; the list is newest first and never contains drafts for anonymous calls).
async fn endpoint_for(channel: &str) -> Result<url::Url, String> {
    if channel != CHANNEL_BETA {
        return url::Url::parse(STABLE_ENDPOINT).map_err(|e| e.to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(concat!("Soul/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let releases: Vec<serde_json::Value> = client
        .get(format!("https://api.github.com/repos/{REPO}/releases?per_page=15"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("GitHub releases: {}", e.without_url()))?
        .json()
        .await
        .map_err(|e| format!("GitHub releases: {}", e.without_url()))?;
    let tag = releases
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter(|r| {
            r["assets"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x["name"] == "latest.json"))
        })
        .find_map(|r| r["tag_name"].as_str())
        .ok_or_else(|| "No published release with latest.json yet".to_string())?;
    url::Url::parse(&format!(
        "https://github.com/{REPO}/releases/download/{tag}/latest.json"
    ))
    .map_err(|e| e.to_string())
}

/// Check for an update on the configured channel. `manual` = the user pressed the button (errors
/// are shown); background checks fail quietly and start the download when auto-update is on.
pub async fn check(app: &AppHandle, manual: bool) -> Result<UpdateStatus, String> {
    if INSTALLING.load(Ordering::SeqCst) || DOWNLOADING.load(Ordering::SeqCst) {
        return status().ok_or_else(|| "updater not ready".to_string());
    }
    if CHECKING.swap(true, Ordering::SeqCst) {
        return status().ok_or_else(|| "updater not ready".to_string());
    }
    let started = Instant::now();
    let prev_phase = with_state(|s| s.status.phase).unwrap_or(Phase::Idle);
    // A downloaded update stays ready unless the check finds something newer.
    let keep_ready = matches!(prev_phase, Phase::Ready | Phase::Installed);
    if manual && !keep_ready {
        set_phase(app, Phase::Checking, None);
    }
    let settings = load_settings(app).unwrap_or_default();
    let channel = normalize_channel(&settings.update_channel);
    let result = async {
        let endpoint = endpoint_for(&channel).await?;
        let mut builder = app
            .updater_builder()
            .endpoints(vec![endpoint])
            .map_err(|e| e.to_string())?
            .timeout(HTTP_TIMEOUT);
        if let Some(appimage) = std::env::var_os("APPIMAGE") {
            builder = builder.executable_path(appimage);
        }
        builder
            .build()
            .map_err(|e| e.to_string())?
            .check()
            .await
            .map_err(|e| match e {
                // 404 on latest.json: nothing published on this channel yet.
                tauri_plugin_updater::Error::ReleaseNotFound => {
                    "No published release found on this channel yet".to_string()
                }
                e => e.to_string(),
            })
    }
    .await;
    CHECKING.store(false, Ordering::SeqCst);

    let found = match result {
        Ok(found) => found,
        Err(e) => {
            eprintln!("[soul][update] check failed ({channel}): {e}");
            if manual {
                set_phase(app, Phase::Error, Some(e.clone()));
            } else if prev_phase == Phase::Checking {
                set_phase(app, Phase::Idle, None);
            }
            return Err(e);
        }
    };
    eprintln!(
        "[soul][update] check ({channel}) in {} ms → {}",
        started.elapsed().as_millis(),
        found.as_ref().map(|u| u.version.as_str()).unwrap_or("up to date")
    );

    let mut start_download = false;
    with_state(|s| {
        s.status.last_check = Some(now_ms());
        s.status.channel = channel.clone();
        s.status.error = None;
        match found {
            Some(update) => {
                let same = s.status.version.as_deref() == Some(update.version.as_str());
                if !(same && keep_ready) {
                    s.bytes = None;
                    s.status.phase = Phase::Available;
                    s.status.downloaded = 0;
                    s.status.total = None;
                }
                s.status.version = Some(update.version.clone());
                s.status.notes = update.body.clone().filter(|b| !b.trim().is_empty());
                s.status.date = update.raw_json["pub_date"].as_str().map(str::to_string);
                s.status.release_url = Some(format!(
                    "https://github.com/{REPO}/releases/tag/v{}",
                    update.version
                ));
                start_download = settings.auto_update
                    && s.status.install == "auto"
                    && s.status.phase == Phase::Available;
                s.update = Some(update);
            }
            None if !keep_ready => {
                s.status.phase = Phase::UpToDate;
                s.status.version = None;
                s.status.notes = None;
                s.status.date = None;
                s.status.release_url = None;
                s.update = None;
                s.bytes = None;
            }
            None => {}
        }
    });
    emit(app);
    if start_download {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = download(&app).await;
        });
    }
    status().ok_or_else(|| "updater not ready".to_string())
}

/// Download (and verify) the pending update. Idempotent: a second call waits for the first.
pub async fn download(app: &AppHandle) -> Result<(), String> {
    if with_state(|s| s.bytes.is_some()).unwrap_or(false) {
        return Ok(());
    }
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        while DOWNLOADING.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        return if with_state(|s| s.bytes.is_some()).unwrap_or(false) {
            Ok(())
        } else {
            Err(with_state(|s| s.status.error.clone())
                .flatten()
                .unwrap_or_else(|| "download failed".into()))
        };
    }
    let Some(update) = with_state(|s| s.update.clone()).flatten() else {
        DOWNLOADING.store(false, Ordering::SeqCst);
        return Err("No update to download".into());
    };
    with_state(|s| {
        s.status.phase = Phase::Downloading;
        s.status.downloaded = 0;
        s.status.total = None;
        s.status.error = None;
    });
    emit(app);
    eprintln!("[soul][update] downloading v{}", update.version);

    let mut last_emit = Instant::now();
    let progress_app = app.clone();
    let result = update
        .download(
            move |chunk, total| {
                with_state(|s| {
                    s.status.downloaded += chunk as u64;
                    s.status.total = total;
                });
                if last_emit.elapsed() >= Duration::from_millis(150) {
                    last_emit = Instant::now();
                    emit(&progress_app);
                }
            },
            || {},
        )
        .await;
    let out = match result {
        Ok(bytes) => {
            eprintln!("[soul][update] downloaded {} bytes (signature ok)", bytes.len());
            with_state(|s| {
                s.status.downloaded = bytes.len() as u64;
                s.bytes = Some(bytes);
                s.status.phase = Phase::Ready;
            });
            emit(app);
            Ok(())
        }
        Err(e) => {
            let e = e.to_string();
            eprintln!("[soul][update] download failed: {e}");
            set_phase(app, Phase::Error, Some(e.clone()));
            Err(e)
        }
    };
    DOWNLOADING.store(false, Ordering::SeqCst);
    out
}

/// Download if needed, wait until Soul is idle, then install. Windows: the installer takes over
/// and the app exits (it is relaunched afterwards). Linux AppImage: the file is replaced and the
/// frontend relaunches via the process plugin.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    if with_state(|s| s.status.install).unwrap_or("manual") != "auto" {
        return Err("This installation is updated by downloading the new package".into());
    }
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("Update is already being installed".into());
    }
    let result = install_inner(app).await;
    INSTALLING.store(false, Ordering::SeqCst);
    if let Err(e) = &result {
        eprintln!("[soul][update] install failed: {e}");
        set_phase(app, Phase::Error, Some(e.clone()));
        crate::push_error_log(app, format!("Update: {e}"));
    }
    result
}

async fn install_inner(app: &AppHandle) -> Result<(), String> {
    if with_state(|s| s.status.phase == Phase::Installed).unwrap_or(false) {
        return Ok(());
    }
    download(app).await?;
    if soul_is_busy(app) {
        set_phase(app, Phase::Waiting, None);
        eprintln!("[soul][update] install waits for Soul to go idle");
        while soul_is_busy(app) {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    let (update, bytes) = with_state(|s| (s.update.clone(), s.bytes.take()))
        .ok_or("updater not ready")?;
    let (Some(update), Some(bytes)) = (update, bytes) else {
        return Err("Update is not downloaded".into());
    };
    set_phase(app, Phase::Installing, None);
    eprintln!("[soul][update] installing v{}", update.version);
    // Windows: launches the installer and exits the process inside `install`.
    let res = tauri::async_runtime::spawn_blocking(move || update.install(bytes))
        .await
        .map_err(|e| e.to_string())?;
    res.map_err(|e| e.to_string())?;
    set_phase(app, Phase::Installed, None);
    Ok(())
}

pub fn status() -> Option<UpdateStatus> {
    with_state(|s| s.status.clone())
}

// ───────────────────────── commands ─────────────────────────

#[tauri::command]
pub fn update_status() -> Result<UpdateStatus, String> {
    status().ok_or_else(|| "updater not ready".into())
}

#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<UpdateStatus, String> {
    check(&app, true).await
}

#[tauri::command]
pub async fn update_download(app: AppHandle) -> Result<(), String> {
    download(&app).await
}

/// Resolves once the update is installed (Linux) — the caller then relaunches. On Windows the
/// process exits during this call.
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    install(&app).await
}

/// .deb / .rpm / dev build: open the release page so the user grabs the new package.
#[tauri::command]
pub fn update_open_release(app: AppHandle) -> Result<(), String> {
    let url = status()
        .and_then(|s| s.release_url)
        .unwrap_or_else(|| format!("https://github.com/{REPO}/releases/latest"));
    let platform = std::sync::Arc::clone(&app.state::<AppState>().platform);
    std::thread::spawn(move || {
        if let Err(e) = platform.open_url(&url) {
            eprintln!("[soul][update] open release page failed: {e}");
        }
    });
    Ok(())
}

/// Autosaved from Settings → About (owned here; `save_app_settings` keeps the stored values).
#[tauri::command]
pub async fn update_save_config(
    app: AppHandle,
    auto_update: bool,
    channel: String,
) -> Result<UpdateStatus, String> {
    let channel = normalize_channel(&channel);
    let mut settings = load_settings(&app)?;
    let channel_changed = normalize_channel(&settings.update_channel) != channel;
    let auto_enabled = auto_update && !settings.auto_update;
    settings.auto_update = auto_update;
    settings.update_channel = channel.clone();
    save_settings(&app, &settings)?;
    eprintln!("[soul][update] config auto={auto_update} channel={channel}");
    with_state(|s| {
        s.status.auto_update = auto_update;
        s.status.channel = channel.clone();
        if channel_changed && !matches!(s.status.phase, Phase::Downloading | Phase::Installing) {
            // A different channel may offer a different version: forget the old result.
            s.status.phase = Phase::Idle;
            s.status.version = None;
            s.status.notes = None;
            s.status.release_url = None;
            s.update = None;
            s.bytes = None;
        }
    });
    emit(&app);
    if channel_changed || auto_enabled {
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = check(&app2, false).await;
        });
    }
    status().ok_or_else(|| "updater not ready".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_is_normalized() {
        assert_eq!(normalize_channel("beta"), "beta");
        assert_eq!(normalize_channel(" Beta "), "beta");
        assert_eq!(normalize_channel("stable"), "stable");
        assert_eq!(normalize_channel(""), "stable");
        assert_eq!(normalize_channel("nightly"), "stable");
    }

    #[test]
    fn package_managers_get_a_link() {
        assert_eq!(install_kind("nsis"), "auto");
        assert_eq!(install_kind("msi"), "auto");
        assert_eq!(install_kind("appimage"), "auto");
        assert_eq!(install_kind("deb"), "manual");
        assert_eq!(install_kind("rpm"), "manual");
        assert_eq!(install_kind("dev"), "manual");
    }
}
