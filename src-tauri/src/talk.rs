//! Smart talk key: one hold-to-talk key for dictation *and* commands.
//!
//! The platform hook (`platform::hook`) reports raw talk-key / chord / drag events on a channel;
//! this worker thread turns them into actions:
//! - press → mic starts immediately (local buffer), but the Live turn is only opened once the
//!   key has been held for `TAP_MS` — taps and chords (Right Ctrl+C) never reach the network;
//! - release after a hold → transcript → `agent::classify` → paste (dictation) or command;
//! - double-tap-and-hold → command mode (audio streams straight to the commands session);
//! - talk key + left-drag → Circle-to-Ask on the dragged region;
//! - hold while a confirmation card is up → the spoken „tak” / „nie” answers it;
//! - any press while Soul is speaking → barge-in.
//!
//! Nothing here runs on the main thread; hook callbacks only do a channel send.

use crate::ai::live::{LivePool, LiveSession, LiveSetup};
use crate::config::{load_settings, Settings};
use crate::platform::hook::{HookConfig, HookEvent, TalkKey, TAP_MS};
use crate::{agent, ask, emit_overlay, report_error, schedule_hide, AppState};
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// Second press within this long after a tap = command mode.
const DOUBLE_TAP_MS: u64 = 350;
/// Drag preview events to the Ask window at most this often.
const DRAG_EMIT_EVERY: Duration = Duration::from_millis(16);
/// Smallest region accepted from the drag gesture (same as the Ask overlay).
const MIN_DRAG_PX: u32 = 8;

static HOOK_TX: OnceCell<Sender<HookEvent>> = OnceCell::new();
/// Hotkey recorder open in Settings: the hook passes everything through, shortcuts are paused.
static SUSPENDED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureTarget {
    Dictation,
    Commands,
}

impl CaptureTarget {
    fn label(self) -> &'static str {
        match self {
            Self::Dictation => "dictation",
            Self::Commands => "commands",
        }
    }
}

enum GateState {
    /// Mic is running, Live turn not opened yet (could still be a tap / chord).
    Buffering(Vec<Vec<u8>>),
    Streaming,
    Closed,
}

/// Routes the mic's PCM chunks: buffered until `arm()`, then streamed into the Live turn.
pub struct LiveGate {
    live: Arc<LivePool>,
    target: CaptureTarget,
    setup: Option<LiveSetup>,
    /// Commands only: conversation `[CONTEXT]` sent ahead of the audio (follow-ups).
    preamble: Option<String>,
    state: Mutex<GateState>,
}

impl LiveGate {
    fn session(&self) -> &LiveSession {
        match self.target {
            CaptureTarget::Dictation => &self.live.dictation,
            CaptureTarget::Commands => &self.live.commands,
        }
    }

    fn push(&self, pcm: Vec<u8>) {
        let mut state = self.state.lock();
        match &mut *state {
            GateState::Buffering(buf) => buf.push(pcm),
            GateState::Streaming => self.session().push_audio(pcm),
            GateState::Closed => {}
        }
    }

    /// Open the Live turn and flush what was buffered. No-op without Live (REST path).
    pub fn arm(&self) {
        let Some(setup) = self.setup.clone() else {
            return;
        };
        let mut state = self.state.lock();
        if let GateState::Buffering(buf) = std::mem::replace(&mut *state, GateState::Streaming) {
            let session = self.session();
            session.configure(setup);
            session.begin_turn_with_context(self.preamble.clone());
            let chunks = buf.len();
            for pcm in buf {
                session.push_audio(pcm);
            }
            crate::soul_debug!(
                "[soul][hook] {} turn opened, flushed_chunks={chunks}",
                self.target.label()
            );
        }
    }

    /// Drop the capture: cancel the Live turn if it was opened.
    pub fn close(&self) {
        let mut state = self.state.lock();
        if matches!(*state, GateState::Streaming) {
            self.session().cancel_turn();
        }
        *state = GateState::Closed;
    }
}

