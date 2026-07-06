//! Velocity/value metrics over published outcomes.
//!
//! "Value" is a deliberately simple, documented heuristic: a decisive
//! verdict (approve / request_changes) earns its confidence in points, a
//! comment earns 0.3 × confidence, anything else earns nothing. Value per
//! hour divides by pipeline wall-clock actually spent — so a bot that
//! churns out fast low-confidence comments scores visibly worse than one
//! that lands confident verdicts quickly.

use chrono::{TimeZone, Utc};
use sluss_audit::OutcomeRow;

pub struct ValueStats {
    pub decisions: u64,
    pub per_day_7d: f64,
    pub avg_confidence: f64,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub total_value: f64,
    pub value_per_hour: f64,
    /// Bucketed pipeline durations, ready for a bar chart.
    pub latency_buckets: Vec<(String, u64)>,
}

fn decisiveness(verdict: &str) -> f64 {
    match verdict {
        "approve" | "request_changes" => 1.0,
        "comment" => 0.3,
        _ => 0.0,
    }
}

impl ValueStats {
    pub fn compute(outcomes: &[OutcomeRow], now_ms: i64) -> Self {
        let decisions = outcomes.len() as u64;
        let week_ago = now_ms - 7 * 24 * 3_600_000;
        let last_week = outcomes.iter().filter(|o| o.at_unix_ms >= week_ago).count();

        let mut latencies: Vec<i64> = outcomes.iter().map(|o| o.millis.max(0)).collect();
        latencies.sort_unstable();
        let pct = |p: f64| -> i64 {
            if latencies.is_empty() {
                return 0;
            }
            latencies[((latencies.len() - 1) as f64 * p) as usize]
        };

        let total_value: f64 = outcomes
            .iter()
            .map(|o| decisiveness(&o.verdict) * o.confidence)
            .sum();
        let total_hours: f64 = latencies.iter().sum::<i64>() as f64 / 3_600_000.0;
        let avg_confidence = if outcomes.is_empty() {
            0.0
        } else {
            outcomes.iter().map(|o| o.confidence).sum::<f64>() / outcomes.len() as f64
        };

        let mut buckets = [("<5s", 0u64), ("5-15s", 0), ("15-30s", 0), ("30-60s", 0), (">60s", 0)];
        for ms in &latencies {
            let idx = match ms {
                0..=4_999 => 0,
                5_000..=14_999 => 1,
                15_000..=29_999 => 2,
                30_000..=59_999 => 3,
                _ => 4,
            };
            buckets[idx].1 += 1;
        }

        ValueStats {
            decisions,
            per_day_7d: last_week as f64 / 7.0,
            avg_confidence,
            p50_ms: pct(0.5),
            p95_ms: pct(0.95),
            total_value,
            value_per_hour: if total_hours > 0.0 { total_value / total_hours } else { 0.0 },
            latency_buckets: buckets.iter().map(|(l, n)| (l.to_string(), *n)).collect(),
        }
    }
}

pub fn fmt_time(unix_ms: i64) -> String {
    Utc.timestamp_millis_opt(unix_ms)
        .single()
        .map(|t| t.format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| unix_ms.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(verdict: &str, confidence: f64, millis: i64, at: i64) -> OutcomeRow {
        OutcomeRow {
            at_unix_ms: at,
            repo: "a/b".into(),
            number: 1,
            verdict: verdict.into(),
            confidence,
            millis,
        }
    }

    #[test]
    fn value_rewards_decisive_confident_fast() {
        let now = 1_000_000_000;
        let fast_confident = ValueStats::compute(
            &[outcome("approve", 0.9, 10_000, now), outcome("request_changes", 0.9, 10_000, now)],
            now,
        );
        let slow_hedging = ValueStats::compute(
            &[outcome("comment", 0.4, 120_000, now), outcome("comment", 0.4, 120_000, now)],
            now,
        );
        assert!(fast_confident.total_value > slow_hedging.total_value);
        assert!(fast_confident.value_per_hour > slow_hedging.value_per_hour);
    }

    #[test]
    fn empty_is_all_zeroes() {
        let stats = ValueStats::compute(&[], 0);
        assert_eq!(stats.decisions, 0);
        assert_eq!(stats.value_per_hour, 0.0);
        assert_eq!(stats.p95_ms, 0);
    }

    #[test]
    fn buckets_cover_ranges() {
        let now = 0;
        let stats = ValueStats::compute(
            &[outcome("approve", 1.0, 3_000, now), outcome("approve", 1.0, 90_000, now)],
            now,
        );
        assert_eq!(stats.latency_buckets[0].1, 1);
        assert_eq!(stats.latency_buckets[4].1, 1);
    }
}
