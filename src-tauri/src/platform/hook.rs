//! Portable half of the low-level input hook (Smart talk key).
//!
//! OS backends (`windows.rs` WH_KEYBOARD_LL/WH_MOUSE_LL, `linux_x11.rs` XRecord) translate raw
//! input into calls on [`HookCore`], which decides synchronously whether to swallow the event
//! (and whether a swallowed tap must be replayed) and which [`HookEvent`] to hand to the talk
//! worker. Keeping the decisions here keeps every backend thin and the behaviour testable.
//!
//! Rules:
//! - A modifier talk key (Ctrl/Shift/Alt/Win) is never swallowed: holding it alone is harmless.
//!   A long hold of Alt/Win is "masked" on release so the menu bar / Start menu doesn't open.
//! - A non-modifier key or mouse button is swallowed on press; a tap shorter than [`TAP_MS`] is
//!   replayed so it keeps its normal behaviour (unless `swallow_taps`).
//! - Any other key or mouse button pressed while the talk key is held is a chord (e.g. Right
//!   Ctrl+C): the talk is cancelled and nothing is swallowed.
//! - Talk key + left-drag is the Circle-to-Ask gesture (left button swallowed).
//! - AltGr (Right Alt) can never be the talk key, and a talk press while AltGr is down is ignored.

use std::sync::mpsc::Sender;
use std::time::Instant;

/// Hold shorter than this = tap (keeps the key's normal behaviour, never starts a turn).
pub const TAP_MS: u64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TalkKey {
    ControlLeft,
    ControlRight,
    ShiftLeft,
    ShiftRight,
    AltLeft,
    MetaLeft,
    MetaRight,
    CapsLock,
    ScrollLock,
    Pause,
    Insert,
    ContextMenu,
    /// F1–F24.
    F(u8),
    MouseMiddle,
    /// XButton1 / "back".
    Mouse4,
    /// XButton2 / "forward".
    Mouse5,
}

impl TalkKey {
    /// Parse a stored name (KeyboardEvent.code style: "ControlRight", "F13", "Mouse4").
    pub fn parse(s: &str) -> Result<Self, String> {
        let t = s.trim();
        let lower = t.to_ascii_lowercase().replace([' ', '_', '-'], "");
        Ok(match lower.as_str() {
            "controlleft" | "lctrl" | "leftctrl" | "ctrlleft" => Self::ControlLeft,
            "controlright" | "rctrl" | "rightctrl" | "ctrlright" => Self::ControlRight,
            "shiftleft" | "lshift" | "leftshift" => Self::ShiftLeft,
            "shiftright" | "rshift" | "rightshift" => Self::ShiftRight,
            "altleft" | "lalt" | "leftalt" => Self::AltLeft,
            "altright" | "ralt" | "rightalt" | "altgr" | "altgraph" => {
                return Err(
                    "Right Alt is AltGr on Polish layouts (ą, ć, ę…) and can't be the talk key."
                        .into(),
                )
            }
            "metaleft" | "lwin" | "leftwin" | "superleft" | "osleft" => Self::MetaLeft,
            "metaright" | "rwin" | "rightwin" | "superright" | "osright" => Self::MetaRight,
            "capslock" => Self::CapsLock,
            "scrolllock" => Self::ScrollLock,
            "pause" => Self::Pause,
            "insert" => Self::Insert,
            "contextmenu" | "menu" | "apps" => Self::ContextMenu,
            "mousemiddle" | "mouse3" | "middle" => Self::MouseMiddle,
            "mouse4" | "xbutton1" | "mouseback" | "browserback" => Self::Mouse4,
            "mouse5" | "xbutton2" | "mouseforward" | "browserforward" => Self::Mouse5,
            f if f.starts_with('f') && f.len() <= 3 => {
                let n: u8 = f[1..]
                    .parse()
                    .map_err(|_| format!("Unsupported talk key: {t}"))?;
                if !(1..=24).contains(&n) {
                    return Err(format!("Unsupported talk key: {t}"));
                }
                Self::F(n)
            }
            _ => return Err(format!("Unsupported talk key: {t}")),
        })
    }

