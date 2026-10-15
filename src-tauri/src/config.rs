use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

pub const STORE_PATH: &str = "soul-settings.json";
/// Avoid Ctrl+Alt (AltGr on Polish layouts). Ctrl+Win+Space is hold-friendly.
pub const DEFAULT_HOTKEY: &str = "Ctrl+Super+Space";
pub const DEFAULT_ASK_HOTKEY: &str = "Ctrl+Shift+Space";
/// Win+Alt+Space — does not collide with AltGr letter chords.
pub const DEFAULT_AGENT_HOTKEY: &str = "Super+Alt+Space";
/// Default Ask / REST fallback primary — stable Flash-Lite id (not a rolling alias).
pub const DEFAULT_MODEL: &str = "gemini-3.1-flash-lite";
pub const DEFAULT_ASK_MODEL: &str = "gemini-3.1-flash-lite";
pub const DEFAULT_ASK_FALLBACK_MODEL: &str = "gemini-3.5-flash-lite";
pub const DEFAULT_DICTATION_MODEL: &str = "gemini-3.5-transcribe-live";
pub const DEFAULT_COMMANDS_MODEL: &str = "gemini-3.8-live";
pub const DEFAULT_LANGUAGE: &str = "auto";
/// Empty string means system default microphone.
pub const DEFAULT_MICROPHONE: &str = "";
/// "smart" = one hold-to-talk key with automatic dictation/command routing; "separate" = combos.
pub const DEFAULT_TALK_MODE: &str = "smart";
/// Single key / mouse button for Smart mode (names from `platform::hook::TalkKey`).
pub const DEFAULT_TALK_KEY: &str = "ControlRight";
pub const DEFAULT_WAKE_WORD: &str = "Soul";
/// Live API prebuilt voice for spoken replies.
pub const DEFAULT_VOICE: &str = "Charon";
pub const VOICE_NAMES: &[&str] = &[
    "Charon", "Puck", "Kore", "Fenrir", "Aoede", "Leda", "Orus", "Zephyr",
];
/// slow | normal | fast (prompt-level pacing; native-audio models have no rate knob).
pub const DEFAULT_VOICE_SPEED: &str = "normal";
/// neutral | ziomek | professional | sarcastic — tone of spoken replies only.
pub const DEFAULT_PERSONALITY: &str = "neutral";
/// Soul's own voice level (TTS output gain), 0–100.
pub const DEFAULT_VOICE_VOLUME: u8 = 100;
pub const DEFAULT_WAKE_SENSITIVITY: f32 = 0.5;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::enum_variant_names)]
pub enum IslandPosition {
    #[default]
    TopCenter,
    TopLeft,
    TopRight,
}

