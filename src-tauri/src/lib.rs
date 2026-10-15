mod agent;
mod ai;
mod ask;
mod audio;
mod capture;
mod config;
mod dev_samples;
mod island;
mod memory;
mod platform;
mod scenes;
mod talk;
mod timers;
mod voice;
#[cfg(test)]
mod voice_regression;
#[cfg(test)]
mod voice_synth;

use ai::gemini::{
    is_retired_id, model_available, normalize_model_id, pick_default_fallback,
    pick_default_primary, pick_newest_flash_lite, GeminiClient, GeminiModelInfo,
    RECOMMENDED_FLASH_MODEL,
};
use ask::AskState;
use audio::{list_microphones as enumerate_microphones, AudioRecorder, MicrophoneInfo};
use config::{
    is_ctrl_alt_letter_hotkey, load_settings, redact_settings, save_settings, Settings,
    DEFAULT_AGENT_HOTKEY,
    DEFAULT_ASK_HOTKEY, DEFAULT_HOTKEY, DEFAULT_MODEL,
};
use platform::{create_platform, Platform};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalSize,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

const ERROR_LOG_LIMIT: usize = 30;

pub struct AppState {
    pub recorder: Arc<AudioRecorder>,
    pub platform: Arc<dyn Platform>,
    pub busy: parking_lot::Mutex<bool>,
    pub error_log: parking_lot::Mutex<VecDeque<String>>,
    pub ask: AskState,
    pub agent: agent::AgentState,
    pub live: Arc<ai::live::LivePool>,
    pub voice: Arc<voice::VoicePlayer>,
}

pub(crate) use talk::{begin_capture, CaptureTarget};