    pub fn name(self) -> String {
        match self {
            Self::ControlLeft => "ControlLeft".into(),
            Self::ControlRight => "ControlRight".into(),
            Self::ShiftLeft => "ShiftLeft".into(),
            Self::ShiftRight => "ShiftRight".into(),
            Self::AltLeft => "AltLeft".into(),
            Self::MetaLeft => "MetaLeft".into(),
            Self::MetaRight => "MetaRight".into(),
            Self::CapsLock => "CapsLock".into(),
            Self::ScrollLock => "ScrollLock".into(),
            Self::Pause => "Pause".into(),
            Self::Insert => "Insert".into(),
            Self::ContextMenu => "ContextMenu".into(),
            Self::F(n) => format!("F{n}"),
            Self::MouseMiddle => "MouseMiddle".into(),
            Self::Mouse4 => "Mouse4".into(),
            Self::Mouse5 => "Mouse5".into(),
        }
    }

    /// Modifiers are passed through untouched (holding one alone does nothing).
    pub fn is_modifier(self) -> bool {
        matches!(
            self,
            Self::ControlLeft
                | Self::ControlRight
                | Self::ShiftLeft
                | Self::ShiftRight
                | Self::AltLeft
                | Self::MetaLeft
                | Self::MetaRight
        )
    }

    /// Releasing these alone after a hold triggers UI (menu bar / Start) unless masked.
    pub fn needs_release_mask(self) -> bool {
        matches!(self, Self::AltLeft | Self::MetaLeft | Self::MetaRight)
    }

    pub fn is_mouse(self) -> bool {
        matches!(self, Self::MouseMiddle | Self::Mouse4 | Self::Mouse5)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookConfig {
    /// False = pass everything through (Separate mode, recorder open, hook paused).
    pub enabled: bool,
    pub key: TalkKey,
    pub swallow_taps: bool,
    pub drag_ask: bool,
}

impl Default for HookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            key: TalkKey::ControlRight,
            swallow_taps: false,
            drag_ask: true,
        }
    }
}

/// Sent from the hook thread to the talk worker. Never blocks the hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    TalkDown { at: Instant },
    TalkUp { at: Instant, held_ms: u64 },
    /// Another key/button was pressed while the talk key was held — cancel the talk.
    Chord,
    /// Talk key + left button: screen coordinates (physical pixels).
    DragStart { x: i32, y: i32 },
    DragMove { x: i32, y: i32 },
    DragEnd { x: i32, y: i32 },
}

pub type HookSink = Sender<HookEvent>;

/// What the backend must do with the event it just reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Decision {
    /// Drop the event (don't pass it to the focused app).
    pub swallow: bool,
    /// Inject a press+release of the talk key (a swallowed tap keeps its normal behaviour).
    pub replay_tap: bool,
    /// The real release is swallowed; inject a dummy key, then the release itself, so a long
    /// Alt/Win hold doesn't open the menu bar / Start menu.
    pub mask_release: bool,
}

const PASS: Decision = Decision {
    swallow: false,
    replay_tap: false,
    mask_release: false,
};
const SWALLOW: Decision = Decision {
    swallow: true,
    replay_tap: false,
    mask_release: false,
};

#[derive(Debug, Default)]
pub struct HookCore {
    pub config: HookConfig,
    talk_down: bool,
    down_at: Option<Instant>,
    swallowed_down: bool,
    chorded: bool,
    dragging: bool,
    altgr_down: bool,
}

impl HookCore {
    pub fn set_config(&mut self, config: HookConfig) {
        if config != self.config {
            // A key change mid-hold must not leave a stuck state.
            if config.key != self.config.key || !config.enabled {
                self.talk_down = false;
                self.dragging = false;
            }
            self.config = config;
        }
    }

    pub fn set_altgr(&mut self, down: bool) {
        self.altgr_down = down;
    }

