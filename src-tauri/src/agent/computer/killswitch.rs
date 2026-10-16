//! Kill switch for a running computer-agent task. Anything below aborts immediately:
//! - Escape pressed twice within `DOUBLE_ESC` (watched by a 25 ms probe thread),
//! - the mouse jammed into a hard screen corner,
//! - the talk key / command hotkey (talk.rs, `on_agent_pressed`), or a spoken „stop” on it,
//! - the Island Stop button (`agent_stop`).
//!
//! Actions poll [`aborted`] between input chunks, during waits and while the model call runs.

use crate::platform::coords::{in_hard_corner, Monitor};
use crate::platform::Platform;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DOUBLE_ESC: Duration = Duration::from_millis(800);
const PROBE_EVERY: Duration = Duration::from_millis(25);
/// The cursor must sit in the corner this long (a fast pass through doesn't count).
const CORNER_HOLD: Duration = Duration::from_millis(150);
const CORNER_MARGIN: i32 = 3;

static RUNNING: AtomicBool = AtomicBool::new(false);
static ABORTED: AtomicBool = AtomicBool::new(false);
static REASON: Mutex<String> = Mutex::new(String::new());
/// Soul itself is pressing Escape until this instant (ms since `epoch()`): not a kill.
static SELF_ESC_UNTIL: AtomicU64 = AtomicU64::new(0);

fn epoch() -> Instant {
    static E: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *E.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

pub fn aborted() -> bool {
    ABORTED.load(Ordering::SeqCst)
}

pub fn reason() -> String {
    REASON.lock().clone()
}

/// Stop the running task (no-op when idle). Returns whether a task was running.
pub fn abort(why: &str) -> bool {
    if !is_running() {
        return false;
    }
    if !ABORTED.swap(true, Ordering::SeqCst) {
        *REASON.lock() = why.to_string();
        eprintln!("[soul][agent] computer task aborted: {why}");
    }
    true
}

/// Soul presses Escape on purpose (key_combo "esc"): ignore it for `ms`.
pub fn suppress_esc_for(ms: u64) {
    SELF_ESC_UNTIL.store(now_ms() + ms, Ordering::SeqCst);
}

/// Pure detector fed with probe samples (unit-tested).
#[derive(Debug, Default)]
pub struct Detector {
    esc_was_down: bool,
    last_esc: Option<Instant>,
    corner_since: Option<Instant>,
}

impl Detector {
    /// `esc_down` = Escape held now (ignored while `self_esc`), `in_corner` = cursor in a hard
    /// corner. Returns the abort reason, if any.
    pub fn feed(&mut self, now: Instant, esc_down: bool, self_esc: bool, in_corner: bool) -> Option<&'static str> {
        let pressed = esc_down && !self.esc_was_down;
        self.esc_was_down = esc_down;
        if pressed && !self_esc {
            if self.last_esc.is_some_and(|t| now.saturating_duration_since(t) <= DOUBLE_ESC) {
                self.last_esc = None;
                return Some("Esc ×2");
            }
            self.last_esc = Some(now);
        }
        if in_corner {
            let since = *self.corner_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= CORNER_HOLD {
                return Some("mysz w rogu ekranu");
            }
        } else {
            self.corner_since = None;
        }
        None
    }
}

/// Marks a task as running and starts the probe thread; dropping it ends both.
pub struct Guard {
    stop: Arc<AtomicBool>,
}

impl Guard {
    pub fn start(platform: Arc<dyn Platform>) -> Self {
        ABORTED.store(false, Ordering::SeqCst);
        REASON.lock().clear();
        RUNNING.store(true, Ordering::SeqCst);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        let spawned = std::thread::Builder::new().name("soul-killswitch".into()).spawn(move || {
            let monitors: Vec<Monitor> = platform.monitors();
            // Escape already held when the task starts (e.g. the user just closed something) is
            // not a press.
            let mut det = Detector {
                esc_was_down: platform.input_probe().esc_down,
                ..Default::default()
            };
            while !stop_t.load(Ordering::SeqCst) {
                let p = platform.input_probe();
                let corner = p.cursor.is_some_and(|c| in_hard_corner(c, &monitors, CORNER_MARGIN));
                let self_esc = now_ms() < SELF_ESC_UNTIL.load(Ordering::SeqCst);
                if let Some(why) = det.feed(Instant::now(), p.esc_down, self_esc, corner) {
                    abort(why);
                }
                std::thread::sleep(PROBE_EVERY);
            }
        });
        if let Err(e) = spawned {
            eprintln!("[soul][agent] kill switch thread failed: {e}");
        }
        Self { stop }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Sleep `d` in small slices; `false` when the task was aborted meanwhile.
pub fn sleep(d: Duration) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if aborted() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(15).min(end.saturating_duration_since(Instant::now())));
    }
    !aborted()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_escape_aborts_single_does_not() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut d = Detector::default();
        assert_eq!(d.feed(ms(0), true, false, false), None); // first press
        assert_eq!(d.feed(ms(30), true, false, false), None); // still held — not a new press
        assert_eq!(d.feed(ms(100), false, false, false), None);
        assert_eq!(d.feed(ms(400), true, false, false), Some("Esc ×2"));
        // Two presses too far apart.
        let mut d = Detector::default();
        d.feed(ms(0), true, false, false);
        d.feed(ms(50), false, false, false);
        assert_eq!(d.feed(ms(1200), true, false, false), None);
        // Soul's own Escape never counts.
        let mut d = Detector::default();
        d.feed(ms(0), true, true, false);
        d.feed(ms(50), false, true, false);
        assert_eq!(d.feed(ms(100), true, false, false), None);
    }

    #[test]
    fn corner_needs_a_short_hold() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut d = Detector::default();
        assert_eq!(d.feed(ms(0), false, false, true), None);
        assert_eq!(d.feed(ms(100), false, false, true), None);
        assert_eq!(d.feed(ms(160), false, false, true), Some("mysz w rogu ekranu"));
        // Passing through.
        let mut d = Detector::default();
        d.feed(ms(0), false, false, true);
        d.feed(ms(50), false, false, false);
        assert_eq!(d.feed(ms(180), false, false, true), None);
    }
}
