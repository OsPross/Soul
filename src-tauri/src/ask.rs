//! Circle-to-Ask session: capture → select → question → answer.

use crate::ai::gemini::GeminiClient;
use crate::audio::AudioRecorder;
use crate::capture::{self, CaptureResult, CropRect};
use crate::config::load_settings;
use crate::{resolve_fallback_model, retire_model_and_heal, AppState};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Position, Size, WebviewUrl,
    WebviewWindowBuilder,
};

const SILENCE_THRESHOLD: f32 = 0.035;
const SILENCE_MS: u64 = 1500;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AskSessionPayload {
    pub data_url: String,
    pub width: u32,
    pub height: u32,
    pub phase: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AskAnswerPayload {
    pub answer: String,
    pub history: Vec<ChatTurn>,
    pub selection: CropRect,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AskStatusPayload {
    pub state: String,
    pub message: Option<String>,
}

pub struct AskSession {
    pub full_png: Vec<u8>,
    pub cropped_png: Option<Vec<u8>>,
    pub selection: Option<CropRect>,
    pub monitor_x: i32,
    pub monitor_y: i32,
    pub width: u32,
    pub height: u32,
    pub data_url: String,
    pub history: Vec<ChatTurn>,
    pub selection_done: bool,
    pub stop_requested: Arc<AtomicBool>,
    pub silence_watcher: bool,
    pub submitting: Arc<AtomicBool>,
}

pub type AskState = Arc<Mutex<Option<AskSession>>>;

pub fn new_ask_state() -> AskState {
    Arc::new(Mutex::new(None))
}

pub fn is_active(state: &AskState) -> bool {
    state.lock().is_some()
}

/// Capture origin/size of the open session (physical px) — maps hook coordinates to the image.
pub struct SessionGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub selection_done: bool,
}

pub fn session_geometry(app: &AppHandle) -> Option<SessionGeometry> {
    let state = app.state::<AppState>();
    let slot = state.ask.lock();
    slot.as_ref().map(|s| SessionGeometry {
        x: s.monitor_x,
        y: s.monitor_y,
        width: s.width,
        height: s.height,
        selection_done: s.selection_done,
    })
}

/// Start Circle-to-Ask: capture monitor, begin mic, show fullscreen overlay.
pub fn start_ask(app: &AppHandle) -> Result<(), String> {
    let t0 = Instant::now();
    let state = app.state::<AppState>();
    if *state.busy.lock() {
        return Err("Soul is busy. Finish the current action first.".into());
    }
    if state.recorder.is_recording() {
        return Err("Already recording (dictation). Release the PTT hotkey first.".into());
    }
    if is_active(&state.ask) {
        // Second press: request stop / submit if selection done.
        return on_ask_hotkey_again(app);
    }

    *state.busy.lock() = true;

    let settings = load_settings(app).unwrap_or_default();
    let preferred = if settings.microphone.trim().is_empty() {
        None
    } else {
        Some(settings.microphone.clone())
    };

    let capture = match capture::capture_monitor_under_cursor() {
        Ok(c) => c,
        Err(e) => {
            *state.busy.lock() = false;
            return Err(e);
        }
    };

    if let Err(e) = state.recorder.start(preferred) {
        *state.busy.lock() = false;
        return Err(e);
    }
    eprintln!(
        "[soul][ask] stage=mic_start ms={}",
        t0.elapsed().as_millis()
    );

    let stop_requested = Arc::new(AtomicBool::new(false));
    {
        let mut slot = state.ask.lock();
        *slot = Some(AskSession {
            full_png: capture.png.clone(),
            cropped_png: None,
            selection: None,
            monitor_x: capture.monitor_x,
            monitor_y: capture.monitor_y,
            width: capture.width,
            height: capture.height,
            data_url: capture.data_url.clone(),
            history: Vec::new(),
            selection_done: false,
            stop_requested: Arc::clone(&stop_requested),
            silence_watcher: false,
            submitting: Arc::new(AtomicBool::new(false)),
        });
    }

    show_ask_window(app, &capture)?;
    app.emit(
        "ask://session",
        AskSessionPayload {
            data_url: capture.data_url,
            width: capture.width,
            height: capture.height,
            phase: "select".into(),
        },
    )
    .map_err(|e| e.to_string())?;

    eprintln!(
        "[soul][ask] stage=ready ms={} (select a region, speak your question)",
        t0.elapsed().as_millis()
    );
    Ok(())
}

fn on_ask_hotkey_again(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let selection_done = state
        .ask
        .lock()
        .as_ref()
        .map(|s| s.selection_done)
        .unwrap_or(false);

    if !selection_done {
        cancel_ask(app)?;
        return Ok(());
    }

    if let Some(session) = state.ask.lock().as_ref() {
        session.stop_requested.store(true, Ordering::SeqCst);
    }
    // Submit with whatever audio we have (no typed text).
    submit_ask(app, None)
}

pub fn cancel_ask(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    if let Some(session) = state.ask.lock().as_ref() {
        session.stop_requested.store(true, Ordering::SeqCst);
    }
    if state.recorder.is_recording() {
        let _ = state.recorder.stop();
    }
    *state.ask.lock() = None;
    *state.busy.lock() = false;
    hide_ask_window(app);
    let _ = app.emit(
        "ask://closed",
        AskStatusPayload {
            state: "cancelled".into(),
            message: None,
        },
    );
    eprintln!("[soul][ask] cancelled");
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionSelectedArgs {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Called when the user finishes drawing a selection.
pub fn region_selected(app: &AppHandle, rect: RegionSelectedArgs) -> Result<(), String> {
    let t0 = Instant::now();
    let state = app.state::<AppState>();
    let mut slot = state.ask.lock();
    let session = slot
        .as_mut()
        .ok_or_else(|| "No active Circle-to-Ask session.".to_string())?;

    let crop = CropRect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    };
    let cropped = capture::crop_png(&session.full_png, &crop)?;
    session.cropped_png = Some(cropped);
    session.selection = Some(crop.clone());
    session.selection_done = true;

    if !session.silence_watcher {
        session.silence_watcher = true;
        let stop = Arc::clone(&session.stop_requested);
        let app_handle = app.clone();
        let recorder = Arc::clone(&state.recorder);
        thread::spawn(move || {
            wait_for_silence_then_submit(&app_handle, &recorder, stop);
        });
    }

    eprintln!(
        "[soul][ask] stage=selection_done ms={}",
        t0.elapsed().as_millis()
    );
    let _ = app.emit(
        "ask://status",
        AskStatusPayload {
            state: "listening".into(),
            message: Some("Listening… release into silence or press the Ask hotkey again.".into()),
        },
    );
    Ok(())
}

fn wait_for_silence_then_submit(
    app: &AppHandle,
    recorder: &AudioRecorder,
    stop: Arc<AtomicBool>,
) {
    let mut silent_ms = 0u64;
    // Require a short burst of speech first so we don't cut off immediately.
    let mut heard_speech = false;
    let started = Instant::now();

    while !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(50));
        let level = recorder.level();
        if level >= SILENCE_THRESHOLD {
            heard_speech = true;
            silent_ms = 0;
        } else if heard_speech {
            silent_ms += 50;
            if silent_ms >= SILENCE_MS {
                eprintln!(
                    "[soul][ask] stage=silence_detected after_ms={}",
                    started.elapsed().as_millis()
                );
                let _ = submit_ask(app, None);
                return;
            }
        }

        // Safety timeout: 60s
        if started.elapsed() > Duration::from_secs(60) {
            let _ = submit_ask(app, None);
            return;
        }
    }
}

