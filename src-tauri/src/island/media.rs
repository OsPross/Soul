//! Now Playing via Windows GSMTC / Linux MPRIS.
//! All OS media I/O runs on a dedicated worker thread; commands only read a cache.
//! Prefer GSMTC change events; fall back to a 2s poll. Logs only on state changes
//! (unless `SOUL_DEBUG=1`).

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

use crate::soul_debug;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// data:image/…;base64,… or empty
    pub artwork_data_url: String,
    pub playing: bool,
    pub available: bool,
    /// Playback position in milliseconds at `position_updated_at_ms`.
    pub position_ms: u64,
    /// Track duration in milliseconds (0 if unknown).
    pub duration_ms: u64,
    /// Unix epoch ms when `position_ms` was sampled (GSMTC LastUpdatedTime / now).
    pub position_updated_at_ms: u64,
    /// Playback rate (1.0 = normal). Used for local interpolation.
    #[serde(default = "default_playback_rate")]
    pub playback_rate: f64,
}

fn default_playback_rate() -> f64 {
    1.0
}

impl Default for NowPlaying {
    fn default() -> Self {
        Self {
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            artwork_data_url: String::new(),
            playing: false,
            available: false,
            position_ms: 0,
            duration_ms: 0,
            position_updated_at_ms: 0,
            playback_rate: 1.0,
        }
    }
}

impl NowPlaying {
    fn state_key(&self) -> String {
        // Position omitted (high-frequency). Artwork only by length so late-arriving
        // art re-emits, without hashing the whole data URL.
        format!(
            "{}|{}|{}|{}|{}",
            self.title,
            self.artist,
            self.playing,
            self.available,
            self.artwork_data_url.len()
        )
    }
}

enum MediaCmd {
    Refresh,
    Action {
        action: String,
        reply: Sender<Result<String, String>>,
    },
}

struct MediaCache {
    now: NowPlaying,
    tx: Option<Sender<MediaCmd>>,
}

fn cache() -> &'static Mutex<MediaCache> {
    static CACHE: OnceLock<Mutex<MediaCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(MediaCache {
            now: NowPlaying::default(),
            tx: None,
        })
    })
}

/// Cached snapshot — never touches GSMTC/MPRIS.
pub fn cached_now_playing() -> NowPlaying {
    cache().lock().now.clone()
}

pub fn media_action_async(action: String) -> Result<String, String> {
    let tx = cache()
        .lock()
        .tx
        .clone()
        .ok_or_else(|| "Media worker not started.".to_string())?;
    let (reply_tx, reply_rx) = mpsc::channel();
    tx.send(MediaCmd::Action {
        action,
        reply: reply_tx,
    })
    .map_err(|_| "Media worker gone.".to_string())?;
    reply_rx
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| "Media action timed out.".to_string())?
}

