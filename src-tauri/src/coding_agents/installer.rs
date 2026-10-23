//! One-click hooks for coding agents: `soul --agent-hook` entries in each agent's config.
//! Safe writer: strict JSON (a malformed file or non-object root is refused, never overwritten),
//! preview = unified diff + FNV-1a fingerprint of the exact bytes; apply re-checks the
//! fingerprint, writes a dated backup (never over an existing one), then a sibling temp file
//! renamed over the real target (symlinks resolved, so dotfile links survive). Foreign hooks and
//! every other key (`statusLine`…) are left alone.

use super::unified_diff;
use crate::island::plan::{fnv1a, home_dir, strip_bom, write_atomic};
use serde_json::{json, Map, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const MARK: &str = "--agent-hook";

/// (id, display name, config path under the home dir)
const AGENTS: &[(&str, &str, &str)] = &[
    ("claude", "Claude Code", ".claude/settings.json"),
    ("codex", "Codex", ".codex/hooks.json"),
    ("cursor", "Cursor", ".cursor/hooks.json"),
    ("gemini", "Gemini CLI", ".gemini/settings.json"),
    ("copilot", "Copilot CLI", ".copilot/hooks/soul.json"),
];

const CLAUDE_EVENTS: &[(&str, u32)] = &[
    ("SessionStart", 10),
    ("SessionEnd", 10),
    ("UserPromptSubmit", 10),
    ("PreToolUse", 10),
    ("PostToolUse", 10),
    ("PostToolUseFailure", 10),
    ("PermissionRequest", 120),
    ("Notification", 10),
    ("Stop", 10),
    ("StopFailure", 10),
    ("SubagentStart", 10),
    ("SubagentStop", 10),
];
const CODEX_EVENTS: &[(&str, u32)] = &[
    ("SessionStart", 10),
    ("UserPromptSubmit", 10),
    ("PreToolUse", 10),
    ("PermissionRequest", 120),
    ("PostToolUse", 10),
    ("Stop", 10),
    ("SubagentStart", 10),
    ("SubagentStop", 10),
    ("Interrupt", 3),
    ("SessionEnd", 3),
];
const CURSOR_EVENTS: &[&str] = &["sessionStart", "sessionEnd", "beforeSubmitPrompt", "preToolUse", "postToolUse", "postToolUseFailure", "stop"];
const GEMINI_EVENTS: &[&str] = &["SessionStart", "SessionEnd", "BeforeTool", "AfterTool", "BeforeAgent", "AfterAgent"];
const COPILOT_EVENTS: &[(&str, u32)] = &[
    ("sessionStart", 10),
    ("userPromptSubmitted", 10),
    ("preToolUse", 10),
    ("permissionRequest", 120),
    ("postToolUse", 10),
    ("agentStop", 10),
    ("sessionEnd", 10),
    ("notification", 10),
];

/// `"C:/…/soul.exe"` — forward slashes work in Git Bash, cmd and sh alike.
pub fn exe_arg(exe: &Path) -> String {
    format!("\"{}\"", exe.to_string_lossy().replace('\\', "/"))
}

/// (event key, hook entry) pairs Soul adds for `agent`.
pub fn entries(agent: &str, exe: &str) -> Vec<(String, Value)> {
    let cmd = |tail: &str| format!("{exe} {MARK} {tail}");
    match agent {
        "claude" => CLAUDE_EVENTS
            .iter()
            .map(|(e, t)| (e.to_string(), json!({ "hooks": [{ "type": "command", "command": cmd(e), "timeout": t }] })))
            .collect(),
        "codex" => CODEX_EVENTS
            .iter()
            .map(|(e, t)| {
                let mut hook = json!({ "type": "command", "command": cmd("--agent codex"), "timeout": t });
                if *e == "PermissionRequest" {
                    hook["statusMessage"] = json!("Waiting for your answer in the Island (Soul)");
                }
                (e.to_string(), json!({ "hooks": [hook] }))
            })
            .collect(),
        "cursor" => CURSOR_EVENTS.iter().map(|e| (e.to_string(), json!({ "command": cmd("--agent cursor") }))).collect(),
        "gemini" => GEMINI_EVENTS
            .iter()
            .map(|e| {
                let hook = json!({ "type": "command", "command": cmd("--agent gemini"), "timeout": 10_000 });
                (e.to_string(), json!({ "matcher": "*", "hooks": [hook] }))
            })
            .collect(),
        "copilot" => COPILOT_EVENTS
            .iter()
            .map(|(e, t)| {
                let mut hook = json!({ "type": "command", "bash": cmd("--agent copilot"), "timeoutSec": t });
                if cfg!(windows) {
                    hook["powershell"] = json!(format!("& {}", cmd("--agent copilot")));
                }
                (e.to_string(), hook)
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A hook entry written by Soul (its command runs `soul… --agent-hook`).
fn is_ours(entry: &Value) -> bool {
    let s = entry.to_string().to_ascii_lowercase();
    s.contains(MARK) && s.contains("soul")
}

pub fn is_installed(root: &Value) -> bool {
    root.get("hooks")
        .and_then(Value::as_object)
        .is_some_and(|h| h.values().filter_map(Value::as_array).flatten().any(is_ours))
}

/// Remove Soul's entries (and the arrays / `hooks` object only Soul's removal emptied), then add
/// the fresh ones when installing.
pub fn apply_hooks(root: &mut Value, agent: &str, exe: &str, install: bool) -> Result<(), String> {
    let obj = root.as_object_mut().ok_or("not a JSON object")?;
    // Only an empty `hooks` the user wrote themselves survives an uninstall.
    let user_empty = obj.get("hooks").and_then(Value::as_object).is_some_and(Map::is_empty);
    let hooks = obj.entry("hooks").or_insert_with(|| json!({})).as_object_mut().ok_or("\"hooks\" is not an object")?;
    let keys: Vec<String> = hooks.keys().cloned().collect();
    for k in keys {
        if let Some(arr) = hooks.get_mut(&k).and_then(Value::as_array_mut) {
            let before = arr.len();
            arr.retain(|e| !is_ours(e));
            if before > 0 && arr.is_empty() {
                hooks.remove(&k);
            }
        }
    }
    if install {
        for (event, entry) in entries(agent, exe) {
            hooks
                .entry(event.clone())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or_else(|| format!("\"hooks.{event}\" is not a list"))?
                .push(entry);
        }
        if matches!(agent, "cursor" | "copilot") && !obj.contains_key("version") {
            obj.insert("version".into(), json!(1));
        }
    } else if hooks.is_empty() && !user_empty {
        obj.remove("hooks");
    }
    Ok(())
}

fn agent_path(agent: &str) -> Result<(&'static str, PathBuf), String> {
    let (_, name, rel) = AGENTS.iter().find(|(id, _, _)| *id == agent).ok_or("Nieznany agent.")?;
    Ok((name, home_dir().ok_or("Brak katalogu domowego.")?.join(rel)))
}

/// Strict read: missing/empty = `{}`, malformed or non-object = error (nothing gets written).
pub fn read_config(path: &Path) -> Result<(Option<Vec<u8>>, Value), String> {
    let old = match fs::read(path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let value = match old.as_deref().map(strip_bom) {
        Some(b) if !b.iter().all(u8::is_ascii_whitespace) => serde_json::from_slice(b)
            .map_err(|_| format!("{} nie jest poprawnym JSON-em — nic nie zmieniono.", path.display()))?,
        _ => json!({}),
    };
    if !value.is_object() {
        return Err(format!("{} nie jest obiektem JSON — nic nie zmieniono.", path.display()));
    }
    Ok((old, value))
}

struct Plan {
    path: PathBuf,
    old: Option<Vec<u8>>,
    before: Value,
    after: Value,
}

fn plan(agent: &str, install: bool) -> Result<Plan, String> {
    let (_, path) = agent_path(agent)?;
    let (old, before) = read_config(&path)?;
    let mut after = before.clone();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    apply_hooks(&mut after, agent, &exe_arg(&exe), install)?;
    Ok(Plan { path, old, before, after })
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default() + "\n"
}

/// `<file>.bak-YYYYMMDD-HHMMSS` next to `target`, created exclusively (`-2`, `-3`… on a clash).
pub fn write_backup(target: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let name = target.file_name().ok_or("bad path")?.to_string_lossy().into_owned();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    for n in 1..100 {
        let suffix = if n == 1 { String::new() } else { format!("-{n}") };
        let path = target.with_file_name(format!("{name}.bak-{stamp}{suffix}"));
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                f.write_all(bytes).map_err(|e| e.to_string())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("Nie udało się utworzyć kopii zapasowej.".into())
}

/// Fingerprint check → backup → atomic write over the symlink's real target.
pub fn write_checked(path: &Path, old: Option<&[u8]>, fingerprint: &str, text: &str) -> Result<Option<PathBuf>, String> {
    if fnv1a(old.unwrap_or_default()) != fingerprint {
        return Err("Plik zmienił się od podglądu — otwórz podgląd ponownie.".into());
    }
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let backup = old.map(|b| write_backup(&target, b)).transpose()?;
    write_atomic(&target, text.as_bytes())?;
    Ok(backup)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentStatus {
    id: &'static str,
    name: &'static str,
    config_path: String,
    /// The agent's config folder exists (it is probably installed).
    detected: bool,
    installed: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookPlanPreview {
    diff: String,
    backup: bool,
    config_path: String,
    fingerprint: String,
}

#[tauri::command]
pub fn coding_agents_status() -> Vec<CodingAgentStatus> {
    AGENTS
        .iter()
        .filter_map(|(id, name, _)| {
            let (_, path) = agent_path(id).ok()?;
            let installed = read_config(&path).is_ok_and(|(_, v)| is_installed(&v));
            Some(CodingAgentStatus {
                id,
                name,
                detected: path.parent().is_some_and(Path::is_dir),
                config_path: path.to_string_lossy().into_owned(),
                installed,
            })
        })
        .collect()
}

#[tauri::command]
pub fn coding_agents_preview(agent: String, install: bool) -> Result<HookPlanPreview, String> {
    let p = plan(&agent, install)?;
    Ok(HookPlanPreview {
        diff: unified_diff(&pretty(&p.before), &pretty(&p.after), &p.path.to_string_lossy()),
        backup: p.old.is_some(),
        config_path: p.path.to_string_lossy().into_owned(),
        fingerprint: fnv1a(p.old.as_deref().unwrap_or_default()),
    })
}

#[tauri::command]
pub fn coding_agents_apply(agent: String, install: bool, fingerprint: String) -> Result<String, String> {
    let p = plan(&agent, install)?;
    if p.after == p.before && p.old.is_some() {
        return Ok("Bez zmian.".into());
    }
    let backup = write_checked(&p.path, p.old.as_deref(), &fingerprint, &pretty(&p.after))?;
    eprintln!("[soul][agent] coding agent hooks agent={agent} installed={install}");
    Ok(match backup {
        Some(b) => format!("OK — kopia: {}", b.display()),
        None => "OK".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "\"C:/Users/u/AppData/Local/Soul/soul.exe\"";

    #[test]
    fn claude_merge_keeps_foreign_hooks_and_status_line() {
        let foreign = json!({ "matcher": "Bash", "hooks": [{ "type": "command", "command": "~/guard.sh" }] });
        let mut s = json!({ "statusLine": { "type": "command", "command": "x" }, "hooks": { "PreToolUse": [foreign] } });
        let original = s.clone();
        apply_hooks(&mut s, "claude", EXE, true).unwrap();
        assert_eq!(s["statusLine"], original["statusLine"]);
        assert_eq!(s["hooks"]["PreToolUse"][0], original["hooks"]["PreToolUse"][0]);
        assert_eq!(s["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        let cmd = s["hooks"]["PermissionRequest"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, format!("{EXE} --agent-hook PermissionRequest"));
        assert_eq!(s["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"], 120);
        assert_eq!(s["hooks"].as_object().unwrap().len(), CLAUDE_EVENTS.len());
        assert!(is_installed(&s));
        // Installing twice doesn't duplicate.
        apply_hooks(&mut s, "claude", EXE, true).unwrap();
        assert_eq!(s["hooks"]["Stop"].as_array().unwrap().len(), 1);
        apply_hooks(&mut s, "claude", EXE, false).unwrap();
        assert_eq!(s, original);
        assert!(!is_installed(&s));
        let mut empty = json!({});
        apply_hooks(&mut empty, "claude", EXE, true).unwrap();
        apply_hooks(&mut empty, "claude", EXE, false).unwrap();
        assert_eq!(empty, json!({}));
    }

    #[test]
    fn codex_cursor_copilot_shapes() {
        let mut c = json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "notify" }] }] } });
        apply_hooks(&mut c, "codex", EXE, true).unwrap();
        let perm = &c["hooks"]["PermissionRequest"][0]["hooks"][0];
        assert_eq!(perm["command"], format!("{EXE} --agent-hook --agent codex"));
        assert_eq!(perm["statusMessage"], "Waiting for your answer in the Island (Soul)");
        assert_eq!(c["hooks"]["Interrupt"][0]["hooks"][0]["timeout"], 3);
        assert_eq!(c["hooks"]["Stop"].as_array().unwrap().len(), 2);
        apply_hooks(&mut c, "codex", EXE, false).unwrap();
        assert_eq!(c, json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "notify" }] }] } }));
        let mut cur = json!({});
        apply_hooks(&mut cur, "cursor", EXE, true).unwrap();
        assert_eq!(cur["version"], 1);
        assert_eq!(cur["hooks"]["beforeSubmitPrompt"][0]["command"], format!("{EXE} --agent-hook --agent cursor"));
        let mut g = json!({ "theme": "x" });
        apply_hooks(&mut g, "gemini", EXE, true).unwrap();
        assert_eq!(g["hooks"]["BeforeTool"][0]["matcher"], "*");
        assert_eq!(g["hooks"]["AfterAgent"][0]["hooks"][0]["timeout"], 10_000);
        let mut cp = json!({});
        apply_hooks(&mut cp, "copilot", EXE, true).unwrap();
        assert_eq!(cp["hooks"]["permissionRequest"][0]["timeoutSec"], 120);
        assert!(cp["hooks"]["agentStop"][0]["bash"].as_str().unwrap().ends_with("--agent-hook --agent copilot"));
        // Not an object / bad hooks: refused.
        assert!(apply_hooks(&mut json!({ "hooks": [] }), "claude", EXE, true).is_err());
        assert!(apply_hooks(&mut json!({ "hooks": { "Stop": {} } }), "claude", EXE, true).is_err());
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("soul-installer-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn strict_read_fingerprint_and_backup() {
        let dir = temp_dir("io");
        let path = dir.join("settings.json");
        fs::write(&path, b"{ not json").unwrap();
        assert!(read_config(&path).is_err());
        fs::write(&path, b"[1]").unwrap();
        assert!(read_config(&path).is_err());
        fs::write(&path, b"\xEF\xBB\xBF{\"a\":1}").unwrap();
        let (old, v) = read_config(&path).unwrap();
        assert_eq!(v, json!({ "a": 1 }));
        assert_eq!(read_config(&dir.join("missing.json")).unwrap(), (None, json!({})));
        let old = old.unwrap();
        // Stale fingerprint: nothing written.
        assert!(write_checked(&path, Some(&old), "0000", "{}\n").is_err());
        assert_eq!(fs::read(&path).unwrap(), old);
        let b1 = write_checked(&path, Some(&old), &fnv1a(&old), "{\"b\":2}\n").unwrap().unwrap();
        assert_eq!(fs::read(&b1).unwrap(), old);
        assert!(b1.file_name().unwrap().to_string_lossy().starts_with("settings.json.bak-"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"b\":2}\n");
        // A second backup in the same second never overwrites the first.
        let b2 = write_backup(&path, b"second").unwrap();
        assert_ne!(b1, b2);
        assert_eq!(fs::read(&b1).unwrap(), old);
        let _ = fs::remove_dir_all(&dir);
    }
}
