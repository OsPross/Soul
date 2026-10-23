//! Spotify Web API: search and start playback, queue, like, devices — HTTPS only, the Spotify
//! window stays where it is. Blocking; worker threads only.

use super::{auth, local, NOT_CONNECTED, PREMIUM_REQUIRED};
use reqwest::Method;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

const API: &str = "https://api.spotify.com/v1";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyDevice {
    pub id: String,
    pub name: String,
    /// "Computer" | "Smartphone" | "Speaker" | …
    pub kind: String,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyPlaylist {
    pub name: String,
    pub uri: String,
    pub image: Option<String>,
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// One API call. `Ok(None)` = empty body (204).
fn call(method: Method, path: &str, body: Option<Value>) -> Result<Option<Value>, String> {
    let token = auth::valid_token()?;
    let mut req = auth::http().request(method, format!("{API}{path}")).bearer_auth(token);
    req = match body {
        Some(b) => req.json(&b),
        None => req.header(reqwest::header::CONTENT_LENGTH, "0"),
    };
    let resp = req.send().map_err(|e| format!("Spotify: {}", e.without_url()))?;
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let message = v["error"]["message"].as_str().unwrap_or("").to_string();
    match status {
        200..=299 => Ok((!v.is_null()).then_some(v)),
        401 => Err("Spotify: sesja wygasła — połącz konto ponownie w Ustawieniach.".into()),
        403 if v["error"]["reason"] == "PREMIUM_REQUIRED" || message.contains("Premium") => {
            Err(PREMIUM_REQUIRED.into())
        }
        404 if v["error"]["reason"] == "NO_ACTIVE_DEVICE" => {
            Err("Spotify: brak aktywnego urządzenia — uruchom Spotify.".into())
        }
        429 => Err("Spotify: za dużo zapytań, spróbuj za chwilę.".into()),
        s => Err(format!("Spotify (HTTP {s}): {message}")),
    }
}

pub fn list_devices() -> Result<Vec<SpotifyDevice>, String> {
    let v = call(Method::GET, "/me/player/devices", None)?.unwrap_or_default();
    Ok(v["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            Some(SpotifyDevice {
                id: d["id"].as_str()?.to_string(),
                name: d["name"].as_str().unwrap_or_default().to_string(),
                kind: d["type"].as_str().unwrap_or_default().to_string(),
                is_active: d["is_active"].as_bool().unwrap_or(false),
            })
        })
        .collect())
}

/// Active device; else this computer by name; else any computer; else the first one.
fn pick_device<'a>(devices: &'a [SpotifyDevice], host: &str) -> Option<&'a SpotifyDevice> {
    devices
        .iter()
        .find(|d| d.is_active)
        .or_else(|| devices.iter().find(|d| !host.is_empty() && d.name.eq_ignore_ascii_case(host)))
        .or_else(|| devices.iter().find(|d| d.kind.eq_ignore_ascii_case("computer")))
        .or_else(|| devices.first())
}

/// A device to play on: `None` = one is already active. With no device at all, starts the local
/// Spotify minimized and waits (≤ 6 s) for it to register.
pub fn ensure_active_device() -> Result<Option<String>, String> {
    let mut devices = list_devices()?;
    if devices.is_empty() && local::is_installed() {
        local::ensure_spotify_running_background()?;
        local::wait_for(Duration::from_secs(6), Duration::from_millis(600), || {
            devices = list_devices().unwrap_or_default();
            !devices.is_empty()
        });
    }
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    let device = pick_device(&devices, &host)
        .ok_or("Spotify: brak urządzenia — uruchom Spotify na komputerze lub telefonie.")?;
    Ok((!device.is_active).then(|| device.id.clone()))
}

/// Play a track URI or a context (playlist / album / artist) URI.
pub fn play_uri(uri: &str, device_id: Option<&str>) -> Result<String, String> {
    let device = match device_id {
        Some(d) => Some(d.to_string()),
        None => ensure_active_device()?,
    };
    let body = if uri.contains(":track:") {
        json!({ "uris": [uri] })
    } else {
        json!({ "context_uri": uri })
    };
    let path = match device {
        Some(d) => format!("/me/player/play?device_id={}", enc(&d)),
        None => "/me/player/play".into(),
    };
    call(Method::PUT, &path, Some(body))?;
    Ok("Odtwarzam na Spotify".into())
}

/// Resume whatever was loaded, on this computer if nothing is active.
pub fn resume() -> Result<String, String> {
    let path = match ensure_active_device()? {
        Some(d) => format!("/me/player/play?device_id={}", enc(&d)),
        None => "/me/player/play".into(),
    };
    call(Method::PUT, &path, None)?;
    Ok("Spotify: gra".into())
}

/// „Artist – Title” for a track / album, the name otherwise.
fn label(item: &Value) -> String {
    let name = item["name"].as_str().unwrap_or_default();
    let artists: Vec<&str> =
        item["artists"].as_array().into_iter().flatten().filter_map(|a| a["name"].as_str()).collect();
    if artists.is_empty() {
        name.to_string()
    } else {
        format!("{} – {name}", artists.join(", "))
    }
}

fn fold(s: &str) -> String {
    s.split_whitespace()
        .map(crate::agent::router::fold_word)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The user's playlist whose folded name equals, or else contains / is contained in, `query`.
fn match_playlist<'a>(playlists: &'a [SpotifyPlaylist], query: &str) -> Option<&'a SpotifyPlaylist> {
    let q = fold(query);
    if q.is_empty() {
        return None;
    }
    playlists.iter().find(|p| fold(&p.name) == q).or_else(|| {
        playlists.iter().find(|p| {
            let n = fold(&p.name);
            !n.is_empty() && (n.contains(&q) || q.contains(&n))
        })
    })
}

