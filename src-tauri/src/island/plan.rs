//! Claude Code / Codex plan usage for the Island gauges.
//!
//! Codex: `codex app-server` (JSON-RPC over stdio) → `account/rateLimits/read`; only the rate
//! limits leave this module (the account part, with the user's email, is dropped).
//! Claude Code: its statusLine command gets `rate_limits` on stdin. `claude_plan_apply` points
//! `~/.claude/settings.json` → `statusLine.command` at `soul --claude-statusline`, which saves them
//! to `<app data>/claude-plan.json` and still runs the user's previous status line command.

use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

const IDENTIFIER: &str = "com.oskar.soul";
const PLAN_FILE: &str = "claude-plan.json";
const PREVIOUS_FILE: &str = "statusline-previous.json";
const RELAY_FLAG: &str = "--claude-statusline";

// ───────────────────────────── processes ─────────────────────────────

#[cfg(windows)]
pub(crate) fn command(program: &Path) -> Command {
    crate::platform::windows::hidden_command(&program.to_string_lossy())
}

#[cfg(not(windows))]
#[allow(clippy::disallowed_methods)] // no console window to flash outside Windows
pub(crate) fn command(program: &Path) -> Command {
    Command::new(program)
}

fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    let _ = crate::platform::windows::hidden_command("taskkill")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn find_on_path(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| exts.iter().map(move |e| dir.join(format!("{name}{e}"))))
        .find(|p| p.is_file())
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from)
}

// ───────────────────────────── Codex ─────────────────────────────

const REQUESTS: &str = concat!(
    r#"{"id":1,"method":"initialize","params":{"clientInfo":{"name":"soul","version":"1"}}}"#,
    "\n",
    r#"{"method":"initialized"}"#,
    "\n",
    r#"{"id":2,"method":"account/rateLimits/read"}"#,
    "\n",
);
const MAX_LINE: usize = 256 * 1024;
const MAX_TOTAL: usize = 2 * 1024 * 1024;
const CODEX_TIMEOUT: Duration = Duration::from_secs(15);

static BUSY: AtomicBool = AtomicBool::new(false);

struct NotBusy;
impl Drop for NotBusy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

fn find_codex() -> Option<PathBuf> {
    if let Some(p) = find_on_path("codex") {
        return Some(p);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        if let Some(d) = env_path("APPDATA") {
            candidates.push(d.join("npm").join("codex.cmd"));
        }
        if let Some(d) = env_path("LOCALAPPDATA") {
            candidates.push(d.join("Volta").join("bin").join("codex.exe"));
            candidates.push(d.join("pnpm").join("codex.cmd"));
        }
        if let Some(d) = env_path("USERPROFILE") {
            candidates.push(d.join(".bun").join("bin").join("codex.exe"));
            candidates.push(d.join(".local").join("bin").join("codex.exe"));
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = env_path("HOME") {
            for dir in [".local/bin", ".npm-global/bin", ".volta/bin", ".bun/bin", ".local/share/pnpm"] {
                candidates.push(home.join(dir).join("codex"));
            }
            if let Ok(nodes) = fs::read_dir(home.join(".nvm/versions/node")) {
                candidates.extend(nodes.flatten().map(|e| e.path().join("bin/codex")));
            }
        }
        candidates.push("/usr/local/bin/codex".into());
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// The answer to request 2 in one stdout line (`None` for every other line).
pub fn first_answer(line: &[u8]) -> Option<Result<Value, String>> {
    let v: Value = serde_json::from_slice(line).ok()?;
    if v.get("id").and_then(Value::as_i64) != Some(2) {
        return None;
    }
    Some(match v.get("error") {
        Some(e) => Err(format!(
            "Codex: {}",
            e.get("message").and_then(Value::as_str).unwrap_or("rate limits unavailable")
        )),
        None => Ok(limits_only(v.get("result").unwrap_or(&Value::Null))),
    })
}

/// Only the limits: the rest of the result (account, email) never leaves Rust.
pub fn limits_only(result: &Value) -> Value {
    json!({
        "rateLimits": result.get("rateLimits").cloned().unwrap_or(Value::Null),
        "rateLimitResetCredits": result.get("rateLimitResetCredits").cloned().unwrap_or(Value::Null),
    })
}

fn query_codex() -> Result<Value, String> {
    let exe = find_codex().ok_or("Nie znaleziono Codex CLI (codex).")?;
    let mut cmd = command(&exe);
    cmd.arg("app-server").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    // npm / pnpm `.cmd` wrappers look for node.exe next to themselves.
    if let Some(dir) = exe.parent() {
        let rest = std::env::var_os("PATH").unwrap_or_default();
        if let Ok(path) = std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&rest))) {
            cmd.env("PATH", path);
        }
    }
    let mut child = cmd.spawn().map_err(|e| format!("Codex: {e}"))?;
    let result = read_answer(&mut child);
    kill_tree(&mut child);
    result
}

