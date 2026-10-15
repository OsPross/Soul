//! Gemini Live API (BidiGenerateContent over WebSocket) — warm, self-healing sessions.
//!
//! One actor task per feature (dictation, commands) runs on Tauri's tokio runtime and owns its
//! socket. Callers (hotkey handlers, worker threads) talk to it through a non-blocking channel,
//! so nothing here ever blocks the main thread.
//!
//! Push-to-talk maps onto manual VAD: press → `activityStart`, release → `activityEnd`.
//! (`audioStreamEnd` is only valid with server VAD, which would also split a turn whenever the
//! speaker pauses.) Audio pushed while the socket is still (re)connecting is buffered and flushed
//! right after `setupComplete`, so a cold start costs latency, never words.
//!
//! The API key travels in the `x-goog-api-key` header — never in the URL — and every error that
//! leaves this module goes through `redact_secrets`.

use crate::ai::gemini::{normalize_model_id, redact_secrets, FunctionCall};
use crate::ai::quota;
use crate::config::Settings;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const WS_ENDPOINT: &str =
    "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(15);
/// Stop keeping a session warm after this long without a turn; the next press reconnects.
const IDLE_COLD_AFTER: Duration = Duration::from_secs(30 * 60);
/// Cap on audio buffered while connecting (2 min of 16 kHz s16le mono).
const MAX_PENDING_AUDIO_BYTES: usize = 16_000 * 2 * 120;
/// Consecutive warm-connect failures before we stop retrying until the next press/configure.
const MAX_WARM_FAILURES: u32 = 6;
const BACKOFF_MS: [u64; 6] = [500, 1_000, 2_000, 4_000, 8_000, 30_000];

/// Dictation: give the server this long after release before falling back to interim/REST.
const DICTATION_HARD_TIMEOUT: Duration = Duration::from_millis(3_000);
/// Dictation: quiet gap after a post-release final segment that ends the turn.
const DICTATION_QUIET_GAP: Duration = Duration::from_millis(200);
/// Dictation: when every segment was finalized before release, wait this long for stragglers.
const DICTATION_SETTLED_WAIT: Duration = Duration::from_millis(600);

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveKind {
    Dictation,
    Commands,
}

#[derive(Clone, PartialEq)]
pub struct LiveSetup {
    pub api_key: String,
    pub model: String,
    pub kind: LiveKind,
    pub language_codes: Vec<String>,
    pub system_instruction: Option<String>,
    pub tools: Option<Value>,
    /// Prebuilt voice for native-audio replies (commands only).
    pub voice: Option<String>,
    /// `speechConfig.languageCode` for spoken replies (commands only), e.g. `pl-PL`.
    pub speech_language: Option<String>,
}

/// Never print the API key (rule 2) — Debug shows only whether it is set.
impl std::fmt::Debug for LiveSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSetup")
            .field("api_key_set", &!self.api_key.is_empty())
            .field("model", &self.model)
            .field("kind", &self.kind)
            .field("language_codes", &self.language_codes)
            .field("voice", &self.voice)
            .field("speech_language", &self.speech_language)
            .finish_non_exhaustive()
    }
}

impl LiveSetup {
    pub fn dictation(api_key: &str, model: &str, language: &str) -> Self {
        Self {
            api_key: api_key.trim().to_string(),
            model: normalize_model_id(model),
            kind: LiveKind::Dictation,
            language_codes: language_codes(language),
            system_instruction: None,
            tools: None,
            voice: None,
            speech_language: None,
        }
    }

    pub fn commands(
        api_key: &str,
        model: &str,
        system: &str,
        tools: Option<Value>,
        voice: &str,
    ) -> Self {
        let voice = voice.trim();
        Self {
            api_key: api_key.trim().to_string(),
            model: normalize_model_id(model),
            kind: LiveKind::Commands,
            language_codes: Vec::new(),
            system_instruction: Some(system.to_string()),
            tools,
            voice: (!voice.is_empty()).then(|| voice.to_string()),
            speech_language: None,
        }
    }

    /// System instruction for the dictation (transcription) session, e.g. `POLISH_TRANSCRIPTION_HINT`.
    pub fn with_instruction(mut self, text: &str) -> Self {
        let text = text.trim();
        self.system_instruction = (!text.is_empty()).then(|| text.to_string());
        self
    }

    /// Pin the spoken-reply language (`speechConfig.languageCode`).
    pub fn with_speech_language(mut self, code: &str) -> Self {
        let code = code.trim();
        self.speech_language = (!code.is_empty()).then(|| code.to_string());
        self
    }

    fn setup_message(&self, resume_handle: Option<&str>) -> Value {
        // Manual VAD: the hotkey defines the turn boundaries.
        let realtime = json!({ "automaticActivityDetection": { "disabled": true } });
        let mut setup = match self.kind {
            LiveKind::Dictation => {
                // Dedicated transcription model: TEXT only, SMART mode drops fillers/disfluencies.
                let mut tx = json!({ "mode": "SMART" });
                if !self.language_codes.is_empty() {
                    tx["languageCodes"] = json!(self.language_codes);
                }
                let mut s = json!({
                    "model": format!("models/{}", self.model),
                    "generationConfig": { "responseModalities": ["TEXT"] },
                    "realtimeInputConfig": realtime,
                    "inputAudioTranscription": tx,
                });
                if let Some(sys) = &self.system_instruction {
                    s["systemInstruction"] = json!({ "parts": [{ "text": sys }] });
                }
                s
            }
            LiveKind::Commands => {
                // Native-audio Live models answer in AUDIO (24 kHz PCM): spoken replies are
                // played by `voice::VoicePlayer`; tool calls + output transcription drive the UI.
                let mut s = json!({
                    "model": format!("models/{}", self.model),
                    "generationConfig": {
                        "responseModalities": ["AUDIO"],
                        "temperature": 0.0
                    },
                    "realtimeInputConfig": realtime,
                    "inputAudioTranscription": {},
                    "outputAudioTranscription": {},
                    "contextWindowCompression": { "slidingWindow": {} },
                    "sessionResumption": match resume_handle {
                        Some(h) => json!({ "handle": h }),
                        None => json!({}),
                    },
                });
                if let Some(sys) = &self.system_instruction {
                    s["systemInstruction"] = json!({ "parts": [{ "text": sys }] });
                }
                if let Some(tools) = &self.tools {
                    s["tools"] = tools.clone();
                }
                if let Some(voice) = &self.voice {
                    s["generationConfig"]["speechConfig"] = json!({
                        "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": voice } }
                    });
                }
                if let Some(code) = &self.speech_language {
                    // Spoken replies stay in the configured language (never picked by topic).
                    s["generationConfig"]["speechConfig"]["languageCode"] = json!(code);
                }
                s
            }
        };
        if setup.get("model").is_none() {
            setup["model"] = json!(format!("models/{}", self.model));
        }
        json!({ "setup": setup })
    }
}

