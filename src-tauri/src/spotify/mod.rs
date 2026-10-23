//! Spotify in the background: local client control (`local`, no account), and the Web API with
//! an OAuth PKCE login (`auth`, `api`) for playing anything — never with the Spotify window in front.

pub mod api;
pub mod auth;
pub mod local;

use tauri::{AppHandle, Emitter};

/// `api` / `auth` errors that mean „use the local client instead”.
pub const NOT_CONNECTED: &str = "Spotify: konto nie jest połączone (Ustawienia → Wyspa → Spotify).";
pub const PREMIUM_REQUIRED: &str = "Spotify: sterowanie odtwarzaniem przez API wymaga konta Premium.";

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn spotify_auth_status() -> Result<auth::SpotifyStatus, String> {
    blocking(auth::status).await
}

/// Opens the Spotify login in the browser and resolves once the redirect came back (≤ 180 s).
#[tauri::command]
pub async fn spotify_connect(
    app: AppHandle,
    state: tauri::State<'_, crate::AppState>,
    client_id: String,
) -> Result<auth::SpotifyStatus, String> {
    let pending = auth::begin(&client_id)?;
    state.platform.open_url(&pending.url)?;
    let status = blocking(move || auth::finish(pending)).await?.inspect_err(|e| {
        eprintln!("[soul][spotify] connect failed: {e}");
    })?;
    let _ = app.emit("soul://spotify-status", &status);
    Ok(status)
}

#[tauri::command]
pub async fn spotify_disconnect(app: AppHandle) -> Result<auth::SpotifyStatus, String> {
    let status = blocking(|| auth::disconnect().map(|_| auth::status())).await??;
    let _ = app.emit("soul://spotify-status", &status);
    Ok(status)
}

#[tauri::command]
pub async fn spotify_playlists() -> Result<Vec<api::SpotifyPlaylist>, String> {
    blocking(api::list_playlists).await?
}

#[tauri::command]
pub async fn spotify_devices() -> Result<Vec<api::SpotifyDevice>, String> {
    blocking(api::list_devices).await?
}

/// Island search / chips: the same actions as the `spotify` voice tool.
#[tauri::command]
pub async fn spotify_run(args: serde_json::Value) -> Result<String, String> {
    blocking(move || run(&args)).await?
}

fn media(action: &str) -> Result<String, String> {
    crate::island::media::media_action_async(action.to_string())
}

/// The `spotify` tool: `{action, query?, kind?, percent?, delta?, seconds?, state?}`. Blocking.
pub fn run(args: &serde_json::Value) -> Result<String, String> {
    let text = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let int = |k: &str| args.get(k).and_then(|v| v.as_i64());
    let (action, query, state) = (text("action"), text("query"), text("state"));
    let number = |r: String| r.chars().filter(char::is_ascii_digit).collect::<String>();
    match action.as_str() {
        "resume" => resume_in_background(),
        "play" if query.is_empty() => resume_in_background(),
        "play" => {
            let played = if query.starts_with("spotify:") {
                api::play_uri(&query, None)
            } else {
                api::play_search(&query, args.get("kind").and_then(|v| v.as_str()))
            };
            match played {
                Err(e) if api::needs_fallback(&e) => search_in_app(&query),
                r => r,
            }
        }
        "pause" => media("spotify:pause").map(|_| "Spotify: pauza".into()),
        "next" => media("spotify:next").map(|_| "Spotify: następny utwór".into()),
        "prev" | "previous" => media("spotify:prev").map(|_| "Spotify: poprzedni utwór".into()),
        "seek" => {
            let r = match (int("seconds"), int("delta")) {
                (Some(s), _) if s >= 0 => media(&format!("seek:{}", s * 1000)),
                (_, Some(d)) => media(&format!("seek_by:{}", d * 1000)),
                _ => return Err("Podaj, o ile albo do którego momentu przewinąć.".into()),
            };
            r.map(|r| format!("Przewinięto do {}", r.trim_start_matches("Seek to ")))
        }
        "shuffle" => {
            let s = if matches!(state.as_str(), "on" | "off") { state.as_str() } else { "toggle" };
            media(&format!("shuffle:{s}")).map(|r| {
                if r.ends_with("on") { "Losowe odtwarzanie włączone" } else { "Losowe odtwarzanie wyłączone" }.into()
            })
        }
        "repeat" => {
            let s = if matches!(state.as_str(), "off" | "track" | "context") { state.as_str() } else { "toggle" };
            media(&format!("repeat:{s}")).map(|r| {
                match r.trim_start_matches("Repeat ") {
                    "track" => "Powtarzam ten utwór",
                    "context" => "Powtarzanie włączone",
                    _ => "Powtarzanie wyłączone",
                }
                .into()
            })
        }
        "volume" => {
            let r = match (int("percent"), int("delta")) {
                (Some(p), _) => media(&format!("spotify_volume:{}", p.clamp(0, 100))),
                (_, Some(d)) => media(&format!("spotify_volume_delta:{}", d.clamp(-100, 100))),
                _ => return Err("Podaj głośność Spotify w procentach.".into()),
            };
            r.map(|r| format!("Głośność Spotify {}%", number(r)))
        }
        "queue" if !query.is_empty() => api::add_to_queue(&query),
        "like" => api::like_current_track(true),
        "unlike" => api::like_current_track(false),
        "transfer" if !query.is_empty() => api::transfer_playback(&query),
        other => Err(format!("Spotify: nieznana akcja „{other}” albo brak nazwy.")),
    }
}

/// Resume (Web API when connected, else the local session), starting Spotify minimized first.
fn resume_in_background() -> Result<String, String> {
    if auth::is_connected() {
        match api::resume() {
            Err(e) if api::needs_fallback(&e) => {}
            r => return r,
        }
    }
    if local::ensure_spotify_running_background()? {
        // The media session appears only once Spotify has loaded.
        local::wait_for(std::time::Duration::from_secs(8), std::time::Duration::from_millis(500), || {
            media("spotify:play").is_ok()
        });
        return Ok("Spotify uruchomiony w tle".into());
    }
    media("spotify:play").map(|_| "Spotify: gra".into())
}

/// No account (or Free): open `spotify:search:<query>` with the window kept in the background,
/// then resume over the media session.
#[allow(clippy::disallowed_methods)] // Linux `xdg-open`; Windows goes through `shell_open`
fn search_in_app(query: &str) -> Result<String, String> {
    if !local::is_installed() {
        return Err(NOT_CONNECTED.into());
    }
    let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    let uri = format!("spotify:search:{}", encoded.replace('+', "%20"));
    #[cfg(windows)]
    crate::platform::windows::open_uri_in_background(&uri, "Spotify.exe")?;
    #[cfg(not(windows))]
    std::process::Command::new("xdg-open").arg(&uri).spawn().map_err(|e| format!("xdg-open: {e}"))?;
    std::thread::sleep(std::time::Duration::from_millis(800));
    let _ = media("spotify:play");
    Ok(format!(
        "Otworzyłem „{query}” w Spotify w tle. Połącz konto Spotify Premium w Ustawieniach, żeby od razu włączać wybrany utwór."
    ))
}