fn read_answer(child: &mut Child) -> Result<Value, String> {
    let mut stdin = child.stdin.take().ok_or("Codex: no stdin")?;
    stdin.write_all(REQUESTS.as_bytes()).map_err(|e| format!("Codex: {e}"))?;
    let stdout = child.stdout.take().ok_or("Codex: no stdout")?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut total = 0;
        loop {
            let mut line = Vec::new();
            match reader.by_ref().take(MAX_LINE as u64).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(n) => total += n,
            }
            if total > MAX_TOTAL || tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + CODEX_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => {
                if let Some(answer) = first_answer(&line) {
                    drop(stdin);
                    return answer;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => return Err("Codex nie odpowiedział w 15 s.".into()),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("Codex zakończył się bez odpowiedzi (zaloguj się: codex login).".into()),
        }
    }
}

/// `{ rateLimits, rateLimitResetCredits }` from the local Codex CLI (one query at a time).
#[tauri::command]
pub async fn codex_plan_usage() -> Result<Value, String> {
    if BUSY.swap(true, Ordering::SeqCst) {
        return Err("busy".into());
    }
    tauri::async_runtime::spawn_blocking(|| {
        let _guard = NotBusy;
        let t0 = Instant::now();
        let r = query_codex();
        eprintln!("[soul][island] codex plan ok={} ms={}", r.is_ok(), t0.elapsed().as_millis());
        r
    })
    .await
    .map_err(|e| e.to_string())?
}

// ───────────────────────────── Claude statusLine relay ─────────────────────────────

/// Tauri's `app_data_dir` without Tauri (the relay exits before the app is built).
pub(crate) fn data_dir_standalone() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = env_path("APPDATA");
    #[cfg(not(windows))]
    let base = env_path("XDG_DATA_HOME").or_else(|| env_path("HOME").map(|h| h.join(".local/share")));
    Some(base?.join(IDENTIFIER))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub(crate) fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b)
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".soul-tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

/// What `claude-plan.json` gets from one status line input (`None` without `rate_limits`).
pub fn relay_record(input: &[u8], now_ms: u64) -> Option<Value> {
    let v: Value = serde_json::from_slice(strip_bom(input)).ok()?;
    let limits = v.get("rate_limits").filter(|r| r.is_object())?;
    Some(json!({
        "session_id": v.get("session_id").cloned().unwrap_or(Value::Null),
        "rate_limits": limits,
        "updated_at_ms": now_ms,
    }))
}

/// `soul --claude-statusline`: called by Claude Code on every status line refresh.
pub fn run_statusline() {
    let mut input = Vec::new();
    let _ = std::io::stdin().take(4 * 1024 * 1024).read_to_end(&mut input);
    let Some(dir) = data_dir_standalone() else { return };
    if let Some(record) = relay_record(&input, now_ms()) {
        let _ = write_atomic(&dir.join(PLAN_FILE), record.to_string().as_bytes());
    }
    let previous = fs::read(dir.join(PREVIOUS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(strip_bom(&b)).ok());
    let Some(cmd) = previous.as_ref().and_then(|p| p.get("command")).and_then(Value::as_str).filter(|c| !c.trim().is_empty()) else {
        return;
    };
    if let Some(out) = run_previous(cmd, &input) {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
    }
}

#[cfg(windows)]
fn shell() -> Option<PathBuf> {
    // Claude Code runs status lines through Git Bash on Windows; never WSL's bash.
    let ok = |p: &Path| {
        let s = p.to_string_lossy().to_ascii_lowercase();
        p.is_file() && !s.contains("\\system32\\") && !s.contains("\\windowsapps\\")
    };
    if let Some(p) = env_path("CLAUDE_CODE_GIT_BASH_PATH").filter(|p| ok(p)) {
        return Some(p);
    }
    if let Some(git) = find_on_path("git") {
        // <Git>\cmd\git.exe, <Git>\bin\git.exe or <Git>\mingw64\bin\git.exe
        for dir in git.ancestors().skip(1).take(3) {
            for c in [dir.join("bin").join("bash.exe"), dir.join("bash.exe")] {
                if ok(&c) {
                    return Some(c);
                }
            }
        }
    }
    ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|k| env_path(k))
        .map(|pf| pf.join("Git").join("bin").join("bash.exe"))
        .find(|p| ok(p))
}

#[cfg(not(windows))]
fn shell() -> Option<PathBuf> {
    Some("/bin/sh".into())
}

