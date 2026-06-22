//! Tracking of spawned children: reaping, persona lease release, and
//! session outcome classification.

use std::time::{Duration, Instant};

use crate::db::Database;

use super::config::WatchConfig;
use super::locks::SessionLockTracker;
use super::spawn::{SUBMIT_KEY, SpawnedChild};

/// What the submit-confirmation loop should do with one PTY child this tick
/// (#649). Pulled out as a pure decision so the policy is unit-testable
/// without a live PTY -- `drive_submit_confirmation` performs the I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmitAction {
    /// A turn started -- the prompt submitted; advance Spawning -> Running.
    Confirm,
    /// Out of retries or budget -- fail closed (Spawning -> Failed).
    Fail,
    /// Send another Enter this tick.
    Send,
    /// Within the retry interval -- wait for the next tick.
    Wait,
}

/// Pure submit-confirmation policy. `turn_started` short-circuits to
/// `Confirm`; exhausting either `retries >= max` or `spawn_elapsed >=
/// budget` yields `Fail`; otherwise an Enter is due when no prior Enter
/// has been sent or the last one is older than `interval`.
fn submit_action(
    turn_started: bool,
    retries: u32,
    max: u32,
    spawn_elapsed: Duration,
    budget: Duration,
    last_enter_elapsed: Option<Duration>,
    interval: Duration,
) -> SubmitAction {
    if turn_started {
        return SubmitAction::Confirm;
    }
    if retries >= max || spawn_elapsed >= budget {
        return SubmitAction::Fail;
    }
    let due = last_enter_elapsed.map(|e| e >= interval).unwrap_or(true);
    if due {
        SubmitAction::Send
    } else {
        SubmitAction::Wait
    }
}

// -- Agent Tracker -----------------------------------------------------------

/// Tracks spawned child processes for active agent counting and persona
/// wake lease cleanup. Each tracked entry carries the (persona, signal_id)
/// pairs it acquired leases for so that `reap_finished` can release them
/// when the child exits.
pub struct TrackedChild {
    pub repo: String,
    pub child: SpawnedChild,
    /// `(persona_id, signal_id)` pairs this child acquired at spawn.
    pub held_leases: Vec<(String, String)>,
    /// Host identity the leases were acquired under. Required for the
    /// host-scoped release so a sync-resolved late-loser cannot drop the
    /// peer winner's row.
    pub host: String,
    /// UUIDv7 session id for the agent_session_log row (#389).
    pub session_id: String,
    /// RFC3339 timestamp of spawn -- defines the lower bound of the
    /// classification window for productive vs unproductive outcome.
    pub spawn_at: String,
    /// All signal ids bundled into this wake. Used to look up
    /// reflection-parent_id matches at reap time.
    pub signal_ids: Vec<String>,
    /// UUIDv7 of the wake_attempts row (#490, #491). `None` when the
    /// spawn predates the wake_attempts substrate or the enqueue
    /// failed; reap then skips the outcome recording.
    pub attempt_id: Option<String>,
    /// Monotonic spawn instant, for the submit-confirmation budget (#649).
    /// In-memory only -- the persisted `spawned_at` is RFC3339 wall-clock.
    pub spawn_instant: Instant,
    /// When the last submit keystroke was sent, for interval throttling
    /// (#649). `None` until the first Enter goes out.
    pub last_submit_enter: Option<Instant>,
    /// Count of submit keystrokes sent so far (#649). Bounds the retry
    /// loop and is reported in the failure outcome.
    pub submit_retries: u32,
    /// True once a turn was observed to start -- the prompt submitted and
    /// the FSM advanced `Spawning -> Running` (#649). Stops further Enters.
    pub submit_confirmed: bool,
    /// Set when the submit-confirmation loop gave up (#649). Carries the
    /// `outcome` reason so `reap_finished` records `submit_not_confirmed`
    /// on the wake_attempts row instead of a generic abandon.
    pub submit_failed_reason: Option<String>,
    /// Set when `reap_finished` force-reaps a session that outlived the
    /// session budget with no completion signal (#677). Carries the
    /// `session_budget_exceeded` outcome reason so the wedge is recorded
    /// distinctly from a submit failure or a generic error.
    pub budget_exceeded_reason: Option<String>,
}

pub struct AgentTracker {
    children: Vec<TrackedChild>,
}