/// Start the mic for `target`. `gated` = keep the Live turn closed until `LiveGate::arm()`.
/// Returns the gate and the mic notice (fallback device) to show once listening.
pub fn begin_capture(
    app: &AppHandle,
    target: CaptureTarget,
    gated: bool,
) -> Result<(Arc<LiveGate>, Option<String>), String> {
    let state = app.state::<AppState>();
    crate::island::set_activity(match target {
        CaptureTarget::Dictation => crate::island::Activity::Dictation,
        CaptureTarget::Commands => crate::island::Activity::Command,
    });
    let settings = load_settings(app).unwrap_or_default();
    let preferred = if settings.microphone.trim().is_empty() {
        None
    } else {
        Some(settings.microphone.clone())
    };
    let model = match target {
        CaptureTarget::Dictation => &settings.dictation_model,
        CaptureTarget::Commands => &settings.commands_model,
    };
    let setup = crate::live_enabled_for(&settings, model).then(|| match target {
        CaptureTarget::Dictation => crate::ai::live::dictation_setup(&settings),
        CaptureTarget::Commands => crate::ai::live::commands_setup(&settings),
    });
    let streaming = setup.is_some();
    let preamble = match target {
        CaptureTarget::Commands => crate::agent::context_block(&settings),
        CaptureTarget::Dictation => None,
    };
    let gate = Arc::new(LiveGate {
        live: Arc::clone(&state.live),
        target,
        setup,
        preamble,
        state: Mutex::new(GateState::Buffering(Vec::new())),
    });
    if let Some(setup) = &gate.setup {
        // Warm (or keep warm) the socket now; the turn itself opens on arm().
        gate.session().configure(setup.clone());
    }
    if !gated {
        gate.arm();
    }
    let sink: Option<crate::audio::PcmSink> = streaming.then(|| {
        let g = Arc::clone(&gate);
        Box::new(move |pcm: Vec<u8>| g.push(pcm)) as crate::audio::PcmSink
    });
    match state.recorder.start_with_pcm_stream(preferred, sink) {
        Ok(started) => {
            eprintln!(
                "[soul][audio] {} using mic: {}{} live={streaming} gated={gated}",
                target.label(),
                started.device_name,
                if started.used_fallback { " (fallback)" } else { "" }
            );
            if !gated {
                let _ = emit_overlay(app, "listening", started.notice.clone());
            }
            Ok((gate, started.notice))
        }
        Err(e) => {
            gate.close();
            Err(e)
        }
    }
}

pub(crate) fn cancel_capture(app: &AppHandle, gate: &LiveGate) {
    gate.close();
    let state = app.state::<AppState>();
    if state.recorder.is_recording() {
        let _ = state.recorder.stop();
    }
}

pub fn is_suspended() -> bool {
    SUSPENDED.load(Ordering::SeqCst)
}

pub fn set_suspended(suspended: bool) {
    SUSPENDED.store(suspended, Ordering::SeqCst);
}

pub fn hook_config(settings: &Settings, enabled: bool) -> HookConfig {
    HookConfig {
        enabled,
        key: TalkKey::parse(&settings.talk_key).unwrap_or(TalkKey::ControlRight),
        swallow_taps: settings.talk_key_swallow,
        drag_ask: settings.talk_drag_ask,
    }
}

/// Install/configure the hook for Smart mode, or turn it off. Ok(true) = Smart key active.
pub fn apply(app: &AppHandle, settings: &Settings) -> Result<bool, String> {
    let platform = Arc::clone(&app.state::<AppState>().platform);
    if !settings.smart_talk() {
        platform.configure_input_hook(hook_config(settings, false));
        return Ok(false);
    }
    let tx = HOOK_TX.get_or_init(|| start_worker(app.clone())).clone();
    platform.install_input_hook(tx)?;
    platform.configure_input_hook(hook_config(settings, !is_suspended()));
    eprintln!(
        "[soul][hook] smart talk key={} swallow_taps={} drag_ask={} suspended={}",
        settings.talk_key,
        settings.talk_key_swallow,
        settings.talk_drag_ask,
        is_suspended()
    );
    Ok(true)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Smart,
    Command,
    /// Answering a pending confirmation (`agent::confirm`) — the card stays up.
    Confirm,
}

struct Held {
    mode: Mode,
    down_at: Instant,
    gate: Arc<LiveGate>,
    armed: bool,
    notice: Option<String>,
}

struct Drag {
    /// Ask capture origin + size (physical px).
    ox: i32,
    oy: i32,
    w: u32,
    h: u32,
    start: (u32, u32),
    last_emit: Instant,
}