/// Bumped by every `emit_overlay`; a scheduled hide only fires if nothing newer was shown.
static OVERLAY_GEN: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OverlayPayload {
    pub state: String,
    pub message: Option<String>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorLogPayload {
    message: String,
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Result<Settings, String> {
    load_settings(&app)
}

/// Memory changed: the commands session prompt lists remembered facts — reconfigure it now (in the
/// background / after the current turn) instead of on the next command.
pub(crate) fn memory_changed(app: &AppHandle) {
    let app = app.clone();
    thread::spawn(move || {
        if let (Some(state), Ok(settings)) = (app.try_state::<AppState>(), load_settings(&app)) {
            state.live.configure_from_settings(&settings);
        }
        let _ = app.emit("soul://memory-changed", ());
    });
}

/// Voice started / stopped playing: duck other apps' audio meanwhile (setting `duck_music`).
pub(crate) fn on_voice_speaking(app: &AppHandle, speaking: bool) {
    static DUCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let want = speaking && load_settings(app).map(|s| s.duck_music).unwrap_or(false);
    if DUCKED.swap(want, Ordering::SeqCst) == want {
        return;
    }
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let platform = Arc::clone(&state.platform);
    thread::spawn(move || {
        let r = platform.duck_other_audio(want.then_some(0.3));
        eprintln!("[soul][voice] duck={want} ok={}", r.is_ok());
        if let Err(e) = r {
            eprintln!("[soul][voice] duck failed: {e}");
        }
    });
}

// ───────────────────────── memory / scenes (Settings → Pamięć, Sceny) ─────────────────────────

#[tauri::command]
async fn memory_list(query: Option<String>) -> Result<Vec<memory::Fact>, String> {
    tauri::async_runtime::spawn_blocking(move || match query.as_deref().map(str::trim) {
        Some(q) if !q.is_empty() => memory::search(q, 200),
        _ => memory::list(None),
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn memory_add(app: AppHandle, text: String, category: String) -> Result<memory::Fact, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let fact = memory::add_fact(&text, &category, "manual").map(|(f, _)| f);
        island::refresh_plans(&app);
        memory_changed(&app);
        fact
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn memory_update(app: AppHandle, id: i64, text: String, category: String) -> Result<memory::Fact, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let fact = memory::update_fact(id, &text, &category);
        island::refresh_plans(&app);
        memory_changed(&app);
        fact
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn memory_delete(app: AppHandle, id: i64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        memory::delete_fact(id)?;
        island::refresh_plans(&app);
        memory_changed(&app);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn memory_clear(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        memory::clear_all()?;
        island::refresh_plans(&app);
        memory_changed(&app);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn scenes_list() -> Result<Vec<scenes::Scene>, String> {
    tauri::async_runtime::spawn_blocking(scenes::list)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn scene_save(scene: scenes::Scene) -> Result<scenes::Scene, String> {
    tauri::async_runtime::spawn_blocking(move || scenes::save(scene))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn scene_delete(id: i64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || scenes::delete(id))
        .await
        .map_err(|e| e.to_string())?
}

/// "▶ Uruchom" in Settings: runs on a worker thread, progress on the Island.
#[tauri::command]
fn scene_run(app: AppHandle, id: i64) -> Result<(), String> {
    let platform = Arc::clone(&app.state::<AppState>().platform);
    thread::spawn(move || {
        let Some(scene) = scenes::list().ok().and_then(|l| l.into_iter().find(|s| s.id == id)) else {
            report_error(&app, "Scena nie istnieje.".into());
            return;
        };
        island::set_activity(island::Activity::Command);
        match scenes::run(&app, &platform, &scene) {
            Ok(msg) => {
                let _ = emit_overlay(&app, "done", Some(msg));
            }
            Err(e) => report_error(&app, e),
        }
        schedule_hide(&app, 2_500);
    });
    Ok(())
}

/// Island suggestion chip "Utwórz scenę".
#[tauri::command]
async fn scene_create_suggested(app: AppHandle, apps: Vec<String>) -> Result<scenes::Scene, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let scene = scenes::create_suggested(apps)?;
        let _ = app.emit("soul://scenes-changed", ());
        island::toast(
            &app,
            "scene",
            &format!("Utworzono scenę „{}”. Edytuj ją w Ustawieniach → Sceny.", scene.name),
            None,
        );
        Ok(scene)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Frontend sound cues land in the console log as `[soul][sfx]` too.
#[tauri::command]
fn sfx_log(cue: String, pack: String) {
    soul_debug!("[soul][sfx] play cue={cue} pack={pack}");
}

#[tauri::command]
fn save_app_settings(
    app: AppHandle,
    settings: Settings,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let previous = load_settings(&app).unwrap_or_default();
    let mut settings = settings;
    // Keep retirement ledger even if an older Settings UI omitted the field.
    if settings.retired_models.is_empty() && !previous.retired_models.is_empty() {
        settings.retired_models = previous.retired_models.clone();
    }
    // Appearance is owned by `save_appearance` (saved live); a stale form copy must not win.
    settings.appearance = previous.appearance.clone();

    // Validate hotkeys BEFORE persisting anything.
    for (label, hk) in [
        ("Dictation", &settings.hotkey),
        ("Circle-to-Ask", &settings.ask_hotkey),
        ("Voice commands", &settings.agent_hotkey),
    ] {
        parse_hotkey(hk).map_err(|e| format!("{label} hotkey '{hk}': {e}"))?;
        if is_ctrl_alt_letter_hotkey(hk) {
            return Err(format!(
                "{label} hotkey '{hk}' uses Ctrl+Alt+letter, which collides with Polish AltGr (ć, ą, ę…). Use Ctrl+Win+… or Win+Alt+… instead."
            ));
        }
    }
    let (a, b, c) = (
        parse_hotkey(&settings.hotkey)?,
        parse_hotkey(&settings.ask_hotkey)?,
        parse_hotkey(&settings.agent_hotkey)?,
    );
    // In Smart mode only the Ask combo is registered next to the talk key.
    if !settings.smart_talk() && (a == b || a == c || b == c) {
        return Err("Each action needs a different hotkey.".into());
    }
    normalize_talk_settings(&mut settings)?;

    let hotkeys_changed = previous.hotkey != settings.hotkey
        || previous.ask_hotkey != settings.ask_hotkey
        || previous.agent_hotkey != settings.agent_hotkey
        || previous.smart_talk() != settings.smart_talk();
    let smart_active = apply_talk_mode(&app, &settings);
    if hotkeys_changed {
        if let Err(e) = reregister_hotkeys(&app, &settings, smart_active) {
            // Roll back so the user is never left without working hotkeys.
            let smart_prev = apply_talk_mode(&app, &previous);
            let _ = reregister_hotkeys(&app, &previous, smart_prev);
            return Err(e);
        }
    }
    save_settings(&app, &settings)?;

    // Restart level meter if mic changed while Settings is open.
    if previous.microphone != settings.microphone && state.recorder.is_monitoring() {
        let preferred = if settings.microphone.trim().is_empty() {
            None
        } else {
            Some(settings.microphone.clone())
        };
        let _ = state.recorder.start_monitor(preferred, app.clone());
    }

    island::on_settings_changed(&app, &settings);
    state.voice.set_volume(settings.voice_volume);
    if previous.duck_music && !settings.duck_music {
        let platform = Arc::clone(&state.platform);
        thread::spawn(move || {
            let _ = platform.duck_other_audio(None);
        });
    }
    state.live.configure_from_settings(&settings);
    eprintln!("[soul] settings saved: {}", redact_settings(&settings));
    Ok(())
}

/// Validate / normalize Smart-talk, voice and personality fields in place.
fn normalize_talk_settings(settings: &mut Settings) -> Result<(), String> {
    settings.talk_mode = if settings.smart_talk() {
        "smart".into()
    } else {
        "separate".into()
    };
    let key = platform::hook::TalkKey::parse(&settings.talk_key)
        .map_err(|e| format!("Talk key: {e}"))?;
    settings.talk_key = key.name();
    settings.wake_word = settings.wake_word.trim().to_string();
    if settings.wake_word.is_empty() {
        settings.wake_word = config::DEFAULT_WAKE_WORD.to_string();
    }
    if !config::VOICE_NAMES
        .iter()
        .any(|v| v.eq_ignore_ascii_case(settings.voice_name.trim()))
    {
        settings.voice_name = config::DEFAULT_VOICE.to_string();
    }
    let speed = settings.voice_speed.trim().to_ascii_lowercase();
    settings.voice_speed = match speed.as_str() {
        "slow" | "fast" => speed,
        _ => config::DEFAULT_VOICE_SPEED.to_string(),
    };
    let personality = settings.personality.trim().to_ascii_lowercase();
    settings.personality = match personality.as_str() {
        "neutral" | "ziomek" | "professional" | "sarcastic" => personality,
        _ => config::DEFAULT_PERSONALITY.to_string(),
    };
    Ok(())
}

/// Smart mode → install/configure the input hook; falls back to separate hotkeys (and says so)
/// when the hook is unavailable (e.g. Wayland). Returns true when the talk key is live.
fn apply_talk_mode(app: &AppHandle, settings: &Settings) -> bool {
    match talk::apply(app, settings) {
        Ok(active) => active,
        Err(e) => {
            push_error_log(app, format!("{e} — the separate dictation/command hotkeys stay active."));
            false
        }
    }
}

/// Pause all hotkeys while the Settings recorder captures a key (and resume afterwards).
#[tauri::command]
fn set_hotkeys_suspended(app: AppHandle, suspended: bool) -> Result<(), String> {
    talk::set_suspended(suspended);
    let settings = load_settings(&app)?;
    let smart = apply_talk_mode(&app, &settings);
    if suspended {
        let _ = app.global_shortcut().unregister_all();
        eprintln!("[soul][hook] hotkeys suspended (recorder open)");
        Ok(())
    } else {
        eprintln!("[soul][hook] hotkeys resumed");
        reregister_hotkeys(&app, &settings, smart)
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VoicePreviewPayload {
    ok: bool,
    message: Option<String>,
}

/// Speak a short sample with the given (unsaved) voice / speed / personality.
#[tauri::command]
fn voice_preview(
    app: AppHandle,
    voice: String,
    speed: String,
    personality: String,
) -> Result<(), String> {
    let mut settings = load_settings(&app)?;
    if settings.api_key.trim().is_empty() {
        return Err("Add your Gemini API key first.".into());
    }
    settings.voice_name = voice;
    settings.voice_speed = speed;
    settings.personality = personality;
    normalize_talk_settings(&mut settings)?;
    if ai::quota::is_daily_exhausted(&settings.commands_model) {
        return Err(format!(
            "'{}' is out of daily quota (resets at midnight Pacific).",
            settings.commands_model
        ));
    }
    let system = format!(
        "{} This is a voice preview in Settings: reply with one or two short sentences introducing \
yourself as Soul, in Polish, in your personality. Do not call tools.",
        ai::gemini::agent_voice_system_prompt(&settings)
    );
    let setup = ai::live::LiveSetup::commands(
        &settings.api_key,
        &settings.commands_model,
        &system,
        None,
        &settings.voice_name,
    )
    .with_speech_language(&ai::gemini::speech_language_code(&settings.language));
    let player = Arc::clone(&app.state::<AppState>().voice);
    agent::barge_in(&app);
    let generation = player.generation();
    let voice_name = settings.voice_name.clone();
    tauri::async_runtime::spawn(async move {
        let t0 = Instant::now();
        let mut first_ms: Option<u128> = None;
        let result = ai::live::speak_once(setup, "Przedstaw się krótko.".into(), |pcm| {
            first_ms.get_or_insert_with(|| t0.elapsed().as_millis());
            player.push(generation, pcm);
        })
        .await;
        let payload = match result {
            Ok(bytes) => {
                eprintln!(
                    "[soul][voice] preview voice={voice_name} first_audio_ms={} audio_ms={} total_ms={}",
                    first_ms.unwrap_or(0),
                    bytes / 48,
                    t0.elapsed().as_millis()
                );
                VoicePreviewPayload {
                    ok: true,
                    message: None,
                }
            }
            Err(e) => {
                eprintln!("[soul][voice] preview failed: {e}");
                VoicePreviewPayload {
                    ok: false,
                    message: Some(e),
                }
            }
        };
        let _ = app.emit("soul://voice-preview", payload);
    });
    Ok(())
}

/// Persist the appearance / animation config and broadcast it so every window applies it live.
#[tauri::command]
async fn save_appearance(app: AppHandle, appearance: serde_json::Value) -> Result<(), String> {
    if !appearance.is_object() {
        return Err("Appearance must be an object.".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut settings = load_settings(&app)?;
        settings.appearance = appearance;
        save_settings(&app, &settings)?;
        let _ = app.emit("soul://appearance", settings.appearance.clone());
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Battery / fullscreen hints for the animation "battery saver" (cheap OS queries).
#[tauri::command]
async fn system_power_state(app: AppHandle) -> Result<platform::PowerState, String> {
    tauri::async_runtime::spawn_blocking(move || Ok(app.state::<AppState>().platform.power_state()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn clear_model_notice(app: AppHandle) -> Result<(), String> {
    let mut settings = load_settings(&app)?;
    if settings.model_notice.take().is_some() {
        save_settings(&app, &settings)?;
    }
    Ok(())
}

#[tauri::command]
fn test_api_key(api_key: String, model: Option<String>) -> Result<String, String> {
    let model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let client = GeminiClient::new(api_key, model);
    client.test_key()
}

#[tauri::command]
fn list_gemini_models(api_key: String) -> Result<Vec<GeminiModelInfo>, String> {
    let client = GeminiClient::new(api_key, RECOMMENDED_FLASH_MODEL);
    client.list_audio_generate_models()
}

#[tauri::command]
fn open_settings_window(app: AppHandle) -> Result<(), String> {
    show_settings(&app)
}

#[tauri::command]
fn get_error_log(state: tauri::State<'_, AppState>) -> Vec<String> {
    state.error_log.lock().iter().cloned().collect()
}

#[tauri::command]
fn clear_error_log(state: tauri::State<'_, AppState>) {
    state.error_log.lock().clear();
}

#[tauri::command]
fn list_microphones() -> Result<Vec<MicrophoneInfo>, String> {
    enumerate_microphones()
}

#[tauri::command]
fn start_mic_monitor(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    device: Option<String>,
) -> Result<String, String> {
    let preferred = match device {
        Some(name) if !name.trim().is_empty() => Some(name),
        _ => {
            let settings = load_settings(&app).unwrap_or_default();
            if settings.microphone.trim().is_empty() {
                None
            } else {
                Some(settings.microphone)
            }
        }
    };
    let result = state.recorder.start_monitor(preferred, app.clone())?;
    if let Some(notice) = &result.notice {
        let _ = app.emit(
            "soul://mic-notice",
            ErrorLogPayload {
                message: notice.clone(),
            },
        );
    }
    Ok(result.device_name)
}

#[tauri::command]
fn stop_mic_monitor(state: tauri::State<'_, AppState>) {
    state.recorder.stop_monitor();
}

#[tauri::command]
fn ask_start(app: AppHandle) -> Result<(), String> {
    ask::start_ask(&app).map_err(|e| {
        push_error_log(&app, e.clone());
        e
    })
}

#[tauri::command]
fn ask_cancel(app: AppHandle) -> Result<(), String> {
    ask::cancel_ask(&app)
}

#[tauri::command]
fn ask_region_selected(app: AppHandle, region: ask::RegionSelectedArgs) -> Result<(), String> {
    ask::region_selected(&app, region)
}

#[tauri::command]
fn ask_submit(app: AppHandle, text: Option<String>) -> Result<(), String> {
    ask::submit_ask(&app, text)
}

#[tauri::command]
fn ask_followup(app: AppHandle, text: Option<String>, with_audio: bool) -> Result<(), String> {
    ask::followup_ask(&app, text, with_audio)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(AppState {
            recorder: Arc::new(AudioRecorder::new()),
            platform: create_platform(),
            busy: parking_lot::Mutex::new(false),
            error_log: parking_lot::Mutex::new(VecDeque::new()),
            ask: ask::new_ask_state(),
            agent: agent::new_agent_state(),
            live: ai::live::LivePool::new(),
            voice: voice::VoicePlayer::new(),
        })
        .on_window_event(|window, event| {
            // Closing Settings hides it (the tray reopens it) and releases the mic level meter.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "settings" {
                    api.prevent_close();
                    let _ = window.hide();
                    if let Some(state) = window.app_handle().try_state::<AppState>() {
                        state.recorder.stop_monitor();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_app_settings,
            clear_model_notice,
            test_api_key,
            list_gemini_models,
            open_settings_window,
            get_error_log,
            clear_error_log,
            list_microphones,
            start_mic_monitor,
            stop_mic_monitor,
            dev_samples::voice_sample_start,
            dev_samples::voice_sample_stop,
            dev_samples::voice_sample_cancel,
            ask_start,
            ask_cancel,
            ask_region_selected,
            ask_submit,
            ask_followup,
            set_hotkeys_suspended,
            voice_preview,
            save_appearance,
            system_power_state,
            agent::agent_confirm,
            memory_list,
            memory_add,
            memory_update,
            memory_delete,
            memory_clear,
            scenes_list,
            scene_save,
            scene_delete,
            scene_run,
            scene_create_suggested,
            timers::timers_list,
            timers::timer_cancel,
            sfx_log,
            island::island_get_snapshot,
            island::island_set_expanded,
            island::island_set_mode,
            island::island_media_action,
            island::island_get_now_playing,
            island::island_clipboard_list,
            island::island_clipboard_clear,
            island::island_clipboard_copy,
            island::island_launch_pinned,
            island::island_set_volume,
            island::island_list_apps,
            island::island_search_apps,
            island::island_update_pins
        ])
        .setup(|app| {
            setup_tray(app.handle())?;
            if let Err(e) = memory::init(app.handle()) {
                eprintln!("[soul][memory] unavailable: {e}");
            }
            island::init(app.handle());
            timers::start_worker(app.handle().clone());
            app.state::<AppState>().voice.attach(app.handle().clone());
            app.state::<AppState>()
                .recorder
                .attach_level_emitter(app.handle().clone());

            let mut settings = load_settings(app.handle()).unwrap_or_default();
            let mut dirty = false;
            let normalized = normalize_model_id(&settings.model);
            if normalized.is_empty() {
                settings.model = DEFAULT_MODEL.to_string();
                dirty = true;
            } else if normalized != settings.model {
                settings.model = normalized;
                dirty = true;
            }
            let fallback = normalize_model_id(&settings.fallback_model);
            if fallback != settings.fallback_model {
                settings.fallback_model = fallback;
                dirty = true;
            }
            if settings.ask_hotkey.trim().is_empty() {
                settings.ask_hotkey = DEFAULT_ASK_HOTKEY.to_string();
                dirty = true;
            }
            if settings.agent_hotkey.trim().is_empty() {
                settings.agent_hotkey = DEFAULT_AGENT_HOTKEY.to_string();
                dirty = true;
            }
            if dirty {
                let _ = save_settings(app.handle(), &settings);
            }
            eprintln!("[soul] starting with {}", redact_settings(&settings));
            app.state::<AppState>().voice.set_volume(settings.voice_volume);

            // Warm Live sessions in the background (non-blocking) so the first PTT is fast.
            app.state::<AppState>().live.configure_from_settings(&settings);

            let app_handle = app.handle().clone();
            // Validate primary/fallback against the live models list (and heal retired ids).
            thread::spawn(move || {
                validate_models_on_startup(&app_handle);
            });
            let app_handle = app.handle().clone();

            let ptt = parse_hotkey(&settings.hotkey).unwrap_or_else(|_| {
                parse_hotkey(DEFAULT_HOTKEY).expect("default hotkey must parse")
            });
            let ask_hk = parse_hotkey(&settings.ask_hotkey).unwrap_or_else(|_| {
                parse_hotkey(DEFAULT_ASK_HOTKEY).expect("default ask hotkey must parse")
            });
            let agent_hk = parse_hotkey(&settings.agent_hotkey).unwrap_or_else(|_| {
                parse_hotkey(DEFAULT_AGENT_HOTKEY).expect("default agent hotkey must parse")
            });
            // Smart mode: one talk key via the low-level hook (falls back to the combos).
            let smart_active = apply_talk_mode(app.handle(), &settings);
            app.handle().plugin(
                tauri_plugin_global_shortcut::Builder::new()
                    .with_handler(move |app, shortcut, event| {
                        // Polish AltGr = Right Alt = Ctrl+Alt. Don't fire hotkeys while AltGr is down.
                        // Let Released through so a hold-to-talk session can always end.
                        if event.state() == ShortcutState::Pressed && is_altgr_down() {
                            return;
                        }
                        if talk::is_suspended() {
                            return;
                        }
                        let Ok(current) = load_settings(app) else {
                            return;
                        };
                        let Ok(ptt_sc) = parse_hotkey(&current.hotkey) else {
                            return;
                        };
                        let Ok(ask_sc) = parse_hotkey(&current.ask_hotkey) else {
                            return;
                        };
                        let Ok(agent_sc) = parse_hotkey(&current.agent_hotkey) else {
                            return;
                        };

                        if *shortcut == ask_sc {
                            if event.state() == ShortcutState::Pressed {
                                if let Err(e) = ask::start_ask(app) {
                                    push_error_log(app, e.clone());
                                    let _ = emit_overlay(app, "error", Some(e));
                                    schedule_hide(app, 5000);
                                }
                            }
                            return;
                        }

                        if *shortcut == agent_sc {
                            if ask::is_active(&app.state::<AppState>().ask) {
                                return;
                            }
                            match event.state() {
                                ShortcutState::Pressed => agent::on_agent_pressed(app),
                                ShortcutState::Released => agent::on_agent_released(app),
                            }
                            return;
                        }

                        if *shortcut != ptt_sc {
                            return;
                        }
                        // Don't run dictation while Circle-to-Ask is open.
                        if ask::is_active(&app.state::<AppState>().ask) {
                            return;
                        }

                        match event.state() {
                            ShortcutState::Pressed => on_ptt_pressed(app),
                            ShortcutState::Released => on_ptt_released(app),
                        }
                    })
                    .build(),
            )?;

            if !smart_active {
                if let Err(e) = app.global_shortcut().register(ptt) {
                    eprintln!("[soul] failed to register PTT hotkey: {e}");
                    let _ = emit_overlay(
                        &app_handle,
                        "error",
                        Some(format!("Could not register dictation hotkey: {e}")),
                    );
                }
            }
            if let Err(e) = app.global_shortcut().register(ask_hk) {
                eprintln!("[soul] failed to register Ask hotkey: {e}");
                let _ = emit_overlay(
                    &app_handle,
                    "error",
                    Some(format!("Could not register Circle-to-Ask hotkey: {e}")),
                );
            }
            if !smart_active {
                if let Err(e) = app.global_shortcut().register(agent_hk) {
                    eprintln!("[soul] failed to register Agent hotkey: {e}");
                    let _ = emit_overlay(
                        &app_handle,
                        "error",
                        Some(format!("Could not register voice-command hotkey: {e}")),
                    );
                }
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Soul");
}

fn setup_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let settings_i = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&settings_i, &quit_i])?;

    let tray = TrayIconBuilder::new()
        .icon(app.default_window_icon().cloned().expect("missing app icon"))
        .menu(&menu)
        .tooltip("Soul")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "settings" => {
                let _ = show_settings(app);
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = show_settings(tray.app_handle());
            }
        })
        .build(app)?;

    // Keep tray alive for the app lifetime.
    let _ = tray;
    Ok(())
}

fn show_settings(app: &AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("settings") {
        win.show().map_err(|e| e.to_string())?;
        win.set_focus().map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("Settings window not found".into())
    }
}

fn show_overlay(app: &AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("overlay") {
        win.show().map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("Overlay window not found".into())
    }
}

fn hide_overlay(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("overlay") {
        let _ = win.hide();
    }
    // Island stays visible; only clear the assistant banner.
    island::clear_assistant(app);
}

pub(crate) fn emit_overlay(
    app: &AppHandle,
    state: &str,
    message: Option<String>,
) -> Result<(), String> {
    OVERLAY_GEN.fetch_add(1, Ordering::SeqCst);
    let settings = load_settings(app).unwrap_or_default();
    if settings.island_enabled {
        // The Island renders every state, including the Confirm/Cancel card for destructive tools.
        island::emit_assistant_state(app, state, message);
        return Ok(());
    }

    show_overlay(app)?;
    if let Some(win) = app.get_webview_window("overlay") {
        let (w, h) = match state {
            "error" => (420u32, 120u32),
            "confirm" => (360u32, 100u32),
            "speaking" if message.as_ref().is_some_and(|m| m.len() > 40) => (420u32, 120u32),
            "thinking" | "done" if message.as_ref().is_some_and(|m| m.len() > 40) => {
                (360u32, 88u32)
            }
            _ => (280u32, 72u32),
        };
        let _ = win.set_size(tauri::Size::Physical(PhysicalSize::new(w, h)));
    }
    app.emit(
        "overlay://state",
        OverlayPayload {
            state: state.to_string(),
            message,
        },
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn push_error_log(app: &AppHandle, message: String) {
    eprintln!("[soul][error] {message}");
    if let Some(state) = app.try_state::<AppState>() {
        let mut log = state.error_log.lock();
        log.push_front(message.clone());
        while log.len() > ERROR_LOG_LIMIT {
            log.pop_back();
        }
    }
    let _ = app.emit(
        "soul://error-log",
        ErrorLogPayload {
            message: message.clone(),
        },
    );
}

pub(crate) fn report_error(app: &AppHandle, message: String) {
    push_error_log(app, message.clone());
    let _ = emit_overlay(app, "error", Some(message));
}

/// True when this press should stream to the Live API (setting on, key set, model not exhausted).
pub(crate) fn live_enabled_for(settings: &Settings, model: &str) -> bool {
    settings.use_live_api
        && !settings.api_key.trim().is_empty()
        && !ai::quota::is_daily_exhausted(model)
}

fn on_ptt_pressed(app: &AppHandle) {
    let state = app.state::<AppState>();
    agent::barge_in(app);
    if *state.busy.lock() {
        return;
    }
    if state.recorder.is_recording() {
        return;
    }
    // Live: begin a turn on the warm session and stream PCM into it while the key is held.
    // Everything here is a non-blocking channel send; connecting happens on the tokio runtime.
    if let Err(e) = begin_capture(app, CaptureTarget::Dictation, false) {
        report_error(app, e);
        schedule_hide(app, 8000);
    }
}

fn on_ptt_released(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.recorder.is_recording() {
        return;
    }

    {
        let mut busy = state.busy.lock();
        if *busy {
            return;
        }
        *busy = true;
    }

    let released = Instant::now();
    // stop() joins the PCM feeder, so the last chunk is queued before activityEnd.
    let wav = state.recorder.stop();
    let live_events = state.live.dictation.end_turn(released);
    eprintln!(
        "[soul][dictation] stage=recorder_stopped ms={} live={}",
        released.elapsed().as_millis(),
        live_events.is_some()
    );

    if live_events.is_none() {
        if let Err(e) = &wav {
            *state.busy.lock() = false;
            report_error(app, e.clone());
            schedule_hide(app, 8000);
            return;
        }
    }

    let _ = emit_overlay(app, "processing", None);

    let app_handle = app.clone();
    let platform = Arc::clone(&state.platform);
    let live = Arc::clone(&state.live);

    thread::spawn(move || {
        let result = run_dictation(&app_handle, &platform, live_events, wav, released);
        live.dictation.finish_turn();

        match result {
            Ok(text) => {
                island::emit_caption(&app_handle, &text);
                let _ = emit_overlay(&app_handle, "done", None);
                schedule_hide(&app_handle, 700);
            }
            Err(e) => {
                report_error(&app_handle, e);
                schedule_hide(&app_handle, 10000);
            }
        }

        if let Some(state) = app_handle.try_state::<AppState>() {
            *state.busy.lock() = false;
        }
    });
}

/// What the dictation session heard: Live transcript (raw) or REST transcript (already cleaned).
pub(crate) struct Heard {
    pub text: String,
    pub live: bool,
}

/// Worker-thread half of dictation: Live transcript (or REST fallback) → cleanup → paste.
fn run_dictation(
    app: &AppHandle,
    platform: &Arc<dyn Platform>,
    live_events: Option<std::sync::mpsc::Receiver<ai::live::LiveEvent>>,
    wav: Result<Vec<u8>, String>,
    released: Instant,
) -> Result<String, String> {
    let settings = load_settings(app)?;
    let heard = transcribe_turn(app, &settings, live_events, wav, released)?;
    paste_dictation(platform, &settings, heard, released)
}

/// Live transcript, or the REST transcript of the WAV when Live is off / failed.
fn transcribe_turn(
    app: &AppHandle,
    settings: &Settings,
    live_events: Option<std::sync::mpsc::Receiver<ai::live::LiveEvent>>,
    wav: Result<Vec<u8>, String>,
    released: Instant,
) -> Result<Heard, String> {
    let rest_model = settings.effective_ask_model().to_string();
    let rest_fallback = resolve_fallback_model(settings, &ai::gemini::cached_models());
    let client = GeminiClient::new(&settings.api_key, &rest_model);

    let live_result = live_events.map(|rx| ai::live::collect_dictation(rx, released));
    match live_result {
        Some(Ok(r)) if !r.text.trim().is_empty() => {
            eprintln!(
                "[soul][live] dictation final ms={} source={} chars={}",
                r.final_ms,
                r.source,
                r.text.chars().count()
            );
            Ok(Heard {
                text: r.text,
                live: true,
            })
        }
        other => {
            let reason = match other {
                Some(Err(e)) => e,
                Some(Ok(_)) => "empty Live transcript".to_string(),
                None => "Live API off".to_string(),
            };
            if live_failure_is_model_gone(&reason) {
                let app = app.clone();
                thread::spawn(move || revalidate_models(&app));
            }
            let wav = wav?;
            eprintln!("[soul][dictation] REST fallback ({reason}) → {rest_model}, then {rest_fallback}");
            if settings.use_live_api {
                let _ = emit_overlay(app, "retrying", Some(format!("Live failed — {rest_model}…")));
            }
            let app_for_status = app.clone();
            let app_for_retire = app.clone();
            let text = client.transcribe_and_clean_resilient(
                &wav,
                &settings.language,
                Some(&rest_fallback),
                move |st, message| {
                    let _ = emit_overlay(&app_for_status, st, message);
                },
                move |retired| {
                    retire_model_and_heal(&app_for_retire, retired);
                },
            )?;
            Ok(Heard { text, live: false })
        }
    }
}

/// Cleanup (local filler removal; LLM pass only for self-corrections) → paste.
fn paste_dictation(
    platform: &Arc<dyn Platform>,
    settings: &Settings,
    heard: Heard,
    released: Instant,
) -> Result<String, String> {
    let text = if heard.live {
        let cleaned = ai::cleanup::local_cleanup(&heard.text);
        if ai::cleanup::needs_llm_cleanup(&cleaned) {
            let rest_fallback = resolve_fallback_model(settings, &ai::gemini::cached_models());
            let client = GeminiClient::new(&settings.api_key, settings.effective_ask_model());
            // Self-correction detected: short text-only pass, bounded so it can't stall paste.
            match client.cleanup_dictation_text(
                &cleaned,
                &settings.language,
                Some(&rest_fallback),
                std::time::Duration::from_millis(1500),
            ) {
                Ok(out) if ai::cleanup::plausible_cleanup(&cleaned, &out) => out,
                Ok(_) => {
                    eprintln!("[soul][dictation] llm cleanup rejected (not a cleanup)");
                    cleaned
                }
                Err(e) => {
                    eprintln!("[soul][dictation] llm cleanup skipped: {e}");
                    cleaned
                }
            }
        } else {
            cleaned
        }
    } else {
        heard.text
    };
    if text.trim().is_empty() {
        return Err("Transcription was empty.".into());
    }
    let t_paste = Instant::now();
    platform.paste_text(&text)?;
    eprintln!(
        "[soul][live] dictation total ms={} paste_ms={}",
        released.elapsed().as_millis(),
        t_paste.elapsed().as_millis()
    );
    Ok(text)
}

/// Smart talk key released after a hold: transcribe on the dictation session, then route the
/// transcript — dictation is pasted, commands go to the agent (with a spoken reply).
pub(crate) fn on_smart_released(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.recorder.is_recording() {
        return;
    }
    {
        let mut busy = state.busy.lock();
        if *busy {
            return;
        }
        *busy = true;
    }

    let released = Instant::now();
    let wav = state.recorder.stop();
    let live_events = state.live.dictation.end_turn(released);
    eprintln!(
        "[soul][dictation] stage=recorder_stopped ms={} live={} smart=true",
        released.elapsed().as_millis(),
        live_events.is_some()
    );
    if live_events.is_none() {
        if let Err(e) = &wav {
            *state.busy.lock() = false;
            report_error(app, e.clone());
            schedule_hide(app, 8000);
            return;
        }
    }
    let _ = emit_overlay(app, "processing", None);

    let app_handle = app.clone();
    let platform = Arc::clone(&state.platform);
    let live = Arc::clone(&state.live);
    thread::spawn(move || {
        let release_busy = |app: &AppHandle| {
            if let Some(state) = app.try_state::<AppState>() {
                *state.busy.lock() = false;
            }
        };
        let settings = load_settings(&app_handle).unwrap_or_default();
        let heard = transcribe_turn(&app_handle, &settings, live_events, wav, released);
        live.dictation.finish_turn();
        let heard = match heard {
            Ok(h) => h,
            Err(e) => {
                report_error(&app_handle, e);
                schedule_hide(&app_handle, 10000);
                release_busy(&app_handle);
                return;
            }
        };
        match agent::classify_smart(&heard.text, &settings.wake_word) {
            agent::Intent::Command { text, reason } => {
                eprintln!(
                    "[soul][dictation] smart route=command reason={reason} ms={}",
                    released.elapsed().as_millis()
                );
                island::set_activity(island::Activity::Command);
                // Releases `busy` itself before the spoken reply plays.
                agent::run_text_command(&app_handle, &platform, &live, &text, released);
            }
            agent::Intent::Dictation => {
                eprintln!(
                    "[soul][dictation] smart route=dictation chars={} ms={}",
                    heard.text.chars().count(),
                    released.elapsed().as_millis()
                );
                match paste_dictation(&platform, &settings, heard, released) {
                    Ok(text) => {
                        island::emit_caption(&app_handle, &text);
                        let _ = emit_overlay(&app_handle, "done", None);
                        schedule_hide(&app_handle, 700);
                    }
                    Err(e) => {
                        report_error(&app_handle, e);
                        schedule_hide(&app_handle, 10000);
                    }
                }
                release_busy(&app_handle);
            }
        }
    });
}

/// Live setup rejected because the model id is unknown/retired → re-validate models.
pub(crate) fn live_failure_is_model_gone(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("setup rejected")
        && (lower.contains("not found") || lower.contains("not supported") || lower.contains("no longer"))
}

pub(crate) fn schedule_hide(app: &AppHandle, delay_ms: u64) {
    let app = app.clone();
    let shown = OVERLAY_GEN.load(Ordering::SeqCst);
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(delay_ms));
        if OVERLAY_GEN.load(Ordering::SeqCst) != shown {
            // Something newer is on screen (next turn, spoken reply) — leave it.
            return;
        }
        let settings = load_settings(&app).unwrap_or_default();
        if settings.island_enabled {
            // Hide confirm overlay if open; clear island assistant banner.
            if let Some(win) = app.get_webview_window("overlay") {
                let _ = win.hide();
            }
            island::clear_assistant(&app);
        } else {
            hide_overlay(&app);
        }
    });
}

pub(crate) fn resolve_fallback_model(settings: &Settings, models: &[GeminiModelInfo]) -> String {
    let primary = normalize_model_id(settings.effective_ask_model());
    let configured = normalize_model_id(settings.effective_rest_fallback());
    if models.is_empty() {
        // No list cached yet (offline / not fetched) — trust the configured chain.
        return if configured != primary {
            configured
        } else {
            normalize_model_id(config::DEFAULT_ASK_FALLBACK_MODEL)
        };
    }
    if model_available(&configured, models, &settings.retired_models)
        && configured != primary
    {
        return configured;
    }
    // Prefer gemini-3.5-flash-lite for Ask when present.
    let prefer = normalize_model_id(config::DEFAULT_ASK_FALLBACK_MODEL);
    if model_available(&prefer, models, &settings.retired_models) && prefer != primary {
        return prefer;
    }
    pick_default_fallback(&primary, models, &settings.retired_models)
}

pub(crate) fn revalidate_models(app: &AppHandle) {
    validate_models_on_startup(app);
}

/// Validate saved models (REST + Live) against the live list; heal + notice when needed.
fn validate_models_on_startup(app: &AppHandle) {
    let Ok(mut settings) = load_settings(app) else {
        return;
    };
    if settings.api_key.trim().is_empty() {
        return;
    }

    let client = GeminiClient::new(&settings.api_key, &settings.model);
    let models = match client.list_audio_generate_models() {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => {
            eprintln!("[soul] model validation: empty models list");
            return;
        }
        Err(e) => {
            eprintln!("[soul] model validation skipped: {e}");
            return;
        }
    };

    let changes = heal_settings_models(&mut settings, &models);
    if changes.is_empty() {
        eprintln!(
            "[soul] model validation ok: primary={} fallback={}",
            settings.model, settings.fallback_model
        );
        return;
    }

    let notice = format!("Models updated: {}.", changes.join("; "));
    settings.model_notice = Some(notice.clone());
    if let Err(e) = save_settings(app, &settings) {
        eprintln!("[soul] failed to save healed models: {e}");
        return;
    }
    eprintln!("[soul] {notice}");
    emit_model_notice(app, &notice);
    app.state::<AppState>().live.configure_from_settings(&settings);
}

/// Pick a Live model for dictation (`transcribe`) or commands from the live list.
fn pick_live_model(models: &[GeminiModelInfo], dictation: bool, retired: &[String]) -> Option<String> {
    let preferred = if dictation {
        config::DEFAULT_DICTATION_MODEL
    } else {
        config::DEFAULT_COMMANDS_MODEL
    };
    let usable = |m: &&GeminiModelInfo| m.supports_live && !is_retired_id(&m.id, retired);
    if models.iter().filter(usable).any(|m| m.id == preferred) {
        return Some(preferred.to_string());
    }
    models
        .iter()
        .filter(usable)
        .filter(|m| {
            let id = m.id.to_ascii_lowercase();
            if dictation {
                id.contains("transcribe")
            } else {
                !id.contains("transcribe")
                    && !id.contains("translate")
                    && !id.contains("robotics")
                    && !id.contains("thinking")
            }
        })
        .max_by_key(|m| ai::gemini::parse_gemini_version(&m.id).unwrap_or((0, 0)))
        .map(|m| m.id.clone())
}

/// Ensure primary/fallback exist in `models` and are not retired. Returns human change notes.
fn heal_settings_models(settings: &mut Settings, models: &[GeminiModelInfo]) -> Vec<String> {
    let mut changes = Vec::new();
    let retired = settings.retired_models.clone();

    let primary_ok = model_available(&settings.model, models, &retired);
    if !primary_ok {
        let old = settings.model.clone();
        // If it was saved but missing from the list, treat as retired.
        let old_norm = normalize_model_id(&old);
        if !old_norm.is_empty() && !is_retired_id(&old_norm, &settings.retired_models) {
            settings.retired_models.push(old_norm.clone());
        }
        settings.model = pick_default_primary(models, &settings.retired_models);
        changes.push(format!("primary '{old}' → '{}'", settings.model));
    }

    let fallback_ok = model_available(&settings.fallback_model, models, &settings.retired_models)
        && normalize_model_id(&settings.fallback_model) != normalize_model_id(&settings.model);
    if !fallback_ok {
        let old = settings.fallback_model.clone();
        let old_norm = normalize_model_id(&old);
        if !old_norm.is_empty()
            && !model_available(&old_norm, models, &[])
            && !is_retired_id(&old_norm, &settings.retired_models)
        {
            settings.retired_models.push(old_norm);
        }
        let next = pick_newest_flash_lite(models, &settings.model, &settings.retired_models)
            .unwrap_or_else(|| {
                pick_default_fallback(&settings.model, models, &settings.retired_models)
            });
        settings.fallback_model = next;
        if old.is_empty() {
            changes.push(format!(
                "fallback auto-set to '{}'",
                settings.fallback_model
            ));
        } else {
            changes.push(format!(
                "fallback '{old}' → '{}'",
                settings.fallback_model
            ));
        }
    }

    // Live models must exist on this key and support bidiGenerateContent.
    for (dictation, label) in [(true, "dictation"), (false, "commands")] {
        let current = if dictation {
            &settings.dictation_model
        } else {
            &settings.commands_model
        };
        let id = normalize_model_id(current);
        let ok = models
            .iter()
            .any(|m| m.id == id && m.supports_live && !is_retired_id(&id, &settings.retired_models));
        if ok {
            continue;
        }
        if let Some(next) = pick_live_model(models, dictation, &settings.retired_models) {
            changes.push(format!("{label} (Live) '{id}' → '{next}'"));
            if dictation {
                settings.dictation_model = next;
            } else {
                settings.commands_model = next;
            }
        }
    }

    changes
}

/// Mark a model retired (404 / no longer available), auto-pick replacements, persist + notify.
pub(crate) fn retire_model_and_heal(app: &AppHandle, retired: &str) {
    let retired = normalize_model_id(retired);
    if retired.is_empty() {
        return;
    }

    let Ok(mut settings) = load_settings(app) else {
        return;
    };
    if is_retired_id(&retired, &settings.retired_models) {
        // Still heal slots in case a race left a retired id selected.
    } else {
        settings.retired_models.push(retired.clone());
    }

    let client = GeminiClient::new(&settings.api_key, DEFAULT_MODEL);
    let models = client.list_audio_generate_models().unwrap_or_default();

    let mut changes = Vec::new();
    if normalize_model_id(&settings.model) == retired {
        settings.model = pick_default_primary(&models, &settings.retired_models);
        changes.push(format!("primary → {}", settings.model));
    }
    if normalize_model_id(&settings.fallback_model) == retired
        || settings.fallback_model.is_empty()
        || normalize_model_id(&settings.fallback_model) == normalize_model_id(&settings.model)
        || (!models.is_empty()
            && !model_available(
                &settings.fallback_model,
                &models,
                &settings.retired_models,
            ))
    {
        settings.fallback_model =
            pick_default_fallback(&settings.model, &models, &settings.retired_models);
        changes.push(format!("fallback → {}", settings.fallback_model));
    }

    let notice = if changes.is_empty() {
        format!("Model '{retired}' is no longer available and was marked retired.")
    } else {
        format!(
            "Model '{retired}' is no longer available and was retired. Updated: {}.",
            changes.join(", ")
        )
    };
    settings.model_notice = Some(notice.clone());
    if let Err(e) = save_settings(app, &settings) {
        eprintln!("[soul] failed to persist retired model: {e}");
        return;
    }
    eprintln!("[soul] {notice}");
    emit_model_notice(app, &notice);
}

fn emit_model_notice(app: &AppHandle, message: &str) {
    let _ = app.emit(
        "soul://model-notice",
        ErrorLogPayload {
            message: message.to_string(),
        },
    );
}

/// True when Right Alt (AltGr / VK_RMENU) is currently held.
fn is_altgr_down() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_RMENU};
        // High bit set => key is down.
        unsafe { GetAsyncKeyState(VK_RMENU.0 as i32) as u16 & 0x8000 != 0 }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Register the global combos for the current mode: Ask always; dictation + commands only when
/// the Smart talk key is not active.
fn reregister_hotkeys(app: &AppHandle, settings: &Settings, smart_active: bool) -> Result<(), String> {
    let ptt = &settings.hotkey;
    let ask_hk = &settings.ask_hotkey;
    let agent_hk = &settings.agent_hotkey;
    let ptt_sc = parse_hotkey(ptt)?;
    let ask_sc = parse_hotkey(ask_hk)?;
    let agent_sc = parse_hotkey(agent_hk)?;
    let _ = app.global_shortcut().unregister_all();
    app.global_shortcut()
        .register(ask_sc)
        .map_err(|e| format!("Failed to register Ask hotkey '{ask_hk}': {e}"))?;
    if smart_active {
        return Ok(());
    }
    app.global_shortcut()
        .register(ptt_sc)
        .map_err(|e| format!("Failed to register dictation hotkey '{ptt}': {e}"))?;
    app.global_shortcut()
        .register(agent_sc)
        .map_err(|e| format!("Failed to register Agent hotkey '{agent_hk}': {e}"))?;
    Ok(())
}

fn parse_hotkey(hotkey: &str) -> Result<Shortcut, String> {
    // Accept forms like "Ctrl+Alt+Space", "Control+Alt+Space".
    let normalized = hotkey
        .replace(' ', "")
        .replace("Control", "Ctrl")
        .replace("control", "Ctrl")
        .replace("ctrl", "Ctrl")
        .replace("CMD", "Command")
        .replace("Cmd", "Command");

    let mut modifiers = Modifiers::empty();
    let mut key: Option<Code> = None;

    for part in normalized.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "alt" | "option" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            "super" | "meta" | "command" | "cmd" | "win" => modifiers |= Modifiers::SUPER,
            "space" => key = Some(Code::Space),
            "enter" | "return" => key = Some(Code::Enter),
            "tab" => key = Some(Code::Tab),
            "escape" | "esc" => key = Some(Code::Escape),
            other if other.len() == 1 => {
                let ch = other.chars().next().unwrap();
                key = Some(letter_code(ch)?);
            }
            other => {
                return Err(format!("Unsupported hotkey key: {other}"));
            }
        }
    }

    let code = key.ok_or_else(|| "Hotkey must include a key (e.g. Space)".to_string())?;
    Ok(Shortcut::new(Some(modifiers), code))
}

fn letter_code(ch: char) -> Result<Code, String> {
    Ok(match ch.to_ascii_uppercase() {
        'A' => Code::KeyA,
        'B' => Code::KeyB,
        'C' => Code::KeyC,
        'D' => Code::KeyD,
        'E' => Code::KeyE,
        'F' => Code::KeyF,
        'G' => Code::KeyG,
        'H' => Code::KeyH,
        'I' => Code::KeyI,
        'J' => Code::KeyJ,
        'K' => Code::KeyK,
        'L' => Code::KeyL,
        'M' => Code::KeyM,
        'N' => Code::KeyN,
        'O' => Code::KeyO,
        'P' => Code::KeyP,
        'Q' => Code::KeyQ,
        'R' => Code::KeyR,
        'S' => Code::KeyS,
        'T' => Code::KeyT,
        'U' => Code::KeyU,
        'V' => Code::KeyV,
        'W' => Code::KeyW,
        'X' => Code::KeyX,
        'Y' => Code::KeyY,
        'Z' => Code::KeyZ,
        '0' => Code::Digit0,
        '1' => Code::Digit1,
        '2' => Code::Digit2,
        '3' => Code::Digit3,
        '4' => Code::Digit4,
        '5' => Code::Digit5,
        '6' => Code::Digit6,
        '7' => Code::Digit7,
        '8' => Code::Digit8,
        '9' => Code::Digit9,
        other => return Err(format!("Unsupported key: {other}")),
    })
}
