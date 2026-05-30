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

use chrono::{DateTime, Duration, Utc};

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

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
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
}
