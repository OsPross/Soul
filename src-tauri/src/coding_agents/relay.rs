//! `soul --agent-hook [--agent <name>] [<Event>]`: the hook command every coding agent runs.
//!
//! Hard rule: never block the agent. Soul not running → exit at once with the agent's safe
//! „no opinion” stdout. Only a `PermissionRequest` from an agent that takes decisions waits for
//! the Island (≤ `DECISION_BUDGET`); everything else is fire and forget (≤ 2 s). Nothing is ever
//! allowed without a human decision: no answer → no output (the agent asks in its terminal).

use crate::island::plan::strip_bom;
use serde_json::{json, Map, Value};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
pub const FIRE_AND_FORGET_BUDGET: Duration = Duration::from_secs(2);
pub const DECISION_BUDGET: Duration = Duration::from_secs(110);

const MAX_STDIN: u64 = 16 * 1024 * 1024;
const MAX_FIELD_LEN: usize = 2_000;
const MAX_DIFF_FIELD_LEN: usize = 256 * 1024;
const MAX_DIFF_TOTAL: usize = 512 * 1024;
const DROPPED_FIELDS: &[&str] = &["tool_response", "tool_output", "transcript_path"];
const DIFF_TOOLS: &[&str] = &["Edit", "MultiEdit", "Write"];
const DIFF_KEYS: &[&str] = &["old_string", "new_string", "content"];
const INPUT_ALIASES: &[(&str, &str)] = &[
    ("CommandLine", "command"),
    ("FilePath", "file_path"),
    ("Path", "path"),
    ("Url", "url"),
    ("Query", "query"),
    ("Pattern", "pattern"),
];

/// Agents whose `PermissionRequest` can be answered from the Island ("" = Claude Code).
pub fn takes_decisions(agent: &str) -> bool {
    matches!(agent, "" | "codex" | "copilot" | "muse")
}

/// The hook process environment (a struct so tests don't touch the real one).
#[derive(Default)]
pub struct HookEnv {
    pub entrypoint: Option<String>,
    pub cursor_trace_id: Option<String>,
    pub askpass_node: Option<String>,
    pub gemini_session_id: Option<String>,
    pub cwd: Option<String>,
}

impl HookEnv {
    fn from_process() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        HookEnv {
            entrypoint: var("CLAUDE_CODE_ENTRYPOINT"),
            cursor_trace_id: var("CURSOR_TRACE_ID"),
            askpass_node: var("VSCODE_GIT_ASKPASS_NODE"),
            gemini_session_id: var("GEMINI_SESSION_ID"),
            cwd: std::env::current_dir().ok().map(|d| d.to_string_lossy().into_owned()),
        }
    }
}

pub struct Hook {
    pub payload: Value,
    /// `soul_agent` ("" = plain Claude Code).
    pub agent: String,
    pub event: String,
    /// Claude Code's `AskUserQuestion` input, whole (the payload copy may be truncated).
    pub question: Option<Value>,
}

/// `[--agent <name>] [<Event>]` (the args after `--agent-hook`).
pub fn parse_args(args: &[String]) -> (Option<String>, Option<String>) {
    let (mut agent, mut event) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--agent" {
            agent = it.next().cloned();
        } else if !a.starts_with("--") && event.is_none() {
            event = Some(a.clone());
        }
    }
    (agent, event)
}

/// Gemini CLI / Antigravity / Copilot CLI / Cursor event names → Claude Code's.
pub fn normalize_event(raw: &str) -> String {
    match raw {
        "BeforeTool" | "BeforeToolSelection" => "PreToolUse",
        "AfterTool" | "AfterModel" => "PostToolUse",
        "BeforeAgent" => "UserPromptSubmit",
        "AfterAgent" => "Stop",
        "startup" => "SessionStart",
        "exit" => "SessionEnd",
        "PreInvocation" => "UserPromptSubmit",
        "PostInvocation" => "PostToolUse",
        "sessionStart" | "session_start" => "SessionStart",
        "userPromptSubmitted" | "beforeSubmitPrompt" | "user_prompt_submit" => "UserPromptSubmit",
        "preToolUse" | "pre_tool_use" => "PreToolUse",
        "postToolUse" | "post_tool_use" => "PostToolUse",
        "postToolUseFailure" => "PostToolUseFailure",
        "permissionRequest" => "PermissionRequest",
        "agentStop" | "stop" => "Stop",
        "sessionEnd" | "session_end" => "SessionEnd",
        "notification" => "Notification",
        "subagentStart" => "SubagentStart",
        "subagentStop" => "SubagentStop",
        other => other,
    }
    .to_string()
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_str().filter(|s| !s.is_empty())
}

