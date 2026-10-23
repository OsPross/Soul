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
    /// "spotify" | "browser" | "other" ("" when nothing plays).
    #[serde(default)]
    pub source_app: String,
    #[serde(default)]
    pub shuffle: bool,
    /// "off" | "track" | "context"
    #[serde(default)]
    pub repeat: String,
    /// Spotify's own mixer volume, 0–100, while it has an audio session.
    #[serde(default)]
    pub app_volume: Option<u8>,
    #[serde(default)]
    pub spotify_running: bool,
    #[serde(default)]
    pub spotify_installed: bool,
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
            source_app: String::new(),
            shuffle: false,
            repeat: "off".into(),
            app_volume: None,
            spotify_running: false,
            spotify_installed: false,
        }
    }
}

impl NowPlaying {
    fn state_key(&self) -> String {
        // Position omitted (high-frequency). Artwork only by length so late-arriving
        // art re-emits, without hashing the whole data URL.
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{:?}|{}|{}",
            self.title,
            self.artist,
            self.playing,
            self.available,
            self.artwork_data_url.len(),
            self.shuffle,
            self.repeat,
            self.app_volume,
            self.source_app,
            self.spotify_running
        )
    }
}

/// "Spotify.exe" (installer) and "SpotifyAB.SpotifyMusic_…!Spotify" (Microsoft Store); MPRIS bus names too.
pub fn is_spotify_app_id(app_id: &str) -> bool {
    app_id.to_ascii_lowercase().contains("spotify")
}

/// `NowPlaying::source_app` for a GSMTC app id / MPRIS bus name.
fn source_kind(app_id: &str) -> &'static str {
    let id = app_id.to_ascii_lowercase();
    if is_spotify_app_id(&id) {
        "spotify"
    } else if ["chrome", "msedge", "firefox", "opera", "brave", "vivaldi", "chromium", "vio"]
        .iter()
        .any(|b| id.contains(b))
    {
        "browser"
    } else {
        "other"
    }
}

/// A parsed media action string (`island_media_action`, `media_control`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum MediaAction {
    Play,
    Pause,
    Toggle,
    Next,
    Prev,
    /// Absolute position, ms.
    Seek(u64),
    /// Relative, ms (+ forward).
    SeekBy(i64),
    /// `None` = toggle.
    Shuffle(Option<bool>),
    /// "off" | "track" | "context"; `None` = cycle.
    Repeat(Option<&'static str>),
    SpotifyVolume(u8),
    SpotifyVolumeDelta(i64),
}

/// `"seek:90000"`, `"shuffle:toggle"`, `"spotify_volume:40"`… A `spotify:` prefix (→ `true`) targets
/// only the Spotify session; otherwise the playing session, else Spotify, else the current one.
pub(crate) fn parse_action(action: &str) -> Result<(bool, MediaAction), String> {
    use MediaAction::*;
    let lower = action.trim().to_ascii_lowercase();
    let (spotify, rest) = match lower.strip_prefix("spotify:") {
        Some(r) => (true, r),
        None => (false, lower.as_str()),
    };
    let (verb, arg) = rest.split_once(':').unwrap_or((rest, ""));
    let bad = || format!("Unknown media action '{}'", action.trim());
    let act = match (verb, arg) {
        ("play", "") => Play,
        ("pause", "") => Pause,
        ("play_pause" | "playpause" | "toggle", "") => Toggle,
        ("next", "") => Next,
        ("prev" | "previous", "") => Prev,
        ("seek", ms) => Seek(ms.parse().map_err(|_| bad())?),
        ("seek_by", ms) => SeekBy(ms.parse().map_err(|_| bad())?),
        ("shuffle", "on") => Shuffle(Some(true)),
        ("shuffle", "off") => Shuffle(Some(false)),
        ("shuffle", "toggle") => Shuffle(None),
        ("repeat", "off") => Repeat(Some("off")),
        ("repeat", "track") => Repeat(Some("track")),
        ("repeat", "context") => Repeat(Some("context")),
        ("repeat", "toggle") => Repeat(None),
        ("spotify_volume", p) => SpotifyVolume(p.parse().ok().filter(|p| *p <= 100).ok_or_else(bad)?),
        ("spotify_volume_delta", d) => {
            SpotifyVolumeDelta(d.parse().ok().filter(|d: &i64| d.abs() <= 100).ok_or_else(bad)?)
        }
        _ => return Err(bad()),
    };
    Ok((spotify, act))
}

