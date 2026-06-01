// Autonomy budget (#524).
//
// Bounds how much an agent may spend on its OWN initiative -- self-accepting
// Pending work and taking free-time -- within a rolling weekly window.
// Operator-requested work is never budget-bound. Unbounded autonomous spawning
// already hit subscription-quota exhaustion once (#484); this is the governor
// that keeps the board-driven loop engineering, not a quota bomb.
//
// This module is the primitive plus the spend decision. The Stop-hook state
// machine that decides WHEN to self-accept or take free-time consumes this; it
// is a sibling concern. Here we answer only: is the window current, what is the
// ceiling, and does a given spend fit. The ceiling is keyed on weekly
// rate-limit headroom (legion ingests statusline usage samples) -- more
// headroom raises it; no samples falls back to a conservative default so the
// agent never runs unbounded when it cannot see its own quota.
//
// The burn-rate gate (#551) complements the work-unit budget: it pauses
// self-directed work when the host's real rate-limit headroom (5h or 7d) is
// near exhaustion, independently of abstract autonomy-unit accounting.

use chrono::{DateTime, Duration, Utc};

/// Threshold at which the burn-rate gate denies self-directed work.
/// 90% used means 10% headroom remaining -- enough warning to avoid
/// accidental quota exhaustion while still allowing most of the window.
pub const DEFAULT_BURN_RATE_THRESHOLD_PCT: f64 = 90.0;

/// Length of the rolling budget window, in days.
pub const WINDOW_DAYS: i64 = 7;

/// Ceiling used when weekly headroom is unknown -- enough for a little
/// autonomous progress, not enough to burn a quota. Unit: autonomy actions
/// (one self-accept or one free-time block = one unit by default).
pub const DEFAULT_CEILING: u64 = 15;
/// Floor ceiling even at zero remaining headroom -- a trickle of self-directed
/// work is still allowed; the loop never hard-stops on headroom alone.
pub const MIN_CEILING: u64 = 5;
/// Ceiling at full (100%) weekly headroom.
pub const MAX_CEILING: u64 = 60;

/// Derive the weekly ceiling from remaining weekly headroom (0..=100, where
/// 100 means a full week's quota is free). `None` -- no usage sample for this
/// host -- yields the conservative default rather than running unbounded.
pub fn ceiling_from_headroom(headroom_pct: Option<f64>) -> u64 {
    match headroom_pct {
        None => DEFAULT_CEILING,
        Some(pct) => {
            let frac = pct.clamp(0.0, 100.0) / 100.0;
            let span = (MAX_CEILING - MIN_CEILING) as f64;
            MIN_CEILING + (span * frac).round() as u64
        }
    }
}

/// A rolling weekly autonomy allowance for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutonomyBudget {
    pub window_start: DateTime<Utc>,
    pub spent: u64,
    pub ceiling: u64,
}

impl AutonomyBudget {
    /// A fresh budget whose window opens now.
    pub fn fresh(now: DateTime<Utc>, ceiling: u64) -> Self {
        Self {
            window_start: now,
            spent: 0,
            ceiling,
        }
    }

    /// Units still available this window.
    pub fn remaining(&self) -> u64 {
        self.ceiling.saturating_sub(self.spent)
    }

    /// When the current window rolls over.
    pub fn resets_at(&self) -> DateTime<Utc> {
        self.window_start + Duration::days(WINDOW_DAYS)
    }

    /// True once the window has elapsed and the budget should roll over.
    pub fn window_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.resets_at()
    }
}

/// The result of asking whether a spend fits the budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpendOutcome {
    /// Operator-requested work: never budget-bound, nothing is spent.
    OperatorBypass,
    /// Within budget. `new_spent` is the total to persist.
    Allowed {
        new_spent: u64,
        remaining_after: u64,
    },
    /// Out of budget for this window. The agent stops its own initiative
    /// cleanly; operator-requested work still proceeds.
    Exhausted,
}

