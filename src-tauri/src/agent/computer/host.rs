//! The app side of the computer agent: Island progress, confirmation card, spoken questions,
//! the highlight overlay, capture exclusion and the Gemini client (with fallback + healing).

use super::config::{self, AgentConfig};
use super::{Host, ModelFn, Phase, Progress};
use crate::ai::gemini::GeminiClient;
use crate::ai::pool;
use crate::config::load_settings;
use crate::i18n::Lang;
use crate::platform::ui::{clip, Rect};
use crate::platform::Platform;
use crate::AppState;
use std::sync::Arc;
use std::time::{Duration, Instant};
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

    /// The model the user chose (agent setting, else Circle-to-Ask): leads the pool.
    fn first_model(&self) -> String {
        if self.cfg.model.is_empty() { self.settings.effective_ask_model().to_string() } else { self.cfg.model.clone() }
    }

    /// Rotation pool for this moment (models list, daily quota and the RPM limits of the config).
    fn pool_for(cfg: &AgentConfig, first: &str, retired: &[String]) -> Vec<(String, u32)> {
        let available: Vec<String> =
            crate::ai::gemini::cached_models().into_iter().filter(|m| m.supports_generate).map(|m| m.id).collect();
        pool::models(&available, first, &cfg.model_rpm, retired)
    }
}

/// The account's models list, fetched at most once per 5 minutes while the cache is empty
/// (the pool needs it to skip models the key does not have; never a GET per step).
fn ensure_models(key: &str) {
    static LAST: parking_lot::Mutex<Option<Instant>> = parking_lot::Mutex::new(None);
    if !crate::ai::gemini::cached_models().is_empty() || key.trim().is_empty() {
        return;
    }
    {
        let mut last = LAST.lock();
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(300)) {
            return;
        }
        *last = Some(Instant::now());
    }
    let _ = GeminiClient::new(key, crate::config::DEFAULT_ASK_MODEL).list_audio_generate_models();
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
        let key = self.settings.api_key.clone();
        ensure_models(&key);
        let (cfg, first) = (self.cfg.clone(), self.first_model());
        let ledger = self.settings.retired_models.clone();
        let app = self.app.clone();
        // One in-flight call per step: an older call still looping over models stops sending.
        let call = pool::begin_call();
        Arc::new(move |body| {
            let mut skip: Vec<String> = Vec::new();
            let mut errors: Vec<String> = Vec::new();
            loop {
                if !pool::is_current(call) {
                    return Err("Model call superseded.".into());
                }
                let models = Self::pool_for(&cfg, &first, &ledger);
                let now = Instant::now();
                let mut pool = pool::global().lock();
                let (i, reason) = match pool.pick(&models, &skip, now) {
                    Some(i) => (i, if skip.is_empty() { "first under its RPM limit" } else { "next after a failure" }),
                    // Everything busy (the wait ran out): the model that frees up first.
                    None => match pool.earliest(&models, &skip, now) {
                        Some((i, _)) => (i, "all busy — earliest slot"),
                        None => break,
                    },
                };
                let model = models[i].0.clone();
                pool.record(&model, now);
                drop(pool);
                eprintln!("[soul][agent] model={model} ({reason}, limit {}/min)", models[i].1);
                match GeminiClient::new(&key, &model).agent_generate_once(body.clone(), super::MODEL_REQUEST_TIMEOUT) {
                    Ok(v) => return Ok(v),
                    Err((status, msg)) => {
                        let msg = crate::ai::gemini::redact_secrets(&msg);
                        eprintln!("[soul][agent] model {model} failed ({status:?}): {}", clip(&msg, 160));
                        match pool::classify(status, &msg) {
                            pool::Failure::Fatal => return Err(msg),
                            pool::Failure::Daily => crate::ai::quota::mark_daily_exhausted(&model),
                            pool::Failure::Retire => {
                                if pool::retire(&model) {
                                    crate::retire_model_quietly(&app, &model);
                                }
                            }
                            pool::Failure::Cool => {
                                eprintln!("[soul][agent] model {model} cooling for {} s", pool::COOLDOWN.as_secs());
                                pool::global().lock().cool(&model, Instant::now());
                            }
                        }
                        errors.push(format!("{model}: {}", clip(&msg, 100)));
                        skip.push(model);
                    }
                }
            }
            Err(format!("All models failed or are over their limit. {}", errors.join(" | ")))
        })
    }

    fn model_wait(&self) -> Option<Duration> {
        let models = Self::pool_for(&self.cfg, &self.first_model(), &self.settings.retired_models);
        let w = pool::global().lock().earliest(&models, &[], Instant::now())?.1;
        (!w.is_zero()).then_some(w)
    }

    fn progress(&self, p: &Progress) {
        let _ = self.app.emit("island://agent", p);
        if p.phase == Phase::Running {
            let _ = crate::emit_overlay(&self.app, "thinking", Some(format!("{} · {}/{}", p.text, p.step, p.max)));
        }
        // After the overlay state: while the task runs Vio shows „acting” (and its STOP button).
        let acting = matches!(p.phase, Phase::Running | Phase::Confirm | Phase::Asking);
        super::vio::soul_state(if acting { "acting" } else { "idle" }, if acting { &p.text } else { "" });
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