/// The user's previous status line command: same stdin, its stdout (10 s, 64 KB).
fn run_previous(cmd: &str, input: &[u8]) -> Option<Vec<u8>> {
    const TIMEOUT: Duration = Duration::from_secs(10);
    const MAX_OUT: u64 = 64 * 1024;
    let mut child = command(&shell()?)
        .args(["-c", cmd])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let input = input.to_vec();
    thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.take(MAX_OUT).read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
        thread::sleep(Duration::from_millis(20));
    }
    kill_tree(&mut child);
    // Output already written by a finished command is drained for a moment more.
    rx.recv_timeout(Duration::from_millis(200)).ok()
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("USERPROFILE")
    } else {
        env_path("HOME")
    }
}

fn claude_settings_path() -> Result<PathBuf, String> {
    Ok(home_dir().ok_or("Brak katalogu domowego.")?.join(".claude").join("settings.json"))
}

/// FNV-1a (64-bit) over the exact bytes: the preview must still match when applied.
pub fn fnv1a(bytes: &[u8]) -> String {
    let hash = bytes
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3));
    format!("{hash:016x}")
}

pub fn relay_command(exe: &Path) -> String {
    format!("'{}' {RELAY_FLAG}", exe.to_string_lossy().replace('\\', "/"))
}

fn is_relay(status_line: &Value) -> bool {
    status_line.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(RELAY_FLAG))
}

/// Point `statusLine` at the relay; returns the user's own status line to keep (if any).
pub fn install_status_line(settings: &mut Value, command: &str) -> Option<Value> {
    let obj = settings.as_object_mut()?;
    let previous = obj.get("statusLine").filter(|v| !is_relay(v)).cloned();
    let mut line = json!({ "type": "command", "command": command });
    if let Some(padding) = previous.as_ref().and_then(|p| p.get("padding")) {
        line["padding"] = padding.clone();
    }
    obj.insert("statusLine".into(), line);
    previous
}

/// Put the saved status line back (or remove ours). Someone else's status line is left alone.
pub fn uninstall_status_line(settings: &mut Value, previous: Option<Value>) {
    let Some(obj) = settings.as_object_mut() else { return };
    if !obj.get("statusLine").is_some_and(is_relay) {
        return;
    }
    match previous {
        Some(p) => obj.insert("statusLine".into(), p),
        None => obj.remove("statusLine"),
    };
}

struct Change {
    path: PathBuf,
    old: Option<Vec<u8>>,
    before: Value,
    after: Value,
    /// Install: the user's status line to save in `statusline-previous.json`.
    keep: Option<Value>,
}

