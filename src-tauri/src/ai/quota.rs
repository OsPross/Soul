//! Daily quota exhaustion tracking (429 "per day") until midnight Pacific.

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::America::Los_Angeles;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;

fn map() -> &'static Mutex<HashMap<String, DateTime<Utc>>> {
    static MAP: OnceLock<Mutex<HashMap<String, DateTime<Utc>>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Next midnight in America/Los_Angeles, as UTC.
pub fn next_pacific_midnight_utc() -> DateTime<Utc> {
    let now_la = Utc::now().with_timezone(&Los_Angeles);
    let tomorrow = now_la.date_naive().succ_opt().unwrap_or(now_la.date_naive());
    let midnight = tomorrow
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| now_la.naive_local());
    Los_Angeles
        .from_local_datetime(&midnight)
        .single()
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| Utc::now() + chrono::Duration::hours(24))
}

pub fn mark_daily_exhausted(model: &str) {
    let id = crate::ai::gemini::normalize_model_id(model);
    if id.is_empty() {
        return;
    }
    let until = next_pacific_midnight_utc();
    eprintln!(
        "[soul][quota] model '{id}' exhausted until Pacific midnight ({until})"
    );
    map().lock().insert(id, until);
}

pub fn is_daily_exhausted(model: &str) -> bool {
    let id = crate::ai::gemini::normalize_model_id(model);
    if id.is_empty() {
        return false;
    }
    let mut guard = map().lock();
    match guard.get(&id).copied() {
        Some(until) if Utc::now() < until => true,
        Some(_) => {
            guard.remove(&id);
            false
        }
        None => false,
    }
}

/// True when the error is a *daily* quota limit ("per day"). Per-minute 429s
/// (RESOURCE_EXHAUSTED without a per-day metric) are transient and must not mark a model
/// exhausted until midnight.
pub fn is_daily_quota_error(status: Option<u16>, message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    let per_day = lower.contains("per day")
        || lower.contains("perday")
        || lower.contains("per_day")
        || lower.contains("daily");
    if status == Some(429) {
        return per_day;
    }
    per_day && (lower.contains("quota") || lower.contains("limit") || lower.contains("exhausted"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_per_day_429_counts() {
        assert!(is_daily_quota_error(
            Some(429),
            "Quota exceeded for metric: generate_content_free_tier_requests, limit: 0, quotaId: GenerateRequestsPerDayPerProjectPerModel-FreeTier"
        ));
        assert!(!is_daily_quota_error(
            Some(429),
            "RESOURCE_EXHAUSTED quotaId: GenerateRequestsPerMinutePerProjectPerModel-FreeTier"
        ));
        assert!(is_daily_quota_error(None, "code=1011 reason=You exceeded your quota per day limit"));
    }
}
