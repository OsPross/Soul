//! Voice timers / reminders ("nastaw 10 minut na makaron"): in-memory countdowns shown on the
//! Island (`island://timers`), with a sound, a toast and a short spoken notice when they end.
//! One ticker thread; nothing blocks the UI thread.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const TICK: Duration = Duration::from_millis(250);
const MAX_SECONDS: u64 = 24 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Timer {
    pub id: u64,
    pub label: String,
    /// Unix ms when it fires (the Island counts down locally).
    pub ends_at_ms: i64,
    pub total_ms: i64,
}

static TIMERS: Mutex<Vec<Timer>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static WORKER: OnceLock<()> = OnceLock::new();

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn list() -> Vec<Timer> {
    let mut l = TIMERS.lock().clone();
    l.sort_by_key(|t| t.ends_at_ms);
    l
}

fn broadcast(app: &AppHandle) {
    let _ = app.emit("island://timers", list());
}

/// Start the ticker thread (idempotent).
pub fn start_worker(app: AppHandle) {
    if WORKER.set(()).is_err() {
        return;
    }
    thread::Builder::new()
        .name("soul-timers".into())
        .spawn(move || loop {
            thread::sleep(TICK);
            let now = now_ms();
            let fired: Vec<Timer> = {
                let mut timers = TIMERS.lock();
                let (done, left): (Vec<Timer>, Vec<Timer>) =
                    timers.drain(..).partition(|t| t.ends_at_ms <= now);
                *timers = left;
                done
            };
            for t in fired {
                fire(&app, &t);
            }
        })
        .expect("failed to spawn soul-timers thread");
}

fn fire(app: &AppHandle, t: &Timer) {
    eprintln!("[soul][timer] fired id={} label={:?}", t.id, t.label);
    broadcast(app);
    crate::island::sfx(app, "timer");
    let text = if t.label.is_empty() {
        "⏰ Czas minął!".to_string()
    } else {
        format!("⏰ {} — czas minął!", t.label)
    };
    crate::island::toast(app, "timer", &text, None);
    let spoken = if t.label.is_empty() {
        "Timer się skończył.".to_string()
    } else {
        format!("Timer „{}” się skończył.", t.label)
    };
    crate::agent::speak_notice(app, &spoken);
}

pub fn add(app: &AppHandle, seconds: u64, label: &str) -> Result<Timer, String> {
    if seconds == 0 {
        return Err("Podaj czas timera, np. „10 minut”.".into());
    }
    let seconds = seconds.min(MAX_SECONDS);
    let timer = Timer {
        id: NEXT_ID.fetch_add(1, Ordering::SeqCst),
        label: label.trim().to_string(),
        ends_at_ms: now_ms() + seconds as i64 * 1000,
        total_ms: seconds as i64 * 1000,
    };
    TIMERS.lock().push(timer.clone());
    eprintln!("[soul][timer] add id={} seconds={seconds} label={:?}", timer.id, timer.label);
    broadcast(app);
    Ok(timer)
}

/// Cancel by id, or every timer whose label contains `label` (all when `None` / empty).
pub fn cancel(app: &AppHandle, id: Option<u64>, label: Option<&str>) -> usize {
    let label = label.map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty());
    let removed = {
        let mut timers = TIMERS.lock();
        let before = timers.len();
        timers.retain(|t| match (id, &label) {
            (Some(id), _) => t.id != id,
            (None, Some(l)) => !t.label.to_lowercase().contains(l.as_str()),
            (None, None) => false,
        });
        before - timers.len()
    };
    eprintln!("[soul][timer] cancel removed={removed}");
    broadcast(app);
    removed
}

/// "10 minut", "1 godz. 5 min", "45 s" — for spoken confirmations and the briefing.
pub fn human_duration(ms: i64) -> String {
    let s = (ms.max(0) + 999) / 1000;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{h} godz."));
    }
    if m > 0 {
        parts.push(format!("{m} min"));
    }
    if sec > 0 && h == 0 {
        parts.push(format!("{sec} s"));
    }
    if parts.is_empty() {
        "0 s".into()
    } else {
        parts.join(" ")
    }
}

#[tauri::command]
pub fn timers_list() -> Vec<Timer> {
    list()
}

#[tauri::command]
pub fn timer_cancel(app: AppHandle, id: u64) -> Result<(), String> {
    cancel(&app, Some(id), None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_durations() {
        assert_eq!(human_duration(600_000), "10 min");
        assert_eq!(human_duration(3_900_000), "1 godz. 5 min");
        assert_eq!(human_duration(45_000), "45 s");
        assert_eq!(human_duration(90_000), "1 min 30 s");
    }
}