/// Transcription bias toward Polish orthography (evaluated with the voice regression harness:
/// `SOUL_TEST_DICTATION_PROMPT=1`). Test-only until the A/B shows it helps.
#[cfg(test)]
pub const POLISH_TRANSCRIPTION_HINT: &str = "Transcribe the user's speech verbatim. The speaker is usually Polish: write Polish words with correct Polish spelling and all diacritics (ą ć ę ł ń ó ś ź ż), never replace them with similar-sounding words. Keep English words and names as spoken. Do not translate, answer or add anything.";

/// BCP-47 hints for transcription. Polish is the default: `auto` biases to Polish + English
/// (both still auto-detected), never forces a translation.
pub fn language_codes(language: &str) -> Vec<String> {
    let l = language.trim().to_ascii_lowercase();
    let code = match l.as_str() {
        "" | "auto" => return vec!["pl-PL".into(), "en-US".into()],
        "pl" => "pl-PL",
        "en" => "en-US",
        "de" => "de-DE",
        "fr" => "fr-FR",
        "es" => "es-ES",
        "it" => "it-IT",
        "uk" => "uk-UA",
        "cs" => "cs-CZ",
        "ru" => "ru-RU",
        other => return vec![other.to_string()],
    };
    vec![code.to_string()]
}

#[derive(Debug, Clone)]
pub struct LiveFunctionCall {
    pub id: Option<String>,
    pub call: FunctionCall,
}

#[derive(Debug, Clone)]
pub enum TurnEvent {
    /// Low-latency partial hypothesis for the segment being spoken.
    Interim(String),
    /// Finalized input-transcription segment.
    Transcript(String),
    /// Model text part.
    Text(String),
    /// Transcription of the model's spoken answer.
    OutputTranscript(String),
    /// Spoken answer audio: 24 kHz s16le mono PCM.
    Audio(Vec<u8>),
    ToolCall(Vec<LiveFunctionCall>),
    ToolCancel(Vec<String>),
    /// `turnComplete` / `generationComplete`; `after_end` is true once `activityEnd` was sent.
    Complete { after_end: bool },
    Error(String),
}

#[derive(Debug, Clone)]
pub struct LiveEvent {
    pub at: Instant,
    pub kind: TurnEvent,
}

enum Cmd {
    Configure(LiveSetup),
    Disable,
    Begin(std_mpsc::Sender<LiveEvent>, u64),
    /// A complete user turn given as text (`clientContent`), e.g. a Smart-mode command.
    BeginText(std_mpsc::Sender<LiveEvent>, u64, String),
    Audio(Vec<u8>),
    End(Instant),
    /// Abort a turn (None = whatever is current).
    Cancel(Option<u64>),
    /// Caller is done with a turn (None = whatever is current).
    Finish(Option<u64>),
    ToolResponse(Vec<Value>),
}

/// Handle to one warm Live session. All methods are non-blocking.
pub struct LiveSession {
    label: &'static str,
    tx: UnboundedSender<Cmd>,
    turn_rx: Mutex<Option<std_mpsc::Receiver<LiveEvent>>>,
    next_turn_id: AtomicU64,
    last_turn_id: AtomicU64,
}

impl LiveSession {
    pub fn spawn(label: &'static str) -> Self {
        let (tx, rx) = unbounded_channel();
        tauri::async_runtime::spawn(Actor::new(label).run(rx));
        Self {
            label,
            tx,
            turn_rx: Mutex::new(None),
            next_turn_id: AtomicU64::new(1),
            last_turn_id: AtomicU64::new(0),
        }
    }

    fn new_turn_id(&self) -> u64 {
        let id = self.next_turn_id.fetch_add(1, Ordering::SeqCst);
        self.last_turn_id.store(id, Ordering::SeqCst);
        id
    }

    /// Id of the most recently begun turn (pair with `finish_turn_id` / `cancel_turn_id`).
    pub fn last_turn_id(&self) -> u64 {
        self.last_turn_id.load(Ordering::SeqCst)
    }

    fn send(&self, cmd: Cmd) {
        if self.tx.send(cmd).is_err() {
            eprintln!("[soul][live] {} actor is gone", self.label);
        }
    }

    /// (Re)configure; connects in the background when the config changed. Cheap no-op otherwise.
    pub fn configure(&self, setup: LiveSetup) {
        self.send(Cmd::Configure(setup));
    }

    pub fn disable(&self) {
        self.send(Cmd::Disable);
    }

    /// Start a push-to-talk turn. Events are collected until `end_turn` hands out the receiver.
    pub fn begin_turn(&self) -> u64 {
        let (tx, rx) = std_mpsc::channel();
        let id = self.new_turn_id();
        *self.turn_rx.lock() = Some(rx);
        self.send(Cmd::Begin(tx, id));
        id
    }

