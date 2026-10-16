//! The app side of the computer agent: Island progress, confirmation card, spoken questions,
//! the highlight overlay, capture exclusion and the Gemini client (with fallback + healing).

use super::config::{self, AgentConfig};
use super::{Host, ModelFn, Phase, Progress};
use crate::ai::gemini::GeminiClient;
use crate::config::load_settings;
use crate::i18n::Lang;
use crate::platform::ui::Rect;
use crate::platform::Platform;
use crate::AppState;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

/// Soul's windows that must never end up in the model's screenshot.
const OWN_WINDOWS: &[&str] = &[crate::island::ISLAND_LABEL, "overlay", super::HIGHLIGHT_LABEL];

pub struct AppHost {
    app: AppHandle,
    settings: crate::config::Settings,
    cfg: AgentConfig,
}

impl AppHost {
    pub fn new(app: &AppHandle) -> Self {
        Self {
            app: app.clone(),
            settings: load_settings(app).unwrap_or_default(),
            cfg: config::load(app),
        }
    }
}

impl Host for AppHost {
    fn platform(&self) -> Arc<dyn Platform> {
        Arc::clone(&self.app.state::<AppState>().platform)
    }

    fn config(&self) -> AgentConfig {
        self.cfg.clone()
    }

    fn language(&self) -> String {
        self.settings.language.clone()
    }

    fn ui_lang(&self) -> Lang {
        crate::i18n::ui_lang(&self.settings)
    }

    fn context(&self) -> Option<String> {
        crate::agent::context::active_block(&self.settings)
    }

    fn model(&self) -> ModelFn {
        let model = if self.cfg.model.is_empty() {
            self.settings.effective_ask_model().to_string()
        } else {
            self.cfg.model.clone()
        };
        let fallback = crate::resolve_fallback_model(&self.settings, &crate::ai::gemini::cached_models());
        let key = self.settings.api_key.clone();
        let app = self.app.clone();
        Arc::new(move |body| {
            let app = app.clone();
            GeminiClient::new(&key, &model).agent_generate_resilient(body, Some(&fallback), |_, _| {}, move |retired| {
                crate::retire_model_and_heal(&app, retired);
            })
        })
    }

    fn progress(&self, p: &Progress) {
        let _ = self.app.emit("island://agent", p);
        if p.phase == Phase::Running {
            let _ = crate::emit_overlay(&self.app, "thinking", Some(format!("{} · {}/{}", p.text, p.step, p.max)));
        }
    }

    fn confirm(&self, question: &str) -> bool {
        crate::agent::confirm::request(&self.app, question).unwrap_or(false)
    }

    fn ask(&self, question: &str) -> Option<String> {
        crate::agent::confirm::ask(&self.app, question)
    }

    fn highlight(&self, rect: Rect) {
        super::show_highlight(&self.app, rect);
    }

    fn pointer_guard(&self, at: Rect) -> Option<Box<dyn FnOnce()>> {
        let win = self.app.get_webview_window(crate::island::ISLAND_LABEL)?;
        let pos = win.outer_position().ok()?;
        let size = win.outer_size().ok()?;
        let island = Rect::new(pos.x, pos.y, size.width as i32, size.height as i32);
        island.intersect(&at)?;
        let _ = win.set_ignore_cursor_events(true);
        Some(Box::new(move || {
            let _ = win.set_ignore_cursor_events(false);
        }))
    }

    fn capture_exclusion(&self, on: bool) {
        let platform = self.platform();
        for label in OWN_WINDOWS {
            if let Some(id) = self.app.get_webview_window(label).as_ref().and_then(crate::platform::native_window_id) {
                if let Err(e) = platform.exclude_from_capture(id, on) {
                    crate::soul_debug!("[soul][agent] capture exclusion {label}: {e}");
                }
            }
        }
    }

    fn debug_dir(&self) -> Option<std::path::PathBuf> {
        self.app.path().app_data_dir().ok().map(|d| d.join("agent-debug"))
    }

    fn vio(&self) -> Option<Arc<super::vio::Client>> {
        super::vio::global()
    }
}
