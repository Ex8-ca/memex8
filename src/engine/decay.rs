//! Per-memory-type Weibull temporal decay.
//!
//! Replaces uniform exponential decay with memory-type-specific Weibull
//! distribution parameters (shape k, scale eta). Different memory types
//! decay at different rates:
//!   - k < 1: memories decay slower over time (preferences, profiles)
//!   - k = 1: exponential, uniform behavior (general facts)
//!   - k > 1: memories decay faster over time (events, requests)
//!
//! Parameter table ported from Mnemosyne (mnemosyne/core/weibull.py).

use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// Per-memory-type Weibull parameters (k=shape, eta=scale in hours).
/// Higher eta = slower decay, lower k = more long-term retention.
pub fn weibull_params() -> HashMap<&'static str, (f64, f64)> {
    let mut m = HashMap::new();
    // Long-term stable memories
    m.insert("profile", (0.30, 8760.0)); // ~1 year, very slow
    m.insert("preference", (0.40, 4380.0)); // ~6 months, slow
    m.insert("relationship", (0.35, 8760.0)); // ~1 year, people
    m.insert("learning", (0.70, 1440.0)); // ~2 months
    // Medium-term working knowledge
    m.insert("fact", (0.80, 720.0)); // ~1 month, near-exponential
    m.insert("entity", (0.50, 4380.0)); // ~6 months, slow
    m.insert("setup", (0.60, 2160.0)); // ~3 months
    m.insert("pattern", (0.60, 1680.0)); // ~2.3 months
    m.insert("context", (0.85, 360.0)); // ~15 days
    m.insert("observation", (0.90, 480.0)); // ~20 days
    m.insert("artifact", (0.75, 2160.0)); // ~3 months
    // Decaying / time-sensitive
    m.insert("project", (0.85, 1080.0)); // ~45 days
    m.insert("goal", (0.90, 720.0)); // ~1 month
    m.insert("decision", (1.00, 336.0)); // ~2 weeks
    m.insert("commitment", (1.00, 240.0)); // ~10 days
    // Fast-decaying
    m.insert("event", (1.20, 168.0)); // ~1 week
    m.insert("instruction", (0.90, 480.0)); // ~20 days
    m.insert("error", (1.10, 336.0)); // ~2 weeks
    m.insert("issue", (1.10, 336.0)); // ~2 weeks
    m.insert("request", (1.50, 72.0)); // ~3 days, fastest
    // Default
    m.insert("general", (1.00, 168.0)); // ~1 week
    m
}

/// Compute Weibull-based temporal decay multiplier.
///
/// Returns a value in (0.0, 1.0] — multiplied into a base score to discount
/// older memories. At t=0 (just accessed) returns ~1.0; as t→∞ it approaches 0.
///
/// Args:
///   timestamp: ISO 8601 string (RFC 3339). If parsing fails, returns 1.0
///              (treats unparseable timestamps as "just now" — same as the
///              existing `unwrap_or(0.0)` fallback).
///   query_time: Reference time for scoring. Pass `Utc::now()`.
///   memory_type: Maps to Weibull params; unknown types use "general" default.
pub fn weibull_boost(timestamp: &str, query_time: DateTime<Utc>, memory_type: &str) -> f64 {
    let params = weibull_params();
    let (k, eta_hours) = params
        .get(memory_type)
        .copied()
        .unwrap_or((1.00, 168.0));

    let access_ts = match DateTime::parse_from_rfc3339(timestamp) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return 1.0,
    };

    let elapsed = (query_time - access_ts).num_hours().max(0) as f64;
    let t_hours = elapsed.max(1.0 / 60.0); // floor at 1 minute to avoid div-by-zero

    // Weibull survival function: exp(-(t/eta)^k)
    let exponent = -(t_hours / eta_hours).powf(k);
    exponent.exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(hours_ago: i64) -> String {
        Utc::now()
            .checked_sub_signed(chrono::Duration::hours(hours_ago))
            .unwrap()
            .to_rfc3339()
    }

    #[test]
    fn test_just_accessed_is_near_one() {
        let now = Utc::now();
        let boost = weibull_boost(&t(0), now, "preference");
        // At t=0 we floor to 1 minute → small but non-trivial decay
        assert!(boost > 0.95, "expected >0.95, got {}", boost);
    }

    #[test]
    fn test_preference_decays_slower_than_request() {
        let now = Utc::now();
        // 30 days old
        let pref = weibull_boost(&t(30 * 24), now, "preference");
        let req = weibull_boost(&t(30 * 24), now, "request");
        // Request (k=1.5, eta=72h) decays MUCH faster than preference (k=0.4, eta=4380h)
        assert!(pref > req, "preference {} should exceed request {}", pref, req);
        assert!(pref > 0.5, "30-day-old preference should still be >0.5, got {}", pref);
        assert!(req < 0.05, "30-day-old request should be near-zero, got {}", req);
    }

    #[test]
    fn test_unknown_type_falls_back_to_general() {
        let now = Utc::now();
        let known = weibull_boost(&t(24), now, "general");
        let unknown = weibull_boost(&t(24), now, "not_a_real_type");
        assert!((known - unknown).abs() < 1e-9);
    }

    #[test]
    fn test_unparseable_timestamp_returns_one() {
        let now = Utc::now();
        let boost = weibull_boost("not a real timestamp", now, "preference");
        assert_eq!(boost, 1.0);
    }

    #[test]
    fn test_monotonic_decay() {
        // As t increases, boost should monotonically decrease for any type
        let now = Utc::now();
        let mut prev = 1.0;
        for hours in [1, 24, 168, 720, 4380] {
            let b = weibull_boost(&t(hours), now, "fact");
            assert!(b < prev, "decay not monotonic at {}h: {} >= {}", hours, b, prev);
            prev = b;
        }
    }
}