    /// Send a whole user turn as text; the turn is already "released". Returns its events + id.
    pub fn begin_text_turn(&self, text: &str) -> (std_mpsc::Receiver<LiveEvent>, u64) {
        let (tx, rx) = std_mpsc::channel();
        let id = self.new_turn_id();
        self.turn_rx.lock().take();
        self.send(Cmd::BeginText(tx, id, text.to_string()));
        (rx, id)
    }

    /// Queue ~100 ms of 16 kHz s16le mono PCM.
    pub fn push_audio(&self, pcm: Vec<u8>) {
        if !pcm.is_empty() {
            self.send(Cmd::Audio(pcm));
        }
    }

    /// End the turn (activityEnd). Returns the event stream, or None when no turn was begun.
    pub fn end_turn(&self, released: Instant) -> Option<std_mpsc::Receiver<LiveEvent>> {
        let rx = self.turn_rx.lock().take()?;
        self.send(Cmd::End(released));
        Some(rx)
    }

    /// Abort the current turn without waiting for a result.
    pub fn cancel_turn(&self) {
        self.turn_rx.lock().take();
        self.send(Cmd::Cancel(None));
    }

    /// Abort turn `id` only if it is still the actor's turn (barge-in on a spoken reply).
    pub fn cancel_turn_id(&self, id: u64) {
        self.send(Cmd::Cancel(Some(id)));
    }

    /// Caller has its result; the actor may run deferred reconnects (goAway, config change).
    pub fn finish_turn(&self) {
        self.send(Cmd::Finish(None));
    }

    /// Like `finish_turn`, but a no-op if a newer turn has started meanwhile.
    pub fn finish_turn_id(&self, id: u64) {
        self.send(Cmd::Finish(Some(id)));
    }

    /// Reply to a `toolCall` (`functionResponses` entries with id/name/response).
    pub fn send_tool_responses(&self, responses: Vec<Value>) {
        if !responses.is_empty() {
            self.send(Cmd::ToolResponse(responses));
        }
    }
}

/// Shared warm sessions for dictation + commands.
pub struct LivePool {
    pub dictation: LiveSession,
    pub commands: LiveSession,
}

impl LivePool {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            dictation: LiveSession::spawn("dictation"),
            commands: LiveSession::spawn("commands"),
        })
    }

    /// Apply settings: warm both sessions, or shut them down when Live is off / no key.
    pub fn configure_from_settings(&self, settings: &Settings) {
        if !settings.use_live_api || settings.api_key.trim().is_empty() {
            self.dictation.disable();
            self.commands.disable();
            return;
        }
        self.dictation.configure(dictation_setup(settings));
        self.commands.configure(commands_setup(settings));
    }
}

pub fn dictation_setup(settings: &Settings) -> LiveSetup {
    LiveSetup::dictation(&settings.api_key, &settings.dictation_model, &settings.language)
}

pub fn commands_setup(settings: &Settings) -> LiveSetup {
    let tools = json!([{ "functionDeclarations": crate::ai::gemini::agent_tool_declarations() }]);
    let system = crate::ai::gemini::agent_voice_system_prompt(settings);
    LiveSetup::commands(
        &settings.api_key,
        &settings.commands_model,
        &system,
        Some(tools),
        &settings.voice_name,
    )
    .with_speech_language(&crate::ai::gemini::speech_language_code(&settings.language))
}

struct Turn {
    id: u64,
    events: std_mpsc::Sender<LiveEvent>,
    /// Text turn (`clientContent`) instead of streamed audio.
    text: Option<String>,
    /// A toolCall was delivered and not answered yet: a turnComplete must not end the turn,
    /// the model still has to speak after our toolResponse.
    tool_pending: bool,
    opened: bool,
    ended: bool,
    /// All audio of this turn; `audio[..sent_chunks]` is on the wire. Kept so a turn can be
    /// replayed once on a fresh socket when a warm connection turns out to be dead.
    audio: Vec<Vec<u8>>,
    audio_bytes: usize,
    sent_chunks: usize,
    replayed: bool,
    got_output: bool,
    released: Option<Instant>,
    first_token_logged: bool,
    /// Receiver dropped (caller finished early) — auto-answer any further tool calls.
    detached: bool,
}

impl Turn {
    fn emit(&mut self, kind: TurnEvent) {
        self.got_output = true;
        if self.detached {
            return;
        }
        if self
            .events
            .send(LiveEvent {
                at: Instant::now(),
                kind,
            })
            .is_err()
        {
            self.detached = true;
        }
    }
}

struct Actor {
    label: &'static str,
    setup: Option<LiveSetup>,
    ws: Option<Ws>,
    resume_handle: Option<String>,
    turn: Option<Turn>,
    /// Ignore model output until the server closes the interrupted previous turn.
    stale_outputs: bool,
    /// Reconnect once the current turn is over (goAway or config change mid-turn).
    reconnect_after_turn: bool,
    failures: u32,
    retry_at: Option<tokio::time::Instant>,
    last_used: Instant,
}