impl AgentTracker {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    /// Record a spawned child process together with the leases it holds and
    /// the host identity under which they were acquired.
    #[allow(clippy::too_many_arguments)]
    pub fn track(
        &mut self,
        repo: String,
        child: SpawnedChild,
        held_leases: Vec<(String, String)>,
        host: String,
        session_id: String,
        spawn_at: String,
        signal_ids: Vec<String>,
        attempt_id: Option<String>,
    ) {
        self.children.push(TrackedChild {
            repo,
            child,
            held_leases,
            host,
            session_id,
            spawn_at,
            signal_ids,
            attempt_id,
            spawn_instant: Instant::now(),
            last_submit_enter: None,
            submit_retries: 0,
            submit_confirmed: false,
            submit_failed_reason: None,
            budget_exceeded_reason: None,
        });
    }

    /// Reap finished child processes, removing them from tracking and
    /// releasing their held persona wake leases. Uses host-scoped release so
    /// a late-loser whose lease was overwritten by sync conflict resolution
    /// cannot accidentally drop the peer winner's row. Release errors are
    /// logged but not propagated -- a missed release ages out via TTL.
    ///
    /// In addition to reaping children that have actually exited (`try_wait`
    /// returns `Some`), this method also handles idle PTY REPLs: a PTY-spawned
    /// `claude` session sits alive after its turn (a REPL does not EOF), so
    /// `try_wait` returns `None` forever. When the child's `exit_observed_at`
    /// is set in the DB (its stop hook fired `legion watch session-end`), the
    /// turn is considered complete: the child is killed best-effort and the full
    /// reap path runs. The session lock is released in both cases so the per-repo
    /// wake gate opens immediately rather than waiting for the TTL.
    ///
    /// `session_budget` is the wedge backstop (#677): a child that is still
    /// alive with no completion signal but has outlived this budget (measured
    /// from spawn) is force-reaped -- killed and run through the full reap path
    /// with a `session_budget_exceeded` outcome -- so a turn that blocks
    /// indefinitely cannot leak the process or hold its persona lease forever.
    /// A zero budget disables the backstop.
    pub fn reap_finished(
        &mut self,
        db: Option<&Database>,
        session_locks: Option<&SessionLockTracker>,
        session_budget: Duration,
    ) {
        self.children.retain_mut(|tracked| {
            // Determine whether this child should be reaped.
            // Two paths:
            //   A. Child already exited: try_wait returns Ok(Some(success)).
            //   B. Child is still alive (try_wait returns Ok(None)) but its
            //      stop hook has set exit_observed_at -- the turn is done and
            //      the REPL is sitting idle. Kill it and run the reap path.
            let reap_result: Option<bool> = match tracked.child.try_wait() {
                Ok(Some(success)) => {
                    // Path A: child exited naturally.
                    eprintln!(
                        "[legion watch] agent for {} exited ({})",
                        tracked.repo,
                        if success { "ok" } else { "error" }
                    );
                    Some(success)
                }
                Ok(None) => {
                    // Path B: child is still alive. Check exit_observed_at to
                    // detect a lingering idle PTY REPL whose turn is complete.
                    // Only consult the DB when we have an attempt_id and a DB
                    // handle; without those we cannot make the determination.
                    let turn_done = match (&tracked.attempt_id, db) {
                        (Some(aid), Some(db)) => {
                            match db.get_wake_attempt(aid) {
                                Ok(Some(ref attempt)) => attempt.exit_observed_at.is_some(),
                                Ok(None) => {
                                    // Row vanished (e.g. retention-pruned) while the
                                    // child is still alive. Log -- otherwise this is a
                                    // no-trace path: the child can no longer be reaped
                                    // via the stop-hook signal and falls back to the
                                    // TTL/exit backstop with no record of why.
                                    eprintln!(
                                        "[legion watch] wake row {} missing while child for {} still alive -- cannot confirm turn completion",
                                        aid, tracked.repo
                                    );
                                    false
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[legion watch] could not check exit_observed_at for {}: {}",
                                        aid, e
                                    );
                                    false
                                }
                            }
                        }
                        _ => false,
                    };

                    if turn_done {
                        // Stop-hook fired: turn complete, child is an idle REPL.
                        eprintln!(
                            "[legion watch] agent for {} turn complete (stop-hook) -- terminating idle REPL",
                            tracked.repo
                        );
                        match tracked.child.kill() {
                            Ok(()) => {
                                // Killed cleanly. Classify as ok: the turn completed
                                // normally (the stop hook firing is the agent's
                                // success signal); the kill is cleanup, not an error.
                                Some(true)
                            }
                            Err(e) => {
                                // Kill failed, so the REPL is STILL ALIVE. Keep it
                                // tracked and retry next poll rather than dropping
                                // it: a dropped-but-alive child leaks the process
                                // AND releases the lock, opening the gate for a
                                // colliding spawn -- the inverse of this fix. The
                                // TTL remains the ultimate backstop.
                                eprintln!(
                                    "[legion watch] kill of idle REPL for {} failed -- keeping tracked for retry: {}",
                                    tracked.repo, e
                                );
                                return true; // keep tracking; do not leak or open the gate
                            }
                        }
                    } else if !session_budget.is_zero()
                        && tracked.spawn_instant.elapsed() >= session_budget
                    {
                        // #677: wedge backstop. The child is alive and mid-turn
                        // but its stop hook never fired (no exit_observed_at)
                        // within the budget -- the turn is blocked indefinitely.
                        // The dead-pid reaper (#673) cannot help: the pid is
                        // still alive. Force-reap so neither the process (nor its
                        // MCP children) leaks and the persona lease is released.
                        eprintln!(
                            "[legion watch] agent for {} exceeded session budget ({}s) with no turn completion -- reaping wedged session",
                            tracked.repo,
                            session_budget.as_secs()
                        );
                        match tracked.child.kill() {
                            Ok(()) => {
                                // Killed: route the outcome to the distinct
                                // session_budget_exceeded reason and classify as
                                // error (a wedged turn produced nothing).
                                tracked.budget_exceeded_reason = Some(format!(
                                    "session_budget_exceeded ({}s)",
                                    session_budget.as_secs()
                                ));
                                Some(false)
                            }
                            Err(e) => {
                                // Kill failed: the session is STILL ALIVE. Keep it
                                // tracked and retry next poll rather than dropping
                                // it -- same reasoning as the idle-REPL path: a
                                // dropped-but-alive child leaks AND opens the gate.
                                eprintln!(
                                    "[legion watch] kill of wedged session for {} failed -- keeping tracked for retry: {}",
                                    tracked.repo, e
                                );
                                return true; // keep tracking; do not leak or open the gate
                            }
                        }
                    } else {
                        // Still genuinely running, within budget, no completion
                        // signal yet.
                        return true; // keep tracking
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[legion watch] error checking agent for {}: {}",
                        tracked.repo, e
                    );
                    return true; // keep tracking -- process may still be running
                }
            };

            let success: bool = reap_result.unwrap_or(false);

            // Release the session lock so the per-repo wake gate clears
            // immediately. A missing lock is Ok (already released by the
            // stop-hook fast path). Errors are logged but never panic the
            // watch thread.
            if let Some(locks) = session_locks
                && let Err(e) = locks.release(&tracked.repo)
            {
                eprintln!(
                    "[legion watch] failed to release session lock for {}: {}",
                    tracked.repo, e
                );
            }

            if let Some(db) = db {
                for (persona, signal_id) in &tracked.held_leases {
                    if let Err(e) =
                        db.release_persona_lease_if_owner(persona, signal_id, &tracked.host)
                    {
                        eprintln!(
                            "[legion watch] failed to release lease {}/{}: {}",
                            persona, signal_id, e
                        );
                    }
                }

                // A child the submit-confirmation loop gave up on (#649,
                // submit_failed_reason) or one force-reaped past the session
                // budget (#677, budget_exceeded_reason) produced no real session to
                // classify. Record the wake_attempt outcome with the distinct
                // reason so the metric trail shows the specific failure
                // ("prompt never submitted" / "session_budget_exceeded")
                // instead of a generic abandon, and skip the session-outcome row.
                if let Some(reason) = tracked
                    .submit_failed_reason
                    .as_ref()
                    .or(tracked.budget_exceeded_reason.as_ref())
                {
                    if let Some(ref attempt_id) = tracked.attempt_id
                        && let Err(e) = db.record_wake_attempt_outcome(attempt_id, "error", reason)
                    {
                        eprintln!(
                            "[legion watch] failed to record terminal outcome {}: {}",
                            attempt_id, e
                        );
                    }
                    return false; // remove from tracking list
                }

                // #389: classify and persist the session outcome.
                // Defensive: errors here log to stderr but never break
                // the reap loop -- a missed log row is recoverable, a
                // panic in the watch thread is not.
                let exit_at = chrono::Utc::now().to_rfc3339();
                let exit_status = if success { "ok" } else { "error" };
                let outcome = if !success {
                    Database::OUTCOME_ERRORED
                } else {
                    match db.classify_session(
                        &tracked.repo,
                        &tracked.signal_ids,
                        &tracked.spawn_at,
                        &exit_at,
                    ) {
                        Ok(true) => Database::OUTCOME_PRODUCTIVE,
                        Ok(false) => Database::OUTCOME_UNPRODUCTIVE,
                        Err(e) => {
                            eprintln!(
                                "[legion watch] classify failed for {} ({}): {}",
                                tracked.repo, tracked.session_id, e
                            );
                            // Conservative: do not record an unknown
                            // outcome as productive. Skip the row.
                            return false;
                        }
                    }
                };
                if let Err(e) = db.record_session_outcome(
                    &tracked.session_id,
                    &tracked.repo,
                    &tracked.signal_ids,
                    &tracked.spawn_at,
                    &exit_at,
                    exit_status,
                    outcome,
                ) {
                    eprintln!(
                        "[legion watch] failed to record session outcome for {} ({}): {}",
                        tracked.repo, tracked.session_id, e
                    );
                }

                // #490: stamp the wake_attempt's terminal state.
                // Idempotent at the DB layer; errors logged but
                // not propagated (the lease release + outcome
                // record above are the load-bearing writes).
                if let Some(ref attempt_id) = tracked.attempt_id
                    && let Err(e) =
                        db.record_wake_attempt_outcome(attempt_id, exit_status, outcome)
                {
                    eprintln!(
                        "[legion watch] failed to record wake attempt outcome {}: {}",
                        attempt_id, e
                    );
                }
            }
            false // remove from tracking list
        });
    }

    /// Number of currently active agents.
    pub fn active_count(&self) -> i32 {
        self.children.len() as i32
    }

    /// Drive the submit-confirmation protocol one tick (#649).
    ///
    /// Runs on the health tick, before `reap_finished`. For each PTY child
    /// whose prompt has not yet been confirmed-submitted, this either:
    ///   - observes a turn-start in the ring buffer and advances the FSM
    ///     `Spawning -> Running` (submit landed); or
    ///   - re-sends the submit keystroke (Enter), throttled to at most one
    ///     per `submit_retry_interval_secs`; or
    ///   - gives up once `submit_retry_max` Enters or
    ///     `submit_confirm_budget_secs` wall-clock elapse, marking the child
    ///     so `reap_finished` records `submit_not_confirmed` and tears it
    ///     down (the kill here makes `try_wait` reap it the same tick).
    ///
    /// Print children never enter this loop -- they submit their prompt as
    /// an argv at spawn and are advanced to `Running` by `poll_cycle`.
    pub fn drive_submit_confirmation(&mut self, db: &Database, config: &WatchConfig) {
        use crate::wake_attempts::WakeAttemptState::{Running, Spawning};

        let budget = Duration::from_secs(config.submit_confirm_budget_secs);
        let interval = Duration::from_secs(config.submit_retry_interval_secs);

        for tracked in self.children.iter_mut() {
            // Only PTY children that are still unconfirmed and carry an
            // attempt row are eligible. `turn_started()` is false for Print,
            // but skipping by variant avoids send_keys returning the
            // unsupported error every tick.
            if tracked.submit_confirmed
                || tracked.submit_failed_reason.is_some()
                || !matches!(tracked.child, SpawnedChild::Pty(_))
            {
                continue;
            }
            let Some(attempt_id) = tracked.attempt_id.clone() else {
                continue;
            };

            let action = submit_action(
                tracked.child.turn_started(),
                tracked.submit_retries,
                config.submit_retry_max,
                tracked.spawn_instant.elapsed(),
                budget,
                tracked.last_submit_enter.map(|t| t.elapsed()),
                interval,
            );

            match action {
                SubmitAction::Confirm => {
                    tracked.submit_confirmed = true;
                    if let Err(e) = db.transition_wake_attempt(&attempt_id, Spawning, Running) {
                        eprintln!(
                            "[legion watch] transition Spawning->Running for {} ({}): {}",
                            tracked.repo, attempt_id, e
                        );
                    }
                    eprintln!(
                        "[legion watch] submit confirmed for {} after {} enter(s)",
                        tracked.repo, tracked.submit_retries
                    );
                }
                SubmitAction::Fail => {
                    eprintln!(
                        "[legion watch] submit NOT confirmed for {} after {} enter(s) -- failing",
                        tracked.repo, tracked.submit_retries
                    );
                    tracked.submit_failed_reason = Some(format!(
                        "submit_not_confirmed (retries={})",
                        tracked.submit_retries
                    ));
                    // Kill so reap_finished reaps it this tick; the reason
                    // field routes the terminal outcome to submit_not_confirmed.
                    if let Err(e) = tracked.child.kill() {
                        eprintln!(
                            "[legion watch] kill of unconfirmed child for {} failed: {}",
                            tracked.repo, e
                        );
                    }
                }
                SubmitAction::Send => match tracked.child.send_keys(SUBMIT_KEY) {
                    Ok(()) => {
                        tracked.submit_retries += 1;
                        tracked.last_submit_enter = Some(Instant::now());
                    }
                    Err(e) => {
                        // A failed PTY write means the child's input is
                        // broken -- retrying cannot help. Fail closed rather
                        // than counting a phantom Enter against the budget or
                        // logging an Enter that never reached the TUI.
                        eprintln!(
                            "[legion watch] submit keystroke for {} failed -- failing: {}",
                            tracked.repo, e
                        );
                        tracked.submit_failed_reason = Some(format!(
                            "submit_send_failed (retries={})",
                            tracked.submit_retries
                        ));
                        if let Err(ke) = tracked.child.kill() {
                            eprintln!(
                                "[legion watch] kill after send failure for {} failed: {}",
                                tracked.repo, ke
                            );
                        }
                    }
                },
                SubmitAction::Wait => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_storage;

    // -- reap_finished: exit_observed_at path ---------------------------------

    // Spawn a long-lived child (sleep) and record its state in the tracker so
    // the exit_observed_at branch of reap_finished can be exercised without
    // waiting for the process to exit naturally.
    #[cfg(unix)]
    #[test]
    fn reap_finished_kills_idle_pty_repl_when_exit_observed() {
        // A live child process (sleep) that will not exit on its own stands in
        // for the idle PTY REPL. Once exit_observed_at is set on the DB row,
        // reap_finished must kill the child and remove it from tracking.
        let (db, _index, data_dir) = test_storage();
        let locks = SessionLockTracker::new(data_dir.path(), 3600);

        // Spawn a long-lived child -- sleep 9999 simulates an idle REPL.
        let child_proc = std::process::Command::new("sleep")
            .arg("9999")
            .spawn()
            .expect("spawn sleep");
        let child_pid = child_proc.id();

        // Set up the wake_attempts row with a known attempt_id.
        let attempt_id = uuid::Uuid::now_v7().to_string();
        let signal_id = db
            .insert_reflection("kelex", "@legion question:test-reap", "team")
            .expect("insert signal")
            .id;
        db.enqueue_wake_attempt(&attempt_id, "legion", "legion", &[signal_id])
            .expect("enqueue");

        // Record the session lock so we can verify it is released.
        locks
            .record_spawn("legion", child_pid)
            .expect("record spawn");
        assert!(
            locks.active_pid("legion").is_some(),
            "precondition: lock must be active after record_spawn"
        );

        // Mark exit_observed_at to simulate the stop hook firing.
        db.mark_wake_attempt_exit_observed(&attempt_id)
            .expect("mark exit observed");

        // Build the tracked child entry (Print branch wraps std::process::Child).
        let spawned = SpawnedChild::Print(child_proc);
        let session_id = uuid::Uuid::now_v7().to_string();
        let spawn_at = chrono::Utc::now().to_rfc3339();
        let mut tracker = AgentTracker::new();
        tracker.track(
            "legion".to_string(),
            spawned,
            Vec::new(),
            "test-host".to_string(),
            session_id,
            spawn_at,
            Vec::new(),
            Some(attempt_id.clone()),
        );

        assert_eq!(tracker.active_count(), 1, "precondition: child is tracked");

        // Run the reaper with session_locks provided.
        tracker.reap_finished(Some(&db), Some(&locks), Duration::from_secs(3600));

        // The child must have been reaped (removed from tracking).
        assert_eq!(
            tracker.active_count(),
            0,
            "child must be reaped when exit_observed_at is set"
        );

        // The session lock must have been released.
        assert!(
            locks.active_pid("legion").is_none(),
            "session lock must be released after reap"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reap_finished_keeps_running_child_without_exit_observed() {
        // A live child with no exit_observed_at must NOT be reaped -- it is
        // genuinely mid-turn and we must not interrupt it.
        let (db, _index, data_dir) = test_storage();
        let locks = SessionLockTracker::new(data_dir.path(), 3600);

        let child_proc = std::process::Command::new("sleep")
            .arg("9999")
            .spawn()
            .expect("spawn sleep");

        // Enqueue a wake attempt but do NOT mark exit_observed_at.
        let attempt_id = uuid::Uuid::now_v7().to_string();
        let sig = db
            .insert_reflection("kelex", "@legion question:active-turn", "team")
            .expect("signal")
            .id;
        db.enqueue_wake_attempt(&attempt_id, "legion", "legion", &[sig])
            .expect("enqueue");

        locks
            .record_spawn("legion", child_proc.id())
            .expect("record spawn");
        assert!(
            locks.active_pid("legion").is_some(),
            "precondition: lock is active"
        );

        let spawned = SpawnedChild::Print(child_proc);
        let session_id = uuid::Uuid::now_v7().to_string();
        let spawn_at = chrono::Utc::now().to_rfc3339();
        let mut tracker = AgentTracker::new();
        tracker.track(
            "legion".to_string(),
            spawned,
            Vec::new(),
            "test-host".to_string(),
            session_id,
            spawn_at,
            Vec::new(),
            Some(attempt_id),
        );

        assert_eq!(tracker.active_count(), 1, "precondition: child tracked");

        // Reaper: child is live and exit_observed_at is NOT set.
        tracker.reap_finished(Some(&db), Some(&locks), Duration::from_secs(3600));

        // Child must still be tracked -- the turn is genuinely in progress.
        assert_eq!(
            tracker.active_count(),
            1,
            "active child with no exit_observed_at must stay tracked"
        );

        // Session lock must still be held.
        assert!(
            locks.active_pid("legion").is_some(),
            "session lock must remain active for an in-progress turn"
        );

        // Clean up the long-running child so we don't leak it in the test suite.
        tracker.children.iter_mut().for_each(|t| {
            let _ = t.child.kill();
        });
    }

    // -- reap_finished: session-budget wedge backstop (#677) ------------------

    #[cfg(unix)]
    #[test]
    fn reap_finished_reaps_wedged_session_past_budget() {
        // A live child (sleep) that is mid-turn (Running) with NO
        // exit_observed_at, alive past the session budget, is a wedged session:
        // it must be force-reaped, its session lock and persona lease released,
        // and its wake_attempt settled to Failed with the session_budget_exceeded
        // outcome -- the eavesdrop case from #676 that the dead-pid reaper misses
        // because the pid is still alive.
        let (db, _index, data_dir) = test_storage();
        let locks = SessionLockTracker::new(data_dir.path(), 3600);

        let child_proc = std::process::Command::new("sleep")
            .arg("9999")
            .spawn()
            .expect("spawn sleep");
        let child_pid = child_proc.id();

        // wake_attempts row driven Claimed -> Spawning -> Running so the wedge is
        // mid-turn (post-submit), exactly like a session that started its turn
        // and then blocked.
        let attempt_id = uuid::Uuid::now_v7().to_string();
        let signal_id = db
            .insert_reflection("kelex", "@legion question:wedged", "team")
            .expect("insert signal")
            .id;
        db.enqueue_wake_attempt(
            &attempt_id,
            "legion",
            "legion",
            std::slice::from_ref(&signal_id),
        )
        .expect("enqueue");
        db.try_claim_wake_attempt(&attempt_id, "test-host")
            .expect("claim");
        use crate::wake_attempts::WakeAttemptState::{Claimed, Running, Spawning};
        db.transition_wake_attempt(&attempt_id, Claimed, Spawning)
            .expect("Claimed->Spawning");
        db.transition_wake_attempt(&attempt_id, Spawning, Running)
            .expect("Spawning->Running");

        // Hold a persona lease under the same host the tracked child records, so
        // we can assert the reap releases it (the load-bearing fix: a wedged
        // session must not keep blocking future wakes of its persona).
        assert!(
            db.try_acquire_persona_lease(
                "legion",
                &signal_id,
                "test-host",
                Duration::from_secs(3600)
            )
            .expect("acquire lease"),
            "precondition: lease acquired"
        );

        locks
            .record_spawn("legion", child_pid)
            .expect("record spawn");

        let mut tracker = AgentTracker::new();
        tracker.track(
            "legion".to_string(),
            SpawnedChild::Print(child_proc),
            vec![("legion".to_string(), signal_id.clone())],
            "test-host".to_string(),
            uuid::Uuid::now_v7().to_string(),
            chrono::Utc::now().to_rfc3339(),
            vec![signal_id.clone()],
            Some(attempt_id.clone()),
        );

        // Backdate the monotonic spawn instant so the child reads as 120s old
        // against a 60s budget -- no sleep needed to cross the threshold.
        tracker.children[0].spawn_instant = Instant::now() - Duration::from_secs(120);

        tracker.reap_finished(Some(&db), Some(&locks), Duration::from_secs(60));

        assert_eq!(
            tracker.active_count(),
            0,
            "wedged session past budget must be reaped"
        );
        assert!(
            locks.active_pid("legion").is_none(),
            "session lock must be released after a wedge reap"
        );
        let row = db
            .get_wake_attempt(&attempt_id)
            .expect("get attempt")
            .expect("row exists");
        assert_eq!(row.state, crate::wake_attempts::WakeAttemptState::Failed);
        assert_eq!(
            row.outcome.as_deref(),
            Some("session_budget_exceeded (60s)"),
            "the distinct wedge reason must land on the row"
        );
        assert!(
            db.list_persona_leases(Some("legion"))
                .expect("list leases")
                .is_empty(),
            "the wedged session's persona lease must be released"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reap_finished_keeps_wedged_session_when_budget_disabled() {
        // A zero budget disables the wedge backstop: a live, mid-turn child with
        // no exit_observed_at stays tracked no matter how old it is, so an
        // operator who sets session_budget_secs = 0 opts out of force-reaping.
        let (db, _index, data_dir) = test_storage();
        let locks = SessionLockTracker::new(data_dir.path(), 3600);

        let child_proc = std::process::Command::new("sleep")
            .arg("9999")
            .spawn()
            .expect("spawn sleep");
        let attempt_id = uuid::Uuid::now_v7().to_string();
        let sig = db
            .insert_reflection("kelex", "@legion question:no-budget", "team")
            .expect("signal")
            .id;
        db.enqueue_wake_attempt(&attempt_id, "legion", "legion", &[sig])
            .expect("enqueue");
        locks
            .record_spawn("legion", child_proc.id())
            .expect("record spawn");

        let mut tracker = AgentTracker::new();
        tracker.track(
            "legion".to_string(),
            SpawnedChild::Print(child_proc),
            Vec::new(),
            "test-host".to_string(),
            uuid::Uuid::now_v7().to_string(),
            chrono::Utc::now().to_rfc3339(),
            Vec::new(),
            Some(attempt_id),
        );
        // Very old child, but budget disabled.
        tracker.children[0].spawn_instant = Instant::now() - Duration::from_secs(99_999);

        tracker.reap_finished(Some(&db), Some(&locks), Duration::ZERO);

        assert_eq!(
            tracker.active_count(),
            1,
            "a zero budget must disable the wedge backstop"
        );

        // Clean up the long-running child.
        tracker.children.iter_mut().for_each(|t| {
            let _ = t.child.kill();
        });
    }

    // -- submit_action policy (#649) ------------------------------------------

    #[test]
    fn submit_action_confirms_on_turn_start() {
        // turn_started short-circuits everything else, even past the budget.
        let a = submit_action(
            true,
            99,
            12,
            Duration::from_secs(999),
            Duration::from_secs(60),
            None,
            Duration::from_secs(4),
        );
        assert_eq!(a, SubmitAction::Confirm);
    }

    #[test]
    fn submit_action_fails_on_exhausted_retries_or_budget() {
        // Retry cap reached.
        assert_eq!(
            submit_action(
                false,
                12,
                12,
                Duration::from_secs(1),
                Duration::from_secs(60),
                Some(Duration::from_secs(10)),
                Duration::from_secs(4),
            ),
            SubmitAction::Fail
        );
        // Wall-clock budget reached even with retries left.
        assert_eq!(
            submit_action(
                false,
                3,
                12,
                Duration::from_secs(60),
                Duration::from_secs(60),
                Some(Duration::from_secs(10)),
                Duration::from_secs(4),
            ),
            SubmitAction::Fail
        );
    }

    #[test]
    fn submit_action_sends_then_waits_within_interval() {
        // No prior Enter -> send immediately.
        assert_eq!(
            submit_action(
                false,
                0,
                12,
                Duration::from_secs(1),
                Duration::from_secs(60),
                None,
                Duration::from_secs(4),
            ),
            SubmitAction::Send
        );
        // Last Enter older than interval -> send again.
        assert_eq!(
            submit_action(
                false,
                1,
                12,
                Duration::from_secs(6),
                Duration::from_secs(60),
                Some(Duration::from_secs(5)),
                Duration::from_secs(4),
            ),
            SubmitAction::Send
        );
        // Last Enter within interval -> wait.
        assert_eq!(
            submit_action(
                false,
                1,
                12,
                Duration::from_secs(2),
                Duration::from_secs(60),
                Some(Duration::from_secs(1)),
                Duration::from_secs(4),
            ),
            SubmitAction::Wait
        );
    }

    // -- reap routing for submit failures (#649) ------------------------------

    #[cfg(unix)]
    #[test]
    fn reap_records_submit_not_confirmed_outcome() {
        // A child the submit loop gave up on (submit_failed_reason set, then
        // killed) must reap to a Failed wake_attempt with the distinct
        // submit_not_confirmed outcome -- not a generic abandon -- and must
        // release its leases.
        let (db, _index, data_dir) = test_storage();
        let locks = SessionLockTracker::new(data_dir.path(), 3600);

        // Spawn + immediately kill a child so try_wait reports exited.
        let mut child_proc = std::process::Command::new("sleep")
            .arg("9999")
            .spawn()
            .expect("spawn sleep");
        let child_pid = child_proc.id();
        child_proc.kill().expect("kill child");

        // wake_attempts row driven to Spawning (where the submit loop fails).
        let attempt_id = uuid::Uuid::now_v7().to_string();
        let signal_id = db
            .insert_reflection("kelex", "@legion question:submit-fail", "team")
            .expect("insert signal")
            .id;
        db.enqueue_wake_attempt(&attempt_id, "legion", "legion", &[signal_id])
            .expect("enqueue");
        db.try_claim_wake_attempt(&attempt_id, "test-host")
            .expect("claim");
        use crate::wake_attempts::WakeAttemptState::{Claimed, Spawning};
        db.transition_wake_attempt(&attempt_id, Claimed, Spawning)
            .expect("Claimed->Spawning");

        locks
            .record_spawn("legion", child_pid)
            .expect("record spawn");

        let mut tracker = AgentTracker::new();
        tracker.track(
            "legion".to_string(),
            SpawnedChild::Print(child_proc),
            Vec::new(),
            "test-host".to_string(),
            uuid::Uuid::now_v7().to_string(),
            chrono::Utc::now().to_rfc3339(),
            Vec::new(),
            Some(attempt_id.clone()),
        );
        assert!(
            locks.active_pid("legion").is_some(),
            "precondition: session lock is held before reap"
        );

        // Mark it as the submit loop would on give-up.
        tracker.children[0].submit_failed_reason =
            Some("submit_not_confirmed (retries=12)".to_string());

        tracker.reap_finished(Some(&db), Some(&locks), Duration::from_secs(3600));

        assert_eq!(tracker.active_count(), 0, "failed child must be reaped");
        let row = db
            .get_wake_attempt(&attempt_id)
            .expect("get attempt")
            .expect("row exists");
        assert_eq!(row.state, crate::wake_attempts::WakeAttemptState::Failed);
        assert_eq!(
            row.outcome.as_deref(),
            Some("submit_not_confirmed (retries=12)"),
            "the distinct submit-failure reason must land on the row"
        );
        // The submit-failure path must release the session lock like any
        // other reap, so the per-repo wake gate reopens.
        assert!(
            locks.active_pid("legion").is_none(),
            "session lock must be released after a submit-failure reap"
        );
    }
}