impl Drag {
    fn local(&self, x: i32, y: i32) -> (u32, u32) {
        (
            (x - self.ox).clamp(0, self.w as i32) as u32,
            (y - self.oy).clamp(0, self.h as i32) as u32,
        )
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DragPayload {
    phase: &'static str,
    x: u32,
    y: u32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SelectedPayload {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn start_worker(app: AppHandle) -> Sender<HookEvent> {
    let (tx, rx) = mpsc::channel::<HookEvent>();
    thread::Builder::new()
        .name("soul-talk".into())
        .spawn(move || {
            let mut w = Worker {
                app,
                held: None,
                ignore_release: false,
                last_tap_up: None,
                drag: None,
            };
            loop {
                let timeout = match &w.held {
                    Some(h) if !h.armed => (h.down_at + Duration::from_millis(TAP_MS))
                        .saturating_duration_since(Instant::now()),
                    _ => Duration::from_secs(3600),
                };
                match rx.recv_timeout(timeout) {
                    Ok(ev) => w.handle(ev),
                    Err(RecvTimeoutError::Timeout) => w.arm_if_due(),
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("failed to spawn soul-talk thread");
    tx
}

struct Worker {
    app: AppHandle,
    held: Option<Held>,
    /// The press was ignored or cancelled (busy, chord, drag) — its release does nothing.
    ignore_release: bool,
    last_tap_up: Option<Instant>,
    drag: Option<Drag>,
}

impl Worker {
    fn handle(&mut self, ev: HookEvent) {
        if is_suspended() {
            return;
        }
        match ev {
            HookEvent::TalkDown { at } => self.on_down(at),
            HookEvent::TalkUp { at, held_ms } => self.on_up(at, held_ms),
            HookEvent::Chord => {
                if let Some(h) = self.held.take() {
                    self.cancel(h, "chord");
                    self.ignore_release = true;
                }
                self.last_tap_up = None;
            }
            HookEvent::DragStart { x, y } => self.on_drag_start(x, y),
            HookEvent::DragMove { x, y } => {
                if let Some(d) = self.drag.as_mut() {
                    if d.last_emit.elapsed() >= DRAG_EMIT_EVERY {
                        d.last_emit = Instant::now();
                        let (lx, ly) = d.local(x, y);
                        let _ = self.app.emit(
                            "ask://drag",
                            DragPayload {
                                phase: "move",
                                x: lx,
                                y: ly,
                            },
                        );
                    }
                }
            }
            HookEvent::DragEnd { x, y } => self.on_drag_end(x, y),
        }
    }

    fn on_down(&mut self, at: Instant) {
        let t0 = Instant::now();
        agent::barge_in(&self.app);
        // Kill switch: the talk key stops a running computer-agent task (unless the agent is
        // waiting for a spoken answer — then the key records it, below).
        if !agent::confirm::is_pending(&self.app) && agent::computer::killswitch::abort("klawisz mówienia") {
            self.ignore_release = true;
            return;
        }
        if let Some(stale) = self.held.take() {
            // Release got lost (shouldn't happen) — never leave the mic running.
            self.cancel(stale, "stale");
        }
        self.ignore_release = false;
        let state = self.app.state::<AppState>();
        // A pending confirmation outranks `busy` (the command waiting on it holds it).
        if agent::confirm::is_pending(&self.app) && !state.recorder.is_recording() {
            match begin_capture(&self.app, CaptureTarget::Dictation, true) {
                Ok((gate, notice)) => {
                    eprintln!("[soul][hook] press mode=Confirm");
                    self.held = Some(Held {
                        mode: Mode::Confirm,
                        down_at: at,
                        gate,
                        armed: false,
                        notice,
                    });
                    self.arm_if_due();
                }
                Err(e) => {
                    self.ignore_release = true;
                    report_error(&self.app, e);
                }
            }
            return;
        }
        if *state.busy.lock() || ask::is_active(&state.ask) || state.recorder.is_recording() {
            eprintln!("[soul][hook] press ignored (busy / Ask open / recording)");
            self.ignore_release = true;
            return;
        }
        let mode = match self.last_tap_up.take() {
            Some(tap) if at.saturating_duration_since(tap) <= Duration::from_millis(DOUBLE_TAP_MS) => {
                Mode::Command
            }
            _ => Mode::Smart,
        };
        let target = match mode {
            Mode::Smart | Mode::Confirm => CaptureTarget::Dictation,
            Mode::Command => CaptureTarget::Commands,
        };
        match begin_capture(&self.app, target, true) {
            Ok((gate, notice)) => {
                eprintln!(
                    "[soul][hook] press mode={mode:?} mic_start_ms={} hook_to_worker_ms={}",
                    t0.elapsed().as_millis(),
                    t0.saturating_duration_since(at).as_millis()
                );
                self.held = Some(Held {
                    mode,
                    down_at: at,
                    gate,
                    armed: false,
                    notice,
                });
                // The timer may already be due if the mic took long to open.
                self.arm_if_due();
            }
            Err(e) => {
                self.ignore_release = true;
                report_error(&self.app, e);
                schedule_hide(&self.app, 8000);
            }
        }
    }

    fn arm_if_due(&mut self) {
        let Some(h) = self.held.as_mut() else {
            return;
        };
        if h.armed || h.down_at.elapsed() < Duration::from_millis(TAP_MS) {
            return;
        }
        h.armed = true;
        h.gate.arm();
        eprintln!(
            "[soul][hook] armed mode={:?} ms_since_press={}",
            h.mode,
            h.down_at.elapsed().as_millis()
        );
        let msg = match h.mode {
            Mode::Command => Some("Command mode".to_string()),
            Mode::Smart => h.notice.clone(),
            Mode::Confirm => {
                // No "listening" state: the confirm card stays on screen while the answer is spoken.
                agent::confirm::mark_answering();
                return;
            }
        };
        let _ = emit_overlay(&self.app, "listening", msg);
    }

    fn on_up(&mut self, at: Instant, held_ms: u64) {
        if std::mem::take(&mut self.ignore_release) {
            return;
        }
        let Some(h) = self.held.take() else {
            return;
        };
        if !h.armed {
            // Tap: the key keeps its normal behaviour; remember it for double-tap-and-hold.
            self.cancel(h, "tap");
            self.last_tap_up = Some(at);
            return;
        }
        eprintln!("[soul][hook] release mode={:?} held_ms={held_ms}", h.mode);
        match h.mode {
            Mode::Smart => crate::on_smart_released(&self.app),
            Mode::Command => agent::on_agent_released(&self.app),
            Mode::Confirm => agent::confirm::on_answer_released(&self.app),
        }
    }

    fn cancel(&self, h: Held, why: &str) {
        let t0 = Instant::now();
        cancel_capture(&self.app, &h.gate);
        if h.mode == Mode::Confirm {
            agent::confirm::cancel_answer();
        }
        eprintln!(
            "[soul][hook] cancelled why={why} mode={:?} held_ms={} cancel_ms={}",
            h.mode,
            h.down_at.elapsed().as_millis(),
            t0.elapsed().as_millis()
        );
        if h.armed {
            schedule_hide(&self.app, 0);
        }
    }

    fn on_drag_start(&mut self, x: i32, y: i32) {
        let t0 = Instant::now();
        if let Some(h) = self.held.take() {
            self.cancel(h, "drag");
        }
        self.ignore_release = true;
        self.last_tap_up = None;

        let active = ask::is_active(&self.app.state::<AppState>().ask);
        if !active {
            if let Err(e) = ask::start_ask(&self.app) {
                report_error(&self.app, e);
                schedule_hide(&self.app, 5000);
                self.drag = None;
                return;
            }
        }
        let Some(geo) = ask::session_geometry(&self.app).filter(|g| !g.selection_done) else {
            self.drag = None;
            return;
        };
        let mut drag = Drag {
            ox: geo.x,
            oy: geo.y,
            w: geo.width,
            h: geo.height,
            start: (0, 0),
            last_emit: Instant::now(),
        };
        drag.start = drag.local(x, y);
        let _ = self.app.emit(
            "ask://drag",
            DragPayload {
                phase: "start",
                x: drag.start.0,
                y: drag.start.1,
            },
        );
        eprintln!(
            "[soul][hook] drag start ask_ready_ms={} new_session={}",
            t0.elapsed().as_millis(),
            !active
        );
        self.drag = Some(drag);
    }

    fn on_drag_end(&mut self, x: i32, y: i32) {
        let Some(d) = self.drag.take() else {
            return;
        };
        let (ex, ey) = d.local(x, y);
        let rect = SelectedPayload {
            x: d.start.0.min(ex),
            y: d.start.1.min(ey),
            width: d.start.0.abs_diff(ex),
            height: d.start.1.abs_diff(ey),
        };
        if rect.width < MIN_DRAG_PX || rect.height < MIN_DRAG_PX {
            // Too small: leave the overlay in select mode so the user can drag normally.
            let _ = self.app.emit(
                "ask://drag",
                DragPayload {
                    phase: "cancel",
                    x: ex,
                    y: ey,
                },
            );
            return;
        }
        let args = ask::RegionSelectedArgs {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        };
        match ask::region_selected(&self.app, args) {
            Ok(()) => {
                eprintln!(
                    "[soul][hook] drag end region={}x{}+{}+{}",
                    rect.width, rect.height, rect.x, rect.y
                );
                let _ = self.app.emit("ask://selected", rect);
            }
            Err(e) => crate::push_error_log(&self.app, e),
        }
    }
}
