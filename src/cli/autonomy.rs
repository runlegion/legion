//! `legion autonomy` handlers: budget, spend, lease (carved from main.rs, #610).

use clap::Subcommand;

use crate::cli::util::open_db;
use crate::{autonomy, db, error, statusline};

/// The kind of self-directed work a gate spend covers.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub(crate) enum AutonomyKind {
    /// The agent accepting its own Pending card (Pending -> Accepted).
    SelfAccept,
    /// Sanctioned exploration when the board is dry; output feeds reflections.
    FreeTime,
}

#[derive(Subcommand, Debug)]
pub(crate) enum AutonomyAction {
    /// Show the current window: spent / ceiling, remaining, and reset date.
    Status {
        /// Repository name (the agent whose budget to show).
        #[arg(long)]
        repo: String,

        /// Emit a concise SessionStart/Stop reminder framing instead of the
        /// status line: tells the agent it has sanctioned units to spend on
        /// self-directed work (or that the week's budget is spent).
        #[arg(long)]
        banner: bool,
    },
    /// Ask whether one unit of autonomous work fits the budget. Records the
    /// spend and exits 0 when allowed; exits non-zero (cleanly) when the
    /// weekly budget is exhausted. --operator bypasses entirely.
    Gate {
        /// Repository name (the agent spending).
        #[arg(long)]
        repo: String,

        /// What the spend is for.
        #[arg(long, value_enum)]
        kind: AutonomyKind,

        /// Operator-requested work: never budget-bound, nothing is spent.
        #[arg(long)]
        operator: bool,

        /// Units to spend (default 1).
        #[arg(long, default_value = "1")]
        cost: u64,

        /// Deny self-directed work when rate-limit used-% reaches this value
        /// (0-100). Default 90 means 10% headroom remaining triggers the gate.
        #[arg(long, default_value = "90.0")]
        burn_rate_threshold: f64,
    },
}

/// This host's remaining WEEKLY rate-limit headroom as a percentage
/// (100 - the seven-day used-%), or `None` when there is no rate sample for
/// this host. `None` makes the autonomy ceiling fall back to a conservative
/// default rather than running unbounded while blind to its own quota.
fn local_weekly_headroom(db: &db::Database) -> Option<f64> {
    let host = statusline::resolve_hostname();
    let sample = db.latest_rate_limit_sample_for_host(&host).ok()??;
    sample
        .seven_day_pct
        .map(|used| (100.0 - used).clamp(0.0, 100.0))
}