pub fn list_playlists() -> Result<Vec<SpotifyPlaylist>, String> {
    let v = call(Method::GET, "/me/playlists?limit=50", None)?.unwrap_or_default();
    Ok(v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(SpotifyPlaylist {
                name: p["name"].as_str()?.to_string(),
                uri: p["uri"].as_str()?.to_string(),
                image: p["images"][0]["url"].as_str().map(str::to_string),
            })
        })
        .collect())
}

/// First search hit of `kind` (track | playlist | album | artist).
fn search(query: &str, kind: &str) -> Result<Value, String> {
    let path = format!("/search?q={}&type={kind}&limit=5", enc(query));
    let v = call(Method::GET, &path, None)?.unwrap_or_default();
    v[format!("{kind}s")]["items"]
        .as_array()
        .and_then(|items| items.iter().find(|i| !i.is_null()).cloned())
        .ok_or_else(|| format!("Spotify: nic nie znaleziono dla „{query}”."))
}

/// Search and play. A playlist is looked up in the user's own playlists first.
pub fn play_search(query: &str, kind: Option<&str>) -> Result<String, String> {
    let kind = kind.filter(|k| matches!(*k, "playlist" | "album" | "artist")).unwrap_or("track");
    if kind == "playlist" {
        if let Some(p) = match_playlist(&list_playlists()?, query) {
            play_uri(&p.uri, None)?;
            return Ok(format!("Odtwarzam na Spotify: {}", p.name));
        }
    }
    let item = search(query, kind)?;
    let uri = item["uri"].as_str().ok_or("Spotify: wynik bez URI.")?;
    play_uri(uri, None)?;
    Ok(format!("Odtwarzam na Spotify: {}", label(&item)))
}

pub fn add_to_queue(query: &str) -> Result<String, String> {
    let item = search(query, "track")?;
    let uri = item["uri"].as_str().ok_or("Spotify: wynik bez URI.")?;
    let mut path = format!("/me/player/queue?uri={}", enc(uri));
    if let Some(d) = ensure_active_device()? {
        path.push_str(&format!("&device_id={}", enc(&d)));
    }
    call(Method::POST, &path, None)?;
    Ok(format!("Dodano do kolejki: {}", label(&item)))
}

pub fn like_current_track(like: bool) -> Result<String, String> {
    let v = call(Method::GET, "/me/player/currently-playing", None)?
        .ok_or("Spotify: teraz nic nie gra.")?;
    let item = &v["item"];
    let id = item["id"].as_str().ok_or("Spotify: teraz nic nie gra.")?;
    let method = if like { Method::PUT } else { Method::DELETE };
    call(method, &format!("/me/tracks?ids={}", enc(id)), None)?;
    Ok(if like {
        format!("Dodano do polubionych: {}", label(item))
    } else {
        format!("Usunięto z polubionych: {}", label(item))
    })
}

/// Move playback to a device given by id or (part of its) name.
pub fn transfer_playback(device: &str) -> Result<String, String> {
    let devices = list_devices()?;
    let q = fold(device);
    let d = devices
        .iter()
        .find(|d| d.id == device)
        .or_else(|| devices.iter().find(|d| !q.is_empty() && fold(&d.name).contains(&q)))
        .ok_or_else(|| format!("Spotify: nie znam urządzenia „{device}”."))?;
    call(Method::PUT, "/me/player", Some(json!({ "device_ids": [d.id], "play": true })))?;
    Ok(format!("Spotify gra na: {}", d.name))
}

/// Errors after which the local `spotify:search:` path is the better answer.
pub fn needs_fallback(err: &str) -> bool {
    err == NOT_CONNECTED || err == PREMIUM_REQUIRED
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, kind: &str, active: bool) -> SpotifyDevice {
        SpotifyDevice { id: name.to_lowercase(), name: name.into(), kind: kind.into(), is_active: active }
    }

    #[test]
    fn picks_a_device() {
        let d = [dev("Phone", "Smartphone", false), dev("DESKTOP-1", "Computer", false), dev("Laptop", "Computer", false)];
        assert_eq!(pick_device(&d, "desktop-1").unwrap().name, "DESKTOP-1");
        assert_eq!(pick_device(&d, "other").unwrap().name, "DESKTOP-1");
        let d = [dev("Phone", "Smartphone", false), dev("Echo", "Speaker", true)];
        assert_eq!(pick_device(&d, "x").unwrap().name, "Echo");
        assert!(pick_device(&[], "x").is_none());
    }

    #[test]
    fn matches_own_playlists() {
        let p = |n: &str| SpotifyPlaylist { name: n.into(), uri: format!("spotify:playlist:{n}"), image: None };
        let lists = [p("Trening 💪"), p("Śniadanie"), p("trening")];
        assert_eq!(match_playlist(&lists, "Trening").unwrap().name, "Trening 💪");
        assert_eq!(match_playlist(&lists, "sniadanie").unwrap().name, "Śniadanie");
        assert!(match_playlist(&lists, "jazz").is_none());
        assert!(match_playlist(&lists, "").is_none());
    }

    #[test]
    fn track_labels() {
        let t = json!({ "name": "Mori", "artists": [{ "name": "Dawid Podsiadło" }] });
        assert_eq!(label(&t), "Dawid Podsiadło – Mori");
        assert_eq!(label(&json!({ "name": "Trening" })), "Trening");
    }
}
