//! Settings → Agent (store key `computerAgent`, autosaved by `agent_save_config`; never touched by
//! `save_app_settings`) and the in-memory debug log of recent steps.

use super::safety;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

pub const STORE_KEY: &str = "computerAgent";
pub const MAX_STEPS_LIMIT: u32 = 20;
const LOG_LIMIT: usize = 80;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentConfig {
    pub enabled: bool,
    /// 3–20.
    pub max_steps: u32,
    /// Ring around the target before clicking.
    pub highlight: bool,
    /// `always` | `risky`.
    pub confirm_level: String,
    /// Process / title / URL substrings the agent refuses to touch.
    pub blocklist: Vec<String>,
    /// REST model id; empty = the Circle-to-Ask model.
    pub model: String,
    /// Keep step screenshots in the app data dir (`agent-debug/`, cleared per task).
    pub debug_screenshots: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_steps: MAX_STEPS_LIMIT,
            highlight: true,
            confirm_level: "risky".into(),
            blocklist: safety::default_blocklist(),
            model: String::new(),
            debug_screenshots: false,
        }
    }
}

impl AgentConfig {
    pub fn normalized(mut self) -> Self {
        self.max_steps = self.max_steps.clamp(3, MAX_STEPS_LIMIT);
        self.confirm_level = match safety::ConfirmLevel::parse(&self.confirm_level) {
            safety::ConfirmLevel::Always => "always".into(),
            safety::ConfirmLevel::Risky => "risky".into(),
        };
        self.blocklist = self
            .blocklist
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| s.chars().count() >= 3)
            .take(200)
            .collect();
        self.model = crate::ai::gemini::normalize_model_id(self.model.trim());
        self
    }
}

pub fn load<R: Runtime>(app: &AppHandle<R>) -> AgentConfig {
    app.store(crate::config::STORE_PATH)
        .ok()
        .and_then(|s| s.get(STORE_KEY))
        .and_then(|v| serde_json::from_value::<AgentConfig>(v).ok())
        .unwrap_or_default()
        .normalized()
}

pub fn save<R: Runtime>(app: &AppHandle<R>, cfg: AgentConfig) -> Result<AgentConfig, String> {
    let cfg = cfg.normalized();
    let store = app.store(crate::config::STORE_PATH).map_err(|e| format!("Failed to open settings store: {e}"))?;
    store.set(STORE_KEY, serde_json::to_value(&cfg).map_err(|e| e.to_string())?);
    store.save().map_err(|e| format!("Failed to save settings: {e}"))?;
    Ok(cfg)
}

/// One executed (or refused) step, for Settings → Agent → debug log.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepLog {
    /// Unix ms.
    pub at: u64,
    pub task: String,
    pub step: u32,
    pub action: String,
    pub result: String,
    pub ok: bool,
    pub ms: u64,
    /// Model call (vs. local plan).
    pub model: bool,
    pub screenshot: bool,
}

static LOG: Mutex<VecDeque<StepLog>> = Mutex::new(VecDeque::new());

pub fn log_step(entry: StepLog) {
    let mut log = LOG.lock();
    log.push_front(entry);
    while log.len() > LOG_LIMIT {
        log.pop_back();
    }
}

pub fn recent_steps() -> Vec<StepLog> {
    LOG.lock().iter().cloned().collect()
}

pub fn clear_log() {
    LOG.lock().clear();
}

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_normalization() {
        let c = AgentConfig::default();
        assert!(c.enabled && c.highlight && c.max_steps == 20 && c.confirm_level == "risky");
        assert!(c.blocklist.iter().any(|b| b == "keepass"));
        let c = AgentConfig {
            max_steps: 99,
            confirm_level: "ALWAYS".into(),
            blocklist: vec![" mbank ".into(), "x".into()],
            model: " models/gemini-3.1-flash-lite ".into(),
            ..Default::default()
        }
        .normalized();
        assert_eq!(c.max_steps, 20);
        assert_eq!(c.confirm_level, "always");
        assert_eq!(c.blocklist, vec!["mbank".to_string()]);
        assert_eq!(c.model, "gemini-3.1-flash-lite");
        // Partial JSON from an older UI keeps defaults for the rest.
        let c: AgentConfig = serde_json::from_value(serde_json::json!({ "maxSteps": 5 })).unwrap();
        assert_eq!(c.max_steps, 5);
        assert!(c.enabled && !c.blocklist.is_empty());
    }
}
