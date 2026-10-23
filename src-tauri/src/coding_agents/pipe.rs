//! Same-user IPC between `soul --agent-hook` and the running app: `\\.\pipe\soul-<SID>` on
//! Windows (both ends check the other process runs as the same user), `$XDG_RUNTIME_DIR/soul.sock`
//! on Linux (0700 dir, `SO_PEERCRED` on both ends). One JSON line per connection.
//!
//! `PermissionRequest` (agents that take decisions) is two-phase: the Island must `ack` within
//! `ACK_TIMEOUT` (else the pipe closes and the terminal asks in < 1 s), then a decision has
//! `DECISION_TIMEOUT`. The reply is one `{"decision": …}` line; no reply = no opinion.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

pub const ACK_TIMEOUT: Duration = Duration::from_millis(800);
pub const DECISION_TIMEOUT: Duration = Duration::from_secs(108);
const READ_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_MESSAGE: u64 = 4 * 1024 * 1024;

pub enum Reply {
    Ack,
    Decision(String),
    Decline,
}

static PENDING: once_cell::sync::Lazy<parking_lot::Mutex<HashMap<String, UnboundedSender<Reply>>>> =
    once_cell::sync::Lazy::new(Default::default);
/// The acked request on screen: (id, is an `AskUserQuestion`).
static ACTIVE: parking_lot::Mutex<Option<(String, bool)>> = parking_lot::const_mutex(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

// ───────────────────────────── client (the hook process) ─────────────────────────────

#[cfg(windows)]
fn pipe_name() -> Option<String> {
    let sid = crate::platform::windows::user_sid(None).or_else(|| {
        std::env::var("USERNAME").ok().map(|u| u.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect())
    })?;
    Some(format!(r"\\.\pipe\soul-{sid}"))
}

#[cfg(windows)]
fn connect() -> Option<std::fs::File> {
    use std::os::windows::io::AsRawHandle;
    const ERROR_PIPE_BUSY: i32 = 231;
    let name = pipe_name()?;
    let deadline = std::time::Instant::now() + super::relay::CONNECT_TIMEOUT;
    let file = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
            Ok(f) => break f,
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => return None,
        }
    };
    // Not a single byte before the server is proven to run as this user.
    let server = crate::platform::windows::pipe_peer_pid(file.as_raw_handle() as isize, true)?;
    let own = crate::platform::windows::user_sid(None)?;
    (crate::platform::windows::user_sid(Some(server))? == own).then_some(file)
}

#[cfg(target_os = "linux")]
fn socket_path() -> Option<std::path::PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| format!("/run/user/{uid}").into());
    let meta = std::fs::metadata(&dir).ok()?;
    (meta.is_dir() && meta.uid() == uid && meta.permissions().mode() & 0o777 == 0o700).then(|| dir.join("soul.sock"))
}