impl IslandPosition {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "topleft" | "top_left" | "left" => Self::TopLeft,
            "topright" | "top_right" | "right" => Self::TopRight,
            _ => Self::TopCenter,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TopCenter => "topCenter",
            Self::TopLeft => "topLeft",
            Self::TopRight => "topRight",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IslandSections {
    pub now_playing: bool,
    pub clipboard: bool,
    pub plans: bool,
    pub quick_apps: bool,
    pub quick_settings: bool,
}

impl Default for IslandSections {
    fn default() -> Self {
        Self {
            now_playing: true,
            clipboard: true,
            plans: true,
            quick_apps: true,
            quick_settings: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub api_key: String,
    pub hotkey: String,
    pub ask_hotkey: String,
    pub agent_hotkey: String,
    /// Legacy / Ask primary (kept for older settings files). Prefer `ask_model`.
    pub model: String,
    pub fallback_model: String,
    /// Live API dictation / transcribe model.
    #[serde(default = "default_dictation_model")]
    pub dictation_model: String,
    /// Live API voice-commands model.
    #[serde(default = "default_commands_model")]
    pub commands_model: String,
    /// Circle-to-Ask REST model.
    #[serde(default = "default_ask_model")]
    pub ask_model: String,
    /// Stream dictation/commands over the Live API (false = REST only).
    #[serde(default = "default_true")]
    pub use_live_api: bool,
    pub language: String,
    /// Preferred input device name, or empty for system default.
    pub microphone: String,
    /// Model ids that returned 404 / “no longer available” — skipped forever until cleared.
    #[serde(default)]
    pub retired_models: Vec<String>,
    /// User-facing notice about model auto-migration (shown in Settings).
    #[serde(default)]
    pub model_notice: Option<String>,
    #[serde(default = "default_true")]
    pub island_enabled: bool,
    #[serde(default)]
    pub island_position: IslandPosition,
    #[serde(default)]
    pub island_sections: IslandSections,
    /// Pinned app names for Island Quick apps (fuzzy-matched via open_app).
    #[serde(default)]
    pub pinned_apps: Vec<String>,
    /// "smart" (one talk key, auto-routing) or "separate" (dictation + command combos).
    #[serde(default = "default_talk_mode")]
    pub talk_mode: String,
    /// Smart-mode hold-to-talk key, e.g. "ControlRight", "Mouse4", "F13".
    #[serde(default = "default_talk_key")]
    pub talk_key: String,
    /// Swallow the talk key even on short taps (default: taps keep their normal behaviour).
    #[serde(default)]
    pub talk_key_swallow: bool,
    /// Talk key + left-drag starts Circle-to-Ask on the dragged region.
    #[serde(default = "default_true")]
    pub talk_drag_ask: bool,
    /// Transcripts starting with this word are routed to commands.
    #[serde(default = "default_wake_word")]
    pub wake_word: String,
    /// Speak command/question replies (Live native audio).
    #[serde(default = "default_true")]
    pub voice_enabled: bool,
    #[serde(default = "default_voice")]
    pub voice_name: String,
    #[serde(default = "default_voice_speed")]
    pub voice_speed: String,
    /// Keep spoken replies to a short confirmation.
    #[serde(default)]
    pub voice_brief: bool,
    #[serde(default = "default_personality")]
    pub personality: String,
    /// "Głośność głosu Soul" — gain applied to spoken replies (0–100).
    #[serde(default = "default_voice_volume")]
    pub voice_volume: u8,
    /// Lower other apps' audio while Soul speaks.
    #[serde(default)]
    pub duck_music: bool,
    /// "Otwieraj przeglądarkę przy wyszukiwaniu" — question searches also get a spoken summary.
    #[serde(default = "default_true")]
    pub search_open_browser: bool,
    /// Save durable facts stated in commands ("gram na serwerze X") with an Island chip.
    #[serde(default = "default_true")]
    pub auto_remember: bool,
    /// Offline wake word ("Soul", rustpotter): listen locally, open a turn on detection.
    /// Owned by `wake::save_config` (autosaved), never overwritten by `save_app_settings`.
    #[serde(default)]
    pub wake_enabled: bool,
    /// 0.0 (strict, fewer false triggers) – 1.0 (sensitive).
    #[serde(default = "default_wake_sensitivity")]
    pub wake_sensitivity: f32,
    /// No wake word while a fullscreen app / game is focused.
    #[serde(default = "default_true")]
    pub wake_pause_in_games: bool,
    /// Check for updates on startup + every 6 h and download them in the background.
    /// Owned by `updater::update_save_config` (autosaved), kept by `save_app_settings`.
    #[serde(default = "default_true")]
    pub auto_update: bool,
    /// `stable` | `beta` (beta also offers prereleases).
    #[serde(default = "default_update_channel")]
    pub update_channel: String,
    /// Visual / animation preferences (accent, spirit, Island, animation config). Owned by the
    /// frontend (`src/anim/config.ts`); Rust only stores it and broadcasts `soul://appearance`.
    #[serde(default)]
    pub appearance: serde_json::Value,
}

fn default_dictation_model() -> String {
    DEFAULT_DICTATION_MODEL.to_string()
}
fn default_commands_model() -> String {
    DEFAULT_COMMANDS_MODEL.to_string()
}
fn default_ask_model() -> String {
    DEFAULT_ASK_MODEL.to_string()
}

fn default_true() -> bool {
    true
}
fn default_talk_mode() -> String {
    DEFAULT_TALK_MODE.to_string()
}
fn default_talk_key() -> String {
    DEFAULT_TALK_KEY.to_string()
}
fn default_wake_word() -> String {
    DEFAULT_WAKE_WORD.to_string()
}
fn default_voice() -> String {
    DEFAULT_VOICE.to_string()
}
fn default_voice_speed() -> String {
    DEFAULT_VOICE_SPEED.to_string()
}
fn default_personality() -> String {
    DEFAULT_PERSONALITY.to_string()
}
fn default_voice_volume() -> u8 {
    DEFAULT_VOICE_VOLUME
}
fn default_wake_sensitivity() -> f32 {
    DEFAULT_WAKE_SENSITIVITY
}
fn default_update_channel() -> String {
    crate::updater::CHANNEL_STABLE.to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            hotkey: DEFAULT_HOTKEY.to_string(),
            ask_hotkey: DEFAULT_ASK_HOTKEY.to_string(),
            agent_hotkey: DEFAULT_AGENT_HOTKEY.to_string(),
            model: DEFAULT_ASK_MODEL.to_string(),
            fallback_model: DEFAULT_ASK_FALLBACK_MODEL.to_string(),
            dictation_model: DEFAULT_DICTATION_MODEL.to_string(),
            commands_model: DEFAULT_COMMANDS_MODEL.to_string(),
            ask_model: DEFAULT_ASK_MODEL.to_string(),
            use_live_api: true,
            language: DEFAULT_LANGUAGE.to_string(),
            microphone: DEFAULT_MICROPHONE.to_string(),
            retired_models: Vec::new(),
            model_notice: None,
            island_enabled: true,
            island_position: IslandPosition::TopCenter,
            island_sections: IslandSections::default(),
            pinned_apps: Vec::new(),
            talk_mode: DEFAULT_TALK_MODE.to_string(),
            talk_key: DEFAULT_TALK_KEY.to_string(),
            talk_key_swallow: false,
            talk_drag_ask: true,
            wake_word: DEFAULT_WAKE_WORD.to_string(),
            voice_enabled: true,
            voice_name: DEFAULT_VOICE.to_string(),
            voice_speed: DEFAULT_VOICE_SPEED.to_string(),
            voice_brief: false,
            personality: DEFAULT_PERSONALITY.to_string(),
            voice_volume: DEFAULT_VOICE_VOLUME,
            duck_music: false,
            search_open_browser: true,
            auto_remember: true,
            wake_enabled: false,
            wake_sensitivity: DEFAULT_WAKE_SENSITIVITY,
            wake_pause_in_games: true,
            auto_update: true,
            update_channel: default_update_channel(),
            appearance: serde_json::Value::Null,
        }
    }
}

impl Settings {
    /// Effective Ask REST model (ask_model, else legacy model).
    pub fn effective_ask_model(&self) -> &str {
        if !self.ask_model.trim().is_empty() {
            &self.ask_model
        } else {
            &self.model
        }
    }

