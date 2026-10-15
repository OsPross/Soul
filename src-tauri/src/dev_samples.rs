//! Dev-only voice regression samples: Settings → Zaawansowane records a WAV from the selected
//! microphone into `tests/voice/` and registers it in `tests/voice/cases.json`.
//! The harness that replays them is `voice_regression.rs`. Release builds refuse these commands.

use crate::config::load_settings;
use crate::AppState;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

pub const CASES_FILE: &str = "cases.json";

/// `tests/voice/` in the source tree (dev builds only — the path is baked in at compile time).
pub fn voice_test_dir() -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    {
        Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("tests")
            .join("voice"))
    }
    #[cfg(not(debug_assertions))]
    {
        Err("Test samples can only be recorded in a dev build (npm run tauri dev).".into())
    }
}

/// "Następna piosenka" → "nastepna_piosenka.wav". ASCII letters/digits, `_` and `-` only.
pub fn sample_file_name(name: &str) -> Result<String, String> {
    let stem = name.trim();
    let stem = stem
        .strip_suffix(".wav")
        .or_else(|| stem.strip_suffix(".WAV"))
        .unwrap_or(stem);
    let mut out = String::new();
    for c in stem.chars() {
        if c.is_whitespace() {
            out.push('_');
        } else if c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push_str(&crate::agent::fold_word(&c.to_string()));
        }
    }
    let out = out.trim_matches(['_', '-']).to_string();
    if out.is_empty() || !out.is_ascii() {
        return Err("Use letters, digits, '_' or '-' in the sample name.".into());
    }
    Ok(format!("{out}.wav"))
}

pub fn read_cases(dir: &Path) -> Result<Vec<Value>, String> {
    let path = dir.join(CASES_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    match serde_json::from_str::<Value>(&raw).map_err(|e| format!("{}: {e}", path.display()))? {
        Value::Array(cases) => Ok(cases),
        _ => Err(format!("{} must be a JSON array", path.display())),
    }
}

pub fn write_cases(dir: &Path, cases: &[Value]) -> Result<(), String> {
    let path = dir.join(CASES_FILE);
    let mut raw = serde_json::to_string_pretty(cases).map_err(|e| e.to_string())?;
    raw.push('\n');
    std::fs::write(&path, raw).map_err(|e| format!("{}: {e}", path.display()))
}

#[tauri::command]
pub async fn voice_sample_start(app: AppHandle) -> Result<String, String> {
    voice_test_dir()?;
    let state = app.state::<AppState>();
    if *state.busy.lock() || state.recorder.is_recording() {
        return Err("Soul is busy — try again in a moment.".into());
    }
    let settings = load_settings(&app).unwrap_or_default();
    let preferred = Some(settings.microphone).filter(|m| !m.trim().is_empty());
    let started = state.recorder.start(preferred)?;
    eprintln!("[soul][audio] test sample recording device={}", started.device_name);
    Ok(started.device_name)
}

/// Stop recording and save `tests/voice/<name>.wav` (16 kHz mono). A new file is added to
/// `cases.json` with the expected `route` (if given); re-recording keeps existing expectations.
#[tauri::command]
pub async fn voice_sample_stop(
    app: AppHandle,
    name: String,
    route: Option<String>,
) -> Result<String, String> {
    let dir = voice_test_dir()?;
    // Validate before stopping, so a bad name doesn't throw the take away.
    let file = sample_file_name(&name)?;
    let wav = app.state::<AppState>().recorder.stop()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(&file);
    std::fs::write(&path, &wav).map_err(|e| format!("{}: {e}", path.display()))?;

    let mut cases = read_cases(&dir)?;
    if !cases.iter().any(|c| c["file"] == json!(file)) {
        let expect = match route.as_deref() {
            Some(r @ ("command" | "dictation")) => json!({ "route": r }),
            _ => json!({}),
        };
        cases.push(json!({ "file": file, "expect": expect }));
        write_cases(&dir, &cases)?;
    }
    eprintln!(
        "[soul][audio] test sample saved file={file} bytes={}",
        wav.len()
    );
    Ok(path.canonicalize().unwrap_or(path).display().to_string())
}

#[tauri::command]
pub async fn voice_sample_cancel(app: AppHandle) {
    let _ = app.state::<AppState>().recorder.stop();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_names_are_folded() {
        assert_eq!(sample_file_name("Następna piosenka").unwrap(), "nastepna_piosenka.wav");
        assert_eq!(sample_file_name("dyktowanie_faktura.wav").unwrap(), "dyktowanie_faktura.wav");
        assert_eq!(sample_file_name("  ścisz-do-20 ").unwrap(), "scisz-do-20.wav");
        assert!(sample_file_name("../..").is_err());
        assert!(sample_file_name("   ").is_err());
    }
}