    /// Talk key went down (auto-repeat included).
    pub fn talk_press(&mut self, now: Instant, emit: &mut Option<HookEvent>) -> Decision {
        if !self.config.enabled {
            return PASS;
        }
        if self.talk_down {
            // Auto-repeat: keep swallowing what we swallowed.
            return if self.swallowed_down { SWALLOW } else { PASS };
        }
        if self.altgr_down {
            return PASS;
        }
        self.talk_down = true;
        self.down_at = Some(now);
        self.chorded = false;
        self.swallowed_down = !self.config.key.is_modifier();
        *emit = Some(HookEvent::TalkDown { at: now });
        if self.swallowed_down {
            SWALLOW
        } else {
            PASS
        }
    }

    pub fn talk_release(&mut self, now: Instant, emit: &mut Option<HookEvent>) -> Decision {
        if !self.talk_down {
            return PASS;
        }
        self.talk_down = false;
        let held_ms = self
            .down_at
            .map(|d| now.saturating_duration_since(d).as_millis() as u64)
            .unwrap_or(0);
        *emit = Some(HookEvent::TalkUp { at: now, held_ms });
        let tap = held_ms < TAP_MS;
        if self.swallowed_down {
            return Decision {
                swallow: true,
                replay_tap: tap && !self.chorded && !self.config.swallow_taps,
                mask_release: false,
            };
        }
        // Masking = swallow the real release; the backend injects mask key + release.
        let mask = !tap && !self.chorded && self.config.key.needs_release_mask();
        Decision {
            swallow: mask,
            replay_tap: false,
            mask_release: mask,
        }
    }

    /// Any non-talk key went down (not injected, not the AltGr fake Ctrl).
    pub fn other_key_down(&mut self, emit: &mut Option<HookEvent>) -> Decision {
        if self.config.enabled && self.talk_down && !self.chorded && !self.dragging {
            self.chorded = true;
            *emit = Some(HookEvent::Chord);
        }
        PASS
    }

    /// Left button. Swallowed only while it is part of the drag gesture.
    pub fn left_button(
        &mut self,
        down: bool,
        x: i32,
        y: i32,
        emit: &mut Option<HookEvent>,
    ) -> Decision {
        if !self.config.enabled {
            return PASS;
        }
        if down {
            if self.talk_down && self.config.drag_ask && !self.chorded && !self.dragging {
                self.dragging = true;
                // The talk turn is over; a later talk release must not replay or mask.
                self.chorded = true;
                *emit = Some(HookEvent::DragStart { x, y });
                return SWALLOW;
            }
            return self.other_key_down(emit);
        }
        if self.dragging {
            self.dragging = false;
            *emit = Some(HookEvent::DragEnd { x, y });
            return SWALLOW;
        }
        PASS
    }