fn plan_change(app: &AppHandle, install: bool) -> Result<Change, String> {
    let path = claude_settings_path()?;
    let old = match fs::read(&path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let before: Value = match old.as_deref().map(strip_bom) {
        Some(b) if !b.iter().all(u8::is_ascii_whitespace) => serde_json::from_slice(b)
            .map_err(|_| "~/.claude/settings.json nie jest poprawnym JSON-em — nic nie zmieniono.".to_string())?,
        _ => json!({}),
    };
    if !before.is_object() {
        return Err("~/.claude/settings.json nie jest obiektem JSON — nic nie zmieniono.".into());
    }
    let mut after = before.clone();
    let keep = if install {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        install_status_line(&mut after, &relay_command(&exe))
    } else {
        let saved = fs::read(data_dir(app)?.join(PREVIOUS_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(strip_bom(&b)).ok());
        uninstall_status_line(&mut after, saved);
        None
    };
    Ok(Change { path, old, before, after, keep })
}

fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

fn backup_path(settings: &Path) -> PathBuf {
    settings.with_file_name(format!("settings.json.bak-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")))
}

/// `- old` / `+ new` lines of the `statusLine` entry (the only key that changes).
pub fn status_diff(before: &Value, after: &Value) -> String {
    let side = |v: &Value, sign: char| match v.get("statusLine") {
        Some(s) => format!("\"statusLine\": {}", serde_json::to_string_pretty(s).unwrap_or_default())
            .lines()
            .map(|l| format!("{sign} {l}\n"))
            .collect::<String>(),
        None => String::new(),
    };
    format!("{}{}", side(before, '-'), side(after, '+'))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeRelayStatus {
    installed: bool,
    settings_path: String,
    usage: Option<Value>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookPreview {
    diff: String,
    backup: String,
    settings_path: String,
    fingerprint: String,
}

#[tauri::command]
pub fn claude_plan_status(app: AppHandle) -> Result<ClaudeRelayStatus, String> {
    let path = claude_settings_path()?;
    let installed = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(strip_bom(&b)).ok())
        .is_some_and(|s| s.get("statusLine").is_some_and(is_relay));
    let usage = fs::read(data_dir(&app)?.join(PLAN_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(strip_bom(&b)).ok());
    Ok(ClaudeRelayStatus { installed, settings_path: path.to_string_lossy().into_owned(), usage })
}

#[tauri::command]
pub fn claude_plan_preview(app: AppHandle, install: bool) -> Result<HookPreview, String> {
    let c = plan_change(&app, install)?;
    Ok(HookPreview {
        diff: status_diff(&c.before, &c.after),
        backup: if c.old.is_some() { backup_path(&c.path).to_string_lossy().into_owned() } else { String::new() },
        settings_path: c.path.to_string_lossy().into_owned(),
        fingerprint: fnv1a(c.old.as_deref().unwrap_or_default()),
    })
}

#[tauri::command]
pub fn claude_plan_apply(app: AppHandle, install: bool, fingerprint: String) -> Result<String, String> {
    let c = plan_change(&app, install)?;
    if fnv1a(c.old.as_deref().unwrap_or_default()) != fingerprint {
        return Err("settings.json zmienił się od podglądu — otwórz podgląd ponownie.".into());
    }
    let dir = data_dir(&app)?;
    if let Some(keep) = &c.keep {
        write_atomic(&dir.join(PREVIOUS_FILE), keep.to_string().as_bytes())?;
    }
    let backup = match &c.old {
        Some(old) => {
            let b = backup_path(&c.path);
            fs::write(&b, old).map_err(|e| e.to_string())?;
            b.to_string_lossy().into_owned()
        }
        None => String::new(),
    };
    let text = serde_json::to_string_pretty(&c.after).map_err(|e| e.to_string())? + "\n";
    write_atomic(&c.path, text.as_bytes())?;
    if !install {
        let _ = fs::remove_file(dir.join(PREVIOUS_FILE));
    }
    eprintln!("[soul][island] claude statusLine relay installed={install}");
    Ok(if backup.is_empty() { "OK".into() } else { format!("OK — kopia: {backup}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_answer_keeps_only_limits() {
        assert!(first_answer(br#"{"id":1,"result":{}}"#).is_none());
        assert!(first_answer(b"not json").is_none());
        let line = br#"{"id":2,"result":{"account":{"email":"a@b.c"},"rateLimits":{"primary":{"usedPercent":5}},"rateLimitResetCredits":2}}"#;
        let v = first_answer(line).unwrap().unwrap();
        assert_eq!(v["rateLimits"]["primary"]["usedPercent"], 5);
        assert_eq!(v["rateLimitResetCredits"], 2);
        assert!(!v.to_string().contains("email"));
        assert!(first_answer(br#"{"id":2,"error":{"message":"not logged in"}}"#).unwrap().is_err());
    }

    #[test]
    fn relay_saves_rate_limits_only() {
        let input = b"\xEF\xBB\xBF{\"session_id\":\"s1\",\"cwd\":\"C:/x\",\"rate_limits\":{\"five_hour\":{\"used_percentage\":7}}}";
        let r = relay_record(input, 42).unwrap();
        assert_eq!(r, json!({ "session_id": "s1", "rate_limits": { "five_hour": { "used_percentage": 7 } }, "updated_at_ms": 42 }));
        assert!(relay_record(br#"{"session_id":"s1"}"#, 1).is_none());
    }

    #[test]
    fn status_line_install_and_restore() {
        let cmd = relay_command(Path::new(r"C:\Program Files\Soul\soul.exe"));
        assert_eq!(cmd, "'C:/Program Files/Soul/soul.exe' --claude-statusline");
        let mine = json!({ "type": "command", "command": "~/bin/status.sh", "padding": 1 });
        let mut s = json!({ "model": "opus", "statusLine": mine });
        let keep = install_status_line(&mut s, &cmd);
        assert_eq!(keep, Some(mine.clone()));
        assert_eq!(s["statusLine"], json!({ "type": "command", "command": cmd, "padding": 1 }));
        assert_eq!(s["model"], "opus");
        // Installing again never replaces the saved status line with ours.
        assert_eq!(install_status_line(&mut s.clone(), &cmd), None);
        assert!(status_diff(&json!({ "statusLine": mine }), &s).contains("+   \"command\": \"'C:/Program Files"));
        uninstall_status_line(&mut s, keep);
        assert_eq!(s["statusLine"], mine);
        // Someone else's status line stays; ours without a saved one is removed.
        uninstall_status_line(&mut s, None);
        assert_eq!(s["statusLine"], mine);
        let mut fresh = json!({});
        install_status_line(&mut fresh, &cmd);
        uninstall_status_line(&mut fresh, None);
        assert_eq!(fresh, json!({}));
    }

    #[test]
    fn fingerprint_and_backup_name() {
        assert_eq!(fnv1a(b""), "cbf29ce484222325");
        assert_ne!(fnv1a(b"{}"), fnv1a(b"{ }"));
        let b = backup_path(Path::new("/home/u/.claude/settings.json"));
        let name = b.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("settings.json.bak-") && name.len() == "settings.json.bak-20261023-120000".len(), "{name}");
    }
}