/// Start media worker: event-driven refresh + 2s poll backup.
pub fn start_worker(app: AppHandle) {
    let (tx, rx) = mpsc::channel::<MediaCmd>();
    cache().lock().tx = Some(tx.clone());

    // Platform event hook (Windows GSMTC) pushes Refresh into the same channel.
    #[cfg(windows)]
    windows_gsmtc::start_event_bridge(tx.clone());

    thread::Builder::new()
        .name("soul-media".into())
        .spawn(move || {
            let _ = tx.send(MediaCmd::Refresh);
            let mut last_key = String::new();
            let mut had_session = false;

            loop {
                // 2s poll backup; events also wake via Refresh.
                let cmd = rx.recv_timeout(Duration::from_secs(2));
                match cmd {
                    Ok(MediaCmd::Action { action, reply }) => {
                        let t0 = std::time::Instant::now();
                        let result = run_action_with_timeout(&action);
                        if t0.elapsed().as_millis() > 200 {
                            eprintln!(
                                "[soul][watchdog] WARN media_action '{}' took {}ms",
                                action,
                                t0.elapsed().as_millis()
                            );
                        }
                        let _ = reply.send(result);
                        let _ = tx.send(MediaCmd::Refresh);
                    }
                    Ok(MediaCmd::Refresh) | Err(mpsc::RecvTimeoutError::Timeout) => {
                        let t0 = std::time::Instant::now();
                        let np = fetch_with_timeout();
                        if t0.elapsed().as_millis() > 200 {
                            eprintln!(
                                "[soul][watchdog] WARN media_fetch took {}ms",
                                t0.elapsed().as_millis()
                            );
                        }

                        let key = np.state_key();
                        let session_now = np.available;
                        if session_now != had_session {
                            if session_now {
                                eprintln!(
                                    "[soul][island][media] session appeared: {} — {}",
                                    np.artist, np.title
                                );
                            } else {
                                eprintln!("[soul][island][media] session disappeared");
                            }
                            had_session = session_now;
                        } else if key != last_key && session_now {
                            eprintln!(
                                "[soul][island][media] track/playback: {} — {} ({})",
                                np.artist,
                                np.title,
                                if np.playing { "playing" } else { "paused" }
                            );
                        }

                        {
                            cache().lock().now = np.clone();
                        }
                        if key != last_key {
                            last_key = key;
                            let _ = app.emit("island://now-playing", np);
                        } else if np.available && np.playing {
                            // Periodic timeline resync (frontend interpolates in between).
                            // Artwork stripped: unchanged, and can be ~700 KB of base64.
                            let mut slim = np;
                            slim.artwork_data_url.clear();
                            let _ = app.emit("island://now-playing", slim);
                        } else {
                            soul_debug!("[soul][island][media] poll: unchanged");
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("spawn media worker");
}

fn fetch_with_timeout() -> NowPlaying {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(platform_fetch());
    });
    match rx.recv_timeout(Duration::from_millis(900)) {
        Ok(np) => np,
        Err(_) => {
            soul_debug!("[soul][island][media] fetch timed out (900ms)");
            cached_now_playing()
        }
    }
}

fn run_action_with_timeout(action: &str) -> Result<String, String> {
    let action = action.to_string();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(platform_action(&action));
    });
    rx.recv_timeout(Duration::from_millis(1500))
        .map_err(|_| "Media action timed out.".to_string())?
}

fn platform_fetch() -> NowPlaying {
    #[cfg(windows)]
    {
        windows_gsmtc::fetch()
    }
    #[cfg(target_os = "linux")]
    {
        linux_mpris::fetch()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        NowPlaying::default()
    }
}

fn platform_action(action: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        windows_gsmtc::action(action)
    }
    #[cfg(target_os = "linux")]
    {
        linux_mpris::action(action)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = action;
        Err("Media controls unsupported on this platform.".into())
    }
}

#[cfg(windows)]
mod windows_gsmtc {
    use super::{MediaCmd, NowPlaying};
    use crate::soul_debug;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use std::sync::mpsc::Sender;
    use std::thread;
    use windows::Foundation::TypedEventHandler;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession,
        GlobalSystemMediaTransportControlsSessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus,
        MediaPropertiesChangedEventArgs, PlaybackInfoChangedEventArgs, SessionsChangedEventArgs,
    };
    use windows::Storage::Streams::DataReader;

    #[derive(Debug)]
    enum FetchErr {
        /// Normal when nothing is playing — not logged every poll.
        NoSession,
        Other(String),
    }

    pub fn fetch() -> NowPlaying {
        match fetch_inner() {
            Ok(np) => np,
            Err(FetchErr::NoSession) => {
                soul_debug!("[soul][island][media] GSMTC: no active session (quiet)");
                NowPlaying::default()
            }
            Err(FetchErr::Other(e)) => {
                // Unexpected errors: once per call is ok but still gate spam via debug
                // unless it's rare — log at info once through worker state machine instead.
                soul_debug!("[soul][island][media] GSMTC error: {e}");
                NowPlaying::default()
            }
        }
    }

    fn fetch_inner() -> Result<NowPlaying, FetchErr> {
        let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .map_err(|e| FetchErr::Other(e.to_string()))?
            .get()
            .map_err(|e| FetchErr::Other(e.to_string()))?;
        let session = match manager.GetCurrentSession() {
            Ok(s) => s,
            Err(_) => return Err(FetchErr::NoSession),
        };
        read_session(&session)
    }

    fn read_session(
        session: &GlobalSystemMediaTransportControlsSession,
    ) -> Result<NowPlaying, FetchErr> {
        let props = session
            .TryGetMediaPropertiesAsync()
            .map_err(|e| FetchErr::Other(e.to_string()))?
            .get()
            .map_err(|e| FetchErr::Other(e.to_string()))?;

        let title = props.Title().map(|s| s.to_string()).unwrap_or_default();
        let artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
        let album = props.AlbumTitle().map(|s| s.to_string()).unwrap_or_default();

        let playback_info = session.GetPlaybackInfo().ok();
        let playback = playback_info
            .as_ref()
            .and_then(|info| info.PlaybackStatus().ok())
            .map(|s| s == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
            .unwrap_or(false);
        let playback_rate = playback_info
            .as_ref()
            .and_then(|info| info.PlaybackRate().ok())
            .and_then(|r| r.Value().ok())
            .unwrap_or(1.0);

        let artwork_data_url = cached_thumbnail(&title, &artist, &props);

        let (position_ms, duration_ms, position_updated_at_ms) = session
            .GetTimelineProperties()
            .ok()
            .map(|t| {
                let pos = t
                    .Position()
                    .map(|ts| (ts.Duration.max(0) / 10_000) as u64)
                    .unwrap_or(0);
                let end = t
                    .EndTime()
                    .map(|ts| (ts.Duration.max(0) / 10_000) as u64)
                    .unwrap_or(0);
                let updated = t
                    .LastUpdatedTime()
                    .ok()
                    .map(windows_datetime_to_unix_ms)
                    .unwrap_or_else(unix_now_ms);
                (pos, end, updated)
            })
            .unwrap_or_else(|| (0, 0, unix_now_ms()));

        Ok(NowPlaying {
            title,
            artist,
            album,
            artwork_data_url,
            playing: playback,
            available: true,
            position_ms,
            duration_ms,
            position_updated_at_ms,
            playback_rate: if playback_rate.is_finite() && playback_rate > 0.0 {
                playback_rate
            } else {
                1.0
            },
        })
    }

    /// Thumbnail decode is slow (>200 ms); re-read only when the track changes
    /// or the previous read came back empty.
    fn cached_thumbnail(
        title: &str,
        artist: &str,
        props: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties,
    ) -> String {
        static CACHE: std::sync::OnceLock<parking_lot::Mutex<(String, String)>> =
            std::sync::OnceLock::new();
        let cache = CACHE.get_or_init(|| parking_lot::Mutex::new((String::new(), String::new())));
        let key = format!("{title}|{artist}");
        {
            let c = cache.lock();
            if c.0 == key && !c.1.is_empty() {
                return c.1.clone();
            }
        }
        let url = read_thumbnail(props).unwrap_or_default();
        *cache.lock() = (key, url.clone());
        url
    }

    fn windows_datetime_to_unix_ms(dt: windows::Foundation::DateTime) -> u64 {
        // UniversalTime: 100-ns ticks since 1601-01-01 UTC.
        const EPOCH_DIFF_100NS: i64 = 116_444_736_000_000_000;
        let ticks = dt.UniversalTime;
        let unix_100ns = ticks.saturating_sub(EPOCH_DIFF_100NS);
        (unix_100ns.max(0) / 10_000) as u64
    }

    fn unix_now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn read_thumbnail(
        props: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties,
    ) -> Result<String, String> {
        let thumb = props.Thumbnail().map_err(|e| e.to_string())?;
        let stream = thumb
            .OpenReadAsync()
            .map_err(|e| e.to_string())?
            .get()
            .map_err(|e| e.to_string())?;
        let size = stream.Size().map_err(|e| e.to_string())? as u32;
        if size == 0 || size > 512_000 {
            return Ok(String::new());
        }
        let reader = DataReader::CreateDataReader(&stream).map_err(|e| e.to_string())?;
        reader
            .LoadAsync(size)
            .map_err(|e| e.to_string())?
            .get()
            .map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; size as usize];
        reader.ReadBytes(&mut buf).map_err(|e| e.to_string())?;
        let b64 = BASE64.encode(&buf);
        let mime = if buf.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
            "image/png"
        } else {
            "image/jpeg"
        };
        Ok(format!("data:{mime};base64,{b64}"))
    }

    /// Subscribe to GSMTC SessionsChanged / MediaPropertiesChanged / PlaybackInfoChanged.
    /// Keeps the manager alive on a parked thread and pushes `Refresh` into the worker.
    pub fn start_event_bridge(tx: Sender<MediaCmd>) {
        thread::Builder::new()
            .name("soul-gsmtc-events".into())
            .spawn(move || {
                let manager = match GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
                    .and_then(|op| op.get())
                {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("[soul][island][media] GSMTC event bridge failed: {e}");
                        return;
                    }
                };

                let session_bind_tx = tx.clone();
                let sessions_handler = TypedEventHandler::<
                    GlobalSystemMediaTransportControlsSessionManager,
                    SessionsChangedEventArgs,
                >::new(move |sender, _args| {
                    soul_debug!("[soul][island][media] SessionsChanged");
                    if let Some(mgr) = sender {
                        if let Ok(session) = mgr.GetCurrentSession() {
                            attach_session_handlers(&session, session_bind_tx.clone());
                        }
                    }
                    let _ = session_bind_tx.send(MediaCmd::Refresh);
                    Ok(())
                });
                if let Err(e) = manager.SessionsChanged(&sessions_handler) {
                    eprintln!("[soul][island][media] SessionsChanged subscribe failed: {e}");
                } else {
                    soul_debug!("[soul][island][media] subscribed SessionsChanged");
                }

                if let Ok(session) = manager.GetCurrentSession() {
                    attach_session_handlers(&session, tx.clone());
                }

                // Park forever so COM event callbacks stay alive.
                loop {
                    thread::park();
                }
            })
            .ok();
    }

    fn attach_session_handlers(
        session: &GlobalSystemMediaTransportControlsSession,
        tx: Sender<MediaCmd>,
    ) {
        let tx_props = tx.clone();
        let props_handler = TypedEventHandler::<
            GlobalSystemMediaTransportControlsSession,
            MediaPropertiesChangedEventArgs,
        >::new(move |_s, _a| {
            soul_debug!("[soul][island][media] MediaPropertiesChanged");
            let _ = tx_props.send(MediaCmd::Refresh);
            Ok(())
        });
        if let Err(e) = session.MediaPropertiesChanged(&props_handler) {
            soul_debug!("[soul][island][media] MediaPropertiesChanged subscribe failed: {e}");
        }

        let tx_play = tx;
        let play_handler = TypedEventHandler::<
            GlobalSystemMediaTransportControlsSession,
            PlaybackInfoChangedEventArgs,
        >::new(move |_s, _a| {
            soul_debug!("[soul][island][media] PlaybackInfoChanged");
            let _ = tx_play.send(MediaCmd::Refresh);
            Ok(())
        });
        if let Err(e) = session.PlaybackInfoChanged(&play_handler) {
            soul_debug!("[soul][island][media] PlaybackInfoChanged subscribe failed: {e}");
        }
    }

    pub fn action(action: &str) -> Result<String, String> {
        let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .map_err(|e| e.to_string())?
            .get()
            .map_err(|e| e.to_string())?;
        let session = manager
            .GetCurrentSession()
            .map_err(|_| "No active media session.".to_string())?;

        match action.trim().to_ascii_lowercase().as_str() {
            "play" => {
                session
                    .TryPlayAsync()
                    .map_err(|e| e.to_string())?
                    .get()
                    .map_err(|e| e.to_string())?;
                Ok("Playing".into())
            }
            "pause" => {
                session
                    .TryPauseAsync()
                    .map_err(|e| e.to_string())?
                    .get()
                    .map_err(|e| e.to_string())?;
                Ok("Paused".into())
            }
            "play_pause" | "playpause" | "toggle" => {
                session
                    .TryTogglePlayPauseAsync()
                    .map_err(|e| e.to_string())?
                    .get()
                    .map_err(|e| e.to_string())?;
                Ok("Toggled play/pause".into())
            }
            "next" => {
                session
                    .TrySkipNextAsync()
                    .map_err(|e| e.to_string())?
                    .get()
                    .map_err(|e| e.to_string())?;
                Ok("Next track".into())
            }
            "prev" | "previous" => {
                session
                    .TrySkipPreviousAsync()
                    .map_err(|e| e.to_string())?
                    .get()
                    .map_err(|e| e.to_string())?;
                Ok("Previous track".into())
            }
            other => Err(format!("Unknown media action '{other}'")),
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_mpris {
    use super::NowPlaying;
    use crate::soul_debug;
    use zbus::blocking::Connection;
    use zbus::zvariant::OwnedValue;

    pub fn fetch() -> NowPlaying {
        match fetch_inner() {
            Ok(np) => np,
            Err(e) if e.contains("No MPRIS") => {
                soul_debug!("[soul][island][media] MPRIS: no player (quiet)");
                NowPlaying::default()
            }
            Err(e) => {
                soul_debug!("[soul][island][media] MPRIS error: {e}");
                NowPlaying::default()
            }
        }
    }

    fn fetch_inner() -> Result<NowPlaying, String> {
        let conn = Connection::session().map_err(|e| e.to_string())?;
        let names: Vec<String> = conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/",
                Some("org.freedesktop.DBus"),
                "ListNames",
                &(),
            )
            .map_err(|e| e.to_string())?
            .body()
            .deserialize()
            .map_err(|e| e.to_string())?;

        let player = names
            .into_iter()
            .find(|n| n.starts_with("org.mpris.MediaPlayer2."))
            .ok_or_else(|| "No MPRIS players".to_string())?;

        let proxy = zbus::blocking::Proxy::new(
            &conn,
            player.as_str(),
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
        )
        .map_err(|e| e.to_string())?;

        let status: String = proxy
            .get_property("PlaybackStatus")
            .unwrap_or_else(|_| "Stopped".into());
        let playing = status.eq_ignore_ascii_case("Playing");

        let metadata: std::collections::HashMap<String, OwnedValue> =
            proxy.get_property("Metadata").unwrap_or_default();

        let title = string_meta(&metadata, "xesam:title");
        let artist = artists_meta(&metadata);
        let album = string_meta(&metadata, "xesam:album");
        let art_url = string_meta(&metadata, "mpris:artUrl");
        let artwork_data_url = if art_url.starts_with("http") {
            art_url
        } else {
            String::new()
        };

        // mpris:length is microseconds; Position is also microseconds.
        let duration_ms = metadata
            .get("mpris:length")
            .and_then(|v| i64::try_from(v).ok())
            .map(|us| (us.max(0) as u64) / 1000)
            .unwrap_or(0);
        let position_ms = proxy
            .get_property::<i64>("Position")
            .ok()
            .map(|us| (us.max(0) as u64) / 1000)
            .unwrap_or(0);

        let position_updated_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        Ok(NowPlaying {
            title,
            artist,
            album,
            artwork_data_url,
            playing,
            available: true,
            position_ms,
            duration_ms,
            position_updated_at_ms,
            playback_rate: 1.0,
        })
    }

    fn string_meta(
        map: &std::collections::HashMap<String, OwnedValue>,
        key: &str,
    ) -> String {
        map.get(key)
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| String::try_from(v).ok())
            .unwrap_or_default()
    }

    fn artists_meta(map: &std::collections::HashMap<String, OwnedValue>) -> String {
        map.get("xesam:artist")
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| <Vec<String>>::try_from(v).ok())
            .map(|a| a.join(", "))
            .unwrap_or_default()
    }

    pub fn action(action: &str) -> Result<String, String> {
        let conn = Connection::session().map_err(|e| e.to_string())?;
        let names: Vec<String> = conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/",
                Some("org.freedesktop.DBus"),
                "ListNames",
                &(),
            )
            .map_err(|e| e.to_string())?
            .body()
            .deserialize()
            .map_err(|e| e.to_string())?;
        let player = names
            .into_iter()
            .find(|n| n.starts_with("org.mpris.MediaPlayer2."))
            .ok_or_else(|| "No MPRIS players".to_string())?;
        let proxy = zbus::blocking::Proxy::new(
            &conn,
            player.as_str(),
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
        )
        .map_err(|e| e.to_string())?;

        let method = match action.trim().to_ascii_lowercase().as_str() {
            "play" => "Play",
            "pause" => "Pause",
            "play_pause" | "playpause" | "toggle" => "PlayPause",
            "next" => "Next",
            "prev" | "previous" => "Previous",
            other => return Err(format!("Unknown media action '{other}'")),
        };
        proxy.call_method(method, &()).map_err(|e| e.to_string())?;
        Ok(method.into())
    }
}