/// Decide whether `cost` units of autonomous work fit `budget`.
///
/// Operator work bypasses the budget entirely. Otherwise the spend is allowed
/// only if it fits within the remaining allowance; a zero-remaining or
/// insufficient budget is `Exhausted`, never a partial spend.
pub fn decide_spend(budget: &AutonomyBudget, cost: u64, operator: bool) -> SpendOutcome {
    if operator {
        return SpendOutcome::OperatorBypass;
    }
    if budget.remaining() >= cost {
        let new_spent = budget.spent + cost;
        SpendOutcome::Allowed {
            new_spent,
            remaining_after: budget.ceiling.saturating_sub(new_spent),
        }
    } else {
        SpendOutcome::Exhausted
    }
}

/// Roll the window over if it has expired, recomputing the ceiling from current
/// headroom; otherwise return the budget unchanged. Call this on every read so
/// a stale window resets lazily without a background job.
pub fn rolled_over(
    budget: AutonomyBudget,
    now: DateTime<Utc>,
    fresh_ceiling: u64,
) -> AutonomyBudget {
    if budget.window_expired(now) {
        AutonomyBudget::fresh(now, fresh_ceiling)
    } else {
        budget
    }
}

/// The verdict returned by the burn-rate gate.
///
/// Mirrors the shape of `SpendOutcome` so callers chain the two gates
/// with a consistent match arm. The burn-rate check runs first; a
/// `Throttled` result short-circuits before `decide_spend` is reached.
#[derive(Debug, Clone, PartialEq)]
pub enum BurnRateOutcome {
    /// Operator-requested work: never rate-gated; bypass recorded in output.
    OperatorBypass,
    /// Both windows are below threshold (or no sample exists): work may proceed.
    Allowed {
        /// The five-hour used-% at decision time, if a sample existed.
        five_hour_pct: Option<f64>,
        /// The seven-day used-% at decision time, if a sample existed.
        seven_day_pct: Option<f64>,
    },
    /// At least one window is at or above threshold: self-directed work paused.
    ///
    /// Operator work still proceeds.
    Throttled {
        /// Which window triggered the gate ("5h", "7d", or "5h and 7d").
        window: String,
        /// The used-% that exceeded the threshold.
        used_pct: f64,
        /// The threshold that was exceeded.
        threshold_pct: f64,
    },
}

/// Evaluate the burn-rate gate from a rate-limit sample.
///
/// - `operator = true` always returns `OperatorBypass`, regardless of sample.
/// - `sample = None` (no row for this host) returns `Allowed` with both
///   percentages as `None` -- fail-open so the work-unit budget remains the
///   sole governor when no usage data exists.
/// - Returns `Throttled` when `five_hour_pct >= threshold` OR
///   `seven_day_pct >= threshold`. If both exceed, the `window` field names
///   both ("5h and 7d"). The `used_pct` field carries the higher of the two.
/// - `None` fields within a sample count as below threshold: not observed is
///   not the same as exhausted; the work-unit budget is the backstop.
/// - `threshold_pct` is clamped to `[0.0, 100.0]` before comparison.
pub fn decide_burn_rate(
    sample: Option<&crate::statusline::RateLimitSample>,
    operator: bool,
    threshold_pct: f64,
) -> BurnRateOutcome {
    if operator {
        return BurnRateOutcome::OperatorBypass;
    }
    let threshold = threshold_pct.clamp(0.0, 100.0);
    let (five_hour_pct, seven_day_pct) = match sample {
        None => {
            return BurnRateOutcome::Allowed {
                five_hour_pct: None,
                seven_day_pct: None,
            };
        }
        Some(s) => (s.five_hour_pct, s.seven_day_pct),
    };

    let fh_over = five_hour_pct.is_some_and(|p| p >= threshold);
    let sd_over = seven_day_pct.is_some_and(|p| p >= threshold);

    match (fh_over, sd_over) {
        (false, false) => BurnRateOutcome::Allowed {
            five_hour_pct,
            seven_day_pct,
        },
        (true, false) => BurnRateOutcome::Throttled {
            window: "5h".to_string(),
            used_pct: five_hour_pct.unwrap_or(0.0),
            threshold_pct: threshold,
        },
        (false, true) => BurnRateOutcome::Throttled {
            window: "7d".to_string(),
            used_pct: seven_day_pct.unwrap_or(0.0),
            threshold_pct: threshold,
        },
        (true, true) => {
            // Report the higher of the two in used_pct.
            let fh = five_hour_pct.unwrap_or(0.0);
            let sd = seven_day_pct.unwrap_or(0.0);
            BurnRateOutcome::Throttled {
                window: "5h and 7d".to_string(),
                used_pct: f64::max(fh, sd),
                threshold_pct: threshold,
            }
        }
    }
}