    /// True when the single talk key (low-level hook) drives dictation + commands.
    pub fn smart_talk(&self) -> bool {
        !self.talk_mode.eq_ignore_ascii_case("separate")
    }

    pub fn effective_rest_fallback(&self) -> &str {
        if !self.fallback_model.trim().is_empty() {
            &self.fallback_model
        } else {
            DEFAULT_ASK_FALLBACK_MODEL
        }
    }
}

pub fn load_settings<R: Runtime>(app: &AppHandle<R>) -> Result<Settings, String> {
    let store = app
        .store(STORE_PATH)
        .map_err(|e| format!("Failed to open settings store: {e}"))?;

    let mut settings = Settings {
        api_key: load_api_key(&store),
        ..Settings::default()
    };
    if let Some(v) = store.get("hotkey") {
        settings.hotkey = v.as_str().unwrap_or(DEFAULT_HOTKEY).to_string();
    }
    if let Some(v) = store.get("askHotkey") {
        settings.ask_hotkey = v.as_str().unwrap_or(DEFAULT_ASK_HOTKEY).to_string();
    }
    if let Some(v) = store.get("agentHotkey") {
        settings.agent_hotkey = v.as_str().unwrap_or(DEFAULT_AGENT_HOTKEY).to_string();
    }
    if let Some(v) = store.get("model") {
        settings.model = v.as_str().unwrap_or(DEFAULT_ASK_MODEL).to_string();
        settings.ask_model = settings.model.clone();
    }
    if let Some(v) = store.get("fallbackModel") {
        settings.fallback_model = v
            .as_str()
            .unwrap_or(DEFAULT_ASK_FALLBACK_MODEL)
            .to_string();
    }
    if let Some(v) = store.get("dictationModel") {
        settings.dictation_model = v
            .as_str()
            .unwrap_or(DEFAULT_DICTATION_MODEL)
            .to_string();
    }
    if let Some(v) = store.get("commandsModel") {
        settings.commands_model = v
            .as_str()
            .unwrap_or(DEFAULT_COMMANDS_MODEL)
            .to_string();
    }
    if let Some(v) = store.get("askModel") {
        settings.ask_model = v.as_str().unwrap_or(DEFAULT_ASK_MODEL).to_string();
        settings.model = settings.ask_model.clone();
    }
    if settings.dictation_model.trim().is_empty() {
        settings.dictation_model = DEFAULT_DICTATION_MODEL.to_string();
    }
    if settings.commands_model.trim().is_empty() {
        settings.commands_model = DEFAULT_COMMANDS_MODEL.to_string();
    }
    if settings.ask_model.trim().is_empty() {
        settings.ask_model = if settings.model.trim().is_empty() {
            DEFAULT_ASK_MODEL.to_string()
        } else {
            settings.model.clone()
        };
    }
    if settings.fallback_model.trim().is_empty() {
        settings.fallback_model = DEFAULT_ASK_FALLBACK_MODEL.to_string();
    }
    if let Some(v) = store.get("useLiveApi") {
        settings.use_live_api = v.as_bool().unwrap_or(true);
    }
    if let Some(v) = store.get("language") {
        settings.language = v.as_str().unwrap_or(DEFAULT_LANGUAGE).to_string();
    }
    if let Some(v) = store.get("microphone") {
        settings.microphone = v.as_str().unwrap_or_default().to_string();
    }
    if let Some(v) = store.get("retiredModels") {
        if let Some(arr) = v.as_array() {
            settings.retired_models = arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
        }
    }
    if let Some(v) = store.get("modelNotice") {
        settings.model_notice = v.as_str().map(|s| s.to_string()).filter(|s| !s.is_empty());
    }
    if let Some(v) = store.get("islandEnabled") {
        settings.island_enabled = v.as_bool().unwrap_or(true);
    }
    if let Some(v) = store.get("islandPosition") {
        if let Some(s) = v.as_str() {
            settings.island_position = IslandPosition::parse(s);
        }
    }
    if let Some(v) = store.get("islandSections") {
        if let Ok(sec) = serde_json::from_value::<IslandSections>(v.clone()) {
            settings.island_sections = sec;
        }
    }
    if let Some(v) = store.get("pinnedApps") {
        if let Some(arr) = v.as_array() {
            settings.pinned_apps = arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
        }
    }

    let str_or = |key: &str, default: &str| -> String {
        store
            .get(key)
            .and_then(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let bool_or = |key: &str, default: bool| -> bool {
        store.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    };
    settings.talk_mode = str_or("talkMode", DEFAULT_TALK_MODE);
    settings.talk_key = str_or("talkKey", DEFAULT_TALK_KEY);
    settings.talk_key_swallow = bool_or("talkKeySwallow", false);
    settings.talk_drag_ask = bool_or("talkDragAsk", true);
    settings.wake_word = str_or("wakeWord", DEFAULT_WAKE_WORD);
    settings.voice_enabled = bool_or("voiceEnabled", true);
    settings.voice_name = str_or("voiceName", DEFAULT_VOICE);
    settings.voice_speed = str_or("voiceSpeed", DEFAULT_VOICE_SPEED);
    settings.voice_brief = bool_or("voiceBrief", false);
    settings.personality = str_or("personality", DEFAULT_PERSONALITY);
    settings.voice_volume = store
        .get("voiceVolume")
        .and_then(|v| v.as_u64())
        .map(|v| v.min(100) as u8)
        .unwrap_or(DEFAULT_VOICE_VOLUME);
    settings.duck_music = bool_or("duckMusic", false);
    settings.search_open_browser = bool_or("searchOpenBrowser", true);
    settings.auto_remember = bool_or("autoRemember", true);
    settings.wake_enabled = bool_or("wakeEnabled", false);
    settings.wake_sensitivity = store
        .get("wakeSensitivity")
        .and_then(|v| v.as_f64())
        .map(|v| (v as f32).clamp(0.0, 1.0))
        .unwrap_or(DEFAULT_WAKE_SENSITIVITY);
    settings.wake_pause_in_games = bool_or("wakePauseInGames", true);
    settings.auto_update = bool_or("autoUpdate", true);
    settings.update_channel = crate::updater::normalize_channel(&str_or("updateChannel", crate::updater::CHANNEL_STABLE));
    if let Some(v) = store.get("appearance") {
        if v.is_object() {
            settings.appearance = v;
        }
    }

    if settings.ask_hotkey.trim().is_empty() {
        settings.ask_hotkey = DEFAULT_ASK_HOTKEY.to_string();
    }
    if settings.agent_hotkey.trim().is_empty() {
        settings.agent_hotkey = DEFAULT_AGENT_HOTKEY.to_string();
    }

    // Migrate AltGr-colliding defaults (Ctrl+Alt+…) to safe combos.
    let migrated = migrate_altgr_hotkeys(&mut settings);
    if migrated {
        // Persist so the next launch doesn't keep the old AltGr-colliding combos.
        let _ = save_settings(app, &settings);
    }

    Ok(settings)
}

/// Ctrl+Alt is AltGr on Polish Windows layouts. Old defaults (and any Ctrl+Alt+<letter>
/// combo) are replaced by the safe default for that slot.
fn migrate_altgr_hotkeys(settings: &mut Settings) -> bool {
    let mut changed = false;
    // Old dictation default was Ctrl+Alt+Space (not a letter, but the same AltGr family).
    if normalize_hotkey_label(&settings.hotkey) == "ctrl+alt+space"
        || is_ctrl_alt_letter_hotkey(&settings.hotkey)
    {
        eprintln!(
            "[soul][config] migrating dictation hotkey '{}' -> '{}' (AltGr conflict)",
            settings.hotkey, DEFAULT_HOTKEY
        );
        settings.hotkey = DEFAULT_HOTKEY.to_string();
        changed = true;
    }
    if is_ctrl_alt_letter_hotkey(&settings.agent_hotkey) {
        eprintln!(
            "[soul][config] migrating commands hotkey '{}' -> '{}' (AltGr conflict)",
            settings.agent_hotkey, DEFAULT_AGENT_HOTKEY
        );
        settings.agent_hotkey = DEFAULT_AGENT_HOTKEY.to_string();
        changed = true;
    }
    if is_ctrl_alt_letter_hotkey(&settings.ask_hotkey) {
        eprintln!(
            "[soul][config] migrating ask hotkey '{}' -> '{}' (AltGr conflict)",
            settings.ask_hotkey, DEFAULT_ASK_HOTKEY
        );
        settings.ask_hotkey = DEFAULT_ASK_HOTKEY.to_string();
        changed = true;
    }
    changed
}

fn normalize_hotkey_label(s: &str) -> String {
    s.replace(' ', "")
        .to_ascii_lowercase()
        .replace("control", "ctrl")
        .replace("command", "super")
        .replace("cmd", "super")
        .replace("win", "super")
        .replace("meta", "super")
}

/// True when the combo is Ctrl+Alt(+Shift)+<letter/digit> without Win: on Polish layouts
/// Right Alt (AltGr) is reported as Ctrl+Alt, so this would fire while typing ć, ą, ę, Ą…
pub fn is_ctrl_alt_letter_hotkey(hotkey: &str) -> bool {
    let n = normalize_hotkey_label(hotkey);
    let parts: Vec<&str> = n.split('+').filter(|p| !p.is_empty()).collect();
    let has = |names: &[&str]| parts.iter().any(|p| names.contains(p));
    let has_key = parts.iter().any(|p| {
        p.len() == 1 && p.chars().all(|c| c.is_ascii_alphanumeric())
    });
    has(&["ctrl"]) && has(&["alt", "option"]) && !has(&["super"]) && has_key
}

/// API key: OS credential store (`secrets.rs`). A plaintext `apiKey` left in the settings file
/// (older versions, or a session without a credential store) is newer by construction — we only
/// write it there when the store failed — so it is moved into the store and deleted from the file.
fn load_api_key<R: Runtime>(store: &tauri_plugin_store::Store<R>) -> String {
    let file_key = store
        .get("apiKey")
        .and_then(|v| v.as_str().map(|s| s.trim().to_string()))
        .unwrap_or_default();
    match crate::secrets::load() {
        crate::secrets::Lookup::Pending | crate::secrets::Lookup::Unavailable(_) => file_key,
        found if file_key.is_empty() => match found {
            crate::secrets::Lookup::Found(k) => k,
            _ => String::new(),
        },
        _ => match crate::secrets::store(&file_key) {
            Ok(()) => {
                store.delete("apiKey");
                match store.save() {
                    Ok(()) => eprintln!(
                        "[soul][secrets] moved the API key from {STORE_PATH} to {}",
                        crate::secrets::store_name()
                    ),
                    Err(e) => eprintln!("[soul][secrets] key stored, but removing it from {STORE_PATH} failed: {e}"),
                }
                file_key
            }
            Err(e) => {
                eprintln!("[soul][secrets] keeping the API key in {STORE_PATH}: {e}");
                file_key
            }
        },
    }
}

/// Credential store first; the settings file only when there is no usable store.
fn save_api_key<R: Runtime>(store: &tauri_plugin_store::Store<R>, key: &str) {
    match crate::secrets::store(key) {
        Ok(()) => {
            store.delete("apiKey");
        }
        Err(e) => {
            eprintln!("[soul][secrets] credential store unavailable, saving the API key in {STORE_PATH}: {e}");
            store.set("apiKey", serde_json::Value::String(key.trim().to_string()));
        }
    }
}

pub fn save_settings<R: Runtime>(app: &AppHandle<R>, settings: &Settings) -> Result<(), String> {
    let store = app
        .store(STORE_PATH)
        .map_err(|e| format!("Failed to open settings store: {e}"))?;

    save_api_key(&store, &settings.api_key);
    store.set("hotkey", serde_json::Value::String(settings.hotkey.clone()));
    store.set(
        "askHotkey",
        serde_json::Value::String(settings.ask_hotkey.clone()),
    );
    store.set(
        "agentHotkey",
        serde_json::Value::String(settings.agent_hotkey.clone()),
    );
    // Keep legacy `model` in sync with ask_model for older builds.
    let ask = if settings.ask_model.trim().is_empty() {
        settings.model.clone()
    } else {
        settings.ask_model.clone()
    };
    store.set("model", serde_json::Value::String(ask.clone()));
    store.set("askModel", serde_json::Value::String(ask));
    store.set(
        "fallbackModel",
        serde_json::Value::String(settings.fallback_model.clone()),
    );
    store.set(
        "dictationModel",
        serde_json::Value::String(settings.dictation_model.clone()),
    );
    store.set(
        "commandsModel",
        serde_json::Value::String(settings.commands_model.clone()),
    );
    store.set("useLiveApi", serde_json::Value::Bool(settings.use_live_api));
    store.set(
        "language",
        serde_json::Value::String(settings.language.clone()),
    );
    store.set(
        "microphone",
        serde_json::Value::String(settings.microphone.clone()),
    );
    store.set(
        "retiredModels",
        serde_json::Value::Array(
            settings
                .retired_models
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        ),
    );
    store.set(
        "modelNotice",
        match &settings.model_notice {
            Some(n) => serde_json::Value::String(n.clone()),
            None => serde_json::Value::Null,
        },
    );
    store.set(
        "islandEnabled",
        serde_json::Value::Bool(settings.island_enabled),
    );
    store.set(
        "islandPosition",
        serde_json::Value::String(settings.island_position.as_str().into()),
    );
    store.set(
        "islandSections",
        serde_json::to_value(&settings.island_sections).unwrap_or_default(),
    );
    store.set(
        "pinnedApps",
        serde_json::Value::Array(
            settings
                .pinned_apps
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        ),
    );
    store.set("talkMode", serde_json::Value::String(settings.talk_mode.clone()));
    store.set("talkKey", serde_json::Value::String(settings.talk_key.clone()));
    store.set("talkKeySwallow", serde_json::Value::Bool(settings.talk_key_swallow));
    store.set("talkDragAsk", serde_json::Value::Bool(settings.talk_drag_ask));
    store.set("wakeWord", serde_json::Value::String(settings.wake_word.clone()));
    store.set("voiceEnabled", serde_json::Value::Bool(settings.voice_enabled));
    store.set("voiceName", serde_json::Value::String(settings.voice_name.clone()));
    store.set("voiceSpeed", serde_json::Value::String(settings.voice_speed.clone()));
    store.set("voiceBrief", serde_json::Value::Bool(settings.voice_brief));
    store.set("personality", serde_json::Value::String(settings.personality.clone()));
    store.set("voiceVolume", serde_json::Value::from(settings.voice_volume.min(100)));
    store.set("duckMusic", serde_json::Value::Bool(settings.duck_music));
    store.set("searchOpenBrowser", serde_json::Value::Bool(settings.search_open_browser));
    store.set("autoRemember", serde_json::Value::Bool(settings.auto_remember));
    store.set("wakeEnabled", serde_json::Value::Bool(settings.wake_enabled));
    store.set(
        "wakeSensitivity",
        serde_json::Value::from(settings.wake_sensitivity.clamp(0.0, 1.0) as f64),
    );
    store.set("wakePauseInGames", serde_json::Value::Bool(settings.wake_pause_in_games));
    store.set("autoUpdate", serde_json::Value::Bool(settings.auto_update));
    store.set("updateChannel", serde_json::Value::String(settings.update_channel.clone()));
    if settings.appearance.is_object() {
        store.set("appearance", settings.appearance.clone());
    }
    store
        .save()
        .map_err(|e| format!("Failed to save settings: {e}"))?;
    Ok(())
}

/// Returns a redacted view safe for logging (never includes the raw API key).
pub fn redact_settings(settings: &Settings) -> serde_json::Value {
    serde_json::json!({
        "apiKeySet": !settings.api_key.is_empty(),
        "hotkey": settings.hotkey,
        "askHotkey": settings.ask_hotkey,
        "agentHotkey": settings.agent_hotkey,
        "model": settings.effective_ask_model(),
        "askModel": settings.effective_ask_model(),
        "dictationModel": settings.dictation_model,
        "commandsModel": settings.commands_model,
        "useLiveApi": settings.use_live_api,
        "fallbackModel": settings.fallback_model,
        "language": settings.language,
        "microphone": if settings.microphone.is_empty() {
            "system-default"
        } else {
            &settings.microphone
        },
        "retiredModels": settings.retired_models,
        "modelNotice": settings.model_notice,
        "islandEnabled": settings.island_enabled,
        "islandPosition": settings.island_position.as_str(),
        "pinnedApps": settings.pinned_apps,
        "talkMode": settings.talk_mode,
        "talkKey": settings.talk_key,
        "talkKeySwallow": settings.talk_key_swallow,
        "talkDragAsk": settings.talk_drag_ask,
        "wakeWord": settings.wake_word,
        "voiceEnabled": settings.voice_enabled,
        "voiceName": settings.voice_name,
        "voiceSpeed": settings.voice_speed,
        "voiceBrief": settings.voice_brief,
        "personality": settings.personality,
        "voiceVolume": settings.voice_volume,
        "duckMusic": settings.duck_music,
        "searchOpenBrowser": settings.search_open_browser,
        "autoRemember": settings.auto_remember,
        "wakeEnabled": settings.wake_enabled,
        "wakeSensitivity": settings.wake_sensitivity,
        "wakePauseInGames": settings.wake_pause_in_games,
        "autoUpdate": settings.auto_update,
        "updateChannel": settings.update_channel,
        "appearanceSet": settings.appearance.is_object(),
    })
}