impl Actor {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            setup: None,
            ws: None,
            resume_handle: None,
            turn: None,
            stale_outputs: false,
            reconnect_after_turn: false,
            failures: 0,
            retry_at: None,
            last_used: Instant::now(),
        }
    }

    async fn run(mut self, mut rx: UnboundedReceiver<Cmd>) {
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let retry_at = self.retry_at;
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(cmd) => self.handle_cmd(cmd).await,
                    None => break,
                },
                msg = next_message(&mut self.ws) => self.handle_ws(msg).await,
                _ = sleep_until_opt(retry_at) => {
                    self.retry_at = None;
                    if self.ws.is_none() && self.should_stay_warm() {
                        self.ensure_connected("reconnect").await;
                    }
                },
                _ = ping.tick() => self.keepalive().await,
            }
        }
        self.close("shutdown").await;
    }

    fn has_active_turn(&self) -> bool {
        self.turn.as_ref().is_some_and(|t| !t.detached)
    }

    fn should_stay_warm(&self) -> bool {
        self.setup.is_some()
            && (self.has_active_turn() || self.last_used.elapsed() < IDLE_COLD_AFTER)
    }

    async fn handle_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Configure(setup) => {
                if self.setup.as_ref() == Some(&setup) {
                    if self.ws.is_none() && self.retry_at.is_none() {
                        self.failures = 0;
                        self.ensure_connected("warm").await;
                    }
                    return;
                }
                let changed_session = self.setup.is_some();
                self.setup = Some(setup);
                self.resume_handle = None;
                self.failures = 0;
                self.last_used = Instant::now();
                if self.has_active_turn() {
                    self.reconnect_after_turn = true;
                    return;
                }
                if changed_session {
                    self.close("config changed").await;
                }
                self.ensure_connected("warm").await;
            }
            Cmd::Disable => {
                self.fail_turn("Live API disabled".into());
                self.setup = None;
                self.retry_at = None;
                self.resume_handle = None;
                self.close("disabled").await;
            }
            Cmd::Begin(events, id) => self.begin(events, id, None).await,
            Cmd::BeginText(events, id, text) => self.begin(events, id, Some(text)).await,
            Cmd::Audio(pcm) => {
                let Some(turn) = self.turn.as_mut() else {
                    return;
                };
                if turn.ended || turn.audio_bytes + pcm.len() > MAX_PENDING_AUDIO_BYTES {
                    return;
                }
                turn.audio_bytes += pcm.len();
                turn.audio.push(pcm.clone());
                if turn.opened && self.ws.is_some() && self.send_audio(pcm).await {
                    if let Some(t) = self.turn.as_mut() {
                        t.sent_chunks += 1;
                    }
                }
            }
            Cmd::End(released) => {
                let Some(turn) = self.turn.as_mut() else {
                    return;
                };
                turn.ended = true;
                turn.released = Some(released);
                if turn.opened && self.ws.is_some() {
                    let audio_ms = turn.audio_bytes / 32;
                    self.send_json(json!({ "realtimeInput": { "activityEnd": {} } }))
                        .await;
                    soul_live_log(self.label, &format!("activity_end audio_ms={audio_ms}"));
                }
                // Not opened yet: open_turn() flushes the buffer and ends it after setupComplete.
            }
            Cmd::Cancel(id) => {
                if id.is_some() && self.turn.as_ref().map(|t| t.id) != id {
                    return;
                }
                if let Some(turn) = self.turn.take() {
                    if turn.opened && !turn.ended {
                        self.send_json(json!({ "realtimeInput": { "activityEnd": {} } }))
                            .await;
                    }
                    if turn.opened {
                        self.stale_outputs = true;
                    }
                }
                self.after_turn().await;
            }
            Cmd::Finish(id) => {
                if id.is_some() && self.turn.as_ref().map(|t| t.id) != id {
                    return;
                }
                if let Some(turn) = self.turn.as_mut() {
                    turn.detached = true;
                }
                // Keep the Turn so late tool calls are auto-answered; it is replaced on next Begin.
                self.after_turn().await;
            }
            Cmd::ToolResponse(responses) => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.tool_pending = false;
                }
                self.send_json(json!({ "toolResponse": { "functionResponses": responses } }))
                    .await;
            }
        }
    }

    async fn begin(&mut self, events: std_mpsc::Sender<LiveEvent>, id: u64, text: Option<String>) {
        self.last_used = Instant::now();
        if let Some(mut old) = self.turn.take() {
            old.emit(TurnEvent::Error("superseded by a new turn".into()));
            if old.opened && !old.ended {
                self.send_json(json!({ "realtimeInput": { "activityEnd": {} } }))
                    .await;
            }
            if old.ended || old.opened {
                self.stale_outputs = true;
            }
        }
        let is_text = text.is_some();
        self.turn = Some(Turn {
            id,
            events,
            text,
            tool_pending: false,
            opened: false,
            ended: is_text,
            audio: Vec::new(),
            audio_bytes: 0,
            sent_chunks: 0,
            replayed: false,
            got_output: false,
            released: is_text.then(Instant::now),
            first_token_logged: false,
            detached: false,
        });
        if self.ws.is_none() {
            self.failures = 0;
            self.ensure_connected("press").await;
        } else {
            self.open_turn().await;
        }
    }

    async fn after_turn(&mut self) {
        if self.reconnect_after_turn {
            self.reconnect_after_turn = false;
            self.close("deferred reconnect").await;
            self.ensure_connected("deferred reconnect").await;
        }
    }

    /// Connect if needed. Returns true when a socket is ready.
    async fn ensure_connected(&mut self, reason: &str) -> bool {
        if self.ws.is_some() {
            return true;
        }
        let Some(setup) = self.setup.clone() else {
            self.fail_turn("Live API is not configured".into());
            return false;
        };
        if setup.api_key.is_empty() {
            self.fail_turn("No API key set".into());
            return false;
        }
        if quota::is_daily_exhausted(&setup.model) {
            self.fail_turn(format!(
                "Live model '{}' is exhausted for today (resets at midnight Pacific)",
                setup.model
            ));
            return false;
        }

        let t0 = Instant::now();
        let mut result = connect(&setup, self.resume_handle.as_deref()).await;
        if result.is_err() && self.resume_handle.is_some() {
            // Stale/expired resumption handle — start a fresh session.
            self.resume_handle = None;
            result = connect(&setup, None).await;
        }
        match result {
            Ok(ws) => {
                soul_live_log(
                    self.label,
                    &format!(
                        "connect ms={} model={} reason={reason}",
                        t0.elapsed().as_millis(),
                        setup.model
                    ),
                );
                self.ws = Some(ws);
                self.failures = 0;
                self.retry_at = None;
                self.open_turn().await;
                self.ws.is_some()
            }
            Err(e) => {
                self.failures += 1;
                eprintln!(
                    "[soul][live] {} connect failed ms={} model={} attempt={}: {e}",
                    self.label,
                    t0.elapsed().as_millis(),
                    setup.model,
                    self.failures
                );
                if quota::is_daily_quota_error(Some(429), &e) {
                    quota::mark_daily_exhausted(&setup.model);
                }
                self.fail_turn(e);
                self.schedule_retry();
                false
            }
        }
    }

    fn schedule_retry(&mut self) {
        if self.failures >= MAX_WARM_FAILURES || !self.should_stay_warm() {
            self.retry_at = None;
            return;
        }
        let idx = (self.failures as usize).saturating_sub(1).min(BACKOFF_MS.len() - 1);
        self.retry_at =
            Some(tokio::time::Instant::now() + Duration::from_millis(BACKOFF_MS[idx]));
    }

    /// Send activityStart + all not-yet-sent audio (+ activityEnd if already released).
    async fn open_turn(&mut self) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        if turn.opened || turn.detached || self.ws.is_none() {
            return;
        }
        turn.opened = true;
        if let Some(text) = turn.text.clone() {
            let chars = text.chars().count();
            if self
                .send_json(json!({
                    "clientContent": {
                        "turns": [{ "role": "user", "parts": [{ "text": text }] }],
                        "turnComplete": true
                    }
                }))
                .await
            {
                soul_live_log(self.label, &format!("text_turn chars={chars}"));
            }
            return;
        }
        let backlog: Vec<Vec<u8>> = turn.audio[turn.sent_chunks..].to_vec();
        let ended = turn.ended;

        if !self
            .send_json(json!({ "realtimeInput": { "activityStart": {} } }))
            .await
        {
            return;
        }
        let mut flushed = 0usize;
        for chunk in backlog {
            flushed += chunk.len();
            if !self.send_audio(chunk).await {
                return;
            }
            if let Some(t) = self.turn.as_mut() {
                t.sent_chunks += 1;
            }
        }
        if flushed > 0 {
            soul_live_log(self.label, &format!("flushed_buffered_audio_ms={}", flushed / 32));
        }
        if ended {
            self.send_json(json!({ "realtimeInput": { "activityEnd": {} } }))
                .await;
            soul_live_log(self.label, "activity_end (after connect)");
        }
    }

    async fn send_audio(&mut self, pcm: Vec<u8>) -> bool {
        self.send_json(json!({
            "realtimeInput": {
                "audio": {
                    "data": BASE64.encode(&pcm),
                    "mimeType": "audio/pcm;rate=16000"
                }
            }
        }))
        .await
    }

    async fn send_json(&mut self, v: Value) -> bool {
        let Some(ws) = self.ws.as_mut() else {
            return false;
        };
        match ws.send(Message::Text(v.to_string().into())).await {
            Ok(()) => true,
            Err(e) => {
                let msg = format!("Live send failed: {}", redact_secrets(&e.to_string()));
                self.connection_lost(msg).await;
                false
            }
        }
    }

    async fn keepalive(&mut self) {
        if self.ws.is_none() {
            return;
        }
        if !self.has_active_turn() && self.last_used.elapsed() >= IDLE_COLD_AFTER {
            self.close("idle").await;
            return;
        }
        if let Some(ws) = self.ws.as_mut() {
            if let Err(e) = ws.send(Message::Ping(Vec::new().into())).await {
                let msg = format!("Live ping failed: {}", redact_secrets(&e.to_string()));
                self.connection_lost(msg).await;
            }
        }
    }

    async fn close(&mut self, why: &str) {
        if let Some(mut ws) = self.ws.take() {
            // Don't hang on a dead peer.
            let _ = tokio::time::timeout(Duration::from_millis(500), ws.close(None)).await;
            crate::soul_debug!("[soul][live] {} closed ({why})", self.label);
        }
    }

    async fn connection_lost(&mut self, reason: String) {
        if self.ws.take().is_none() {
            return;
        }
        eprintln!("[soul][live] {} connection lost: {reason}", self.label);
        self.failures += 1;
        if self.turn.as_ref().is_some_and(|t| t.detached) {
            self.turn = None;
        }
        // A warm socket that died silently: replay the whole turn once on a fresh connection.
        if let Some(turn) = self.turn.as_mut() {
            if !turn.replayed && !turn.got_output {
                turn.replayed = true;
                turn.opened = false;
                turn.sent_chunks = 0;
                self.stale_outputs = false;
                self.resume_handle = None;
                soul_live_log(self.label, "replaying turn on a fresh connection");
                self.retry_at = Some(tokio::time::Instant::now());
                return;
            }
        }
        self.fail_turn(reason);
        // Server-side close (idle timeout) is routine: reconnect with backoff while warm.
        self.schedule_retry();
    }

    fn fail_turn(&mut self, reason: String) {
        if let Some(mut turn) = self.turn.take() {
            turn.emit(TurnEvent::Error(reason));
        }
        self.stale_outputs = false;
    }

    async fn handle_ws(
        &mut self,
        msg: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    ) {
        let msg = match msg {
            None => {
                self.connection_lost("socket closed".into()).await;
                return;
            }
            Some(Err(e)) => {
                let m = format!("Live read failed: {}", redact_secrets(&e.to_string()));
                self.connection_lost(m).await;
                return;
            }
            Some(Ok(m)) => m,
        };
        let value: Value = match &msg {
            Message::Text(t) => serde_json::from_str(t.as_str()).unwrap_or(Value::Null),
            Message::Binary(b) => serde_json::from_slice(b).unwrap_or(Value::Null),
            Message::Ping(_) => {
                if let Some(ws) = self.ws.as_mut() {
                    let _ = ws.flush().await;
                }
                return;
            }
            Message::Close(frame) => {
                let reason = frame
                    .as_ref()
                    .map(|f| format!("code={} reason={}", u16::from(f.code), f.reason))
                    .unwrap_or_else(|| "no reason".into());
                let reason = redact_secrets(&reason);
                if let Some(setup) = &self.setup {
                    if quota::is_daily_quota_error(Some(429), &reason) {
                        quota::mark_daily_exhausted(&setup.model);
                    }
                }
                self.connection_lost(format!("Live closed by server: {reason}"))
                    .await;
                return;
            }
            _ => return,
        };
        if value.is_null() {
            return;
        }
        self.handle_server_message(value).await;
    }

    async fn handle_server_message(&mut self, v: Value) {
        if let Some(update) = v.get("sessionResumptionUpdate") {
            let resumable = update.get("resumable").and_then(Value::as_bool) == Some(true);
            let handle = update.get("newHandle").and_then(Value::as_str).unwrap_or("");
            if resumable && !handle.is_empty() {
                self.resume_handle = Some(handle.to_string());
            }
        }

        if let Some(go) = v.get("goAway") {
            let left = go.get("timeLeft").and_then(Value::as_str).unwrap_or("?");
            soul_live_log(self.label, &format!("goAway time_left={left}"));
            if self.has_active_turn() {
                self.reconnect_after_turn = true;
            } else {
                self.close("goAway").await;
                self.ensure_connected("goAway").await;
            }
            return;
        }

        if let Some(cancel) = v.get("toolCallCancellation") {
            let ids: Vec<String> = cancel
                .get("ids")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if let Some(turn) = self.turn.as_mut() {
                turn.emit(TurnEvent::ToolCancel(ids));
            }
            return;
        }

        if let Some(tc) = v.get("toolCall") {
            let calls = parse_function_calls(tc);
            self.note_first_token("tool_call");
            let deliver = !self.stale_outputs
                && self.turn.as_ref().is_some_and(|t| !t.detached);
            if deliver {
                if let Some(turn) = self.turn.as_mut() {
                    turn.emit(TurnEvent::ToolCall(calls.clone()));
                    if !turn.detached {
                        turn.tool_pending = true;
                        return;
                    }
                }
            }
            // Nobody will execute these — answer so the session doesn't hang on them.
            let responses: Vec<Value> = calls
                .iter()
                .map(|c| {
                    let mut r = json!({
                        "name": c.call.name,
                        "response": { "result": "Skipped: the command was already handled." }
                    });
                    if let Some(id) = &c.id {
                        r["id"] = json!(id);
                    }
                    r
                })
                .collect();
            self.send_json(json!({ "toolResponse": { "functionResponses": responses } }))
                .await;
            return;
        }

        let Some(sc) = v.get("serverContent") else {
            if let Some(err) = v.get("error") {
                let msg = redact_secrets(&err.to_string());
                self.fail_turn(format!("Live error: {msg}"));
            }
            return;
        };

        let mut events: Vec<TurnEvent> = Vec::new();
        if let Some(t) = sc.pointer("/interimInputTranscription/text").and_then(Value::as_str) {
            events.push(TurnEvent::Interim(t.to_string()));
        }
        if let Some(t) = sc.pointer("/inputTranscription/text").and_then(Value::as_str) {
            events.push(TurnEvent::Transcript(t.to_string()));
        }
        if !self.stale_outputs {
            if let Some(parts) = sc.pointer("/modelTurn/parts").and_then(Value::as_array) {
                for part in parts {
                    if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        events.push(TurnEvent::Text(t.to_string()));
                    }
                    if let Some(pcm) = audio_part(part) {
                        events.push(TurnEvent::Audio(pcm));
                    }
                }
            }
            if let Some(t) = sc.pointer("/outputTranscription/text").and_then(Value::as_str) {
                events.push(TurnEvent::OutputTranscript(t.to_string()));
            }
        }
        let complete = sc.get("turnComplete").and_then(Value::as_bool) == Some(true)
            || sc.get("generationComplete").and_then(Value::as_bool) == Some(true);
        let interrupted = sc.get("interrupted").and_then(Value::as_bool) == Some(true);

        if !events.is_empty() {
            self.note_first_token("content");
        }

        if self.stale_outputs && (complete || interrupted) {
            // This closes the previous (interrupted) turn, not the current one.
            self.stale_outputs = false;
            for e in events {
                if let Some(turn) = self.turn.as_mut() {
                    turn.emit(e);
                }
            }
            return;
        }

        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        for e in events {
            turn.emit(e);
        }
        if complete {
            let after_end = turn.ended;
            turn.emit(TurnEvent::Complete { after_end });
            if after_end
                && !turn.tool_pending
                && sc.get("turnComplete").and_then(Value::as_bool) == Some(true)
            {
                self.turn = None;
                self.after_turn().await;
            }
        }
    }

    fn note_first_token(&mut self, what: &str) {
        let label = self.label;
        if let Some(turn) = self.turn.as_mut() {
            if let (Some(released), false) = (turn.released, turn.first_token_logged) {
                turn.first_token_logged = true;
                soul_live_log(
                    label,
                    &format!("first_token ms={} kind={what}", released.elapsed().as_millis()),
                );
            }
        }
    }
}

