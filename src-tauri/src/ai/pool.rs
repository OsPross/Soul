//! Model pool for the computer agent: free-tier RPM limits are small, so every call picks the
//! first model of the pool that is under its client-side limit (sliding 60 s window) and not
//! cooling down after a 429/503. If everything is busy the caller waits for the earliest slot.

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

pub const WINDOW: Duration = Duration::from_secs(60);
pub const COOLDOWN: Duration = Duration::from_secs(60);
/// Longest wait for a free slot before the call is tried anyway.
pub const MAX_WAIT: Duration = Duration::from_secs(8);

/// Rotation order with default requests/minute (free tier).
pub const DEFAULT_POOL: [(&str, u32); 6] = [
    ("gemini-3.1-flash-lite", 15),
    ("gemini-3.5-flash-lite", 15),
    ("gemini-2.5-flash-lite", 10),
    ("gemini-3-flash", 5),
    ("gemini-3.7-flash", 5),
    ("gemini-3.6-flash", 5),
];

#[derive(Default)]
pub struct Pool {
    hits: HashMap<String, VecDeque<Instant>>,
    cooling: HashMap<String, Instant>,
}

impl Pool {
    fn prune(&mut self, model: &str, now: Instant) -> &mut VecDeque<Instant> {
        let q = self.hits.entry(model.to_string()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= WINDOW) {
            q.pop_front();
        }
        q
    }

    /// Time until `model` can take a request (zero = now). `None` = limit 0 (disabled).
    pub fn wait_for(&mut self, model: &str, limit: u32, now: Instant) -> Option<Duration> {
        if limit == 0 {
            return None;
        }
        let cool = self.cooling.get(model).map_or(Duration::ZERO, |t| (*t + COOLDOWN).saturating_duration_since(now));
        let q = self.prune(model, now);
        let rpm = if q.len() < limit as usize {
            Duration::ZERO
        } else {
            // The (len - limit + 1)-th newest... i.e. the hit that must leave the window first.
            (q[q.len() - limit as usize] + WINDOW).saturating_duration_since(now)
        };
        Some(cool.max(rpm))
    }

    /// First model (in order, not in `skip`) that is free now.
    pub fn pick(&mut self, models: &[(String, u32)], skip: &[String], now: Instant) -> Option<usize> {
        (0..models.len()).find(|&i| !skip.contains(&models[i].0) && self.wait_for(&models[i].0, models[i].1, now) == Some(Duration::ZERO))
    }

    /// Earliest slot over the models not in `skip`: (index, wait).
    pub fn earliest(&mut self, models: &[(String, u32)], skip: &[String], now: Instant) -> Option<(usize, Duration)> {
        (0..models.len())
            .filter(|&i| !skip.contains(&models[i].0))
            .filter_map(|i| self.wait_for(&models[i].0, models[i].1, now).map(|w| (i, w)))
            .min_by_key(|(_, w)| *w)
    }

    pub fn record(&mut self, model: &str, now: Instant) {
        self.prune(model, now).push_back(now);
    }

    pub fn cool(&mut self, model: &str, now: Instant) {
        self.cooling.insert(model.to_string(), now);
    }
}

/// What a failed call means for the model.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// Daily quota: out until Pacific midnight.
    Daily,
    /// Not found / retired: never pick it again.
    Retire,
    /// Invalid key: no model will work.
    Fatal,
    /// 429 / 503 / 400 / network: 60 s cooldown, try the next model.
    Cool,
}

pub fn classify(status: Option<u16>, message: &str) -> Failure {
    if crate::ai::quota::is_daily_quota_error(status, message) {
        Failure::Daily
    } else if status == Some(404) {
        Failure::Retire
    } else if matches!(status, Some(401 | 403)) || message.contains("API key") {
        Failure::Fatal
    } else {
        Failure::Cool
    }
}

/// Models retired in this process (the settings ledger follows, but this does not wait for it).
fn retired() -> &'static Mutex<Vec<String>> {
    static R: std::sync::OnceLock<Mutex<Vec<String>>> = std::sync::OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