/// Spotify's own order: off → context → track → off.
fn next_repeat(current: &str) -> &'static str {
    match current {
        "off" => "context",
        "context" => "track",
        _ => "off",
    }
}

/// Position right now, interpolated from the last sample.
fn live_position_ms(np: &NowPlaying, now_ms: u64) -> u64 {
    let mut pos = np.position_ms;
    if np.playing {
        pos += (now_ms.saturating_sub(np.position_updated_at_ms) as f64 * np.playback_rate) as u64;
    }
    if np.duration_ms > 0 {
        pos.min(np.duration_ms)
    } else {
        pos
    }
}

/// `current + delta`, clamped to `0..=duration` (no upper bound when the duration is unknown).
fn seek_target(current: u64, delta: i64, duration: u64) -> u64 {
    let t = (current as i64).saturating_add(delta).max(0) as u64;
    if duration > 0 {
        t.min(duration)
    } else {
        t
    }
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
        let mut np = platform_fetch();
        np.spotify_installed = crate::spotify::local::is_installed();
        np.spotify_running = np.source_app == "spotify" || crate::spotify::local::is_running();
        if np.spotify_running {
            np.app_volume = crate::spotify::local::get_spotify_volume();
        }
        let _ = tx.send(np);
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
    use crate::spotify::local;
    let (spotify, act) = parse_action(action)?;
    match act {
        MediaAction::SpotifyVolume(p) => {
            return local::set_spotify_volume(p).map(|v| format!("Spotify volume {v}%"));
        }
        MediaAction::SpotifyVolumeDelta(d) => {
            let now = local::get_spotify_volume().ok_or("Spotify has no audio session.")?;
            let target = (i64::from(now) + d).clamp(0, 100) as u8;
            return local::set_spotify_volume(target).map(|v| format!("Spotify volume {v}%"));
        }
        _ => {}
    }
    #[cfg(windows)]
    {
        windows_gsmtc::action(spotify, act)
    }
    #[cfg(target_os = "linux")]
    {
        linux_mpris::action(spotify, act)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (spotify, act);
        Err("Media controls unsupported on this platform.".into())
    }
}

