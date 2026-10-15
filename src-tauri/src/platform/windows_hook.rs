//! Windows low-level input hook (Smart talk key): WH_KEYBOARD_LL + WH_MOUSE_LL on a dedicated
//! thread. The callbacks only lock a tiny mutex, ask `HookCore` what to do and send on a
//! channel — they must return within LowLevelHooksTimeout or Windows skips (and later drops)
//! the hook.

use super::hook::{Decision, HookConfig, HookCore, HookEvent, HookSink, TalkKey};
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY, VK_RMENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

/// dwExtraInfo tag on events Soul injects (injected events are skipped by the hook anyway).
const SOUL_INJECT_TAG: usize = 0x534F_554C;
/// Unassigned VK used to mask an Alt/Win release (same trick AutoHotkey uses).
const VK_MASK: u16 = 0xE8;
/// The fake Left Ctrl that Windows synthesizes for AltGr carries this scan code.
const ALTGR_FAKE_CTRL_SCAN: u32 = 0x21D;
/// Hook callbacks slower than this get a [soul][hook] warning.
const SLOW_CALLBACK_US: u128 = 5_000;

struct Shared {
    core: HookCore,
    sink: Option<HookSink>,
}

static SHARED: Lazy<Mutex<Shared>> = Lazy::new(|| {
    Mutex::new(Shared {
        core: HookCore::default(),
        sink: None,
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
                .spawn(move || hook_thread(tx));
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

fn hook_thread(ready: std::sync::mpsc::Sender<Result<(), String>>) {
    let t0 = Instant::now();
    let installed = unsafe {
        let hinst = GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or_default();
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), hinst, 0).and_then(|kb| {
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hinst, 0).map(|ms| (kb, ms))
        })
    };
    match installed {
        Ok(_) => {
            eprintln!(
                "[soul][hook] installed WH_KEYBOARD_LL + WH_MOUSE_LL ms={}",
                t0.elapsed().as_millis()
            );
            let _ = ready.send(Ok(()));
        }
        Err(e) => {
            let _ = ready.send(Err(format!("Could not install the keyboard hook: {e}")));
            return;
        }
    }
    // LL hooks are called on this thread, so it must pump messages for the app lifetime.
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, HWND::default(), 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn vk_for(key: TalkKey) -> Option<u16> {
    Some(match key {
        TalkKey::ControlLeft => 0xA2,
        TalkKey::ControlRight => 0xA3,
        TalkKey::ShiftLeft => 0xA0,
        TalkKey::ShiftRight => 0xA1,
        TalkKey::AltLeft => 0xA4,
        TalkKey::MetaLeft => 0x5B,
        TalkKey::MetaRight => 0x5C,
        TalkKey::CapsLock => 0x14,
        TalkKey::ScrollLock => 0x91,
        TalkKey::Pause => 0x13,
        TalkKey::Insert => 0x2D,
        TalkKey::ContextMenu => 0x5D,
        TalkKey::F(n) => 0x70 + (n.clamp(1, 24) as u16 - 1),
        TalkKey::MouseMiddle | TalkKey::Mouse4 | TalkKey::Mouse5 => return None,
    })
}

fn is_extended(key: TalkKey) -> bool {
    matches!(
        key,
        TalkKey::ControlRight
            | TalkKey::MetaLeft
            | TalkKey::MetaRight
            | TalkKey::Insert
            | TalkKey::ContextMenu
    )
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if handle_key(info, wparam.0 as u32) {
            return LRESULT(1);
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        if handle_mouse(info, wparam.0 as u32) {
            return LRESULT(1);
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

fn handle_key(info: &KBDLLHOOKSTRUCT, msg: u32) -> bool {
    if info.flags.0 & LLKHF_INJECTED.0 != 0 {
        return false;
    }
    let t0 = Instant::now();
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
    let mut shared = SHARED.lock();
    if info.vkCode == VK_RMENU.0 as u32 {
        shared.core.set_altgr(down);
    }
    // AltGr's synthetic Left Ctrl is neither a talk press nor a chord.
    if info.scanCode == ALTGR_FAKE_CTRL_SCAN || !shared.core.config.enabled {
        return false;
    }
    let key = shared.core.config.key;
    let mut emit = None;
    let decision = if vk_for(key) == Some(info.vkCode as u16) {
        if down {
            shared.core.talk_press(t0, &mut emit)
        } else if up {
            shared.core.talk_release(t0, &mut emit)
        } else {
            Decision::default()
        }
    } else if down {
        shared.core.other_key_down(&mut emit)
    } else {
        Decision::default()
    };
    finish(shared, key, emit, decision, t0)
}

fn handle_mouse(info: &MSLLHOOKSTRUCT, msg: u32) -> bool {
    if info.flags & LLMHF_INJECTED != 0 {
        return false;
    }
    let t0 = Instant::now();
    let mut shared = SHARED.lock();
    if !shared.core.config.enabled {
        return false;
    }
    let key = shared.core.config.key;
    let (x, y) = (info.pt.x, info.pt.y);
    let xbutton = (info.mouseData >> 16) as u16;
    let is_talk = match msg {
        WM_MBUTTONDOWN | WM_MBUTTONUP => key == TalkKey::MouseMiddle,
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            (key == TalkKey::Mouse4 && xbutton == 1) || (key == TalkKey::Mouse5 && xbutton == 2)
        }
        _ => false,
    };
    let mut emit = None;
    let decision = match msg {
        WM_MOUSEMOVE => {
            shared.core.mouse_move(x, y, &mut emit);
            Decision::default()
        }
        WM_LBUTTONDOWN => shared.core.left_button(true, x, y, &mut emit),
        WM_LBUTTONUP => shared.core.left_button(false, x, y, &mut emit),
        WM_MBUTTONDOWN | WM_XBUTTONDOWN if is_talk => shared.core.talk_press(t0, &mut emit),
        WM_MBUTTONUP | WM_XBUTTONUP if is_talk => shared.core.talk_release(t0, &mut emit),
        WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN => shared.core.other_key_down(&mut emit),
        _ => Decision::default(),
    };
    finish(shared, key, emit, decision, t0)
}

fn finish(
    shared: parking_lot::MutexGuard<'_, Shared>,
    key: TalkKey,
    emit: Option<HookEvent>,
    decision: Decision,
    t0: Instant,
) -> bool {
    if let (Some(ev), Some(sink)) = (emit, shared.sink.as_ref()) {
        let _ = sink.send(ev);
    }
    drop(shared);
    if decision.replay_tap || decision.mask_release {
        // Inject from another thread: never call SendInput while Windows waits on the hook.
        std::thread::spawn(move || inject(key, decision));
    }
    let us = t0.elapsed().as_micros();
    if us > SLOW_CALLBACK_US {
        eprintln!("[soul][hook] slow callback us={us}");
    }
    decision.swallow
}

fn key_input(vk: u16, up: bool, extended: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: SOUL_INJECT_TAG,
            },
        },
    }
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: SOUL_INJECT_TAG,
            },
        },
    }
}

fn inject(key: TalkKey, decision: Decision) {
    let inputs: Vec<INPUT> = if decision.mask_release {
        let Some(vk) = vk_for(key) else { return };
        vec![
            key_input(VK_MASK, false, false),
            key_input(VK_MASK, true, false),
            key_input(vk, true, is_extended(key)),
        ]
    } else {
        match key {
            TalkKey::MouseMiddle => vec![
                mouse_input(MOUSEEVENTF_MIDDLEDOWN, 0),
                mouse_input(MOUSEEVENTF_MIDDLEUP, 0),
            ],
            TalkKey::Mouse4 | TalkKey::Mouse5 => {
                let b = if key == TalkKey::Mouse4 { 1 } else { 2 };
                vec![
                    mouse_input(MOUSEEVENTF_XDOWN, b),
                    mouse_input(MOUSEEVENTF_XUP, b),
                ]
            }
            _ => {
                let Some(vk) = vk_for(key) else { return };
                let ext = is_extended(key);
                vec![key_input(vk, false, ext), key_input(vk, true, ext)]
            }
        }
    };
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    crate::soul_debug!(
        "[soul][hook] injected {} event(s) replay_tap={} mask={}",
        sent,
        decision.replay_tap,
        decision.mask_release
    );
}