/// True the first time `model` is retired.
pub fn retire(model: &str) -> bool {
    let id = crate::ai::gemini::normalize_model_id(model);
    let mut r = retired().lock();
    if r.contains(&id) {
        return false;
    }
    r.push(id);
    true
}

static CALL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A new model call: older ones (a timed-out call still looping over models) are stale and send
/// nothing more — one in-flight call per step.
pub fn begin_call() -> u64 {
    CALL_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
}

pub fn is_current(call: u64) -> bool {
    CALL_SEQ.load(std::sync::atomic::Ordering::SeqCst) == call
}

pub fn global() -> &'static Mutex<Pool> {
    static P: std::sync::OnceLock<Mutex<Pool>> = std::sync::OnceLock::new();
    P.get_or_init(|| Mutex::new(Pool::default()))
}

/// The pool for this run: default order, only models the account lists (an empty list =
/// unknown, keep all), without daily-exhausted ones. `first` (the user's chosen model) goes to
/// the front. `rpm` overrides the default limits (by pool name or resolved id).
pub fn models(available: &[String], first: &str, rpm: &HashMap<String, u32>, retired_ledger: &[String]) -> Vec<(String, u32)> {
    let session_retired = retired().lock().clone();
    let first = crate::ai::gemini::normalize_model_id(first);
    let lead = (!first.is_empty()).then(|| {
        let default = DEFAULT_POOL.iter().find(|(m, _)| *m == first).map_or(5, |(_, l)| *l);
        (first.as_str(), default)
    });
    let mut out: Vec<(String, u32)> = Vec::new();
    for (name, default) in lead.into_iter().chain(DEFAULT_POOL.iter().copied()) {
        let Some(id) = resolve(name, available) else { continue };
        let gone = crate::ai::gemini::is_retired_id(&id, retired_ledger) || crate::ai::gemini::is_retired_id(&id, &session_retired);
        if gone || out.iter().any(|(m, _)| *m == id) || crate::ai::quota::is_daily_exhausted(&id) {
            continue;
        }
        let limit = rpm.get(name).or_else(|| rpm.get(&id)).copied().unwrap_or(default);
        out.push((id, limit));
    }
    out
}