#[cfg(windows)]
mod windows_gsmtc {
    use super::{
        is_spotify_app_id, live_position_ms, next_repeat, seek_target, source_kind, unix_now_ms,
        MediaAction, MediaCmd, NowPlaying,
    };
    use crate::soul_debug;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use parking_lot::Mutex;
    use std::sync::mpsc::Sender;
    use std::sync::Arc;
    use std::thread;
    use windows::Foundation::{IAsyncOperation, TypedEventHandler};
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession,
        GlobalSystemMediaTransportControlsSessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus,
        MediaPropertiesChangedEventArgs, PlaybackInfoChangedEventArgs, SessionsChangedEventArgs,
        TimelinePropertiesChangedEventArgs,
    };
    use windows::Media::MediaPlaybackAutoRepeatMode;
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
        let manager = manager().map_err(FetchErr::Other)?;
        let session = pick_session(&manager, false).ok_or(FetchErr::NoSession)?;
        read_session(&session)
    }

    fn manager() -> Result<GlobalSystemMediaTransportControlsSessionManager, String> {
        GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .and_then(|op| op.get())
            .map_err(|e| e.to_string())
    }

    fn app_id(session: &GlobalSystemMediaTransportControlsSession) -> String {
        session.SourceAppUserModelId().map(|id| id.to_string()).unwrap_or_default()
    }

    fn is_playing(session: &GlobalSystemMediaTransportControlsSession) -> bool {
        session
            .GetPlaybackInfo()
            .and_then(|i| i.PlaybackStatus())
            .is_ok_and(|s| s == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
    }

    pub fn find_spotify_session(
        manager: &GlobalSystemMediaTransportControlsSessionManager,
    ) -> Option<GlobalSystemMediaTransportControlsSession> {
        let sessions = manager.GetSessions().ok()?;
        let size = sessions.Size().ok()?;
        (0..size)
            .filter_map(|i| sessions.GetAt(i).ok())
            .find(|s| is_spotify_app_id(&app_id(s)))
    }

    /// The playing current session; else Spotify (stays controllable when paused behind a browser
    /// tab); else the current one. `spotify_only` = Spotify or nothing.
    fn pick_session(
        manager: &GlobalSystemMediaTransportControlsSessionManager,
        spotify_only: bool,
    ) -> Option<GlobalSystemMediaTransportControlsSession> {
        let spotify = find_spotify_session(manager);
        if spotify_only {
            return spotify;
        }
        match manager.GetCurrentSession().ok() {
            Some(current) if is_playing(&current) => Some(current),
            current => spotify.or(current),
        }
    }

    fn repeat_of(info: Option<&windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackInfo>) -> &'static str {
        match info.and_then(|i| i.AutoRepeatMode().and_then(|r| r.Value()).ok()) {
            Some(MediaPlaybackAutoRepeatMode::Track) => "track",
            Some(MediaPlaybackAutoRepeatMode::List) => "context",
            _ => "off",
        }
    }

    fn shuffle_of(info: Option<&windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackInfo>) -> bool {
        info.and_then(|i| i.IsShuffleActive().and_then(|r| r.Value()).ok()).unwrap_or(false)
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
        let shuffle = shuffle_of(playback_info.as_ref());
        let repeat = repeat_of(playback_info.as_ref());

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
            source_app: source_kind(&app_id(session)).into(),
            shuffle,
            repeat: repeat.into(),
            ..NowPlaying::default()
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

                // Every session gets handlers once (Spotify paused behind a browser still updates).
                let attached: Arc<Mutex<Vec<GlobalSystemMediaTransportControlsSession>>> = Arc::default();
                let attach_all = {
                    let tx = tx.clone();
                    move |mgr: &GlobalSystemMediaTransportControlsSessionManager| {
                        let Ok(sessions) = mgr.GetSessions() else { return };
                        let now: Vec<_> = sessions.into_iter().collect();
                        let mut done = attached.lock();
                        done.retain(|s| now.contains(s));
                        for session in now {
                            if !done.contains(&session) {
                                attach_session_handlers(&session, tx.clone());
                                done.push(session);
                            }
                        }
                    }
                };
                attach_all(&manager);

                let sessions_tx = tx.clone();
                let sessions_handler = TypedEventHandler::<
                    GlobalSystemMediaTransportControlsSessionManager,
                    SessionsChangedEventArgs,
                >::new(move |sender, _args| {
                    soul_debug!("[soul][island][media] SessionsChanged");
                    if let Some(mgr) = sender {
                        attach_all(mgr);
                    }
                    let _ = sessions_tx.send(MediaCmd::Refresh);
                    Ok(())
                });
                if let Err(e) = manager.SessionsChanged(&sessions_handler) {
                    eprintln!("[soul][island][media] SessionsChanged subscribe failed: {e}");
                } else {
                    soul_debug!("[soul][island][media] subscribed SessionsChanged");
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

        let tx_time = tx.clone();
        let time_handler = TypedEventHandler::<
            GlobalSystemMediaTransportControlsSession,
            TimelinePropertiesChangedEventArgs,
        >::new(move |_s, _a| {
            let _ = tx_time.send(MediaCmd::Refresh);
            Ok(())
        });
        if let Err(e) = session.TimelinePropertiesChanged(&time_handler) {
            soul_debug!("[soul][island][media] TimelinePropertiesChanged subscribe failed: {e}");
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

    /// Wait for a `Try…Async` call; `false` = the player refused it.
    fn accepted(op: windows::core::Result<IAsyncOperation<bool>>) -> Result<bool, String> {
        op.and_then(|op| op.get()).map_err(|e| e.to_string())
    }

    fn refused(ok: bool, label: &str) -> Result<String, String> {
        if ok {
            Ok(label.into())
        } else {
            Err(format!("The player refused: {label}"))
        }
    }

    pub fn action(spotify_only: bool, act: MediaAction) -> Result<String, String> {
        let manager = manager()?;
        let session = pick_session(&manager, spotify_only).ok_or_else(|| {
            if spotify_only { "Spotify is not running." } else { "No active media session." }.to_string()
        })?;
        let info = session.GetPlaybackInfo().ok();
        match act {
            MediaAction::Play => accepted(session.TryPlayAsync()).map(|_| "Playing".into()),
            MediaAction::Pause => accepted(session.TryPauseAsync()).map(|_| "Paused".into()),
            MediaAction::Toggle => {
                accepted(session.TryTogglePlayPauseAsync()).map(|_| "Toggled play/pause".into())
            }
            MediaAction::Next => accepted(session.TrySkipNextAsync()).map(|_| "Next track".into()),
            MediaAction::Prev => accepted(session.TrySkipPreviousAsync()).map(|_| "Previous track".into()),
            MediaAction::Seek(_) | MediaAction::SeekBy(_) => {
                let target = match act {
                    MediaAction::SeekBy(delta) => {
                        let np = read_session(&session).map_err(|e| format!("{e:?}"))?;
                        seek_target(live_position_ms(&np, unix_now_ms()), delta, np.duration_ms)
                    }
                    MediaAction::Seek(ms) => ms,
                    _ => 0,
                };
                let start = session
                    .GetTimelineProperties()
                    .and_then(|t| t.StartTime())
                    .map(|t| t.Duration)
                    .unwrap_or(0);
                let ok = accepted(session.TryChangePlaybackPositionAsync(start + target as i64 * 10_000))?;
                refused(ok, &format!("Seek to {}:{:02}", target / 60_000, target / 1000 % 60))
            }
            MediaAction::Shuffle(on) => {
                let on = on.unwrap_or_else(|| !shuffle_of(info.as_ref()));
                let ok = accepted(session.TryChangeShuffleActiveAsync(on))?;
                refused(ok, if on { "Shuffle on" } else { "Shuffle off" })
            }
            MediaAction::Repeat(mode) => {
                let mode = mode.unwrap_or_else(|| next_repeat(repeat_of(info.as_ref())));
                let win = match mode {
                    "track" => MediaPlaybackAutoRepeatMode::Track,
                    "context" => MediaPlaybackAutoRepeatMode::List,
                    _ => MediaPlaybackAutoRepeatMode::None,
                };
                let ok = accepted(session.TryChangeAutoRepeatModeAsync(win))?;
                refused(ok, &format!("Repeat {mode}"))
            }
            MediaAction::SpotifyVolume(_) | MediaAction::SpotifyVolumeDelta(_) => {
                Err("Volume is not a session action.".into())
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_mpris {
    use super::{live_position_ms, next_repeat, seek_target, source_kind, unix_now_ms, MediaAction, NowPlaying};
    use crate::soul_debug;
    use zbus::blocking::Connection;
    use zbus::zvariant::OwnedValue;

    const IFACE: &str = "org.mpris.MediaPlayer2.Player";
    const PATH: &str = "/org/mpris/MediaPlayer2";
    const SPOTIFY: &str = "org.mpris.MediaPlayer2.spotify";

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

    /// The playing player; else Spotify; else the first one. `spotify_only` = Spotify or nothing.
    fn pick_player(conn: &Connection, spotify_only: bool) -> Result<String, String> {
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
        let players: Vec<String> =
            names.into_iter().filter(|n| n.starts_with("org.mpris.MediaPlayer2.")).collect();
        let spotify = players.iter().find(|n| n.starts_with(SPOTIFY)).cloned();
        if spotify_only {
            return spotify.ok_or_else(|| "Spotify is not running.".to_string());
        }
        let playing = players.iter().find(|n| {
            zbus::blocking::Proxy::new(conn, n.as_str(), PATH, IFACE)
                .ok()
                .and_then(|p| p.get_property::<String>("PlaybackStatus").ok())
                .is_some_and(|s| s.eq_ignore_ascii_case("Playing"))
        });
        playing
            .cloned()
            .or(spotify)
            .or_else(|| players.first().cloned())
            .ok_or_else(|| "No MPRIS players".to_string())
    }

    fn fetch_inner() -> Result<NowPlaying, String> {
        let conn = Connection::session().map_err(|e| e.to_string())?;
        let player = pick_player(&conn, false)?;
        read_player(&conn, &player)
    }

    fn read_player(conn: &Connection, player: &str) -> Result<NowPlaying, String> {
        let proxy = zbus::blocking::Proxy::new(conn, player, PATH, IFACE).map_err(|e| e.to_string())?;

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
        let repeat = match proxy.get_property::<String>("LoopStatus").unwrap_or_default().as_str() {
            "Track" => "track",
            "Playlist" => "context",
            _ => "off",
        };

        Ok(NowPlaying {
            title,
            artist,
            album,
            artwork_data_url,
            playing,
            available: true,
            position_ms,
            duration_ms,
            position_updated_at_ms: unix_now_ms(),
            playback_rate: 1.0,
            source_app: source_kind(player).into(),
            shuffle: proxy.get_property::<bool>("Shuffle").unwrap_or(false),
            repeat: repeat.into(),
            ..NowPlaying::default()
        })
    }

    /// Spotify's MPRIS `Volume` (0.0–1.0); `set` changes it first.
    pub fn spotify_volume(set: Option<f64>) -> Result<f64, String> {
        let conn = Connection::session().map_err(|e| e.to_string())?;
        let proxy = zbus::blocking::Proxy::new(&conn, SPOTIFY, PATH, IFACE).map_err(|e| e.to_string())?;
        if let Some(v) = set {
            proxy.set_property("Volume", v.clamp(0.0, 1.0)).map_err(|e| e.to_string())?;
        }
        proxy.get_property::<f64>("Volume").map_err(|e| e.to_string())
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

    pub fn action(spotify_only: bool, act: MediaAction) -> Result<String, String> {
        let conn = Connection::session().map_err(|e| e.to_string())?;
        let player = pick_player(&conn, spotify_only)?;
        let proxy =
            zbus::blocking::Proxy::new(&conn, player.as_str(), PATH, IFACE).map_err(|e| e.to_string())?;
        let call = |method: &str| proxy.call_method(method, &()).map(|_| ()).map_err(|e| e.to_string());
        match act {
            MediaAction::Play => call("Play").map(|_| "Playing".into()),
            MediaAction::Pause => call("Pause").map(|_| "Paused".into()),
            MediaAction::Toggle => call("PlayPause").map(|_| "Toggled play/pause".into()),
            MediaAction::Next => call("Next").map(|_| "Next track".into()),
            MediaAction::Prev => call("Previous").map(|_| "Previous track".into()),
            MediaAction::Seek(_) | MediaAction::SeekBy(_) => {
                // MPRIS `Seek` is relative (µs): turn both forms into an offset from now.
                let np = read_player(&conn, &player)?;
                let now = live_position_ms(&np, unix_now_ms());
                let target = match act {
                    MediaAction::SeekBy(delta) => seek_target(now, delta, np.duration_ms),
                    MediaAction::Seek(ms) => ms,
                    _ => now,
                };
                let offset_us = (target as i64 - now as i64) * 1000;
                proxy.call_method("Seek", &(offset_us,)).map_err(|e| e.to_string())?;
                Ok(format!("Seek to {}:{:02}", target / 60_000, target / 1000 % 60))
            }
            MediaAction::Shuffle(on) => {
                let on = on.unwrap_or_else(|| !proxy.get_property::<bool>("Shuffle").unwrap_or(false));
                proxy.set_property("Shuffle", on).map_err(|e| e.to_string())?;
                Ok(if on { "Shuffle on" } else { "Shuffle off" }.into())
            }
            MediaAction::Repeat(mode) => {
                let mode = mode.unwrap_or_else(|| {
                    let now = match proxy.get_property::<String>("LoopStatus").unwrap_or_default().as_str() {
                        "Track" => "track",
                        "Playlist" => "context",
                        _ => "off",
                    };
                    next_repeat(now)
                });
                let status = match mode {
                    "track" => "Track",
                    "context" => "Playlist",
                    _ => "None",
                };
                proxy.set_property("LoopStatus", status).map_err(|e| e.to_string())?;
                Ok(format!("Repeat {mode}"))
            }
            MediaAction::SpotifyVolume(_) | MediaAction::SpotifyVolumeDelta(_) => {
                Err("Volume is not a session action.".into())
            }
        }
    }
}

/// Spotify's MPRIS volume (0.0–1.0) for `spotify::local`; `set` changes it first.
#[cfg(target_os = "linux")]
pub(crate) fn spotify_mpris_volume(set: Option<f64>) -> Result<f64, String> {
    linux_mpris::spotify_volume(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use MediaAction::*;

    #[test]
    fn spotify_app_ids() {
        assert!(is_spotify_app_id("Spotify.exe"));
        assert!(is_spotify_app_id("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"));
        assert!(is_spotify_app_id("org.mpris.MediaPlayer2.spotify"));
        assert!(!is_spotify_app_id("chrome.exe"));
        assert_eq!(source_kind("msedge.exe"), "browser");
        assert_eq!(source_kind("Spotify.exe"), "spotify");
        assert_eq!(source_kind("vlc.exe"), "other");
    }

    #[test]
    fn parses_actions() {
        assert_eq!(parse_action("play"), Ok((false, Play)));
        assert_eq!(parse_action(" Toggle "), Ok((false, Toggle)));
        assert_eq!(parse_action("spotify:next"), Ok((true, Next)));
        assert_eq!(parse_action("seek:90000"), Ok((false, Seek(90_000))));
        assert_eq!(parse_action("seek_by:+30000"), Ok((false, SeekBy(30_000))));
        assert_eq!(parse_action("spotify:seek_by:-15000"), Ok((true, SeekBy(-15_000))));
        assert_eq!(parse_action("shuffle:on"), Ok((false, Shuffle(Some(true)))));
        assert_eq!(parse_action("shuffle:toggle"), Ok((false, Shuffle(None))));
        assert_eq!(parse_action("repeat:context"), Ok((false, Repeat(Some("context")))));
        assert_eq!(parse_action("repeat:toggle"), Ok((false, Repeat(None))));
        assert_eq!(parse_action("spotify_volume:40"), Ok((false, SpotifyVolume(40))));
        assert_eq!(parse_action("spotify_volume_delta:-20"), Ok((false, SpotifyVolumeDelta(-20))));
        for bad in ["seek:abc", "seek:-5", "spotify_volume:101", "spotify_volume_delta:200", "repeat:all", "dance"] {
            assert!(parse_action(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn seek_math() {
        assert_eq!(seek_target(10_000, -15_000, 200_000), 0);
        assert_eq!(seek_target(190_000, 30_000, 200_000), 200_000);
        assert_eq!(seek_target(10_000, 30_000, 0), 40_000);
        let np = NowPlaying {
            position_ms: 1_000,
            position_updated_at_ms: 5_000,
            duration_ms: 10_000,
            playing: true,
            ..NowPlaying::default()
        };
        assert_eq!(live_position_ms(&np, 7_000), 3_000);
        assert_eq!(live_position_ms(&np, 60_000), 10_000);
        assert_eq!(live_position_ms(&NowPlaying { playing: false, ..np }, 7_000), 1_000);
        assert_eq!(next_repeat("off"), "context");
        assert_eq!(next_repeat("context"), "track");
        assert_eq!(next_repeat("track"), "off");
    }
}
