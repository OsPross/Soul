//! X11 input hook for the Smart talk key, via the RECORD extension (passive snooping).
//!
//! RECORD can observe but not swallow events, so on X11: a modifier talk key behaves exactly as
//! on Windows; a mouse side button / F-key still performs its normal action too; and the
//! talk-key + drag gesture also reaches the app under the cursor. `HookCore` still decides
//! talk/tap/chord/drag, the swallow/replay parts of its decisions are simply not applied.

use super::hook::{HookConfig, HookCore, HookSink, TalkKey};
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use std::time::Instant;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::record::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::x11_utils::TryParse;

const RECORD_FROM_SERVER: u8 = 0;
/// X11 core event size inside RECORD data.
const EVENT_LEN: usize = 32;

const XK_SHIFT_L: u32 = 0xffe1;
const XK_SHIFT_R: u32 = 0xffe2;
const XK_CONTROL_L: u32 = 0xffe3;
const XK_CONTROL_R: u32 = 0xffe4;
const XK_CAPS_LOCK: u32 = 0xffe5;
const XK_ALT_L: u32 = 0xffe9;
const XK_SUPER_L: u32 = 0xffeb;
const XK_SUPER_R: u32 = 0xffec;
const XK_SCROLL_LOCK: u32 = 0xff14;
const XK_PAUSE: u32 = 0xff13;
const XK_INSERT: u32 = 0xff63;
const XK_MENU: u32 = 0xff67;
const XK_F1: u32 = 0xffbe;
/// AltGr on most X11 layouts (Polish included).
const XK_ISO_LEVEL3_SHIFT: u32 = 0xfe03;

struct Shared {
    core: HookCore,
    sink: Option<HookSink>,
    /// keycode → first keysyms, filled once the RECORD thread connects.
    keymap: Vec<(u8, Vec<u32>)>,
}

static SHARED: Lazy<Mutex<Shared>> = Lazy::new(|| {
    Mutex::new(Shared {
        core: HookCore::default(),
        sink: None,
        keymap: Vec::new(),
    })
});
static INSTALLED: OnceCell<Result<(), String>> = OnceCell::new();

pub fn install(sink: HookSink) -> Result<(), String> {
    SHARED.lock().sink = Some(sink);
    INSTALLED
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("soul-input-hook".into())
                .spawn(move || {
                    if let Err(e) = record_thread(&tx) {
                        eprintln!("[soul][hook] XRecord stopped: {e}");
                        let _ = tx.send(Err(e));
                    }
                });
            if let Err(e) = spawned {
                return Err(format!("Failed to start input hook thread: {e}"));
            }
            rx.recv()
                .unwrap_or_else(|_| Err("Input hook thread exited".into()))
        })
        .clone()
}

pub fn configure(config: HookConfig) {
    SHARED.lock().core.set_config(config);
}

fn keysym_for(key: TalkKey) -> Option<u32> {
    Some(match key {
        TalkKey::ControlLeft => XK_CONTROL_L,
        TalkKey::ControlRight => XK_CONTROL_R,
        TalkKey::ShiftLeft => XK_SHIFT_L,
        TalkKey::ShiftRight => XK_SHIFT_R,
        TalkKey::AltLeft => XK_ALT_L,
        TalkKey::MetaLeft => XK_SUPER_L,
        TalkKey::MetaRight => XK_SUPER_R,
        TalkKey::CapsLock => XK_CAPS_LOCK,
        TalkKey::ScrollLock => XK_SCROLL_LOCK,
        TalkKey::Pause => XK_PAUSE,
        TalkKey::Insert => XK_INSERT,
        TalkKey::ContextMenu => XK_MENU,
        TalkKey::F(n) => XK_F1 + (n.clamp(1, 24) as u32 - 1),
        TalkKey::MouseMiddle | TalkKey::Mouse4 | TalkKey::Mouse5 => return None,
    })
}

fn button_for(key: TalkKey) -> Option<u8> {
    match key {
        TalkKey::MouseMiddle => Some(2),
        TalkKey::Mouse4 => Some(8),
        TalkKey::Mouse5 => Some(9),
        _ => None,
    }
}

fn keycode_has(keymap: &[(u8, Vec<u32>)], keycode: u8, keysym: u32) -> bool {
    keymap
        .iter()
        .any(|(code, syms)| *code == keycode && syms.contains(&keysym))
}