/// A pool name matches the exact id or its `-preview` / `-001` style variant.
fn resolve(name: &str, available: &[String]) -> Option<String> {
    if available.is_empty() || available.iter().any(|a| a == name) {
        return Some(name.to_string());
    }
    let lite = name.contains("lite");
    available
        .iter()
        .filter(|a| a.strip_prefix(name).is_some_and(|rest| rest.starts_with('-')))
        .filter(|a| lite || !a.contains("lite"))
        .filter(|a| !["image", "tts", "live", "audio", "embedding", "thinking"].iter().any(|bad| a.contains(bad)))
        .min_by_key(|a| a.len())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool3() -> Vec<(String, u32)> {
        vec![("a".into(), 2), ("b".into(), 1), ("c".into(), 1)]
    }

    #[test]
    fn rotates_when_a_model_is_at_its_limit() {
        let (mut p, m, t0) = (Pool::default(), pool3(), Instant::now());
        let mut order = Vec::new();
        for _ in 0..4 {
            let i = p.pick(&m, &[], t0).expect("a free model");
            p.record(&m[i].0, t0);
            order.push(m[i].0.clone());
        }
        assert_eq!(order, ["a", "a", "b", "c"]);
        assert_eq!(p.pick(&m, &[], t0), None);
        // The window slides: after 60 s `a` is free again.
        assert_eq!(p.pick(&m, &[], t0 + WINDOW), Some(0));
    }

    #[test]
    fn cooldown_is_60_s_and_skips_to_the_next_model() {
        let (mut p, m, t0) = (Pool::default(), pool3(), Instant::now());
        p.cool("a", t0);
        assert_eq!(p.pick(&m, &[], t0 + Duration::from_secs(1)), Some(1));
        assert_eq!(p.wait_for("a", 2, t0 + Duration::from_secs(20)), Some(Duration::from_secs(40)));
        assert_eq!(p.pick(&m, &[], t0 + COOLDOWN), Some(0));
    }

    #[test]
    fn earliest_slot_when_everything_is_busy_and_skip_list() {
        let (mut p, m, t0) = (Pool::default(), pool3(), Instant::now());
        p.record("a", t0);
        p.record("a", t0 + Duration::from_secs(5));
        p.record("b", t0 + Duration::from_secs(10));
        p.cool("c", t0 + Duration::from_secs(20));
        let now = t0 + Duration::from_secs(30);
        assert_eq!(p.pick(&m, &[], now), None);
        // a frees at t0+60 (30 s), b at t0+70 (40 s), c at t0+80 (50 s).
        assert_eq!(p.earliest(&m, &[], now), Some((0, Duration::from_secs(30))));
        assert_eq!(p.earliest(&m, &["a".into()], now), Some((1, Duration::from_secs(40))));
        // A limit of 0 disables a model.
        assert_eq!(p.wait_for("a", 0, now), None);
    }

    #[test]
    fn pool_resolves_ids_from_the_models_list() {
        let avail: Vec<String> = [
            "gemini-3.1-flash-lite",
            "gemini-3-flash-preview",
            "gemini-3-flash-lite-preview",
            "gemini-2.5-flash-lite",
            "gemini-3.7-flash-image",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let got: Vec<String> = models(&avail, "", &HashMap::new(), &[]).into_iter().map(|(m, _)| m).collect();
        // 3.5/3.6 flash and the image-only 3.7 are not available; -preview variants resolve.
        assert_eq!(got, ["gemini-3.1-flash-lite", "gemini-2.5-flash-lite", "gemini-3-flash-preview"]);
        // Unknown list (offline): the whole default pool, with configurable limits.
        let rpm = HashMap::from([("gemini-3.1-flash-lite".to_string(), 3)]);
        let all = models(&[], "", &rpm, &[]);
        assert_eq!(all.len(), 6);
        assert_eq!(all[0], ("gemini-3.1-flash-lite".to_string(), 3));
        assert_eq!(all[2].1, 10);
        // The user's chosen model leads.
        assert_eq!(models(&[], "gemini-3.6-flash", &HashMap::new(), &[])[0].0, "gemini-3.6-flash");
    }

    #[test]
    fn retired_models_are_never_picked_again() {
        // From the settings ledger ...
        let ledger = vec!["models/gemini-2.5-flash-lite".to_string()];
        let m = models(&[], "", &HashMap::new(), &ledger);
        assert!(m.iter().all(|(id, _)| id != "gemini-2.5-flash-lite"), "{m:?}");
        // ... and from this process, before the ledger is written.
        assert!(retire("models/gemini-x-retired-test"));
        assert!(!retire("gemini-x-retired-test"));
        let m = models(&[], "gemini-x-retired-test", &HashMap::new(), &[]);
        assert!(m.iter().all(|(id, _)| id != "gemini-x-retired-test"), "{m:?}");
    }

    #[test]
    fn failures_are_classified_and_cooled_for_60_s() {
        assert_eq!(classify(Some(404), "Model not found (404)"), Failure::Retire);
        assert_eq!(classify(Some(429), "quota exceeded for metric per day"), Failure::Daily);
        assert_eq!(classify(Some(429), "Gemini rate limit hit (429)"), Failure::Cool);
        assert_eq!(classify(Some(503), "unavailable"), Failure::Cool);
        assert_eq!(classify(Some(400), "Thinking level MINIMAL is not supported"), Failure::Cool);
        assert_eq!(classify(None, "Network error"), Failure::Cool);
        assert_eq!(classify(Some(403), "Invalid or unauthorized Gemini API key."), Failure::Fatal);
        // A cooled model is skipped for 60 s, whatever the status.
        let (mut p, m, t0) = (Pool::default(), pool3(), Instant::now());
        p.cool("a", t0);
        for s in [1, 30, 59] {
            assert_ne!(p.pick(&m, &[], t0 + Duration::from_secs(s)), Some(0), "{s}s");
        }
        assert_eq!(p.pick(&m, &[], t0 + Duration::from_secs(60)), Some(0));
    }

    #[test]
    fn only_the_newest_call_may_continue() {
        let first = begin_call();
        assert!(is_current(first));
        let second = begin_call();
        assert!(!is_current(first) && is_current(second));
    }
}