pub fn submit_ask(app: &AppHandle, text: Option<String>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let (cropped, selection, history, was_recording, submitting) = {
        let mut slot = state.ask.lock();
        let session = slot
            .as_mut()
            .ok_or_else(|| "No active Circle-to-Ask session.".to_string())?;
        if session
            .submitting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }
        session.stop_requested.store(true, Ordering::SeqCst);
        let Some(cropped) = session.cropped_png.clone() else {
            session.submitting.store(false, Ordering::SeqCst);
            return Err("Select a screen region first.".into());
        };
        let Some(selection) = session.selection.clone() else {
            session.submitting.store(false, Ordering::SeqCst);
            return Err("Select a screen region first.".into());
        };
        let history = session.history.clone();
        let submitting = Arc::clone(&session.submitting);
        (cropped, selection, history, state.recorder.is_recording(), submitting)
    };

    let wav = if was_recording {
        match state.recorder.stop() {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                // Typed-only questions can proceed without audio.
                if text.as_ref().map(|t| !t.trim().is_empty()).unwrap_or(false) {
                    None
                } else {
                    return Err(e);
                }
            }
        }
    } else {
        None
    };

    let _ = app.emit(
        "ask://status",
        AskStatusPayload {
            state: "processing".into(),
            message: Some("Thinking…".into()),
        },
    );

    let app_handle = app.clone();
    thread::spawn(move || {
        let t0 = Instant::now();
        let result = (|| {
            let settings = load_settings(&app_handle)?;
            let client =
                GeminiClient::new(&settings.api_key, settings.effective_ask_model());
            let models = crate::ai::gemini::models_cached_or_fetch(&client);
            let fallback = resolve_fallback_model(&settings, &models);

            let hist: Vec<(String, String)> = history
                .iter()
                .map(|t| (t.role.clone(), t.text.clone()))
                .collect();

            let app_for_status = app_handle.clone();
            let app_for_retire = app_handle.clone();
            let answer = client.ask_screen_resilient(
                &cropped,
                text.as_deref(),
                wav.as_deref(),
                &settings.language,
                &hist,
                Some(&fallback),
                move |st, msg| {
                    let _ = app_for_status.emit(
                        "ask://status",
                        AskStatusPayload {
                            state: st.into(),
                            message: msg,
                        },
                    );
                },
                move |retired| {
                    retire_model_and_heal(&app_for_retire, retired);
                },
            )?;
            eprintln!(
                "[soul][ask] stage=gemini_done ms={}",
                t0.elapsed().as_millis()
            );
            Ok::<_, String>(answer)
        })();

        match result {
            Ok(answer) => {
                let mut history = history;
                let q = text
                    .clone()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| "(voice question)".into());
                history.push(ChatTurn {
                    role: "user".into(),
                    text: q,
                });
                history.push(ChatTurn {
                    role: "model".into(),
                    text: answer.clone(),
                });

                if let Some(session) = app_handle.state::<AppState>().ask.lock().as_mut() {
                    session.history = history.clone();
                    session.silence_watcher = false;
                    session.stop_requested = Arc::new(AtomicBool::new(false));
                    session.submitting.store(false, Ordering::SeqCst);
                }

                let _ = app_handle.emit(
                    "ask://answer",
                    AskAnswerPayload {
                        answer,
                        history,
                        selection,
                    },
                );
                // Stay in ask mode for follow-ups; release busy so follow-ups can record.
                if let Some(state) = app_handle.try_state::<AppState>() {
                    *state.busy.lock() = false;
                }
            }
            Err(e) => {
                crate::push_error_log(&app_handle, e.clone());
                let _ = app_handle.emit(
                    "ask://status",
                    AskStatusPayload {
                        state: "error".into(),
                        message: Some(e),
                    },
                );
                submitting.store(false, Ordering::SeqCst);
                if let Some(state) = app_handle.try_state::<AppState>() {
                    *state.busy.lock() = false;
                }
            }
        }
    });

    Ok(())
}

