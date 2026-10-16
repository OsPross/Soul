//! Daily count of REST model calls (`generateContent`), shown read-only in Settings so the user
//! can see that nothing in the background spends AI calls. Live (voice) sessions are not counted.
//! Persisted in `<app data>/rest-usage.json`; the count starts at 0 on a new local day.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const FILE: &str = "rest-usage.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Local date, YYYY-MM-DD.
    pub date: String,
    pub count: u32,
}

static STATE: Mutex<Option<(PathBuf, Usage)>> = Mutex::new(None);

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// `prev` after one more call on `date`.
pub fn bump(prev: &Usage, date: &str) -> Usage {
    let count = if prev.date == date { prev.count.saturating_add(1) } else { 1 };
    Usage { date: date.to_string(), count }
}

/// App startup: load the stored count.
pub fn init(dir: PathBuf) {
    let path = dir.join(FILE);
    let usage = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    *STATE.lock() = Some((path, usage));
}

/// One REST model request is about to be sent.
pub fn count_call() {
    let mut st = STATE.lock();
    let Some((path, usage)) = st.as_mut() else { return };
    *usage = bump(usage, &today());
    let _ = std::fs::write(path, serde_json::to_string(usage).unwrap_or_default());
}

/// Today's count (0 on a new day).
pub fn current() -> Usage {
    let date = today();
    match STATE.lock().as_ref() {
        Some((_, u)) if u.date == date => u.clone(),
        _ => Usage { date, count: 0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_per_day() {
        let u = bump(&Usage::default(), "2026-10-16");
        assert_eq!(u, Usage { date: "2026-10-16".into(), count: 1 });
        let u = bump(&u, "2026-10-16");
        assert_eq!(u.count, 2);
        assert_eq!(bump(&u, "2026-10-17"), Usage { date: "2026-10-17".into(), count: 1 }, "a new day starts at 1");
    }
}