pub(crate) fn handle(action: AutonomyAction) -> error::Result<()> {
    let database = open_db()?;
    let now = chrono::Utc::now();

    // Recompute the ceiling from this host's remaining weekly headroom
    // on every read; a stale window then rolls over against it.
    let fresh_ceiling = autonomy::ceiling_from_headroom(local_weekly_headroom(&database));

    // Load-or-init, then roll the window over if it expired.
    let load = |repo: &str| -> error::Result<autonomy::AutonomyBudget> {
        let current = match database.get_autonomy_budget(repo)? {
            Some(b) => autonomy::rolled_over(b, now, fresh_ceiling),
            None => autonomy::AutonomyBudget::fresh(now, fresh_ceiling),
        };
        Ok(current)
    };

    match action {
        AutonomyAction::Status { repo, banner } => {
            let budget = load(&repo)?;
            let resets = budget.resets_at().format("%Y-%m-%d");

            // Fetch the latest rate-limit sample for the burn-rate status line.
            // Fail-open: a DB error here is not fatal to the status display.
            let host = statusline::resolve_hostname();
            let rate_sample: Option<crate::statusline::RateLimitSample> = database
                .latest_rate_limit_sample_for_host(&host)
                .unwrap_or(None);

            if !banner {
                println!(
                    "[legion] autonomy budget for {repo}: {}/{} units spent, {} remaining. \
                     Window resets {resets}.",
                    budget.spent,
                    budget.ceiling,
                    budget.remaining(),
                );
                if let Some(line) = autonomy::burn_rate_status_line(
                    rate_sample.as_ref(),
                    autonomy::DEFAULT_BURN_RATE_THRESHOLD_PCT,
                ) {
                    println!("[legion] {line}");
                }
            } else if budget.remaining() > 0 {
                // The reminder that turns the primitive into behavior:
                // the agent self-directs because it knows the budget is
                // there, instead of waiting to be told.
                println!(
                    "[Legion] Autonomy budget: {} of {} units this week (resets {resets}). \
                     Self-directed work is sanctioned -- self-accept a Pending card or take \
                     free-time without asking. Operator-requested work never draws on this.",
                    budget.remaining(),
                    budget.ceiling,
                );
                // Include burn-rate line in banner only when headroom is
                // approaching threshold (remaining < 2x threshold means
                // used > 100 - 2*(100 - threshold) = 2*threshold - 100).
                let threshold = autonomy::DEFAULT_BURN_RATE_THRESHOLD_PCT;
                let approaching_cutoff = 2.0 * threshold - 100.0;
                let near = rate_sample.as_ref().is_some_and(|s| {
                    s.five_hour_pct.is_some_and(|p| p > approaching_cutoff)
                        || s.seven_day_pct.is_some_and(|p| p > approaching_cutoff)
                });
                if near
                    && let Some(line) =
                        autonomy::burn_rate_status_line(rate_sample.as_ref(), threshold)
                {
                    println!("[Legion] {line}");
                }
            } else {
                println!(
                    "[Legion] Autonomy budget spent for this week (resets {resets}). \
                     Pause self-directed initiative; operator-requested work still proceeds."
                );
                // Always include burn-rate in exhausted banner so the
                // agent has full picture.
                if let Some(line) = autonomy::burn_rate_status_line(
                    rate_sample.as_ref(),
                    autonomy::DEFAULT_BURN_RATE_THRESHOLD_PCT,
                ) {
                    println!("[Legion] {line}");
                }
            }
        }
        AutonomyAction::Gate {
            repo,
            kind,
            operator,
            cost,
            burn_rate_threshold,
        } => {
            // Burn-rate check runs first, before the work-unit budget.
            // Fail-open on DB error: log to stderr, proceed to work-unit gate.
            let host = statusline::resolve_hostname();
            let rate_sample: Option<crate::statusline::RateLimitSample> = database
                .latest_rate_limit_sample_for_host(&host)
                .map_err(|e| {
                    eprintln!(
                        "[legion] warning: could not read rate-limit sample: {e}. \
                         Proceeding with work-unit budget only."
                    );
                })
                .unwrap_or(None);

            match autonomy::decide_burn_rate(rate_sample.as_ref(), operator, burn_rate_threshold) {
                autonomy::BurnRateOutcome::OperatorBypass => {
                    // Operator bypass from burn-rate gate -- proceed directly
                    // to work-unit budget check, which also has operator bypass.
                }
                autonomy::BurnRateOutcome::Throttled {
                    window,
                    used_pct,
                    threshold_pct,
                } => {
                    // Self-directed work paused. No write needed (read-only gate).
                    println!(
                        "[legion] rate-limit headroom low: {window} window at {used_pct:.0}% \
                         used (threshold {threshold_pct:.0}%). Self-directed work paused; \
                         operator-requested work still proceeds."
                    );
                    std::process::exit(1);
                }
                autonomy::BurnRateOutcome::Allowed { .. } => {
                    // Headroom is fine; fall through to work-unit budget check.
                }
            }

            let budget = load(&repo)?;
            match autonomy::decide_spend(&budget, cost, operator) {
                autonomy::SpendOutcome::OperatorBypass => {
                    println!("[legion] operator-requested work -- not budget-gated.");
                }
                autonomy::SpendOutcome::Allowed {
                    new_spent,
                    remaining_after,
                } => {
                    let updated = autonomy::AutonomyBudget {
                        spent: new_spent,
                        ..budget
                    };
                    database.upsert_autonomy_budget(&repo, &updated)?;
                    let label = match kind {
                        AutonomyKind::SelfAccept => "self-accept",
                        AutonomyKind::FreeTime => "free-time",
                    };
                    println!(
                        "[legion] {label} allowed -- {remaining_after} of {} autonomy \
                         units left this week.",
                        updated.ceiling
                    );
                    if matches!(kind, AutonomyKind::FreeTime) {
                        println!(
                            "[legion] free-time is idleation: spend it on a reflection \
                             (legion reflect) so the output compounds."
                        );
                    }
                }
                autonomy::SpendOutcome::Exhausted => {
                    // No write: an exhausted check changes nothing. The
                    // stored window_start is unchanged, so the reset date
                    // stays stable across repeated exhausted polls.
                    println!(
                        "[legion] autonomy budget exhausted for this week (resets {}). \
                         Operator-requested work still proceeds; self-directed work waits.",
                        budget.resets_at().format("%Y-%m-%d")
                    );
                    std::process::exit(1);
                }
            }
        }
    }
    Ok(())
}
