//! Scenes (macros): user-defined ordered steps ("granie" → Discord, FiveM, Spotify playlist,
//! volume 40%, Do Not Disturb). Stored in the memory DB; run step by step on the caller's worker
//! thread with Island progress chips. Only registered actions — never a shell.
//!
//! Also suggests a scene when the same 2+ apps are opened together 3+ times.

use crate::memory::{self, with_db};
use crate::platform::Platform;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tauri::AppHandle;

/// Launches closer than this belong to one "session" (opened together).
const TOGETHER_MS: i64 = 3 * 60 * 1000;
/// Same set of apps opened together this many times → suggest a scene.
const SUGGEST_AFTER: usize = 3;
const MAX_DELAY_MS: u64 = 60_000;
const STEP_KINDS: &[&str] = &[
    "open_app", "open_url", "set_volume", "media", "youtube_play", "web_search", "delay", "dnd",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SceneStep {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub steps: Vec<SceneStep>,
    #[serde(default)]
    pub updated_at: i64,
}

pub fn list() -> Result<Vec<Scene>, String> {
    with_db(|c| {
        let mut st = c.prepare("SELECT id, name, triggers, steps, updated_at FROM scenes ORDER BY name")?;
        let rows = st.query_map([], |r| {
            let triggers: String = r.get(2)?;
            let steps: String = r.get(3)?;
            Ok(Scene {
                id: r.get(0)?,
                name: r.get(1)?,
                triggers: serde_json::from_str(&triggers).unwrap_or_default(),
                steps: serde_json::from_str(&steps).unwrap_or_default(),
                updated_at: r.get(4)?,
            })
        })?;
        rows.collect()
    })
}

/// Validate and insert / replace (by id, else by name).
pub fn save(scene: Scene) -> Result<Scene, String> {
    let scene = validate(scene)?;
    let triggers = serde_json::to_string(&scene.triggers).unwrap_or_else(|_| "[]".into());
    let steps = serde_json::to_string(&scene.steps).unwrap_or_else(|_| "[]".into());
    let now = memory::now_ms();
    let id = with_db(|c| {
        if scene.id > 0 {
            c.execute(
                "UPDATE scenes SET name=?1, triggers=?2, steps=?3, updated_at=?4 WHERE id=?5",
                params![scene.name, triggers, steps, now, scene.id],
            )?;
            return Ok(scene.id);
        }
        c.execute(
            "INSERT INTO scenes (name, triggers, steps, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET triggers=excluded.triggers, steps=excluded.steps, updated_at=excluded.updated_at",
            params![scene.name, triggers, steps, now],
        )?;
        c.query_row("SELECT id FROM scenes WHERE name=?1", [&scene.name], |r| r.get(0))
    })?;
    eprintln!(
        "[soul][scene] saved id={id} name={:?} steps={} triggers={}",
        scene.name,
        scene.steps.len(),
        scene.triggers.len()
    );
    Ok(Scene {
        id,
        updated_at: now,
        ..scene
    })
}

pub fn delete(id: i64) -> Result<(), String> {
    with_db(|c| c.execute("DELETE FROM scenes WHERE id=?1", [id]).map(|_| ()))?;
    eprintln!("[soul][scene] deleted id={id}");
    Ok(())
}

fn validate(mut scene: Scene) -> Result<Scene, String> {
    scene.name = scene.name.trim().to_string();
    if scene.name.is_empty() {
        return Err("Scena musi mieć nazwę.".into());
    }
    scene.triggers = scene
        .triggers
        .iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    scene.steps = scene
        .steps
        .into_iter()
        .map(|s| SceneStep {
            kind: s.kind.trim().to_ascii_lowercase(),
            value: s.value.trim().to_string(),
        })
        .filter(|s| s.kind == "dnd" || s.kind == "media" || !s.value.is_empty())
        .collect();
    if scene.steps.is_empty() {
        return Err("Scena potrzebuje co najmniej jednego kroku.".into());
    }
    if let Some(bad) = scene.steps.iter().find(|s| !STEP_KINDS.contains(&s.kind.as_str())) {
        return Err(format!("Nieznany typ kroku „{}”.", bad.kind));
    }
    Ok(scene)
}

/// Build a scene from the `create_scene` tool / router args.
pub fn from_args(args: &Value) -> Result<Scene, String> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let steps = args
        .get("steps")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let kind = s.get("type").and_then(Value::as_str)?.to_string();
                    let value = match s.get("value") {
                        Some(Value::String(v)) => v.clone(),
                        Some(Value::Number(n)) => n.to_string(),
                        _ => String::new(),
                    };
                    Some(SceneStep { kind, value })
                })
                .collect()
        })
        .unwrap_or_default();
    let triggers = args
        .get("triggers")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
        .unwrap_or_default();
    Ok(Scene {
        id: 0,
        name,
        triggers,
        steps,
        updated_at: 0,
    })
}