fn soul_live_log(label: &str, msg: &str) {
    eprintln!("[soul][live] {label} {msg}");
}

async fn next_message(
    ws: &mut Option<Ws>,
) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
    match ws.as_mut() {
        Some(ws) => ws.next().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until_opt(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn parse_function_calls(tc: &Value) -> Vec<LiveFunctionCall> {
    tc.get("functionCalls")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|fc| {
                    let name = fc.get("name").and_then(Value::as_str)?.to_string();
                    if name.is_empty() {
                        return None;
                    }
                    Some(LiveFunctionCall {
                        id: fc.get("id").and_then(Value::as_str).map(String::from),
                        call: FunctionCall {
                            name,
                            args: fc.get("args").cloned().unwrap_or_else(|| json!({})),
                        },
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn connect(setup: &LiveSetup, resume_handle: Option<&str>) -> Result<Ws, String> {
    match connect_once(setup, resume_handle).await {
        Err(e) if setup.speech_language.is_some() && e.to_ascii_lowercase().contains("language") => {
            // Some native-audio models pick the language themselves and reject languageCode;
            // the system instruction still pins the reply language.
            eprintln!("[soul][live] speechConfig.languageCode rejected — retrying without it");
            let mut plain = setup.clone();
            plain.speech_language = None;
            connect_once(&plain, resume_handle).await
        }
        other => other,
    }
}

async fn connect_once(setup: &LiveSetup, resume_handle: Option<&str>) -> Result<Ws, String> {
    let mut request = WS_ENDPOINT
        .into_client_request()
        .map_err(|e| format!("Live request: {e}"))?;
    let key = HeaderValue::from_str(&setup.api_key)
        .map_err(|_| "API key contains invalid characters".to_string())?;
    request.headers_mut().insert("x-goog-api-key", key);

    let (mut ws, _) = tokio::time::timeout(SETUP_TIMEOUT, tokio_tungstenite::connect_async(request))
        .await
        .map_err(|_| "Live connect timed out".to_string())?
        .map_err(|e| format!("Live connect failed: {}", describe_ws_error(&e)))?;

    ws.send(Message::Text(
        setup.setup_message(resume_handle).to_string().into(),
    ))
    .await
    .map_err(|e| format!("Live setup send failed: {}", redact_secrets(&e.to_string())))?;

    let deadline = tokio::time::Instant::now() + SETUP_TIMEOUT;
    loop {
        let next = tokio::time::timeout_at(deadline, ws.next())
            .await
            .map_err(|_| format!("Live setupComplete timed out ({})", setup.model))?;
        let msg = match next {
            None => return Err("Live closed during setup".into()),
            Some(Err(e)) => {
                return Err(format!(
                    "Live setup read failed: {}",
                    redact_secrets(&e.to_string())
                ))
            }
            Some(Ok(m)) => m,
        };
        let value: Value = match &msg {
            Message::Text(t) => serde_json::from_str(t.as_str()).unwrap_or(Value::Null),
            Message::Binary(b) => serde_json::from_slice(b).unwrap_or(Value::Null),
            Message::Close(frame) => {
                let reason = frame
                    .as_ref()
                    .map(|f| format!("code={} reason={}", u16::from(f.code), f.reason))
                    .unwrap_or_else(|| "no reason".into());
                return Err(format!(
                    "Live setup rejected for '{}': {}",
                    setup.model,
                    redact_secrets(&reason)
                ));
            }
            _ => continue,
        };
        if value.get("setupComplete").is_some() {
            return Ok(ws);
        }
        if let Some(err) = value.get("error") {
            return Err(format!(
                "Live setup error: {}",
                redact_secrets(&err.to_string())
            ));
        }
    }
}

/// Decode an `inlineData` PCM part of a model turn (24 kHz s16le mono).
fn audio_part(part: &Value) -> Option<Vec<u8>> {
    let inline = part.get("inlineData")?;
    let mime = inline.get("mimeType").and_then(Value::as_str)?;
    if !mime.starts_with("audio/pcm") {
        return None;
    }
    let data = inline.get("data").and_then(Value::as_str)?;
    BASE64.decode(data).ok().filter(|pcm| !pcm.is_empty())
}

/// One-shot spoken reply on a fresh connection (Settings voice preview): send `text`, stream
/// the reply audio to `on_audio` until turnComplete. Returns PCM bytes received.
pub async fn speak_once(
    setup: LiveSetup,
    text: String,
    mut on_audio: impl FnMut(Vec<u8>) + Send,
) -> Result<usize, String> {
    let mut ws = connect(&setup, None).await?;
    let turn = json!({
        "clientContent": {
            "turns": [{ "role": "user", "parts": [{ "text": text }] }],
            "turnComplete": true
        }
    });
    ws.send(Message::Text(turn.to_string().into()))
        .await
        .map_err(|e| format!("Live send failed: {}", redact_secrets(&e.to_string())))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut bytes = 0usize;
    loop {
        let next = tokio::time::timeout_at(deadline, ws.next())
            .await
            .map_err(|_| "Voice preview timed out".to_string())?;
        let value: Value = match next {
            None => break,
            Some(Err(e)) => {
                return Err(format!(
                    "Live read failed: {}",
                    redact_secrets(&e.to_string())
                ))
            }
            Some(Ok(Message::Text(t))) => serde_json::from_str(t.as_str()).unwrap_or(Value::Null),
            Some(Ok(Message::Binary(b))) => serde_json::from_slice(&b).unwrap_or(Value::Null),
            Some(Ok(Message::Close(_))) => break,
            Some(Ok(_)) => continue,
        };
        if let Some(err) = value.get("error") {
            return Err(format!("Live error: {}", redact_secrets(&err.to_string())));
        }
        if let Some(parts) = value
            .pointer("/serverContent/modelTurn/parts")
            .and_then(Value::as_array)
        {
            for pcm in parts.iter().filter_map(audio_part) {
                bytes += pcm.len();
                on_audio(pcm);
            }
        }
        if value.pointer("/serverContent/turnComplete").and_then(Value::as_bool) == Some(true) {
            break;
        }
    }
    let _ = tokio::time::timeout(Duration::from_millis(500), ws.close(None)).await;
    Ok(bytes)
}

fn describe_ws_error(e: &tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error;
    match e {
        Error::Http(resp) => {
            let body = resp
                .body()
                .as_ref()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            redact_secrets(&format!("HTTP {} {}", resp.status(), body.trim()))
        }
        other => redact_secrets(&other.to_string()),
    }
}

/// Append a transcription piece. Handles both cumulative snapshots and segment deltas.
pub fn merge_transcript(acc: &mut String, piece: &str) {
    let p = piece.trim();
    if p.is_empty() {
        return;
    }
    let a = acc.trim_end();
    if a.is_empty() {
        *acc = p.to_string();
        return;
    }
    if p.starts_with(a) {
        *acc = p.to_string();
        return;
    }
    if a.ends_with(p) {
        return;
    }
    let mut out = a.to_string();
    let joins_without_space = piece.starts_with(|c: char| !c.is_whitespace())
        && p.starts_with([',', '.', '!', '?', ';', ':', ')']);
    if !joins_without_space {
        out.push(' ');
    }
    out.push_str(p);
    *acc = out;
}

#[derive(Debug, Clone)]
pub struct DictationResult {
    pub text: String,
    pub final_ms: u128,
    /// "final" when built from finalized segments, "interim" when the server never finalized.
    pub source: &'static str,
}

/// Wait for the final transcript of a dictation turn (blocking; call from a worker thread).
pub fn collect_dictation(
    rx: std_mpsc::Receiver<LiveEvent>,
    released: Instant,
) -> Result<DictationResult, String> {
    let hard_deadline = released + DICTATION_HARD_TIMEOUT;
    let mut finals = String::new();
    let mut interim = String::new();
    let mut last_final_after_end: Option<Instant> = None;

    let done = |finals: &str, source: &'static str| {
        Ok(DictationResult {
            text: finals.trim().to_string(),
            final_ms: released.elapsed().as_millis(),
            source,
        })
    };

    loop {
        let now = Instant::now();
        let wait_until = if let Some(t) = last_final_after_end {
            (t + DICTATION_QUIET_GAP).min(hard_deadline)
        } else if !finals.is_empty() && interim.trim().is_empty() {
            (released + DICTATION_SETTLED_WAIT).min(hard_deadline)
        } else {
            hard_deadline
        };
        let wait = wait_until.saturating_duration_since(now);

        match rx.recv_timeout(wait) {
            Ok(ev) => match ev.kind {
                TurnEvent::Interim(t) => interim = t,
                TurnEvent::Transcript(t) => {
                    merge_transcript(&mut finals, &t);
                    interim.clear();
                    if ev.at >= released {
                        last_final_after_end = Some(ev.at);
                    }
                }
                TurnEvent::Complete { after_end: true } => {
                    if !finals.trim().is_empty() {
                        return done(&finals, "final");
                    }
                    if !interim.trim().is_empty() {
                        return done(&interim, "interim");
                    }
                    return Err("Live returned an empty transcript".into());
                }
                TurnEvent::Error(e) => return Err(e),
                _ => {}
            },
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                let quiet_done = last_final_after_end.is_some();
                let settled_done = !finals.is_empty()
                    && interim.trim().is_empty()
                    && now >= released + DICTATION_SETTLED_WAIT;
                if (quiet_done || settled_done) && !finals.trim().is_empty() {
                    return done(&finals, "final");
                }
                if now >= hard_deadline {
                    let mut text = finals.clone();
                    if !interim.trim().is_empty() {
                        merge_transcript(&mut text, &interim);
                    }
                    if !text.trim().is_empty() {
                        return done(&text, "interim");
                    }
                    return Err(format!(
                        "Live transcript timed out after {} ms",
                        DICTATION_HARD_TIMEOUT.as_millis()
                    ));
                }
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                if !finals.trim().is_empty() {
                    return done(&finals, "final");
                }
                return Err("Live session ended without a transcript".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_handles_cumulative_and_segments() {
        let mut a = String::new();
        merge_transcript(&mut a, "Dzień dobry");
        merge_transcript(&mut a, "Dzień dobry, co słychać?");
        assert_eq!(a, "Dzień dobry, co słychać?");
        merge_transcript(&mut a, "Spotkanie w piątek.");
        assert_eq!(a, "Dzień dobry, co słychać? Spotkanie w piątek.");
        merge_transcript(&mut a, "Spotkanie w piątek.");
        assert_eq!(a, "Dzień dobry, co słychać? Spotkanie w piątek.");
    }

    #[test]
    fn commands_setup_pins_polish_reply_language() {
        let settings = Settings {
            api_key: "k".into(),
            language: "auto".into(),
            ..Settings::default()
        };
        let msg = commands_setup(&settings).setup_message(None);
        assert_eq!(
            msg.pointer("/setup/generationConfig/speechConfig/languageCode"),
            Some(&json!("pl-PL"))
        );
        let sys = msg
            .pointer("/setup/systemInstruction/parts/0/text")
            .and_then(Value::as_str)
            .unwrap();
        assert!(sys.contains("Always reply in Polish"), "{sys}");
        assert!(sys.contains("Never switch language based on the topic"), "{sys}");

        let en = Settings {
            language: "en".into(),
            ..settings
        };
        let msg = commands_setup(&en).setup_message(None);
        assert_eq!(
            msg.pointer("/setup/generationConfig/speechConfig/languageCode"),
            Some(&json!("en-US"))
        );
    }

    #[test]
    fn auto_language_biases_polish_first() {
        assert_eq!(language_codes("auto"), vec!["pl-PL", "en-US"]);
        assert_eq!(language_codes("pl"), vec!["pl-PL"]);
    }
}