/// Follow-up in the same card (keeps crop + history).
pub fn followup_ask(app: &AppHandle, text: Option<String>, with_audio: bool) -> Result<(), String> {
    let state = app.state::<AppState>();
    {
        let slot = state.ask.lock();
        let session = slot
            .as_ref()
            .ok_or_else(|| "No active Circle-to-Ask session.".to_string())?;
        if session.cropped_png.is_none() {
            return Err("No selection to ask about.".into());
        }
    }

    let typed = text.as_ref().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    if typed.is_some() && !with_audio {
        return submit_ask(app, typed);
    }

    if with_audio && !state.recorder.is_recording() {
        let settings = load_settings(app).unwrap_or_default();
        let preferred = if settings.microphone.trim().is_empty() {
            None
        } else {
            Some(settings.microphone.clone())
        };
        state.recorder.start(preferred)?;
        *state.busy.lock() = true;

        let stop = {
            let mut slot = state.ask.lock();
            let session = slot.as_mut().unwrap();
            session.stop_requested = Arc::new(AtomicBool::new(false));
            session.silence_watcher = true;
            session.selection_done = true;
            Arc::clone(&session.stop_requested)
        };
        let app_handle = app.clone();
        let recorder = Arc::clone(&state.recorder);
        thread::spawn(move || {
            wait_for_silence_then_submit(&app_handle, &recorder, stop);
        });
        let _ = app.emit(
            "ask://status",
            AskStatusPayload {
                state: "listening".into(),
                message: Some("Listening for follow-up…".into()),
            },
        );
        return Ok(());
    }

    submit_ask(app, typed)
}

fn show_ask_window(app: &AppHandle, capture: &CaptureResult) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("ask") {
        win.set_position(Position::Physical(PhysicalPosition::new(
            capture.monitor_x,
            capture.monitor_y,
        )))
        .map_err(|e| e.to_string())?;
        win.set_size(Size::Physical(PhysicalSize::new(
            capture.width,
            capture.height,
        )))
        .map_err(|e| e.to_string())?;
        win.show().map_err(|e| e.to_string())?;
        win.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    let win = WebviewWindowBuilder::new(app, "ask", WebviewUrl::App("index.html".into()))
        .title("Soul Ask")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .visible(true)
        .focused(true)
        .build()
        .map_err(|e| format!("Failed to create ask window: {e}"))?;

    win.set_position(Position::Physical(PhysicalPosition::new(
        capture.monitor_x,
        capture.monitor_y,
    )))
    .map_err(|e| e.to_string())?;
    win.set_size(Size::Physical(PhysicalSize::new(
        capture.width,
        capture.height,
    )))
    .map_err(|e| e.to_string())?;

    Ok(())
}

fn hide_ask_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("ask") {
        let _ = win.hide();
    }
}