fn first_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_array()?.first()?.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn cut(s: &mut String, max: usize) -> bool {
    if s.len() <= max {
        return false;
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    s.truncate(i);
    s.push('…');
    true
}

fn truncate_all(v: &mut Value) {
    match v {
        Value::String(s) => {
            cut(s, MAX_FIELD_LEN);
        }
        Value::Array(a) => a.iter_mut().for_each(truncate_all),
        Value::Object(o) => o.values_mut().for_each(truncate_all),
        _ => {}
    }
}

/// Edit / Write inputs keep their diff strings (≤ 256 KB each, 512 KB together).
fn truncate_diff(v: &mut Value, budget: &mut usize, was_cut: &mut bool) {
    match v {
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                match x {
                    Value::String(s) if DIFF_KEYS.contains(&k.as_str()) => {
                        *was_cut |= cut(s, MAX_DIFF_FIELD_LEN.min(*budget));
                        *budget = budget.saturating_sub(s.len());
                    }
                    _ => truncate_diff(x, budget, was_cut),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| truncate_diff(x, budget, was_cut)),
        other => truncate_all(other),
    }
}

/// One agent's stdin JSON → Soul's payload (Claude Code's field and event names).
pub fn normalize(raw: Value, agent_arg: Option<&str>, event_arg: Option<&str>, env: &HookEnv) -> Hook {
    let mut map = match raw {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let root = Value::Object(map.clone());
    let raw_event = event_arg
        .map(str::to_string)
        .or_else(|| str_at(&root, &["hook_event_name"]).or(str_at(&root, &["hookEventName"])).map(str::to_string))
        .unwrap_or_default();
    let mut event = normalize_event(&raw_event);
    if event == "Stop" && str_at(&root, &["status"]) == Some("error") {
        event = "StopFailure".into();
    }
    map.insert("hook_event_name".into(), json!(event));

    let agent = agent_arg
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .or_else(|| (env.entrypoint.as_deref() == Some("claude-desktop")).then(|| "claude-desktop".to_string()));
    if let Some(a) = &agent {
        map.insert("soul_agent".into(), json!(a));
    }
    let cursor = env.cursor_trace_id.is_some()
        || env.askpass_node.as_deref().is_some_and(|p| {
            let file = p.rsplit(['/', '\\']).next().unwrap_or(p);
            file.to_ascii_lowercase().starts_with("cursor")
        });
    if cursor {
        map.insert("term_editor".into(), json!("cursor"));
    }

    if str_at(&root, &["tool_name"]).is_none() {
        if let Some(name) = str_at(&root, &["toolName"]).or(str_at(&root, &["toolCall", "name"])).or(str_at(&root, &["tool"])) {
            map.insert("tool_name".into(), json!(name));
        }
    }
    if !root.get("tool_input").is_some_and(Value::is_object) {
        let args = root.get("toolArgs").or_else(|| root.get("toolCall").and_then(|c| c.get("args"))).cloned();
        let args = match args {
            Some(Value::String(s)) => serde_json::from_str(&s).ok(),
            other => other,
        };
        if let Some(obj @ Value::Object(_)) = args {
            map.insert("tool_input".into(), obj);
        }
    }
    if let Some(Value::Object(input)) = map.get_mut("tool_input") {
        for (from, to) in INPUT_ALIASES {
            if !input.contains_key(*to) {
                if let Some(v) = input.get(*from).cloned() {
                    input.insert((*to).into(), v);
                }
            }
        }
    }
    if str_at(&root, &["session_id"]).is_none() {
        let id = ["conversationId", "conversation_id", "sessionId"]
            .iter()
            .find_map(|k| str_at(&root, &[k]).map(str::to_string))
            .or_else(|| env.gemini_session_id.clone());
        if let Some(id) = id {
            map.insert("session_id".into(), json!(id));
        }
    }
    if str_at(&root, &["cwd"]).is_none() {
        let cwd = str_at(&root, &["workdir"])
            .map(str::to_string)
            .or_else(|| first_str(&root, "workspacePaths"))
            .or_else(|| first_str(&root, "workspace_roots"))
            .or_else(|| env.cwd.clone());
        if let Some(cwd) = cwd {
            map.insert("cwd".into(), json!(cwd));
        }
    }
    for f in DROPPED_FIELDS {
        map.remove(*f);
    }

    let tool = map.get("tool_name").and_then(Value::as_str).unwrap_or_default().to_string();
    let question = (tool == "AskUserQuestion").then(|| map.get("tool_input").cloned()).flatten();
    let diff = event == "PostToolUse" && DIFF_TOOLS.contains(&tool.as_str());
    let (mut budget, mut was_cut) = (MAX_DIFF_TOTAL, false);
    for (k, v) in map.iter_mut() {
        if diff && k == "tool_input" {
            truncate_diff(v, &mut budget, &mut was_cut);
        } else {
            truncate_all(v);
        }
    }
    if was_cut {
        map.insert("soul_diff_truncated".into(), json!(true));
    }
    Hook { payload: Value::Object(map), agent: agent.unwrap_or_default(), event, question }
}

/// Every question answered with valid option label(s), nothing extra.
pub fn answers_fit(question: &Value, answers: &Value) -> bool {
    let (Some(qs), Some(ans)) = (question.get("questions").and_then(Value::as_array), answers.as_object()) else {
        return false;
    };
    !qs.is_empty()
        && ans.len() == qs.len()
        && qs.iter().all(|q| {
            let labels: Vec<&str> = q
                .get("options")
                .and_then(Value::as_array)
                .map(|o| o.iter().filter_map(|o| o.get("label")?.as_str()).collect())
                .unwrap_or_default();
            let multi = q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false);
            let Some(a) = q.get("question").and_then(Value::as_str).and_then(|t| ans.get(t)).and_then(Value::as_str) else {
                return false;
            };
            labels.contains(&a) || (multi && a.split(", ").all(|p| labels.contains(&p)))
        })
}

fn claude_decision(decision: Value) -> String {
    json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } }).to_string()
}

