//! Tracking of spawned children: reaping, persona lease release, and
//! session outcome classification.

use crate::db::Database;

use super::locks::SessionLockTracker;
use super::spawn::SpawnedChild;

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
    pub fn reap_finished(
        &mut self,
        db: Option<&Database>,
        session_locks: Option<&SessionLockTracker>,
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
                    } else {
                        // Still genuinely running and no completion signal yet.
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
        tracker.reap_finished(Some(&db), Some(&locks));

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
        tracker.reap_finished(Some(&db), Some(&locks));

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
}
