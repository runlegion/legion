//! Wake-attempt FSM types for the watch PTY migration (#487, part of #495).
//!
//! `wake_attempts` is the ACID substrate for the per-attempt lifecycle:
//! enqueue -> claim -> spawn -> run -> exit -> terminal. The persona
//! lease layer (`persona_wake_leases`) keeps its TTL-based mutual
//! exclusion role; the attempt row records what actually happened on a
//! given spawn so peer nodes can see in-flight wakes and crash recovery
//! can reap orphans without racing peers.
//!
//! FSM enforcement mirrors `uncertainty::types::PredictionState` -- the
//! transition table is the load-bearing safety property. An illegal
//! `done -> running` regression coming back via sync conflict resolution
//! is silently rejected at the DB layer rather than ever taking effect.
//!
//! DB methods live in `src/db.rs` (Migration 23 + `try_claim_wake_attempt`
//! and friends). Sync delta + consumer wiring are in #488 / #489 / #490.
//!
//! Until consumers land, `#![allow(dead_code)]` keeps clippy quiet
//! during the soak window.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::error::{LegionError, Result};

/// Lifecycle states for a wake_attempts row.
///
/// String literals match the column form so [`WakeAttemptState::as_str`]
/// and [`WakeAttemptState::from_str`] round-trip through the database
/// without an intermediate mapping table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WakeAttemptState {
    /// Row written; no host has claimed it yet.
    Queued,
    /// A host has won the atomic claim and is preparing to spawn.
    Claimed,
    /// PTY process started; prompt not yet written.
    Spawning,
    /// Prompt written; output flowing.
    Running,
    /// Stop-hook observed OR PTY EOF observed (whichever comes first).
    /// Terminal classification still pending the reaper.
    Exiting,
    /// Terminal: child exited cleanly.
    Done,
    /// Terminal: child exited with non-zero status.
    Failed,
    /// Terminal: reaper timed the attempt out.
    Timeout,
    /// Terminal: crash recovery could not locate the process; gave up.
    Abandoned,
}

impl WakeAttemptState {
    pub fn as_str(&self) -> &'static str {
        match self {
            WakeAttemptState::Queued => "queued",
            WakeAttemptState::Claimed => "claimed",
            WakeAttemptState::Spawning => "spawning",
            WakeAttemptState::Running => "running",
            WakeAttemptState::Exiting => "exiting",
            WakeAttemptState::Done => "done",
            WakeAttemptState::Failed => "failed",
            WakeAttemptState::Timeout => "timeout",
            WakeAttemptState::Abandoned => "abandoned",
        }
    }

    /// Inherent parser, named `parse_state` to avoid shadowing the
    /// std `FromStr::from_str` ambient. Returns
    /// `WakeAttemptStateDecodeError` on an unknown literal -- the row
    /// existed, its `state` column is corrupt. A `WakeAttemptNotFound`
    /// would be a category error since a future caller branching on
    /// not-found to retry would silently swallow real corruption.
    pub fn parse_state(s: &str) -> Result<Self> {
        match s {
            "queued" => Ok(WakeAttemptState::Queued),
            "claimed" => Ok(WakeAttemptState::Claimed),
            "spawning" => Ok(WakeAttemptState::Spawning),
            "running" => Ok(WakeAttemptState::Running),
            "exiting" => Ok(WakeAttemptState::Exiting),
            "done" => Ok(WakeAttemptState::Done),
            "failed" => Ok(WakeAttemptState::Failed),
            "timeout" => Ok(WakeAttemptState::Timeout),
            "abandoned" => Ok(WakeAttemptState::Abandoned),
            other => Err(LegionError::WakeAttemptStateDecodeError(format!(
                "unknown wake attempt state: {other}"
            ))),
        }
    }

    /// True when one of the four terminal classifications.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WakeAttemptState::Done
                | WakeAttemptState::Failed
                | WakeAttemptState::Timeout
                | WakeAttemptState::Abandoned
        )
    }

    /// True when one of the in-progress states crash-recovery cares about
    /// (claimed/spawning/running/exiting -- anything pre-terminal that
    /// owns a host).
    pub fn is_in_flight(&self) -> bool {
        matches!(
            self,
            WakeAttemptState::Claimed
                | WakeAttemptState::Spawning
                | WakeAttemptState::Running
                | WakeAttemptState::Exiting
        )
    }

    /// Lifecycle transition table.
    ///
    /// The legal moves follow the documented path
    /// queued -> claimed -> spawning -> running -> exiting ->
    /// { done | failed | timeout | abandoned }. Two additional shortcuts
    /// reflect real failure modes: any claimed-and-beyond non-terminal
    /// state can short-circuit to `failed` or `abandoned` when the
    /// reaper observes a dead pid before the optimistic stop-hook fires.
    pub fn can_transition_to(&self, next: WakeAttemptState) -> bool {
        use WakeAttemptState::*;
        matches!(
            (self, next),
            (Queued, Claimed)
                | (Queued, Abandoned)
                | (Claimed, Spawning)
                | (Claimed, Abandoned)
                | (Claimed, Failed)
                | (Spawning, Running)
                | (Spawning, Failed)
                | (Spawning, Abandoned)
                | (Running, Exiting)
                | (Running, Failed)
                | (Running, Timeout)
                | (Running, Abandoned)
                | (Exiting, Done)
                | (Exiting, Failed)
                | (Exiting, Timeout)
                | (Exiting, Abandoned)
        )
    }
}