/// Human-readable burn-rate status line for `autonomy status --banner`.
///
/// Returns `None` when no sample exists -- the caller omits the line silently
/// so a host with no rate data does not add noise. When a sample exists,
/// returns a non-empty string describing headroom and, if appropriate, a
/// warning that the gate will fire.
///
/// Example outputs:
///   "Rate headroom: 5h 23% remaining, 7d 41% remaining."
///   "Rate limit warning: 5h 8% remaining (threshold 10%). Self-directed
///    work will be paused at the gate."
pub fn burn_rate_status_line(
    sample: Option<&crate::statusline::RateLimitSample>,
    threshold_pct: f64,
) -> Option<String> {
    let s = sample?;
    let threshold = threshold_pct.clamp(0.0, 100.0);

    let fmt_remaining = |pct: Option<f64>| -> Option<String> {
        pct.map(|p| format!("{:.0}%", (100.0 - p).max(0.0)))
    };

    let fh_remaining = fmt_remaining(s.five_hour_pct);
    let sd_remaining = fmt_remaining(s.seven_day_pct);

    let fh_over = s.five_hour_pct.is_some_and(|p| p >= threshold);
    let sd_over = s.seven_day_pct.is_some_and(|p| p >= threshold);

    let headroom_str = match (&fh_remaining, &sd_remaining) {
        (Some(fh), Some(sd)) => format!("5h {fh} remaining, 7d {sd} remaining"),
        (Some(fh), None) => format!("5h {fh} remaining"),
        (None, Some(sd)) => format!("7d {sd} remaining"),
        (None, None) => return None,
    };

    if fh_over || sd_over {
        let threshold_remaining = format!("{:.0}%", (100.0 - threshold).max(0.0));
        Some(format!(
            "Rate limit warning: {headroom_str} (threshold {threshold_remaining}). \
             Self-directed work will be paused at the gate."
        ))
    } else {
        Some(format!("Rate headroom: {headroom_str}."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statusline::RateLimitSample;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn make_sample(five_hour_pct: Option<f64>, seven_day_pct: Option<f64>) -> RateLimitSample {
        RateLimitSample {
            id: "test-id".to_string(),
            hostname: "testhost".to_string(),
            session_id: "test-session".to_string(),
            sampled_at: "2026-05-31T00:00:00Z".to_string(),
            five_hour_pct,
            five_hour_resets_at: None,
            seven_day_pct,
            seven_day_resets_at: None,
            model: None,
        }
    }

    #[test]
    fn ceiling_scales_with_headroom() {
        assert_eq!(ceiling_from_headroom(None), DEFAULT_CEILING);
        assert_eq!(ceiling_from_headroom(Some(100.0)), MAX_CEILING);
        assert_eq!(ceiling_from_headroom(Some(0.0)), MIN_CEILING);
        // Clamps out-of-range inputs.
        assert_eq!(ceiling_from_headroom(Some(150.0)), MAX_CEILING);
        assert_eq!(ceiling_from_headroom(Some(-10.0)), MIN_CEILING);
        // Midpoint lands between floor and max.
        let mid = ceiling_from_headroom(Some(50.0));
        assert!(mid > MIN_CEILING && mid < MAX_CEILING);
    }

    #[test]
    fn operator_work_bypasses_budget() {
        // Even a fully-spent budget does not block operator work.
        let budget = AutonomyBudget {
            window_start: t("2026-05-01T00:00:00Z"),
            spent: 99,
            ceiling: 10,
        };
        assert_eq!(decide_spend(&budget, 1, true), SpendOutcome::OperatorBypass);
    }

    #[test]
    fn spend_within_budget_is_allowed_and_accumulates() {
        let budget = AutonomyBudget::fresh(t("2026-05-01T00:00:00Z"), 10);
        assert_eq!(
            decide_spend(&budget, 3, false),
            SpendOutcome::Allowed {
                new_spent: 3,
                remaining_after: 7
            }
        );
    }

    #[test]
    fn exhausted_budget_stops_autonomous_work() {
        let budget = AutonomyBudget {
            window_start: t("2026-05-01T00:00:00Z"),
            spent: 10,
            ceiling: 10,
        };
        assert_eq!(decide_spend(&budget, 1, false), SpendOutcome::Exhausted);
        // A spend larger than what remains is also exhausted (no partial spend).
        let budget = AutonomyBudget {
            window_start: t("2026-05-01T00:00:00Z"),
            spent: 8,
            ceiling: 10,
        };
        assert_eq!(decide_spend(&budget, 5, false), SpendOutcome::Exhausted);
    }

    #[test]
    fn window_rolls_over_after_a_week_and_recomputes_ceiling() {
        let budget = AutonomyBudget {
            window_start: t("2026-05-01T00:00:00Z"),
            spent: 10,
            ceiling: 10,
        };
        // Six days in: same window, spend preserved.
        let still = rolled_over(budget.clone(), t("2026-05-07T00:00:00Z"), 60);
        assert_eq!(still.spent, 10);
        assert_eq!(still.ceiling, 10);
        // Eight days in: fresh window, spent reset, ceiling recomputed.
        let fresh = rolled_over(budget, t("2026-05-09T00:00:00Z"), 60);
        assert_eq!(fresh.spent, 0);
        assert_eq!(fresh.ceiling, 60);
        assert_eq!(fresh.window_start, t("2026-05-09T00:00:00Z"));
    }

    #[test]
    fn window_expiry_boundary() {
        let budget = AutonomyBudget::fresh(t("2026-05-01T00:00:00Z"), 10);
        // Exactly at the 7-day mark counts as expired (>=).
        assert!(budget.window_expired(t("2026-05-08T00:00:00Z")));
        assert!(!budget.window_expired(t("2026-05-07T23:59:59Z")));
        assert_eq!(budget.resets_at(), t("2026-05-08T00:00:00Z"));
    }

    // --- burn-rate gate tests ---

    #[test]
    fn burn_rate_operator_bypasses_regardless_of_sample() {
        let sample = make_sample(Some(95.0), Some(95.0));
        assert_eq!(
            decide_burn_rate(Some(&sample), true, 90.0),
            BurnRateOutcome::OperatorBypass
        );
        // Also operator bypass with no sample.
        assert_eq!(
            decide_burn_rate(None, true, 90.0),
            BurnRateOutcome::OperatorBypass
        );
    }

    #[test]
    fn burn_rate_no_sample_is_fail_open() {
        // No sample means no data -- gate must not fire; work-unit budget is the backstop.
        assert_eq!(
            decide_burn_rate(None, false, 90.0),
            BurnRateOutcome::Allowed {
                five_hour_pct: None,
                seven_day_pct: None,
            }
        );
    }

    #[test]
    fn burn_rate_both_below_threshold_is_allowed() {
        let sample = make_sample(Some(80.0), Some(80.0));
        assert_eq!(
            decide_burn_rate(Some(&sample), false, 90.0),
            BurnRateOutcome::Allowed {
                five_hour_pct: Some(80.0),
                seven_day_pct: Some(80.0),
            }
        );
    }

    #[test]
    fn burn_rate_five_hour_over_threshold_throttles() {
        let sample = make_sample(Some(95.0), Some(50.0));
        let outcome = decide_burn_rate(Some(&sample), false, 90.0);
        assert_eq!(
            outcome,
            BurnRateOutcome::Throttled {
                window: "5h".to_string(),
                used_pct: 95.0,
                threshold_pct: 90.0,
            }
        );
    }

    #[test]
    fn burn_rate_seven_day_over_threshold_throttles() {
        let sample = make_sample(Some(50.0), Some(92.0));
        let outcome = decide_burn_rate(Some(&sample), false, 90.0);
        assert_eq!(
            outcome,
            BurnRateOutcome::Throttled {
                window: "7d".to_string(),
                used_pct: 92.0,
                threshold_pct: 90.0,
            }
        );
    }

    #[test]
    fn burn_rate_both_over_threshold_names_both_windows() {
        let sample = make_sample(Some(91.0), Some(93.0));
        let outcome = decide_burn_rate(Some(&sample), false, 90.0);
        assert_eq!(
            outcome,
            BurnRateOutcome::Throttled {
                window: "5h and 7d".to_string(),
                used_pct: 93.0, // higher of 91 and 93
                threshold_pct: 90.0,
            }
        );
    }

    #[test]
    fn burn_rate_absent_five_hour_counts_as_below_threshold() {
        // None for 5h, over-threshold 7d -> 7d fires, 5h does not.
        let sample = make_sample(None, Some(95.0));
        assert_eq!(
            decide_burn_rate(Some(&sample), false, 90.0),
            BurnRateOutcome::Throttled {
                window: "7d".to_string(),
                used_pct: 95.0,
                threshold_pct: 90.0,
            }
        );
    }

    #[test]
    fn burn_rate_both_absent_is_allowed() {
        // Both None within a sample -- treated as below threshold (fail-open).
        let sample = make_sample(None, None);
        assert_eq!(
            decide_burn_rate(Some(&sample), false, 90.0),
            BurnRateOutcome::Allowed {
                five_hour_pct: None,
                seven_day_pct: None,
            }
        );
    }

    #[test]
    fn burn_rate_threshold_clamped_below_zero() {
        // A threshold < 0 clamps to 0, so any non-negative used_pct fires.
        let sample = make_sample(Some(0.0), Some(0.0));
        // 0.0 >= 0.0 (clamped threshold) -> throttled on both.
        let outcome = decide_burn_rate(Some(&sample), false, -5.0);
        assert!(matches!(outcome, BurnRateOutcome::Throttled { .. }));
    }

    #[test]
    fn burn_rate_threshold_clamped_above_100() {
        // A threshold > 100 clamps to 100, so nothing below 100 fires.
        let sample = make_sample(Some(99.9), Some(99.9));
        let outcome = decide_burn_rate(Some(&sample), false, 110.0);
        assert_eq!(
            outcome,
            BurnRateOutcome::Allowed {
                five_hour_pct: Some(99.9),
                seven_day_pct: Some(99.9),
            }
        );
    }

    #[test]
    fn burn_rate_at_exact_threshold_fires() {
        // Boundary: >= threshold means exactly at threshold fires.
        let sample = make_sample(Some(90.0), Some(50.0));
        let outcome = decide_burn_rate(Some(&sample), false, 90.0);
        assert_eq!(
            outcome,
            BurnRateOutcome::Throttled {
                window: "5h".to_string(),
                used_pct: 90.0,
                threshold_pct: 90.0,
            }
        );
    }

    #[test]
    fn burn_rate_status_line_returns_none_when_no_sample() {
        assert_eq!(burn_rate_status_line(None, 90.0), None);
    }

    #[test]
    fn burn_rate_status_line_healthy_shows_remaining() {
        let sample = make_sample(Some(60.0), Some(40.0));
        let line = burn_rate_status_line(Some(&sample), 90.0).expect("should produce a line");
        // 60% used -> 40% remaining for 5h; 40% used -> 60% remaining for 7d.
        assert!(
            line.contains("40%"),
            "5h remaining should be 40%, got: {line}"
        );
        assert!(
            line.contains("60%"),
            "7d remaining should be 60%, got: {line}"
        );
        assert!(
            line.starts_with("Rate headroom:"),
            "healthy line must start with 'Rate headroom:', got: {line}"
        );
        assert!(
            !line.contains("warning"),
            "healthy line must not say warning, got: {line}"
        );
    }

    #[test]
    fn burn_rate_status_line_warning_when_near_threshold() {
        // 5h at 93% -> 7% remaining, which is below 10% (threshold 90%).
        let sample = make_sample(Some(93.0), Some(50.0));
        let line = burn_rate_status_line(Some(&sample), 90.0).expect("should produce a line");
        assert!(
            line.contains("warning"),
            "near-threshold line must say warning, got: {line}"
        );
        assert!(
            line.contains("threshold"),
            "near-threshold line must mention threshold, got: {line}"
        );
        assert!(
            line.contains("paused"),
            "near-threshold line must say paused, got: {line}"
        );
    }

    #[test]
    fn burn_rate_status_line_both_none_returns_none() {
        // A sample where both pct fields are None produces no line.
        let sample = make_sample(None, None);
        assert_eq!(burn_rate_status_line(Some(&sample), 90.0), None);
    }
}