#[cfg(target_os = "linux")]
fn peer_uid(fd: std::os::fd::RawFd) -> Option<u32> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` describe a properly sized ucred buffer.
    let rc = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len) };
    (rc == 0).then_some(cred.uid)
}

#[cfg(target_os = "linux")]
fn connect() -> Option<std::os::unix::net::UnixStream> {
    use std::os::fd::AsRawFd;
    let stream = std::os::unix::net::UnixStream::connect(socket_path()?).ok()?;
    // SAFETY: getuid has no preconditions.
    (peer_uid(stream.as_raw_fd())? == unsafe { libc::getuid() }).then_some(stream)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn connect() -> Option<std::fs::File> {
    None
}

/// Hook side: send the payload; with `wait`, return the Island's decision.
pub fn send(payload: &Value, wait: bool) -> Option<String> {
    let mut stream = connect()?;
    stream.write_all(format!("{payload}\n").as_bytes()).ok()?;
    // The server closes right after its (optional) reply line.
    let mut reply = String::new();
    let _ = Read::by_ref(&mut stream).take(64 * 1024).read_to_string(&mut reply);
    if !wait {
        return None;
    }
    let v: Value = serde_json::from_str(reply.trim()).ok()?;
    v.get("decision")?.as_str().map(str::to_string)
}

// ───────────────────────────── server (the app) ─────────────────────────────

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = serve(app).await {
            eprintln!("[soul][agent] coding agents pipe unavailable: {e}");
        }
    });
}

#[cfg(windows)]
async fn serve(app: AppHandle) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use tokio::net::windows::named_pipe::ServerOptions;
    use crate::platform::windows::{pipe_peer_pid, user_sid};
    let name = pipe_name().ok_or_else(|| std::io::Error::other("no user SID"))?;
    let own = user_sid(None).ok_or_else(|| std::io::Error::other("no user SID"))?;
    // first_pipe_instance: fails when another Soul (or a squatter) already owns the name.
    let mut server = ServerOptions::new().first_pipe_instance(true).create(&name)?;
    loop {
        server.connect().await?;
        let client = std::mem::replace(&mut server, ServerOptions::new().create(&name)?);
        let pid = pipe_peer_pid(client.as_raw_handle() as isize, false);
        if pid.and_then(|p| user_sid(Some(p))).as_deref() != Some(own.as_str()) {
            continue;
        }
        tauri::async_runtime::spawn(handle(app.clone(), client, pid));
    }
}

#[cfg(target_os = "linux")]
async fn serve(app: AppHandle) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = socket_path().ok_or_else(|| std::io::Error::other("no private XDG_RUNTIME_DIR"))?;
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        return Err(std::io::Error::other("another Soul owns the socket"));
    }
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(cred) = stream.peer_cred() else { continue };
        if cred.uid() != uid {
            continue;
        }
        let pid = cred.pid().and_then(|p| u32::try_from(p).ok());
        tauri::async_runtime::spawn(handle(app.clone(), stream, pid));
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
async fn serve(_app: AppHandle) -> std::io::Result<()> {
    Err(std::io::Error::other("unsupported platform"))
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

async fn handle<S>(app: AppHandle, stream: S, pid: Option<u32>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (r, mut w) = tokio::io::split(stream);
    let mut reader = BufReader::new(r.take(MAX_MESSAGE));
    let mut line = String::new();
    if !matches!(tokio::time::timeout(READ_TIMEOUT, reader.read_line(&mut line)).await, Ok(Ok(n)) if n > 0) {
        return;
    }
    let Ok(mut payload) = serde_json::from_str::<Value>(&line) else { return };
    if !payload.is_object() {
        return;
    }
    let event = str_of(&payload, "hook_event_name").to_string();
    let agent = str_of(&payload, "soul_agent").to_string();
    if let (Some(pid), session) = (pid, str_of(&payload, "session_id")) {
        if !session.is_empty() {
            let session = session.to_string();
            tauri::async_runtime::spawn_blocking(move || super::session_window::note(&session, pid));
        }
    }
    if event != "PermissionRequest" || !super::relay::takes_decisions(&agent) {
        let (a, p) = (app.clone(), payload.clone());
        tauri::async_runtime::spawn_blocking(move || super::recap::observe(&a, &p));
        let _ = app.emit_to("island", "island://coding-hook", &payload);
        return;
    }
    if !crate::config::load_settings(&app).map(|s| s.island_enabled).unwrap_or(true) {
        return;
    }
    let id = format!("r{}", NEXT_ID.fetch_add(1, Ordering::SeqCst));
    let is_question = str_of(&payload, "tool_name") == "AskUserQuestion";
    let (tx, mut rx) = unbounded_channel();
    PENDING.lock().insert(id.clone(), tx);
    payload["request_id"] = json!(id);
    let _ = app.emit_to("island", "island://coding-hook", &payload);
    let decision = wait_decision(&mut rx, &mut reader, &id, is_question).await;
    PENDING.lock().remove(&id);
    {
        let mut active = ACTIVE.lock();
        if active.as_ref().is_some_and(|(a, _)| *a == id) {
            *active = None;
        }
    }
    if let Some(d) = &decision {
        eprintln!("[soul][agent] coding agent permission answered from the Island");
        super::recap::record_decision(&app, &agent, d);
        let _ = w.write_all(format!("{}\n", json!({ "decision": d })).as_bytes()).await;
        let _ = w.shutdown().await;
    }
    // Closes the card whatever happened (answered, timed out, the terminal took over).
    let _ = app.emit_to("island", "island://coding-decided", json!({ "requestId": id }));
}

async fn wait_decision<R: AsyncRead + Unpin>(
    rx: &mut UnboundedReceiver<Reply>,
    reader: &mut R,
    id: &str,
    is_question: bool,
) -> Option<String> {
    match tokio::time::timeout(ACK_TIMEOUT, rx.recv()).await.ok().flatten()? {
        Reply::Decision(d) => return Some(d),
        Reply::Decline => return None,
        Reply::Ack => {}
    }
    *ACTIVE.lock() = Some((id.to_string(), is_question));
    let deadline = tokio::time::Instant::now() + DECISION_TIMEOUT;
    let mut buf = [0u8; 64];
    loop {
        tokio::select! {
            r = rx.recv() => match r {
                Some(Reply::Decision(d)) => return Some(d),
                Some(Reply::Ack) => {}
                _ => return None,
            },
            // The hook went away (answered in the terminal, agent interrupted).
            n = reader.read(&mut buf) => if matches!(n, Ok(0) | Err(_)) { return None },
            _ = tokio::time::sleep_until(deadline) => return None,
        }
    }
}

fn reply(request_id: &str, r: Reply) -> bool {
    PENDING.lock().get(request_id).is_some_and(|tx| tx.send(r).is_ok())
}

/// The approval card is on screen.
#[tauri::command]
pub fn coding_agent_ack(request_id: String) -> bool {
    reply(&request_id, Reply::Ack)
}

#[tauri::command]
pub fn coding_agent_decide(request_id: String, decision: String) -> Result<(), String> {
    if !matches!(decision.as_str(), "allow" | "deny" | "always") {
        return Err("Unknown decision.".into());
    }
    reply(&request_id, Reply::Decision(decision)).then_some(()).ok_or_else(|| "The request is gone.".into())
}

/// Hand the prompt back to the agent's terminal.
#[tauri::command]
pub fn coding_agent_decline(request_id: String) {
    reply(&request_id, Reply::Decline);
}

/// `AskUserQuestion` answers: `{ "<question>": "<label>[, <label>…]" }`.
#[tauri::command]
pub fn coding_agent_answer_question(request_id: String, answers: Value) -> Result<(), String> {
    if !answers.is_object() {
        return Err("Answers must be an object.".into());
    }
    let d = json!({ "answers": answers }).to_string();
    reply(&request_id, Reply::Decision(d)).then_some(()).ok_or_else(|| "The request is gone.".into())
}

/// An approval card (not a question) waits for an answer.
pub fn has_pending_approval(_app: &AppHandle) -> bool {
    ACTIVE.lock().as_ref().is_some_and(|(_, question)| !question)
}

/// Spoken „tak” / „nie”: answer the approval on screen.
pub fn answer_active(_app: &AppHandle, decision: &str) -> bool {
    let id = ACTIVE.lock().as_ref().filter(|(_, q)| !q).map(|(id, _)| id.clone());
    id.is_some_and(|id| reply(&id, Reply::Decision(decision.to_string())))
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn pipe_is_named_after_the_user_sid() {
        let own = crate::platform::windows::user_sid(None).unwrap();
        assert!(own.starts_with("S-1-5-"), "{own}");
        assert_eq!(crate::platform::windows::user_sid(Some(std::process::id())), Some(own.clone()));
        assert_eq!(super::pipe_name().unwrap(), format!(r"\\.\pipe\soul-{own}"));
        // Soul not running (no such pipe in a test) → the hook gives up at once.
        let t0 = std::time::Instant::now();
        assert!(super::send(&serde_json::json!({}), true).is_none() || super::connect().is_some());
        assert!(t0.elapsed() < std::time::Duration::from_millis(500));
    }
}
