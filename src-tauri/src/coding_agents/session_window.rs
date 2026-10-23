//! „Open terminal”: which window runs a coding agent session. When a hook connects for an unknown
//! session, its process tree is walked up to the nearest ancestors owning a visible top-level
//! window (Windows Terminal, VS Code, Cursor…) and cached; the Island's ↗ focuses that window.
//! Fallback (classic conhost, closed window, Linux): open the session's folder in VS Code /
//! Explorer / xdg-open.

use crate::platform::close::Proc;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;

const MAX_DEPTH: usize = 16;
const MAX_SESSIONS: usize = 64;
/// Never walk into the shell / session roots (lowercase exe names).
pub const WINDOWS_TREE_TOPS: &[&str] = &[
    "explorer.exe", "services.exe", "wininit.exe", "winlogon.exe", "svchost.exe", "smss.exe", "csrss.exe", "system",
    "sihost.exe", "userinit.exe",
];

static SESSIONS: parking_lot::Mutex<VecDeque<(String, Vec<u32>)>> = parking_lot::const_mutex(VecDeque::new());

/// `pid` and its ancestors (nearest first), stopping before a tree top; loop-safe.
pub fn ancestors(procs: &[Proc], pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut cur = pid;
    while out.len() < MAX_DEPTH && !out.contains(&cur) {
        let Some(p) = procs.iter().find(|p| p.pid == cur) else { break };
        if WINDOWS_TREE_TOPS.contains(&p.exe.to_ascii_lowercase().as_str()) {
            break;
        }
        out.push(cur);
        if p.ppid == 0 {
            break;
        }
        cur = p.ppid;
    }
    out
}

fn folder_name(cwd: &str) -> Option<String> {
    cwd.rsplit(['/', '\\']).find(|s| !s.is_empty()).map(str::to_lowercase)
}

/// The window whose title names the project folder, else the first one.
pub fn pick_window(titles: &[&str], cwd: Option<&str>) -> Option<usize> {
    cwd.and_then(folder_name)
        .and_then(|f| titles.iter().position(|t| t.to_lowercase().contains(&f)))
        .or((!titles.is_empty()).then_some(0))
}

/// Remember the window-owning ancestors of a new session's hook process.
pub fn note(session_id: &str, pid: u32) {
    if SESSIONS.lock().iter().any(|(s, _)| s == session_id) {
        return;
    }
    let owners = window_owners(pid);
    let mut sessions = SESSIONS.lock();
    if sessions.iter().any(|(s, _)| s == session_id) {
        return;
    }
    sessions.push_back((session_id.to_string(), owners));
    while sessions.len() > MAX_SESSIONS {
        sessions.pop_front();
    }
}

#[cfg(windows)]
fn window_owners(pid: u32) -> Vec<u32> {
    use crate::platform::windows::{process_list, top_windows};
    let windows = top_windows();
    ancestors(&process_list(), pid)
        .into_iter()
        .filter(|a| windows.iter().any(|w| w.pid == *a && !w.title.trim().is_empty()))
        .collect()
}

// shortcut: Linux has no window lookup yet (falls back to opening the folder); add _NET_WM_PID when asked.
#[cfg(not(windows))]
fn window_owners(_pid: u32) -> Vec<u32> {
    Vec::new()
}

#[cfg(windows)]
fn focus_owner(owners: &[u32], cwd: Option<&str>) -> bool {
    let windows = crate::platform::windows::top_windows();
    owners.iter().any(|pid| {
        let mine: Vec<_> = windows.iter().filter(|w| w.pid == *pid && !w.title.trim().is_empty()).collect();
        let titles: Vec<&str> = mine.iter().map(|w| w.title.as_str()).collect();
        pick_window(&titles, cwd).is_some_and(|i| crate::platform::windows::focus_hwnd(mine[i].hwnd))
    })
}

#[cfg(not(windows))]
fn focus_owner(_owners: &[u32], _cwd: Option<&str>) -> bool {
    false
}

#[cfg(windows)]
fn reveal(dir: &Path) -> bool {
    crate::platform::windows::shell_open(&dir.to_string_lossy()).is_ok()
}

#[cfg(not(windows))]
fn reveal(dir: &Path) -> bool {
    crate::island::plan::command(Path::new("xdg-open")).arg(dir).stdin(Stdio::null()).spawn().is_ok()
}

/// VS Code when it is on PATH, else the folder (of a file) in the file manager. Never runs `target`.
fn open_in_editor(target: &Path) -> bool {
    if let Some(code) = crate::island::plan::find_on_path("code") {
        let spawned = crate::island::plan::command(&code)
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if spawned.is_ok() {
            return true;
        }
    }
    let dir = if target.is_dir() { Some(target) } else { target.parent() };
    dir.is_some_and(reveal)
}

#[tauri::command]
pub async fn coding_agent_focus_terminal(session_id: Option<String>, cwd: Option<String>) -> bool {
    let owners = session_id.and_then(|id| SESSIONS.lock().iter().find(|(s, _)| *s == id).map(|(_, o)| o.clone()));
    if owners.is_some_and(|o| focus_owner(&o, cwd.as_deref())) {
        return true;
    }
    cwd.map(PathBuf::from).filter(|p| p.is_absolute() && p.is_dir()).is_some_and(|dir| open_in_editor(&dir))
}

#[tauri::command]
pub async fn coding_agent_open_file(path: String) -> bool {
    let p = Path::new(&path);
    p.is_absolute() && p.is_file() && open_in_editor(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, exe: &str) -> Proc {
        Proc { pid, ppid, exe: exe.into(), ..Default::default() }
    }

    #[test]
    fn walks_up_to_the_tree_top() {
        let procs = [
            p(4, 0, "System"),
            p(100, 4, "explorer.exe"),
            p(200, 100, "WindowsTerminal.exe"),
            p(300, 200, "pwsh.exe"),
            p(400, 300, "node.exe"),
            p(500, 400, "soul.exe"),
        ];
        assert_eq!(ancestors(&procs, 500), vec![500, 400, 300, 200]);
        assert_eq!(ancestors(&procs, 999), Vec::<u32>::new());
        // A pid loop (reused parent ids) ends.
        let looped = [p(1, 2, "a"), p(2, 1, "b")];
        assert_eq!(ancestors(&looped, 1), vec![1, 2]);
        let chain: Vec<Proc> = (1..40).map(|i| p(i, i + 1, "x")).collect();
        assert_eq!(ancestors(&chain, 1).len(), MAX_DEPTH);
    }

    #[test]
    fn picks_the_project_window() {
        let titles = ["Windows PowerShell", "Soul — claude", "other"];
        assert_eq!(pick_window(&titles, Some(r"C:\Users\Oskar\Desktop\Soul")), Some(1));
        assert_eq!(pick_window(&titles, Some("/home/u/nothing/")), Some(0));
        assert_eq!(pick_window(&titles, None), Some(0));
        assert_eq!(pick_window(&[], Some("/x")), None);
    }
}