    pub fn mouse_move(&mut self, x: i32, y: i32, emit: &mut Option<HookEvent>) {
        if self.dragging {
            *emit = Some(HookEvent::DragMove { x, y });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn core(key: TalkKey) -> HookCore {
        let mut c = HookCore::default();
        c.set_config(HookConfig {
            enabled: true,
            key,
            swallow_taps: false,
            drag_ask: true,
        });
        c
    }

    #[test]
    fn parses_names_and_rejects_altgr() {
        assert_eq!(TalkKey::parse("ControlRight").unwrap(), TalkKey::ControlRight);
        assert_eq!(TalkKey::parse("F13").unwrap(), TalkKey::F(13));
        assert_eq!(TalkKey::parse("mouse4").unwrap(), TalkKey::Mouse4);
        assert!(TalkKey::parse("AltRight").is_err());
        assert!(TalkKey::parse("F25").is_err());
        assert_eq!(TalkKey::parse(&TalkKey::F(7).name()).unwrap(), TalkKey::F(7));
    }

    #[test]
    fn modifier_is_never_swallowed() {
        let mut c = core(TalkKey::ControlRight);
        let t0 = Instant::now();
        let mut ev = None;
        assert_eq!(c.talk_press(t0, &mut ev), PASS);
        assert!(matches!(ev, Some(HookEvent::TalkDown { .. })));
        let mut ev = None;
        let d = c.talk_release(t0 + Duration::from_millis(900), &mut ev);
        assert_eq!(d, PASS);
        assert!(matches!(ev, Some(HookEvent::TalkUp { held_ms: 900, .. })));
    }

    #[test]
    fn swallowed_tap_is_replayed_but_hold_is_not() {
        let mut c = core(TalkKey::Mouse4);
        let t0 = Instant::now();
        let mut ev = None;
        assert!(c.talk_press(t0, &mut ev).swallow);
        let d = c.talk_release(t0 + Duration::from_millis(80), &mut ev);
        assert!(d.swallow && d.replay_tap);

        assert!(c.talk_press(t0, &mut ev).swallow);
        let d = c.talk_release(t0 + Duration::from_millis(600), &mut ev);
        assert!(d.swallow && !d.replay_tap);
    }

    #[test]
    fn swallow_taps_config_disables_replay() {
        let mut c = core(TalkKey::F(13));
        c.set_config(HookConfig {
            swallow_taps: true,
            ..c.config
        });
        let t0 = Instant::now();
        let mut ev = None;
        c.talk_press(t0, &mut ev);
        let d = c.talk_release(t0 + Duration::from_millis(50), &mut ev);
        assert!(d.swallow && !d.replay_tap);
    }

    #[test]
    fn chord_cancels_and_passes_through() {
        let mut c = core(TalkKey::ControlRight);
        let t0 = Instant::now();
        let mut ev = None;
        c.talk_press(t0, &mut ev);
        let mut ev = None;
        assert_eq!(c.other_key_down(&mut ev), PASS);
        assert_eq!(ev, Some(HookEvent::Chord));
        let mut ev = None;
        assert_eq!(c.other_key_down(&mut ev), PASS);
        assert_eq!(ev, None, "chord is reported once");
    }

    #[test]
    fn long_win_hold_is_masked() {
        let mut c = core(TalkKey::MetaLeft);
        let t0 = Instant::now();
        let mut ev = None;
        c.talk_press(t0, &mut ev);
        let d = c.talk_release(t0 + Duration::from_millis(700), &mut ev);
        assert!(d.mask_release && d.swallow);
        c.talk_press(t0, &mut ev);
        let d = c.talk_release(t0 + Duration::from_millis(60), &mut ev);
        assert!(!d.mask_release, "a tap keeps opening Start");
    }

    #[test]
    fn drag_gesture_swallows_left_button() {
        let mut c = core(TalkKey::ControlRight);
        let t0 = Instant::now();
        let mut ev = None;
        c.talk_press(t0, &mut ev);
        let mut ev = None;
        assert!(c.left_button(true, 10, 20, &mut ev).swallow);
        assert_eq!(ev, Some(HookEvent::DragStart { x: 10, y: 20 }));
        let mut ev = None;
        c.mouse_move(50, 60, &mut ev);
        assert_eq!(ev, Some(HookEvent::DragMove { x: 50, y: 60 }));
        // Releasing the talk key mid-drag doesn't end the drag.
        let mut ev = None;
        c.talk_release(t0 + Duration::from_millis(400), &mut ev);
        let mut ev = None;
        assert!(c.left_button(false, 90, 99, &mut ev).swallow);
        assert_eq!(ev, Some(HookEvent::DragEnd { x: 90, y: 99 }));
        // Plain clicks pass.
        let mut ev = None;
        assert_eq!(c.left_button(true, 1, 1, &mut ev), PASS);
        assert_eq!(c.left_button(false, 1, 1, &mut ev), PASS);
    }

    #[test]
    fn altgr_blocks_talk_press_and_disabled_passes() {
        let mut c = core(TalkKey::ControlLeft);
        c.set_altgr(true);
        let mut ev = None;
        assert_eq!(c.talk_press(Instant::now(), &mut ev), PASS);
        assert!(ev.is_none());
        c.set_altgr(false);
        c.set_config(HookConfig {
            enabled: false,
            ..c.config
        });
        assert_eq!(c.talk_press(Instant::now(), &mut ev), PASS);
        assert!(ev.is_none());
    }
}