/// What the hook prints for `agent` / `event` given the Island's decision (`None` = no answer).
pub fn reply_stdout(agent: &str, event: &str, decision: Option<&str>, question: Option<&Value>) -> Option<String> {
    if event != "PermissionRequest" {
        if agent.eq_ignore_ascii_case("antigravity") && event == "PreToolUse" {
            return Some(r#"{"decision":"ask"}"#.into());
        }
        return matches!(agent, "gemini" | "antigravity" | "copilot" | "muse").then(|| "{}".into());
    }
    match agent {
        "copilot" | "muse" => match decision {
            Some("allow" | "always") => Some(r#"{"permissionDecision":"allow"}"#.into()),
            Some("deny") => Some(r#"{"permissionDecision":"deny"}"#.into()),
            // Copilot fails closed without an answer: hand it back to its own prompt.
            _ if agent == "copilot" => Some(r#"{"permissionDecision":"ask"}"#.into()),
            _ => None,
        },
        "" | "codex" => {
            let d = decision?;
            if d.starts_with('{') {
                let answers = serde_json::from_str::<Value>(d).ok()?.get("answers")?.clone();
                let q = question?;
                if !answers_fit(q, &answers) {
                    return None;
                }
                let mut input = q.clone();
                input.as_object_mut()?.insert("answers".into(), answers);
                return Some(claude_decision(json!({ "behavior": "allow", "updatedInput": input })));
            }
            match d {
                "allow" | "always" => Some(claude_decision(json!({ "behavior": "allow" }))),
                "deny" => Some(claude_decision(json!({ "behavior": "deny", "message": "Denied from Soul" }))),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `soul --agent-hook …` entry point (before Tauri; the caller exits with 0).
pub fn run_cli() {
    let args: Vec<String> = std::env::args().skip_while(|a| a != "--agent-hook").skip(1).collect();
    let (agent, event) = parse_args(&args);
    let mut raw = Vec::new();
    let _ = std::io::stdin().take(MAX_STDIN).read_to_end(&mut raw);
    let value = serde_json::from_slice(strip_bom(&raw)).unwrap_or(Value::Null);
    let Hook { payload, agent, event, question } = normalize(value, agent.as_deref(), event.as_deref(), &HookEnv::from_process());
    let wait = event == "PermissionRequest" && takes_decisions(&agent);
    let budget = if wait { DECISION_BUDGET } else { FIRE_AND_FORGET_BUDGET };
    let (tx, rx) = mpsc::channel();
    // The budget bounds everything (connect, write, the wait); a stuck send is cut off by exit.
    std::thread::spawn(move || {
        let _ = tx.send(super::pipe::send(&payload, wait));
    });
    let decision = rx.recv_timeout(budget).ok().flatten();
    if let Some(out) = reply_stdout(&agent, &event, decision.as_deref(), question.as_ref()) {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(raw: Value, agent: Option<&str>, event: Option<&str>) -> Hook {
        normalize(raw, agent, event, &HookEnv { cwd: Some("/home/u/proj".into()), ..Default::default() })
    }

    #[test]
    fn events_normalize_across_agents() {
        for (raw, want) in [
            ("BeforeTool", "PreToolUse"),
            ("AfterModel", "PostToolUse"),
            ("BeforeAgent", "UserPromptSubmit"),
            ("AfterAgent", "Stop"),
            ("startup", "SessionStart"),
            ("exit", "SessionEnd"),
            ("PreInvocation", "UserPromptSubmit"),
            ("PostInvocation", "PostToolUse"),
            ("userPromptSubmitted", "UserPromptSubmit"),
            ("beforeSubmitPrompt", "UserPromptSubmit"),
            ("pre_tool_use", "PreToolUse"),
            ("permissionRequest", "PermissionRequest"),
            ("agentStop", "Stop"),
            ("session_end", "SessionEnd"),
            ("PreToolUse", "PreToolUse"),
        ] {
            assert_eq!(normalize_event(raw), want, "{raw}");
        }
        let h = norm(json!({ "status": "error" }), Some("copilot"), Some("agentStop"));
        assert_eq!(h.event, "StopFailure");
        assert_eq!(parse_args(&["--agent".into(), "codex".into()]), (Some("codex".into()), None));
        assert_eq!(parse_args(&["Stop".into()]), (None, Some("Stop".into())));
    }

    #[test]
    fn fields_normalize() {
        // Copilot CLI: camelCase, toolArgs as a JSON string, aliases.
        let h = norm(
            json!({ "toolName": "shell", "toolArgs": "{\"CommandLine\":\"cargo test\"}", "sessionId": "s1", "workdir": "C:/w", "transcript_path": "/x" }),
            Some("copilot"),
            Some("preToolUse"),
        );
        assert_eq!(h.payload["tool_name"], "shell");
        assert_eq!(h.payload["tool_input"]["command"], "cargo test");
        assert_eq!(h.payload["session_id"], "s1");
        assert_eq!(h.payload["cwd"], "C:/w");
        assert_eq!(h.payload["soul_agent"], "copilot");
        assert!(h.payload.get("transcript_path").is_none());
        // Cursor: workspace_roots + conversation_id; Antigravity: toolCall.
        let h = norm(json!({ "hook_event_name": "beforeSubmitPrompt", "conversation_id": "c", "workspace_roots": ["/r"] }), Some("cursor"), None);
        assert_eq!((h.event.as_str(), h.payload["session_id"].as_str(), h.payload["cwd"].as_str()), ("UserPromptSubmit", Some("c"), Some("/r")));
        let h = norm(json!({ "toolCall": { "name": "run_command", "args": { "CommandLine": "ls" } } }), Some("antigravity"), Some("PostInvocation"));
        assert_eq!((h.payload["tool_name"].as_str(), h.payload["tool_input"]["command"].as_str()), (Some("run_command"), Some("ls")));
        // Plain Claude Code: no agent tag, cwd from the process; Claude Desktop tagged.
        let h = norm(json!({ "hook_event_name": "Stop", "session_id": "x" }), None, None);
        assert!(h.payload.get("soul_agent").is_none());
        assert_eq!(h.payload["cwd"], "/home/u/proj");
        let env = HookEnv { entrypoint: Some("claude-desktop".into()), askpass_node: Some(r"C:\Cursor\cursor.exe".into()), ..Default::default() };
        let h = normalize(json!({}), None, Some("Stop"), &env);
        assert_eq!((h.agent.as_str(), h.payload["term_editor"].as_str()), ("claude-desktop", Some("cursor")));
        // Gemini session from the environment.
        let env = HookEnv { gemini_session_id: Some("g1".into()), ..Default::default() };
        assert_eq!(normalize(json!({}), Some("gemini"), Some("AfterTool"), &env).payload["session_id"], "g1");
    }

    #[test]
    fn truncation_keeps_diffs_only_after_edits() {
        let big = "ł".repeat(5_000); // 10 KB, 2-byte chars
        let h = norm(json!({ "tool_name": "Edit", "tool_input": { "old_string": big, "new_string": "x", "file_path": big } }), None, Some("PostToolUse"));
        assert_eq!(h.payload["tool_input"]["old_string"].as_str().unwrap().len(), 10_000);
        let fp = h.payload["tool_input"]["file_path"].as_str().unwrap();
        assert!(fp.len() <= MAX_FIELD_LEN + 3 && fp.ends_with('…'));
        assert!(h.payload.get("soul_diff_truncated").is_none());
        // Before the edit (PreToolUse) everything is cut to 2 000 bytes.
        let h = norm(json!({ "tool_name": "Edit", "tool_input": { "old_string": big } }), None, Some("PreToolUse"));
        assert!(h.payload["tool_input"]["old_string"].as_str().unwrap().len() <= MAX_FIELD_LEN + 3);
        // Over 256 KB: cut and flagged.
        let huge = "a".repeat(300 * 1024);
        let h = norm(json!({ "tool_name": "Write", "tool_input": { "content": huge } }), None, Some("PostToolUse"));
        assert_eq!(h.payload["tool_input"]["content"].as_str().unwrap().len(), MAX_DIFF_FIELD_LEN + 3);
        assert_eq!(h.payload["soul_diff_truncated"], true);
    }

    fn question() -> Value {
        json!({ "questions": [
            { "question": "Which DB?", "options": [{ "label": "SQLite" }, { "label": "Postgres" }] },
            { "question": "Extras?", "multiSelect": true, "options": [{ "label": "Tests" }, { "label": "Docs" }] }
        ] })
    }

    #[test]
    fn question_kept_whole_and_answers_validated() {
        let long = "q".repeat(3_000);
        let h = norm(json!({ "tool_name": "AskUserQuestion", "tool_input": { "questions": [{ "question": long }] } }), None, Some("PermissionRequest"));
        assert_eq!(h.question.unwrap()["questions"][0]["question"].as_str().unwrap().len(), 3_000);
        let q = question();
        assert!(answers_fit(&q, &json!({ "Which DB?": "SQLite", "Extras?": "Tests, Docs" })));
        assert!(!answers_fit(&q, &json!({ "Which DB?": "SQLite" })));
        assert!(!answers_fit(&q, &json!({ "Which DB?": "MySQL", "Extras?": "Tests" })));
        assert!(!answers_fit(&q, &json!({ "Which DB?": "SQLite, Postgres", "Extras?": "Tests" })));
        let ok = reply_stdout("", "PermissionRequest", Some(r#"{"answers":{"Which DB?":"Postgres","Extras?":"Docs"}}"#), Some(&q)).unwrap();
        let v: Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert_eq!(v["hookSpecificOutput"]["decision"]["updatedInput"]["answers"]["Which DB?"], "Postgres");
        assert_eq!(v["hookSpecificOutput"]["decision"]["updatedInput"]["questions"], q["questions"]);
        assert_eq!(reply_stdout("", "PermissionRequest", Some(r#"{"answers":{"Which DB?":"x"}}"#), Some(&q)), None);
        assert_eq!(reply_stdout("", "PermissionRequest", Some(r#"{"answers":{}}"#), None), None);
    }

    #[test]
    fn never_allows_without_a_decision() {
        for agent in ["", "codex"] {
            assert_eq!(reply_stdout(agent, "PermissionRequest", None, None), None);
            assert_eq!(reply_stdout(agent, "PermissionRequest", Some("maybe"), None), None);
            assert!(reply_stdout(agent, "PermissionRequest", Some("allow"), None).unwrap().contains(r#""behavior":"allow""#));
            assert!(reply_stdout(agent, "PermissionRequest", Some("always"), None).unwrap().contains(r#""behavior":"allow""#));
            assert!(reply_stdout(agent, "PermissionRequest", Some("deny"), None).unwrap().contains("Denied from Soul"));
        }
        assert_eq!(reply_stdout("copilot", "PermissionRequest", None, None).as_deref(), Some(r#"{"permissionDecision":"ask"}"#));
        assert_eq!(reply_stdout("copilot", "PermissionRequest", Some("allow"), None).as_deref(), Some(r#"{"permissionDecision":"allow"}"#));
        assert_eq!(reply_stdout("muse", "PermissionRequest", Some("deny"), None).as_deref(), Some(r#"{"permissionDecision":"deny"}"#));
        assert_eq!(reply_stdout("muse", "PermissionRequest", None, None), None);
        assert_eq!(reply_stdout("gemini", "PermissionRequest", Some("allow"), None), None);
        // Fire-and-forget events: the safe „no opinion” output.
        assert_eq!(reply_stdout("", "PreToolUse", None, None), None);
        assert_eq!(reply_stdout("codex", "Stop", None, None), None);
        assert_eq!(reply_stdout("gemini", "PreToolUse", None, None).as_deref(), Some("{}"));
        assert_eq!(reply_stdout("Antigravity", "PreToolUse", None, None).as_deref(), Some(r#"{"decision":"ask"}"#));
        assert_eq!(reply_stdout("antigravity", "Stop", None, None).as_deref(), Some("{}"));
        assert!(takes_decisions("") && takes_decisions("codex") && !takes_decisions("cursor") && !takes_decisions("claude-desktop"));
    }
}