fn record_thread(ready: &std::sync::mpsc::Sender<Result<(), String>>) -> Result<(), String> {
    let t0 = Instant::now();
    let err = |e: &dyn std::fmt::Display| format!("X11: {e}");
    let (ctrl, _) = x11rb::connect(None).map_err(|e| err(&e))?;
    let (data, _) = x11rb::connect(None).map_err(|e| err(&e))?;
    if ctrl
        .extension_information(record::X11_EXTENSION_NAME)
        .map_err(|e| err(&e))?
        .is_none()
    {
        return Err("The X server has no RECORD extension (needed for the single talk key)".into());
    }

    // Keycode → keysym table (first two levels are enough for modifiers / F-keys).
    let setup = ctrl.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let mapping = ctrl
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(|e| err(&e))?
        .reply()
        .map_err(|e| err(&e))?;
    let per = mapping.keysyms_per_keycode.max(1) as usize;
    let keymap: Vec<(u8, Vec<u32>)> = mapping
        .keysyms
        .chunks(per)
        .enumerate()
        .map(|(i, syms)| (min + i as u8, syms.iter().take(2).copied().collect()))
        .collect();
    SHARED.lock().keymap = keymap;

    let rc = ctrl.generate_id().map_err(|e| err(&e))?;
    let empty = record::Range8 { first: 0, last: 0 };
    let empty_ext = record::ExtRange {
        major: empty,
        minor: record::Range16 { first: 0, last: 0 },
    };
    let range = record::Range {
        core_requests: empty,
        core_replies: empty,
        ext_requests: empty_ext,
        ext_replies: empty_ext,
        delivered_events: empty,
        device_events: record::Range8 {
            first: xproto::KEY_PRESS_EVENT,
            last: xproto::MOTION_NOTIFY_EVENT,
        },
        errors: empty,
        client_started: false,
        client_died: false,
    };
    ctrl.record_create_context(rc, 0, &[record::CS::ALL_CLIENTS.into()], &[range])
        .map_err(|e| err(&e))?
        .check()
        .map_err(|e| err(&e))?;
    let cookie = data.record_enable_context(rc).map_err(|e| err(&e))?;
    eprintln!(
        "[soul][hook] installed XRecord ms={}",
        t0.elapsed().as_millis()
    );
    let _ = ready.send(Ok(()));

    for reply in cookie {
        let reply = reply.map_err(|e| err(&e))?;
        if reply.client_swapped || reply.category != RECORD_FROM_SERVER {
            continue;
        }
        for chunk in reply.data.chunks(EVENT_LEN) {
            if chunk.len() == EVENT_LEN {
                handle_event(chunk);
            }
        }
    }
    Ok(())
}

fn handle_event(raw: &[u8]) {
    let kind = raw[0] & 0x7f;
    // Key/button/motion events share the layout we need (detail, root_x, root_y).
    let Ok((ev, _)) = xproto::KeyPressEvent::try_parse(raw) else {
        return;
    };
    let now = Instant::now();
    let (x, y) = (ev.root_x as i32, ev.root_y as i32);
    let mut shared = SHARED.lock();
    let key = shared.core.config.key;
    let mut emit = None;
    match kind {
        xproto::KEY_PRESS_EVENT | xproto::KEY_RELEASE_EVENT => {
            let down = kind == xproto::KEY_PRESS_EVENT;
            if keycode_has(&shared.keymap, ev.detail, XK_ISO_LEVEL3_SHIFT) {
                shared.core.set_altgr(down);
            }
            let is_talk = keysym_for(key)
                .is_some_and(|sym| keycode_has(&shared.keymap, ev.detail, sym));
            if is_talk {
                if down {
                    shared.core.talk_press(now, &mut emit);
                } else {
                    shared.core.talk_release(now, &mut emit);
                }
            } else if down {
                shared.core.other_key_down(&mut emit);
            }
        }
        xproto::BUTTON_PRESS_EVENT | xproto::BUTTON_RELEASE_EVENT => {
            let down = kind == xproto::BUTTON_PRESS_EVENT;
            match ev.detail {
                // Wheel "buttons" are neither chords nor talk presses.
                4..=7 => {}
                1 => {
                    shared.core.left_button(down, x, y, &mut emit);
                }
                b if button_for(key) == Some(b) => {
                    if down {
                        shared.core.talk_press(now, &mut emit);
                    } else {
                        shared.core.talk_release(now, &mut emit);
                    }
                }
                _ if down => {
                    shared.core.other_key_down(&mut emit);
                }
                _ => {}
            }
        }
        xproto::MOTION_NOTIFY_EVENT => shared.core.mouse_move(x, y, &mut emit),
        _ => {}
    }
    if let (Some(ev), Some(sink)) = (emit, shared.sink.as_ref()) {
        let _ = sink.send(ev);
    }
}