fn fold(s: &str) -> String {
    s.split_whitespace()
        .map(crate::agent::fold_word)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Scene by (fuzzy-folded) name or trigger phrase.
pub fn find(name: &str) -> Option<Scene> {
    let want = fold(name);
    let want = want
        .strip_prefix("scene ")
        .or_else(|| want.strip_prefix("scena "))
        .unwrap_or(&want)
        .to_string();
    let scenes = list().ok()?;
    scenes
        .iter()
        .find(|s| fold(&s.name) == want)
        .or_else(|| scenes.iter().find(|s| s.triggers.iter().any(|t| fold(t) == want)))
        .or_else(|| scenes.iter().find(|s| want.contains(&fold(&s.name)) && fold(&s.name).len() >= 4))
        .cloned()
}

/// The whole utterance is one of a scene's trigger phrases ("gramy w FiveM").
pub fn match_trigger(text: &str) -> Option<Scene> {
    let said = fold(text);
    if said.is_empty() {
        return None;
    }
    list().ok()?.into_iter().find(|s| {
        s.triggers.iter().any(|t| {
            let t = fold(t);
            !t.is_empty() && (said == t || (t.split(' ').count() >= 2 && said.contains(&t)))
        })
    })
}

fn step_label(s: &SceneStep) -> String {
    match s.kind.as_str() {
        "open_app" | "open_url" | "youtube_play" | "web_search" => s.value.clone(),
        "set_volume" => format!("{}%", s.value.trim_end_matches('%')),
        "media" => format!("♪ {}", s.value),
        "delay" => format!("{} s", s.value.parse::<u64>().unwrap_or(0) / 1000),
        "dnd" => "Nie przeszkadzać".into(),
        other => other.into(),
    }
}

fn run_step(platform: &Arc<dyn Platform>, s: &SceneStep) -> Result<String, String> {
    match s.kind.as_str() {
        "open_app" => platform.open_app(&s.value),
        "open_url" => platform.open_url(&s.value),
        "youtube_play" => platform.youtube_play(&s.value),
        "web_search" => platform.web_search(&s.value),
        "set_volume" => {
            let p: u8 = s
                .value
                .trim()
                .trim_end_matches('%')
                .parse()
                .map_err(|_| format!("Zła głośność „{}”", s.value))?;
            platform.set_volume(p.min(100))
        }
        "media" => platform.media_control(if s.value.is_empty() { "play" } else { &s.value }),
        "delay" => {
            let ms = s.value.trim().parse::<u64>().unwrap_or(1000).min(MAX_DELAY_MS);
            thread::sleep(Duration::from_millis(ms));
            Ok(format!("Odczekano {} ms", ms))
        }
        "dnd" => platform.set_do_not_disturb(!matches!(s.value.as_str(), "off" | "wyl" | "false" | "0")),
        other => Err(format!("Nieznany krok „{other}”")),
    }
}

/// Run a scene step by step (blocking — call from a worker thread). Island shows one chip per step.
pub fn run(app: &AppHandle, platform: &Arc<dyn Platform>, scene: &Scene) -> Result<String, String> {
    let total = scene.steps.len();
    eprintln!("[soul][scene] run name={:?} steps={total}", scene.name);
    let mut failed = Vec::new();
    for (i, step) in scene.steps.iter().enumerate() {
        let label = step_label(step);
        let _ = crate::emit_overlay(
            app,
            "thinking",
            Some(format!("Scena „{}” · {}/{}", scene.name, i + 1, total)),
        );
        crate::island::emit_tool_labeled(app, &step.kind, Some(&label), "start");
        let result = run_step(platform, step);
        match &result {
            Ok(r) => eprintln!("[soul][scene] step {}/{total} {} ok: {r}", i + 1, step.kind),
            Err(e) => {
                eprintln!("[soul][scene] step {}/{total} {} error: {e}", i + 1, step.kind);
                failed.push(format!("{label}: {e}"));
            }
        }
        crate::island::emit_tool_labeled(
            app,
            &step.kind,
            Some(&label),
            if result.is_ok() { "ok" } else { "error" },
        );
    }
    if failed.len() == total {
        return Err(format!("Scena „{}” nie powiodła się: {}", scene.name, failed.join("; ")));
    }
    if failed.is_empty() {
        Ok(format!("Scena „{}” gotowa ({total} kroków).", scene.name))
    } else {
        Ok(format!(
            "Scena „{}”: {}/{total} kroków, błędy: {}",
            scene.name,
            total - failed.len(),
            failed.join("; ")
        ))
    }
}

pub fn run_by_name(app: &AppHandle, platform: &Arc<dyn Platform>, name: &str) -> Result<String, String> {
    match find(name) {
        Some(scene) => run(app, platform, &scene),
        None => {
            let known: Vec<String> = list().unwrap_or_default().into_iter().map(|s| s.name).collect();
            Err(if known.is_empty() {
                format!("Nie ma sceny „{name}”. Utwórz ją głosem albo w Ustawieniach → Sceny.")
            } else {
                format!("Nie ma sceny „{name}”. Znane sceny: {}.", known.join(", "))
            })
        }
    }
}

// ───────────────────────── suggestions ─────────────────────────

/// Record an app the user opened by voice; suggest a scene once a set of apps keeps coming back.
pub fn note_app_opened(app: &AppHandle, name: &str) {
    memory::record_launch(name);
    let since = memory::now_ms() - 30 * 86_400_000;
    let Some(apps) = suggestion(&memory::launches_since(since)) else {
        return;
    };
    let key = format!(
        "scene-suggest:{}",
        apps.iter().map(|a| fold(a)).collect::<Vec<_>>().join("+")
    );
    let covered = list().unwrap_or_default().iter().any(|s| {
        let opened: BTreeSet<String> = s
            .steps
            .iter()
            .filter(|st| st.kind == "open_app")
            .map(|st| fold(&st.value))
            .collect();
        apps.iter().all(|a| opened.contains(&fold(a)))
    });
    if covered || memory::is_dismissed(&key) {
        return;
    }
    memory::dismiss(&key); // suggest each set only once
    eprintln!("[soul][scene] suggest apps={apps:?}");
    crate::island::toast(
        app,
        "scene",
        &format!("Często otwierasz razem: {}. Utworzyć scenę?", apps.join(", ")),
        Some(crate::island::ToastAction {
            label: "Utwórz scenę".into(),
            command: "scene_create_suggested".into(),
            args: serde_json::json!({ "apps": apps }),
        }),
    );
}

/// Apps opened together in the latest session, if the same set appeared in ≥3 sessions.
pub fn suggestion(launches: &[(String, i64)]) -> Option<Vec<String>> {
    let mut sessions: Vec<Vec<String>> = Vec::new();
    let mut last_at = i64::MIN;
    for (app, at) in launches {
        if sessions.is_empty() || at - last_at > TOGETHER_MS {
            sessions.push(Vec::new());
        }
        let cur = sessions.last_mut().unwrap();
        if !cur.iter().any(|a| fold(a) == fold(app)) {
            cur.push(app.clone());
        }
        last_at = *at;
    }
    let current = sessions.last()?;
    if current.len() < 2 {
        return None;
    }
    let want: BTreeSet<String> = current.iter().map(|a| fold(a)).collect();
    let together = sessions
        .iter()
        .filter(|s| {
            let set: BTreeSet<String> = s.iter().map(|a| fold(a)).collect();
            want.is_subset(&set)
        })
        .count();
    (together >= SUGGEST_AFTER).then(|| current.clone())
}

/// Scene from a suggestion: one open_app step per app, named after the apps.
pub fn create_suggested(apps: Vec<String>) -> Result<Scene, String> {
    let name = apps.join(" + ");
    save(Scene {
        id: 0,
        name,
        triggers: Vec::new(),
        steps: apps
            .into_iter()
            .map(|a| SceneStep {
                kind: "open_app".into(),
                value: a,
            })
            .collect(),
        updated_at: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(min: i64) -> i64 {
        1_700_000_000_000 + min * 60_000
    }

    #[test]
    fn suggests_after_three_sessions_together() {
        let mut l = Vec::new();
        for day in 0..2 {
            l.push(("Discord".to_string(), at(day * 1440)));
            l.push(("Spotify".to_string(), at(day * 1440 + 1)));
        }
        l.push(("Discord".to_string(), at(3000)));
        assert_eq!(suggestion(&l), None, "single app so far");
        l.push(("spotify".to_string(), at(3001)));
        assert_eq!(suggestion(&l), Some(vec!["Discord".to_string(), "spotify".to_string()]));
    }

    #[test]
    fn far_apart_launches_are_not_together() {
        let l = vec![
            ("Discord".to_string(), at(0)),
            ("Spotify".to_string(), at(10)),
            ("Discord".to_string(), at(100)),
            ("Spotify".to_string(), at(110)),
            ("Discord".to_string(), at(200)),
            ("Spotify".to_string(), at(210)),
        ];
        assert_eq!(suggestion(&l), None);
    }

    #[test]
    fn validation_rejects_unknown_steps_and_empty() {
        let bad = Scene {
            id: 0,
            name: "x".into(),
            triggers: vec![],
            steps: vec![SceneStep { kind: "shell".into(), value: "rm -rf".into() }],
            updated_at: 0,
        };
        assert!(validate(bad).is_err());
        let args = serde_json::json!({
            "name": " granie ",
            "steps": [{"type": "open_app", "value": "Discord"}, {"type": "set_volume", "value": 40}],
            "triggers": ["gramy w FiveM"]
        });
        let s = validate(from_args(&args).unwrap()).unwrap();
        assert_eq!(s.name, "granie");
        assert_eq!(s.steps[1].value, "40");
        assert_eq!(s.triggers, vec!["gramy w FiveM"]);
    }
}