/// Row representation of a wake_attempts record.
///
/// `signal_ids` is stored as a JSON array TEXT column; the in-memory
/// shape is a flat Vec<String> for callers. `outcome` and `exit_status`
/// stay as free-form strings to avoid roundtripping the existing
/// "productive | unproductive | errored | unknown" + "ok | error | killed"
/// vocabularies through a second enum the rest of the codebase would
/// then have to know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeAttempt {
    pub attempt_id: String,
    pub persona_id: String,
    pub repo_name: String,
    pub signal_ids: Vec<String>,
    pub state: WakeAttemptState,
    pub acquired_by_host: Option<String>,
    pub acquired_at: Option<String>,
    pub spawned_pid: Option<u32>,
    pub spawned_at: Option<String>,
    pub exit_observed_at: Option<String>,
    pub exited_at: Option<String>,
    pub exit_status: Option<String>,
    pub outcome: Option<String>,
    pub deleted_at: Option<String>,
    pub updated_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_states() -> [WakeAttemptState; 9] {
        use WakeAttemptState::*;
        [
            Queued, Claimed, Spawning, Running, Exiting, Done, Failed, Timeout, Abandoned,
        ]
    }

    #[test]
    fn state_strings_round_trip() {
        for s in all_states() {
            let parsed = WakeAttemptState::parse_state(s.as_str()).expect("known state");
            assert_eq!(parsed, s);
        }
    }

    #[test]
    fn parse_state_rejects_unknown() {
        assert!(WakeAttemptState::parse_state("nope").is_err());
    }

    #[test]
    fn is_terminal_classification() {
        use WakeAttemptState::*;
        for s in [Done, Failed, Timeout, Abandoned] {
            assert!(s.is_terminal(), "{s:?} should be terminal");
            assert!(!s.is_in_flight(), "{s:?} should not be in flight");
        }
        {
            let s = Queued;
            assert!(!s.is_terminal());
            assert!(!s.is_in_flight());
        }
        for s in [Claimed, Spawning, Running, Exiting] {
            assert!(!s.is_terminal());
            assert!(s.is_in_flight(), "{s:?} should be in flight");
        }
    }

    #[test]
    fn legal_transitions_match_the_documented_path() {
        use WakeAttemptState::*;
        // Happy path: queued -> claimed -> spawning -> running -> exiting -> done.
        assert!(Queued.can_transition_to(Claimed));
        assert!(Claimed.can_transition_to(Spawning));
        assert!(Spawning.can_transition_to(Running));
        assert!(Running.can_transition_to(Exiting));
        assert!(Exiting.can_transition_to(Done));

        // Failure short-circuits: any in-flight pre-Exiting can fail or
        // be abandoned; any Running can additionally time out.
        assert!(Queued.can_transition_to(Abandoned));
        assert!(Claimed.can_transition_to(Abandoned));
        assert!(Claimed.can_transition_to(Failed));
        assert!(Spawning.can_transition_to(Failed));
        assert!(Spawning.can_transition_to(Abandoned));
        assert!(Running.can_transition_to(Failed));
        assert!(Running.can_transition_to(Timeout));
        assert!(Running.can_transition_to(Abandoned));
        assert!(Exiting.can_transition_to(Failed));
        assert!(Exiting.can_transition_to(Timeout));
        assert!(Exiting.can_transition_to(Abandoned));
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        use WakeAttemptState::*;
        // No reverse moves.
        assert!(!Claimed.can_transition_to(Queued));
        assert!(!Running.can_transition_to(Claimed));
        assert!(!Exiting.can_transition_to(Running));

        // No skipping forward.
        assert!(!Queued.can_transition_to(Running));
        assert!(!Queued.can_transition_to(Done));
        assert!(!Claimed.can_transition_to(Running));
        assert!(!Spawning.can_transition_to(Done));

        // Terminal is sticky -- no transition from any terminal state.
        for terminal in [Done, Failed, Timeout, Abandoned] {
            for target in all_states() {
                assert!(
                    !terminal.can_transition_to(target),
                    "{terminal:?} -> {target:?} must be rejected (terminal is sticky)"
                );
            }
        }

        // No self-loops on non-terminal states either.
        for s in [Queued, Claimed, Spawning, Running, Exiting] {
            assert!(
                !s.can_transition_to(s),
                "{s:?} -> {s:?} must be rejected (self-loops poison sync)"
            );
        }
    }

    #[test]
    fn transition_table_is_exhaustive_check() {
        // Enumerate every pair; legal count must match the documented
        // list to catch accidental additions or deletions to the table.
        let mut legal = 0usize;
        for from in all_states() {
            for to in all_states() {
                if from.can_transition_to(to) {
                    legal += 1;
                }
            }
        }
        // 2 (from Queued) + 3 (from Claimed) + 3 (from Spawning) +
        // 4 (from Running) + 4 (from Exiting) = 16.
        assert_eq!(legal, 16, "transition table size drifted from spec");
    }
}
