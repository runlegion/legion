//! Watch-spawn coordination: persona wake leases (TTL-based mutual
//! exclusion, #308) and wake_attempts (per-spawn FSM rows, #487). The
//! sync delta plumbing for both lives in `super::sync`.

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};

use super::Database;
use crate::error::{LegionError, Result};

/// A persona wake lease -- "host H is handling signal S for persona P until T".
///
/// Acquired by watch before spawning an agent in response to a wake signal.
/// Other watchers (on this node or peers) see the live lease and skip their
/// own spawn. Heartbeats keep `expires_at` rolling forward; a crashed session
/// whose heartbeats stop lets the lease age out via TTL.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PersonaWakeLease {
    pub persona_id: String,
    pub signal_id: String,
    pub acquired_by_host: String,
    pub acquired_at: String,
    pub heartbeat_at: String,
    pub expires_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

fn map_persona_lease_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PersonaWakeLease> {
    Ok(PersonaWakeLease {
        persona_id: row.get(0)?,
        signal_id: row.get(1)?,
        acquired_by_host: row.get(2)?,
        acquired_at: row.get(3)?,
        heartbeat_at: row.get(4)?,
        expires_at: row.get(5)?,
        deleted_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

/// The single definition of "this lease is live": not soft-deleted and not
/// past its TTL. Shared verbatim by every reader/writer that needs to agree
/// on liveness -- `list_persona_leases` (what the CLI displays),
/// `release_persona_lease` (the operator-facing forced release), and
/// `release_persona_leases_by_host` -- so a released-or-expired lease can
/// never render as live in one code path while another still treats it as
/// gone (#679: the list and release paths previously diverged because
/// release only checked `deleted_at`, ignoring `expires_at`, so an
/// expired-but-undeleted row was invisible to `list` yet still "releasable"
/// to `release`).
///
/// Deliberately NOT used by `release_persona_lease_if_owner` -- see that
/// function's doc comment for why the watch reaper's own finalization write
/// must stay unconditional on `expires_at` (PR #764 review: gating it here
/// let an expired-but-undeleted row survive reap and get resurrected by the
/// next same-tick heartbeat).
///
/// Uses one unnumbered `?` placeholder -- callers bind `now` at the position
/// where this fragment lands in the finished SQL text.
const LIVE_LEASE_WHERE: &str = "deleted_at IS NULL AND expires_at > ?";

/// `persona_wake_leases` (#308) and `wake_attempts` (#487) tables.
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    // Migration 17: Persona wake leases for cluster-wide wake coordination (#308).
    //
    // When a signal arrives addressed to a persona (either `--to P` or `--to all`),
    // watch acquires a lease keyed by (persona_id, signal_id) before spawning. Other
    // nodes (or later poll cycles on the same node) see the lease is held and skip
    // the wake. Heartbeats keep the lease fresh; crashes release via TTL.
    //
    // `deleted_at` + `updated_at` carry the usual LWW semantics for smugglr sync.
    // `expires_at` is a denormalized scalar for cheap "is this lease still live"
    // filters without constructing a duration against `heartbeat_at` at query time.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS persona_wake_leases (
                persona_id TEXT NOT NULL,
                signal_id TEXT NOT NULL,
                acquired_by_host TEXT NOT NULL,
                acquired_at TEXT NOT NULL,
                heartbeat_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                deleted_at TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (persona_id, signal_id)
            );
            CREATE INDEX IF NOT EXISTS idx_persona_wake_leases_persona \
                ON persona_wake_leases(persona_id) WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_persona_wake_leases_expires \
                ON persona_wake_leases(expires_at) WHERE deleted_at IS NULL;",
    )?;

    // Migration 23: wake_attempts -- ACID substrate for the watch PTY
    // migration (#487, part of #495).
    //
    // Each row represents one wake spawn from queue through terminal
    // classification. The FSM enforced in Rust
    // (`wake_attempts::WakeAttemptState::can_transition_to`) and
    // mirrored in every UPDATE here keeps `state = from` in the WHERE
    // clause so a sync-resolved late-loser cannot regress a peer's
    // already-terminal row.
    //
    // persona_wake_leases (Migration 17) keeps its TTL-based mutual
    // exclusion role; wake_attempts records what actually happened
    // on each individual spawn so the reaper has a persistent,
    // cluster-visible work item to operate on.
    //
    // signal_ids stored as a JSON array TEXT column to avoid a
    // wake_attempt_signals join table -- N is small (a wake batches
    // a handful of signals at most) and the column is opaque to the
    // SQL layer.
    //
    // Indices target the two hot read paths: crash recovery scans
    // by (acquired_by_host, state) on startup, and operator history
    // queries scan by (persona_id, spawned_at DESC).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS wake_attempts (
                attempt_id TEXT PRIMARY KEY,
                persona_id TEXT NOT NULL,
                repo_name TEXT NOT NULL,
                signal_ids TEXT NOT NULL,
                state TEXT NOT NULL,
                acquired_by_host TEXT,
                acquired_at TEXT,
                spawned_pid INTEGER,
                spawned_at TEXT,
                exit_observed_at TEXT,
                exited_at TEXT,
                exit_status TEXT,
                outcome TEXT,
                deleted_at TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_wake_attempts_host_state
                ON wake_attempts(acquired_by_host, state)
                WHERE deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_wake_attempts_persona_recent
                ON wake_attempts(persona_id, spawned_at DESC)
                WHERE deleted_at IS NULL;",
    )?;
    Ok(())
}

/// Column migrations for `wake_attempts`.
pub(super) fn migrate(conn: &Connection) -> Result<()> {
    // Migration: card_id linkage for delegated cards (#778). Nullable --
    // the overwhelming majority of wake_attempts rows are plain auto-wake
    // spawns with no delegated card behind them. Set once by
    // `Database::set_wake_attempt_card` when `kanban::delegate_card` links
    // an Accepted card to a live attempt; read back by
    // `Database::delegated_card_is_live` (the entry gate, the tick_health
    // auto-revert sweep, and the stop.sh delegated-liveness subcommand all
    // share that one predicate, per the #679 "one predicate" lesson --
    // LIVE_LEASE_WHERE above is the sibling precedent for wake leases).
    // No REFERENCES clause -- PRAGMA foreign_keys is not enabled globally,
    // matching `tasks.document_id` (Migration 16, src/db/kanban.rs).
    if !Database::has_column(conn, "wake_attempts", "card_id")? {
        conn.execute_batch("ALTER TABLE wake_attempts ADD COLUMN card_id TEXT;")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_wake_attempts_card_id \
             ON wake_attempts(card_id) WHERE card_id IS NOT NULL;",
        )?;
    }
    Ok(())
}

impl Database {
    /// Try to acquire a persona wake lease. Returns `Ok(true)` on success,
    /// `Ok(false)` when a live lease for `(persona_id, signal_id)` is already
    /// held. Expired or soft-deleted leases are treated as free and may be
    /// claimed by the caller.
    ///
    /// Atomicity: a single `UPDATE ... WHERE expires_at <= now OR deleted_at IS NOT NULL`
    /// followed by `INSERT OR IGNORE` runs inside a transaction. Both
    /// statements take SQLite's write lock so cross-process races are
    /// serialized by the DB file lock; the caller sees the outcome via
    /// `rows_changed()`. This matches the issue spec: "INSERT OR FAIL with
    /// primary-key collision; first-writer-wins."
    pub fn try_acquire_persona_lease(
        &self,
        persona_id: &str,
        signal_id: &str,
        host: &str,
        lease_ttl: std::time::Duration,
    ) -> Result<bool> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let expires = (now
            + chrono::Duration::from_std(lease_ttl)
                .unwrap_or_else(|_| chrono::Duration::minutes(10)))
        .to_rfc3339();

        let tx = self.conn.unchecked_transaction()?;

        // Reclaim stale rows so INSERT OR IGNORE below can succeed against
        // them. Scoped by PK so this only touches the row we are trying to
        // acquire -- no broad sweep here.
        tx.execute(
            "UPDATE persona_wake_leases \
             SET acquired_by_host = ?1, acquired_at = ?2, heartbeat_at = ?2, \
                 expires_at = ?3, updated_at = ?2, deleted_at = NULL \
             WHERE persona_id = ?4 AND signal_id = ?5 \
               AND (deleted_at IS NOT NULL OR expires_at <= ?2)",
            rusqlite::params![host, &now_str, &expires, persona_id, signal_id],
        )?;

        let inserted = tx.execute(
            "INSERT OR IGNORE INTO persona_wake_leases \
             (persona_id, signal_id, acquired_by_host, acquired_at, heartbeat_at, \
              expires_at, updated_at, deleted_at) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?4, NULL)",
            rusqlite::params![persona_id, signal_id, host, &now_str, &expires],
        )?;

        // If INSERT OR IGNORE inserted (1) or the reclaim UPDATE touched a
        // stale row we now own, the lease is ours. Confirm we hold it by
        // reading back -- covers the edge case where the stale-reclaim
        // UPDATE succeeded but the INSERT was a no-op.
        let holder: Option<String> = tx
            .query_row(
                "SELECT acquired_by_host FROM persona_wake_leases \
                 WHERE persona_id = ?1 AND signal_id = ?2 AND deleted_at IS NULL",
                rusqlite::params![persona_id, signal_id],
                |r| r.get(0),
            )
            .optional()?;

        tx.commit()?;
        let _ = inserted;
        Ok(holder.as_deref() == Some(host))
    }

    /// Refresh every live lease held by `host`, extending `expires_at` to
    /// `now + ttl`. Returns the number of leases touched.
    pub fn heartbeat_persona_leases(&self, host: &str, ttl: std::time::Duration) -> Result<u64> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let expires = (now
            + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::minutes(10)))
        .to_rfc3339();

        let updated = self.conn.execute(
            "UPDATE persona_wake_leases \
             SET heartbeat_at = ?1, expires_at = ?2, updated_at = ?1 \
             WHERE acquired_by_host = ?3 AND deleted_at IS NULL",
            rusqlite::params![&now_str, &expires, host],
        )?;
        Ok(updated as u64)
    }

    /// Soft-delete one lease by (persona_id, signal_id). Returns true if a
    /// matching *live* lease existed (per [`LIVE_LEASE_WHERE`] -- the same
    /// predicate `list_persona_leases` uses), false when it was already
    /// released or had already expired. Idempotent on an already-released
    /// lease.
    ///
    /// Unscoped by host -- used by the operator CLI to forcibly drop any
    /// stuck lease. The watch reaper uses `release_persona_lease_if_owner`
    /// instead so a late-loser whose lease was overwritten by sync cannot
    /// accidentally release the winner's row.
    pub fn release_persona_lease(&self, persona_id: &str, signal_id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let sql = format!(
            "UPDATE persona_wake_leases \
             SET deleted_at = ?, updated_at = ? \
             WHERE persona_id = ? AND signal_id = ? AND {LIVE_LEASE_WHERE}"
        );
        let updated = self.conn.execute(
            &sql,
            rusqlite::params![&now, &now, persona_id, signal_id, &now],
        )?;
        Ok(updated > 0)
    }

    /// Like `release_persona_lease`, but only if the lease is still held by
    /// `host`. Used by the watch reaper -- via `AgentTracker::reap_finished`
    /// -- to finalize a lease the tracker still lists in `held_leases` once
    /// the child it was guarding has been reaped. `acquired_by_host = host`
    /// still guards against a late-loser releasing a peer's row after a
    /// sync-resolved handoff.
    ///
    /// Deliberately does NOT gate on `expires_at` (does not use
    /// [`LIVE_LEASE_WHERE`]): this is a "finalize my own row" write, not a
    /// "report whether this lease still reads as live" query, and those two
    /// questions must stay decoupled. `tick_health` runs
    /// `reap_finished` (which calls this) before
    /// `heartbeat_persona_leases`, and `heartbeat_persona_leases` is
    /// host-scoped but liveness-blind -- it refreshes every row still owned
    /// by `host` with `deleted_at IS NULL`, regardless of `expires_at`. If
    /// this release were gated on `expires_at > now` the same way
    /// `list_persona_leases` is, a lease that had already ticked past its TTL
    /// by the time reap ran would fail to match here (still `deleted_at IS
    /// NULL`, but `expires_at <= now`), and the very next line of the same
    /// tick would have `heartbeat_persona_leases` push `expires_at` back into
    /// the future -- resurrecting a lease the reaper had already finished
    /// with into a permanent ghost that `list_persona_leases` renders as live
    /// forever (PR #764 review finding). Owning the row (`acquired_by_host =
    /// host`) plus not-yet-soft-deleted (`deleted_at IS NULL`) is sufficient
    /// grounds to finalize it here; whether it also happened to still be
    /// unexpired is irrelevant to "am I done with this lease."
    ///
    /// Returns true only when a row this host still owned (and had not
    /// already soft-deleted) was released.
    pub fn release_persona_lease_if_owner(
        &self,
        persona_id: &str,
        signal_id: &str,
        host: &str,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE persona_wake_leases \
             SET deleted_at = ?1, updated_at = ?1 \
             WHERE persona_id = ?2 AND signal_id = ?3 AND acquired_by_host = ?4 \
               AND deleted_at IS NULL",
            rusqlite::params![&now, persona_id, signal_id, host],
        )?;
        Ok(updated > 0)
    }

    /// Soft-delete every live lease (per [`LIVE_LEASE_WHERE`]) held by `host`.
    /// Called on daemon shutdown so a graceful exit does not leave ghost
    /// leases that must age out via TTL.
    #[allow(dead_code)] // wired by a future SIGTERM handler; kept in the API surface now
    pub fn release_persona_leases_by_host(&self, host: &str) -> Result<u64> {
        let now = Utc::now().to_rfc3339();
        let sql = format!(
            "UPDATE persona_wake_leases \
             SET deleted_at = ?, updated_at = ? \
             WHERE acquired_by_host = ? AND {LIVE_LEASE_WHERE}"
        );
        let updated = self
            .conn
            .execute(&sql, rusqlite::params![&now, &now, host, &now])?;
        Ok(updated as u64)
    }

    /// Return every live lease (per [`LIVE_LEASE_WHERE`] -- the same
    /// predicate the release paths use, #679), optionally filtered to a
    /// single persona. Ordered oldest-first by `acquired_at` so the CLI lists
    /// leases in the order they were taken.
    pub fn list_persona_leases(&self, persona: Option<&str>) -> Result<Vec<PersonaWakeLease>> {
        let now = Utc::now().to_rfc3339();
        match persona {
            Some(p) => {
                let sql = format!(
                    "SELECT persona_id, signal_id, acquired_by_host, acquired_at, \
                            heartbeat_at, expires_at, deleted_at, updated_at \
                     FROM persona_wake_leases \
                     WHERE {LIVE_LEASE_WHERE} AND persona_id = ? \
                     ORDER BY acquired_at ASC"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                Ok(stmt
                    .query_map(rusqlite::params![&now, p], map_persona_lease_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            }
            None => {
                let sql = format!(
                    "SELECT persona_id, signal_id, acquired_by_host, acquired_at, \
                            heartbeat_at, expires_at, deleted_at, updated_at \
                     FROM persona_wake_leases \
                     WHERE {LIVE_LEASE_WHERE} \
                     ORDER BY acquired_at ASC"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                Ok(stmt
                    .query_map(rusqlite::params![&now], map_persona_lease_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            }
        }
    }
}

// -- wake_attempts (#487, part of #495) --------------------------------------
//
// Until #488 (sync delta) and #489/#490 (consumer wiring) land, these
// items are reachable only from tests. The wake_attempts module carries
// its own `#![allow(dead_code)]`; here we only need the allow on
// `map_wake_attempt_row` and the new `impl Database` block since the
// surrounding db.rs is mostly live.

/// Column list shared by every `wake_attempts` SELECT, matching
/// `map_wake_attempt_row`'s positional `row.get(N)` calls. Factored out
/// (#778) once a fifth call site needed the same fifteen-plus-`card_id`
/// list `list_local_orphans` / `get_wake_attempt` / `recent_wake_attempts`
/// already carried independently -- mirrors `Database::CARD_COLUMNS`
/// (src/db/kanban.rs), the sibling precedent for cards.
const WAKE_ATTEMPT_COLUMNS: &str = "attempt_id, persona_id, repo_name, signal_ids, state, \
     acquired_by_host, acquired_at, spawned_pid, spawned_at, \
     exit_observed_at, exited_at, exit_status, outcome, \
     deleted_at, updated_at, card_id";

#[allow(dead_code)]
fn map_wake_attempt_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<crate::wake_attempts::WakeAttempt> {
    let state_str: String = row.get(4)?;
    let state = crate::wake_attempts::WakeAttemptState::parse_state(&state_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!("state decode: {}", e))),
        )
    })?;
    let signal_ids_json: String = row.get(3)?;
    let signal_ids: Vec<String> = serde_json::from_str(&signal_ids_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let spawned_pid_i64: Option<i64> = row.get(7)?;
    Ok(crate::wake_attempts::WakeAttempt {
        attempt_id: row.get(0)?,
        persona_id: row.get(1)?,
        repo_name: row.get(2)?,
        signal_ids,
        state,
        acquired_by_host: row.get(5)?,
        acquired_at: row.get(6)?,
        spawned_pid: spawned_pid_i64.map(|v| v as u32),
        spawned_at: row.get(8)?,
        exit_observed_at: row.get(9)?,
        exited_at: row.get(10)?,
        exit_status: row.get(11)?,
        outcome: row.get(12)?,
        deleted_at: row.get(13)?,
        updated_at: row.get(14)?,
        card_id: row.get(15)?,
    })
}

#[allow(dead_code)] // wired by #488 / #489 / #490
impl Database {
    /// Insert a new wake_attempts row in the `queued` state. The caller
    /// is expected to mint a fresh UUIDv7 for `attempt_id`; reusing an
    /// existing id is a programming error and the PK constraint will
    /// surface as `LegionError::Database`.
    pub fn enqueue_wake_attempt(
        &self,
        attempt_id: &str,
        persona_id: &str,
        repo_name: &str,
        signal_ids: &[String],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let signal_ids_json = serde_json::to_string(signal_ids)?;
        self.conn.execute(
            "INSERT INTO wake_attempts \
             (attempt_id, persona_id, repo_name, signal_ids, state, updated_at) \
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5)",
            rusqlite::params![attempt_id, persona_id, repo_name, &signal_ids_json, &now],
        )?;
        Ok(())
    }

    /// Atomic claim. Returns `Ok(true)` when this host won the claim,
    /// `Ok(false)` when the row is either gone, already claimed by
    /// another host, or in a non-`queued` state. Mirrors
    /// `try_acquire_persona_lease`'s atomic UPDATE...WHERE pattern.
    pub fn try_claim_wake_attempt(&self, attempt_id: &str, host: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE wake_attempts \
             SET state = 'claimed', acquired_by_host = ?1, acquired_at = ?2, updated_at = ?2 \
             WHERE attempt_id = ?3 \
               AND state = 'queued' \
               AND acquired_by_host IS NULL \
               AND deleted_at IS NULL",
            rusqlite::params![host, &now, attempt_id],
        )?;
        Ok(updated == 1)
    }

    /// FSM-enforced state transition. Two-layer safety: the Rust-side
    /// `can_transition_to` rejects illegal pairs before SQL, and the
    /// `state = from` predicate in the UPDATE rejects races where the
    /// row has already moved.
    ///
    /// Returns `Ok(())` on a successful transition;
    /// `IllegalWakeAttemptTransition` when the row's current state does
    /// not match `from` or the table forbids `from -> to`;
    /// `WakeAttemptNotFound` when the row is absent or soft-deleted.
    pub fn transition_wake_attempt(
        &self,
        attempt_id: &str,
        from: crate::wake_attempts::WakeAttemptState,
        to: crate::wake_attempts::WakeAttemptState,
    ) -> Result<()> {
        if !from.can_transition_to(to) {
            return self.illegal_transition(attempt_id, from, to);
        }

        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE wake_attempts \
             SET state = ?1, updated_at = ?2 \
             WHERE attempt_id = ?3 AND state = ?4 AND deleted_at IS NULL",
            rusqlite::params![to.as_str(), &now, attempt_id, from.as_str()],
        )?;
        if updated == 1 {
            return Ok(());
        }
        // Distinguish "no such row" from "row exists in a different
        // state". One get_wake_attempt lookup either way -- the FSM
        // pre-check above already filtered out callable-but-illegal pairs.
        match self.get_wake_attempt(attempt_id)? {
            None => Err(LegionError::WakeAttemptNotFound(attempt_id.to_string())),
            Some(row) => Err(LegionError::IllegalWakeAttemptTransition {
                attempt_id: attempt_id.to_string(),
                from: from.as_str().to_string(),
                to: to.as_str().to_string(),
                current: row.state.as_str().to_string(),
            }),
        }
    }

    fn illegal_transition(
        &self,
        attempt_id: &str,
        from: crate::wake_attempts::WakeAttemptState,
        to: crate::wake_attempts::WakeAttemptState,
    ) -> Result<()> {
        let current = self
            .get_wake_attempt(attempt_id)?
            .map(|r| r.state.as_str().to_string())
            .unwrap_or_else(|| "<missing>".to_string());
        Err(LegionError::IllegalWakeAttemptTransition {
            attempt_id: attempt_id.to_string(),
            from: from.as_str().to_string(),
            to: to.as_str().to_string(),
            current,
        })
    }

    /// Record the PID of a spawned PTY child. Called after the child
    /// reaches `Spawning`; the value is a hint for `kill -0` style
    /// liveness probes, not the authority on completion (the PTY fd is).
    pub fn set_wake_attempt_pid(&self, attempt_id: &str, pid: u32) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE wake_attempts \
             SET spawned_pid = ?1, spawned_at = ?2, updated_at = ?2 \
             WHERE attempt_id = ?3 AND deleted_at IS NULL",
            rusqlite::params![pid as i64, &now, attempt_id],
        )?;
        if updated == 1 {
            Ok(())
        } else {
            Err(LegionError::WakeAttemptNotFound(attempt_id.to_string()))
        }
    }

    /// Stop-hook expediter: mark `exit_observed_at` so the reaper can
    /// short-circuit its next poll cycle. NOT authoritative -- the
    /// reaper still confirms via PTY EOF + PID-poll before writing a
    /// terminal state. The hook may legitimately never fire (8-block
    /// stop-hook cap in Claude Code 2.1.143).
    pub fn mark_wake_attempt_exit_observed(&self, attempt_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE wake_attempts \
             SET exit_observed_at = ?1, updated_at = ?1 \
             WHERE attempt_id = ?2 AND deleted_at IS NULL",
            rusqlite::params![&now, attempt_id],
        )?;
        if updated == 1 {
            Ok(())
        } else {
            Err(LegionError::WakeAttemptNotFound(attempt_id.to_string()))
        }
    }

    /// Terminal classification + outcome stamp. Sets `exited_at`,
    /// `exit_status`, `outcome`, and transitions the row to a terminal
    /// FSM state based on `exit_status` (ok -> Done, error -> Failed,
    /// killed -> Failed). The reaper may short-circuit from any
    /// in-flight state when a stop hook + PID-dead race collapses the
    /// lifecycle, so we accept any of claimed/spawning/running/exiting
    /// as the source. Terminal-is-sticky stays load-bearing: the
    /// WHERE clause excludes already-terminal rows so a late hook
    /// cannot rewrite a settled outcome (the call surfaces as
    /// `WakeAttemptNotFound` so the caller notices).
    pub fn record_wake_attempt_outcome(
        &self,
        attempt_id: &str,
        exit_status: &str,
        outcome: &str,
    ) -> Result<()> {
        let terminal = match exit_status {
            "ok" => "done",
            "error" | "killed" => "failed",
            other => {
                return Err(LegionError::IllegalWakeAttemptTransition {
                    attempt_id: attempt_id.to_string(),
                    from: "<reaper>".to_string(),
                    to: other.to_string(),
                    current: "<unknown exit_status>".to_string(),
                });
            }
        };
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE wake_attempts \
             SET state = ?1, exited_at = ?2, exit_status = ?3, outcome = ?4, updated_at = ?2 \
             WHERE attempt_id = ?5 \
               AND state IN ('claimed', 'spawning', 'running', 'exiting') \
               AND deleted_at IS NULL",
            rusqlite::params![terminal, &now, exit_status, outcome, attempt_id],
        )?;
        if updated == 1 {
            return Ok(());
        }
        // Distinguish "no such row" from "row exists but is already
        // terminal (or queued)". Same diagnostic shape as
        // transition_wake_attempt -- callers can branch on the variant
        // without retrying a corruption case as if it were absence.
        match self.get_wake_attempt(attempt_id)? {
            None => Err(LegionError::WakeAttemptNotFound(attempt_id.to_string())),
            Some(row) => Err(LegionError::IllegalWakeAttemptTransition {
                attempt_id: attempt_id.to_string(),
                from: "<reaper>".to_string(),
                to: terminal.to_string(),
                current: row.state.as_str().to_string(),
            }),
        }
    }

    /// Strictly host-scoped orphan scan: rows owned by `self_host` that
    /// are in any pre-terminal in-flight state. The host filter is
    /// load-bearing -- a two-node sweep race could reap a peer's still-
    /// running attempt and stick its persona lease until TTL.
    pub fn list_local_orphans(
        &self,
        self_host: &str,
    ) -> Result<Vec<crate::wake_attempts::WakeAttempt>> {
        let sql = format!(
            "SELECT {WAKE_ATTEMPT_COLUMNS} FROM wake_attempts \
             WHERE acquired_by_host = ?1 \
               AND state IN ('claimed', 'spawning', 'running', 'exiting') \
               AND deleted_at IS NULL \
             ORDER BY acquired_at ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Ok(stmt
            .query_map(rusqlite::params![self_host], map_wake_attempt_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Fetch a single row by id (including soft-deleted ones, for sync
    /// reconciliation). Returns `Ok(None)` when no row exists.
    pub fn get_wake_attempt(
        &self,
        attempt_id: &str,
    ) -> Result<Option<crate::wake_attempts::WakeAttempt>> {
        let sql = format!("SELECT {WAKE_ATTEMPT_COLUMNS} FROM wake_attempts WHERE attempt_id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let row = stmt
            .query_row(rusqlite::params![attempt_id], map_wake_attempt_row)
            .optional()?;
        Ok(row)
    }

    /// Link a wake_attempts row to the kanban card it is delegated work
    /// for (#778). A metadata write, not an FSM transition -- the caller
    /// (`kanban::delegate_card`) is responsible for having already checked
    /// the attempt is live and unclaimed before calling this. Returns
    /// `WakeAttemptNotFound` when the row is absent or soft-deleted, same
    /// contract as `set_wake_attempt_pid`.
    pub fn set_wake_attempt_card(&self, attempt_id: &str, card_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE wake_attempts \
             SET card_id = ?1, updated_at = ?2 \
             WHERE attempt_id = ?3 AND deleted_at IS NULL",
            rusqlite::params![card_id, &now, attempt_id],
        )?;
        if updated == 1 {
            Ok(())
        } else {
            Err(LegionError::WakeAttemptNotFound(attempt_id.to_string()))
        }
    }

    /// Clear the `card_id` link on whatever wake_attempts row currently
    /// points at `card_id` (#778 review fix -- see `kanban::undelegate_card`
    /// for why this must run on every Undelegate, not just some). Matches
    /// by `card_id`, not by `attempt_id`, so the caller does not need to
    /// have looked the linked row up first. A no-op, not an error, when
    /// nothing is currently linked -- `Undelegate` on a card that was never
    /// actually delegated (shouldn't happen given the FSM, but this is a
    /// plain UPDATE, not an FSM transition) must not fail the caller.
    pub fn clear_wake_attempt_card(&self, card_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE wake_attempts SET card_id = NULL, updated_at = ?1 \
             WHERE card_id = ?2 AND deleted_at IS NULL",
            rusqlite::params![&now, card_id],
        )?;
        Ok(())
    }

    /// Find a live, not-yet-claimed-by-a-card wake_attempts row for a repo
    /// -- the auto-discovery path `kanban::delegate_card` falls back to
    /// when the caller does not pass `--attempt-id` explicitly. Newest
    /// (`spawned_at DESC`) wins when more than one is in flight; callers
    /// that need a specific one among several should pass `--attempt-id`.
    /// A `Claimed` row has no `spawned_at` yet (NULL), which SQLite sorts
    /// last in a DESC ordering -- a just-claimed, not-yet-spawned attempt
    /// loses to any already-spawned one. Intentional: prefer handing the
    /// caller a worker that has actually started over one still starting.
    pub fn find_live_wake_attempt_for_repo(
        &self,
        repo_name: &str,
    ) -> Result<Option<crate::wake_attempts::WakeAttempt>> {
        let sql = format!(
            "SELECT {WAKE_ATTEMPT_COLUMNS} FROM wake_attempts \
             WHERE repo_name = ?1 \
               AND card_id IS NULL \
               AND state IN ('claimed', 'spawning', 'running', 'exiting') \
               AND deleted_at IS NULL \
             ORDER BY spawned_at DESC \
             LIMIT 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let row = stmt
            .query_row(rusqlite::params![repo_name], map_wake_attempt_row)
            .optional()?;
        Ok(row)
    }

    /// Fetch the wake_attempts row linked to a card, if any (#778). Most
    /// recent by `updated_at` when more than one row was ever linked
    /// (re-delegation after a manual undelegate) -- only one is meaningful
    /// at a time since `delegate_card` only ever sets `card_id` on a
    /// currently-live, currently-unclaimed row.
    pub fn get_wake_attempt_by_card(
        &self,
        card_id: &str,
    ) -> Result<Option<crate::wake_attempts::WakeAttempt>> {
        let sql = format!(
            "SELECT {WAKE_ATTEMPT_COLUMNS} FROM wake_attempts \
             WHERE card_id = ?1 AND deleted_at IS NULL \
             ORDER BY updated_at DESC \
             LIMIT 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let row = stmt
            .query_row(rusqlite::params![card_id], map_wake_attempt_row)
            .optional()?;
        Ok(row)
    }

    /// The single definition of "this card's delegated work is still
    /// live" (#778, mirroring the #679 `LIVE_LEASE_WHERE` lesson: one
    /// shared predicate, not one per call site that can silently
    /// diverge). Composed of two independent halves, BOTH required:
    ///
    /// 1. The watch daemon's heartbeat is fresh (`watch_heartbeat_alive`).
    ///    `tick_health`'s auto-revert sweep runs *inside* that daemon, so a
    ///    stale/absent heartbeat means nothing is left to keep the linked
    ///    wake_attempts row honest -- treating a daemon-down attempt as
    ///    live would let a genuinely dead delegation coast on a frozen
    ///    "running" row forever.
    /// 2. The card's linked wake_attempts row (via `get_wake_attempt_by_card`)
    ///    exists and is in an in-flight FSM state.
    ///
    /// Absence of either signal reads as NOT live -- this is a fail-closed
    /// predicate by construction (`Option::None` and a stale heartbeat both
    /// fall through to `Ok(false)`, never `Ok(true)`), because it backs the
    /// entry gate, the auto-revert sweep, AND the stop.sh last-line-of-
    /// defense check -- an ambiguous "maybe" must never read as "safe to
    /// stop."
    pub fn delegated_card_is_live(&self, card_id: &str, stale_after_secs: u64) -> Result<bool> {
        if !self.watch_heartbeat_alive(stale_after_secs)? {
            return Ok(false);
        }
        match self.get_wake_attempt_by_card(card_id)? {
            Some(attempt) => Ok(attempt.state.is_in_flight()),
            None => Ok(false),
        }
    }
}

impl Database {
    /// Return the N most recent wake_attempts rows, ordered newest-first.
    ///
    /// Used by `legion watch status` to show a terse wake activity summary.
    /// All states are included (terminal and in-flight) so the operator sees
    /// the full recent history, not just live wakes.
    pub fn recent_wake_attempts(
        &self,
        limit: u32,
    ) -> Result<Vec<crate::wake_attempts::WakeAttempt>> {
        let sql = format!(
            "SELECT {WAKE_ATTEMPT_COLUMNS} FROM wake_attempts \
             WHERE deleted_at IS NULL \
             ORDER BY updated_at DESC \
             LIMIT ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Ok(stmt
            .query_map(rusqlite::params![limit as i64], map_wake_attempt_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;

    // -- Persona wake lease tests -------------------------------------------

    use std::time::Duration;

    #[test]
    fn persona_lease_acquire_succeeds_when_free() {
        let db = test_db();
        let got = db
            .try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        assert!(
            got,
            "first acquire on a free (persona, signal) must succeed"
        );
    }

    #[test]
    fn persona_lease_acquire_fails_when_held_by_another_host() {
        let db = test_db();
        assert!(
            db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
                .unwrap()
        );
        let got = db
            .try_acquire_persona_lease("legion", "sig-1", "hostB", Duration::from_secs(60))
            .unwrap();
        assert!(
            !got,
            "second acquire on a live lease must report 'held' (false)"
        );

        let listed = db.list_persona_leases(Some("legion")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].acquired_by_host, "hostA",
            "hostA's lease must remain untouched by hostB's failed acquire"
        );
    }

    #[test]
    fn persona_lease_acquire_succeeds_after_release() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        let released = db.release_persona_lease("legion", "sig-1").unwrap();
        assert!(released, "release of a live lease must report true");

        let got = db
            .try_acquire_persona_lease("legion", "sig-1", "hostB", Duration::from_secs(60))
            .unwrap();
        assert!(got, "acquire after release must succeed");
    }

    #[test]
    fn persona_lease_release_is_idempotent() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        assert!(db.release_persona_lease("legion", "sig-1").unwrap());
        assert!(
            !db.release_persona_lease("legion", "sig-1").unwrap(),
            "second release of the same lease must report false (already released)"
        );
    }

    #[test]
    fn persona_lease_acquire_succeeds_after_expiry() {
        let db = test_db();
        // TTL of 0 seconds -> lease expires immediately.
        assert!(
            db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(0))
                .unwrap()
        );
        // Sleep long enough that the clock advances past `expires_at`.
        std::thread::sleep(Duration::from_millis(10));
        let got = db
            .try_acquire_persona_lease("legion", "sig-1", "hostB", Duration::from_secs(60))
            .unwrap();
        assert!(
            got,
            "acquire against an expired lease must succeed (hostB takes over)"
        );

        let listed = db.list_persona_leases(Some("legion")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].acquired_by_host, "hostB",
            "the fresh lease must be owned by the reacquirer"
        );
    }

    #[test]
    fn persona_lease_heartbeat_extends_expiry() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        let before = db.list_persona_leases(Some("legion")).unwrap().remove(0);

        std::thread::sleep(Duration::from_millis(20));
        let n = db
            .heartbeat_persona_leases("hostA", Duration::from_secs(3600))
            .unwrap();
        assert_eq!(n, 1, "heartbeat should touch exactly hostA's live lease");

        let after = db.list_persona_leases(Some("legion")).unwrap().remove(0);
        assert!(
            after.expires_at > before.expires_at,
            "heartbeat must push expires_at forward (before: {}, after: {})",
            before.expires_at,
            after.expires_at
        );
        assert!(
            after.heartbeat_at > before.heartbeat_at,
            "heartbeat must advance heartbeat_at"
        );
    }

    #[test]
    fn persona_lease_heartbeat_skips_foreign_hosts() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        let n = db
            .heartbeat_persona_leases("hostB", Duration::from_secs(3600))
            .unwrap();
        assert_eq!(n, 0, "heartbeat must only touch the caller's leases");
    }

    #[test]
    fn persona_lease_list_filters_by_persona() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        db.try_acquire_persona_lease("huttspawn", "sig-2", "hostA", Duration::from_secs(60))
            .unwrap();

        let all = db.list_persona_leases(None).unwrap();
        assert_eq!(all.len(), 2);

        let legion_only = db.list_persona_leases(Some("legion")).unwrap();
        assert_eq!(legion_only.len(), 1);
        assert_eq!(legion_only[0].persona_id, "legion");
    }

    #[test]
    fn persona_lease_list_omits_expired() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(0))
            .unwrap();
        std::thread::sleep(Duration::from_millis(10));
        let listed = db.list_persona_leases(None).unwrap();
        assert!(listed.is_empty(), "expired leases must not appear in list");
    }

    #[test]
    fn persona_lease_list_omits_tombstones() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        db.release_persona_lease("legion", "sig-1").unwrap();
        let listed = db.list_persona_leases(None).unwrap();
        assert!(listed.is_empty(), "released leases must not appear in list");
    }

    #[test]
    fn persona_lease_release_and_list_agree_on_expired_lease() {
        // #679: the list and release paths must share one definition of
        // "live". Before the fix, `release_persona_lease` ignored
        // `expires_at` and would happily soft-delete (and report `true` for)
        // an expired-but-undeleted row that `list_persona_leases` had
        // already stopped displaying -- the exact display-vs-release-path
        // mismatch the issue reported. An expired lease must read as
        // "not live" to BOTH paths, not just the list.
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(0))
            .unwrap();
        std::thread::sleep(Duration::from_millis(10));

        // List already agrees the lease is gone.
        assert!(
            db.list_persona_leases(None).unwrap().is_empty(),
            "expired lease must not appear in list"
        );

        // Release must agree: an expired lease is already "not live", so
        // there is nothing live left to release.
        let released = db.release_persona_lease("legion", "sig-1").unwrap();
        assert!(
            !released,
            "release must report 'no live lease found' for an expired lease, \
             matching what the list already shows"
        );
    }

    #[test]
    fn persona_lease_release_if_owner_finalizes_an_already_expired_lease() {
        // PR #764 review (regression from the #679 fix above): unlike
        // `release_persona_lease`, the reaper's owner-scoped release must
        // NOT agree with `list_persona_leases` on an already-expired lease --
        // it must finalize (soft-delete) it regardless, so the row is gone
        // by the time the same tick's `heartbeat_persona_leases` runs. If
        // this returned `false` here (matching list's "not live" view) the
        // row would stay `deleted_at IS NULL` and the next heartbeat would
        // push `expires_at` forward, resurrecting it as a permanent ghost.
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(0))
            .unwrap();
        std::thread::sleep(Duration::from_millis(10));

        // List already treats it as gone (per #679) ...
        assert!(db.list_persona_leases(None).unwrap().is_empty());

        // ... but the reaper's finalization write must still succeed against
        // the still-undeleted row.
        let released = db
            .release_persona_lease_if_owner("legion", "sig-1", "hostA")
            .unwrap();
        assert!(
            released,
            "owner-scoped release must finalize an expired-but-undeleted \
             lease unconditionally, decoupled from list's liveness view"
        );
    }

    #[test]
    fn persona_lease_reap_then_heartbeat_does_not_resurrect_expired_lease() {
        // Regression test for the PR #764 review finding: reap
        // (`release_persona_lease_if_owner`) followed by heartbeat
        // (`heartbeat_persona_leases`) on the same `tick_health` cycle --
        // exactly `WatchLoop::tick_health`'s order -- must not bring an
        // expired, reaper-finalized lease back to life.
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-ghost", "hostA", Duration::from_secs(0))
            .unwrap();
        std::thread::sleep(Duration::from_millis(10));

        // The lease has expired but is still `deleted_at IS NULL` at this
        // point -- exactly the state the reaper finds it in.
        assert!(
            db.list_persona_leases(None).unwrap().is_empty(),
            "expired lease must already read as not-live before reap runs"
        );

        // Step 1: reap finalizes its own row (as `AgentTracker::reap_finished`
        // does via `held_leases`).
        assert!(
            db.release_persona_lease_if_owner("legion", "sig-ghost", "hostA")
                .unwrap(),
            "reap must finalize the expired lease it owns"
        );

        // Step 2: the same tick's heartbeat sweeps every live lease for this
        // host. Before the fix this would resurrect the just-reaped row.
        db.heartbeat_persona_leases("hostA", Duration::from_secs(60))
            .unwrap();

        assert!(
            db.list_persona_leases(None)
                .unwrap()
                .iter()
                .all(|l| l.signal_id != "sig-ghost"),
            "a reaped lease must not reappear in list_persona_leases after \
             the next heartbeat -- resurrection would make it a permanent ghost"
        );
    }

    #[test]
    fn persona_lease_never_renders_as_live_after_release_or_expiry() {
        // Direct acceptance-criteria test (#679): drive a lease through both
        // terminal paths (explicit release, and TTL expiry) and assert the
        // list never shows it as live in either case.
        let db = test_db();

        // Path 1: explicit release.
        db.try_acquire_persona_lease("legion", "sig-released", "hostA", Duration::from_secs(60))
            .unwrap();
        assert!(db.release_persona_lease("legion", "sig-released").unwrap());
        assert!(
            db.list_persona_leases(Some("legion"))
                .unwrap()
                .iter()
                .all(|l| l.signal_id != "sig-released"),
            "a released lease must never render as live"
        );

        // Path 2: TTL expiry, no explicit release.
        db.try_acquire_persona_lease("legion", "sig-expired", "hostA", Duration::from_secs(0))
            .unwrap();
        std::thread::sleep(Duration::from_millis(10));
        assert!(
            db.list_persona_leases(Some("legion"))
                .unwrap()
                .iter()
                .all(|l| l.signal_id != "sig-expired"),
            "an expired lease must never render as live"
        );
    }

    #[test]
    fn persona_lease_release_by_host_clears_all_host_leases() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "hostA", Duration::from_secs(60))
            .unwrap();
        db.try_acquire_persona_lease("huttspawn", "sig-2", "hostA", Duration::from_secs(60))
            .unwrap();
        db.try_acquire_persona_lease("kessel", "sig-3", "hostB", Duration::from_secs(60))
            .unwrap();

        let cleared = db.release_persona_leases_by_host("hostA").unwrap();
        assert_eq!(cleared, 2, "must release exactly hostA's two leases");

        let remaining = db.list_persona_leases(None).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].acquired_by_host, "hostB");
    }

    #[test]
    fn persona_lease_acquire_succeeds_after_ttl_expires_without_release() {
        // Crash-recovery path: the holder acquires with a short TTL, never
        // calls release (simulating a crash), and after the TTL elapses the
        // next acquirer succeeds. This is the behavior the issue calls out:
        // "session crashes without releasing -> lease expires via heartbeat
        // TTL. Another wake on the same signal succeeds after expiration."
        let db = test_db();
        assert!(
            db.try_acquire_persona_lease(
                "legion",
                "sig-1",
                "crashy-host",
                Duration::from_millis(100)
            )
            .unwrap()
        );

        // While the lease is still live, a second acquire must fail.
        assert!(
            !db.try_acquire_persona_lease(
                "legion",
                "sig-1",
                "recovery-host",
                Duration::from_secs(3600)
            )
            .unwrap(),
            "live lease (even near expiry) must block a concurrent acquire"
        );

        // Wait past the TTL without calling release.
        std::thread::sleep(Duration::from_millis(200));

        assert!(
            db.try_acquire_persona_lease(
                "legion",
                "sig-1",
                "recovery-host",
                Duration::from_secs(3600)
            )
            .unwrap(),
            "after TTL elapses, a new acquirer must succeed"
        );

        let listed = db.list_persona_leases(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].acquired_by_host, "recovery-host",
            "recovery host must own the post-crash lease"
        );
    }

    #[test]
    fn persona_lease_acquire_is_cross_connection_race_safe() {
        // Issue #308 atomicity contract: two independent Database handles
        // against the same file race to acquire the same (persona, signal).
        // Each thread opens its own handle (Database is !Send because it
        // wraps rusqlite::Connection; ownership stays thread-local). Exactly
        // one must win; neither can surface SQLITE_BUSY as Err.
        use std::thread;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("race.sqlite");

        // Prime the schema once so neither racing thread takes the migration
        // path (what's being tested is acquire atomicity, not open atomicity).
        let _ = Database::open(&db_path).unwrap();

        let path_a = db_path.clone();
        let path_b = db_path.clone();

        let t_a = thread::spawn(move || -> Result<bool> {
            let db = Database::open(&path_a)?;
            db.try_acquire_persona_lease("legion", "sig-race", "host-A", Duration::from_secs(60))
        });
        let t_b = thread::spawn(move || -> Result<bool> {
            let db = Database::open(&path_b)?;
            db.try_acquire_persona_lease("legion", "sig-race", "host-B", Duration::from_secs(60))
        });

        let r_a = t_a.join().unwrap();
        let r_b = t_b.join().unwrap();

        let mut wins = 0usize;
        for r in [&r_a, &r_b] {
            match r {
                Ok(true) => wins += 1,
                Ok(false) => {}
                Err(e) => panic!("acquire surfaced SQLITE_BUSY as Err: {e}"),
            }
        }
        assert_eq!(
            wins, 1,
            "exactly one concurrent acquire must win (got {} winners)",
            wins
        );

        let observer = Database::open(&db_path).unwrap();
        let listed = observer.list_persona_leases(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(
            listed[0].acquired_by_host == "host-A" || listed[0].acquired_by_host == "host-B",
            "unexpected host recorded: {}",
            listed[0].acquired_by_host
        );
    }

    #[test]
    fn persona_lease_release_if_owner_refuses_foreign_host() {
        // Guards the late-loser reaper scenario: after sync conflict
        // resolution overwrites local's row with peer's, local's AgentTracker
        // will try to reap and release the lease it thought it held. The
        // host-scoped release must refuse because the row now belongs to
        // peer, preventing the late-loser from dropping the winner's lease.
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "peer", Duration::from_secs(60))
            .unwrap();

        let released = db
            .release_persona_lease_if_owner("legion", "sig-1", "late-loser")
            .unwrap();
        assert!(
            !released,
            "host-scoped release must refuse to touch a row owned by another host"
        );

        let listed = db.list_persona_leases(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].acquired_by_host, "peer",
            "peer's lease must survive the late-loser's release attempt"
        );
    }

    // -- wake_attempts (#487) ------------------------------------------------

    fn wake_id(tag: &str) -> String {
        format!("attempt-{}-{}", tag, uuid::Uuid::now_v7())
    }

    #[test]
    fn wake_attempts_migration_creates_table() {
        let db = test_db();
        // sqlite_master lookup is a stable way to assert the table
        // and indices applied without rebinding to private types.
        let exists: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='wake_attempts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1);
        let indices: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name IN ('idx_wake_attempts_host_state', 'idx_wake_attempts_persona_recent')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indices, 2);
    }

    /// Mirrors `migration_document_id_column_is_idempotent_on_reopen`
    /// (src/db/kanban.rs): a real file-backed DB re-opened a second time
    /// must not fail the has_column-guarded ALTER for `wake_attempts.card_id`.
    #[test]
    fn wake_attempts_card_id_migration_is_idempotent_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_card_id_idempotent.db");

        let db1 = Database::open(&db_path).unwrap();
        let id = wake_id("reopen");
        db1.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        drop(db1);

        let db2 = Database::open(&db_path).expect("second open must not fail");
        let row = db2
            .get_wake_attempt(&id)
            .unwrap()
            .expect("row survives reopen");
        assert!(row.card_id.is_none(), "card_id defaults to NULL");
    }

    #[test]
    fn enqueue_wake_attempt_inserts_queued_row() {
        use crate::wake_attempts::WakeAttemptState;
        let db = test_db();
        let id = wake_id("enqueue");
        db.enqueue_wake_attempt(&id, "legion", "legion", &["sig-1".to_string()])
            .unwrap();
        let row = db.get_wake_attempt(&id).unwrap().expect("row exists");
        assert_eq!(row.state, WakeAttemptState::Queued);
        assert_eq!(row.persona_id, "legion");
        assert_eq!(row.repo_name, "legion");
        assert_eq!(row.signal_ids, vec!["sig-1".to_string()]);
        assert!(row.acquired_by_host.is_none());
    }

    #[test]
    fn try_claim_wake_attempt_is_atomic_one_winner() {
        let db = test_db();
        let id = wake_id("claim");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();

        let host_a_won = db.try_claim_wake_attempt(&id, "host-a").unwrap();
        let host_b_won = db.try_claim_wake_attempt(&id, "host-b").unwrap();
        // First claimer wins; second sees state != queued and returns false.
        assert!(host_a_won, "first host must win the claim");
        assert!(!host_b_won, "second claim must return false");

        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.acquired_by_host.as_deref(), Some("host-a"));
        assert_eq!(row.state, crate::wake_attempts::WakeAttemptState::Claimed);
    }

    #[test]
    fn try_claim_wake_attempt_rejects_already_claimed() {
        let db = test_db();
        let id = wake_id("re-claim");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        assert!(db.try_claim_wake_attempt(&id, "host-a").unwrap());
        // Same host trying twice still loses -- claim is one-shot.
        assert!(!db.try_claim_wake_attempt(&id, "host-a").unwrap());
    }

    #[test]
    fn try_claim_wake_attempt_returns_false_for_unknown_row() {
        let db = test_db();
        assert!(!db.try_claim_wake_attempt("no-such-id", "host-a").unwrap());
    }

    #[test]
    fn transition_wake_attempt_walks_happy_path() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let id = wake_id("happy");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        assert!(db.try_claim_wake_attempt(&id, "host-a").unwrap());

        db.transition_wake_attempt(&id, Claimed, Spawning).unwrap();
        db.transition_wake_attempt(&id, Spawning, Running).unwrap();
        db.transition_wake_attempt(&id, Running, Exiting).unwrap();
        db.transition_wake_attempt(&id, Exiting, Done).unwrap();

        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.state, Done);
    }

    #[test]
    fn transition_wake_attempt_rejects_illegal_pair() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let id = wake_id("illegal");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        assert!(db.try_claim_wake_attempt(&id, "host-a").unwrap());

        // Claimed -> Done is not in the table.
        let err = db
            .transition_wake_attempt(&id, Claimed, Done)
            .expect_err("illegal transition must error");
        match err {
            LegionError::IllegalWakeAttemptTransition {
                attempt_id,
                from,
                to,
                ..
            } => {
                assert_eq!(attempt_id, id);
                assert_eq!(from, "claimed");
                assert_eq!(to, "done");
            }
            other => panic!("expected IllegalWakeAttemptTransition, got {other:?}"),
        }
    }

    #[test]
    fn transition_wake_attempt_rejects_stale_from() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let id = wake_id("stale");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        assert!(db.try_claim_wake_attempt(&id, "host-a").unwrap());
        db.transition_wake_attempt(&id, Claimed, Spawning).unwrap();
        // Caller still thinks the row is `Claimed` but the DB has moved on.
        let err = db
            .transition_wake_attempt(&id, Claimed, Spawning)
            .expect_err("stale `from` must error");
        match err {
            LegionError::IllegalWakeAttemptTransition { current, .. } => {
                assert_eq!(current, "spawning");
            }
            other => panic!("expected IllegalWakeAttemptTransition, got {other:?}"),
        }
    }

    #[test]
    fn transition_wake_attempt_errors_on_missing_row() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let err = db
            .transition_wake_attempt("no-such", Queued, Claimed)
            .expect_err("missing row");
        assert!(matches!(err, LegionError::WakeAttemptNotFound(_)));
    }

    #[test]
    fn record_wake_attempt_outcome_sets_terminal_and_status() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let id = wake_id("outcome-ok");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        db.transition_wake_attempt(&id, Claimed, Spawning).unwrap();
        db.transition_wake_attempt(&id, Spawning, Running).unwrap();

        db.record_wake_attempt_outcome(&id, "ok", "productive")
            .unwrap();
        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.state, Done);
        assert_eq!(row.exit_status.as_deref(), Some("ok"));
        assert_eq!(row.outcome.as_deref(), Some("productive"));
        assert!(row.exited_at.is_some());
    }

    #[test]
    fn record_wake_attempt_outcome_maps_error_to_failed() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();
        let id = wake_id("outcome-fail");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        db.record_wake_attempt_outcome(&id, "killed", "errored")
            .unwrap();
        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.state, Failed);
    }

    #[test]
    fn record_wake_attempt_outcome_leaves_terminal_rows_alone() {
        // Late stop hook + already-settled row must not rewrite the
        // outcome. Terminal-is-sticky is the FSM invariant the
        // transition table protects; record_outcome must respect it
        // and surface the rejection as IllegalWakeAttemptTransition
        // with the actual current state, not WakeAttemptNotFound
        // (which would invite a retry loop on a real corruption).
        let db = test_db();
        let id = wake_id("sticky");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        db.record_wake_attempt_outcome(&id, "ok", "productive")
            .unwrap();
        let err = db
            .record_wake_attempt_outcome(&id, "killed", "errored")
            .expect_err("terminal row must reject re-stamp");
        match err {
            LegionError::IllegalWakeAttemptTransition { current, .. } => {
                assert_eq!(current, "done");
            }
            other => {
                panic!("expected IllegalWakeAttemptTransition with current=done, got {other:?}")
            }
        }
        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.exit_status.as_deref(), Some("ok"));
        assert_eq!(row.outcome.as_deref(), Some("productive"));
    }

    #[test]
    fn list_local_orphans_is_strictly_host_scoped() {
        use crate::wake_attempts::WakeAttemptState::*;
        let db = test_db();

        // This host: one in-flight + one terminal (terminal must NOT appear).
        let local_inflight = wake_id("local-inflight");
        db.enqueue_wake_attempt(&local_inflight, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&local_inflight, "this-host")
            .unwrap();
        db.transition_wake_attempt(&local_inflight, Claimed, Spawning)
            .unwrap();

        let local_done = wake_id("local-done");
        db.enqueue_wake_attempt(&local_done, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&local_done, "this-host").unwrap();
        db.record_wake_attempt_outcome(&local_done, "ok", "productive")
            .unwrap();

        // Peer host: in-flight on a DIFFERENT host must NOT appear here.
        let peer_inflight = wake_id("peer-inflight");
        db.enqueue_wake_attempt(&peer_inflight, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&peer_inflight, "other-host")
            .unwrap();

        let orphans = db.list_local_orphans("this-host").unwrap();
        let ids: Vec<&str> = orphans.iter().map(|r| r.attempt_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![local_inflight.as_str()],
            "only this-host's in-flight row should be returned"
        );
    }

    #[test]
    fn set_wake_attempt_pid_records_pid_and_spawned_at() {
        let db = test_db();
        let id = wake_id("pid");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.set_wake_attempt_pid(&id, 12345).unwrap();
        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.spawned_pid, Some(12345));
        assert!(row.spawned_at.is_some());
    }

    #[test]
    fn mark_wake_attempt_exit_observed_writes_timestamp() {
        let db = test_db();
        let id = wake_id("exit-obs");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.mark_wake_attempt_exit_observed(&id).unwrap();
        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert!(row.exit_observed_at.is_some());
    }

    #[test]
    fn get_wake_attempt_returns_none_for_missing() {
        let db = test_db();
        assert!(db.get_wake_attempt("no-such").unwrap().is_none());
    }

    #[test]
    fn record_wake_attempt_outcome_rejects_unknown_exit_status() {
        let db = test_db();
        let id = wake_id("bad-status");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        let err = db
            .record_wake_attempt_outcome(&id, "purple", "productive")
            .expect_err("unknown exit_status must error");
        assert!(matches!(
            err,
            LegionError::IllegalWakeAttemptTransition { .. }
        ));
    }

    #[test]
    fn set_wake_attempt_pid_errors_on_missing_row() {
        let db = test_db();
        let err = db
            .set_wake_attempt_pid("no-such", 1234)
            .expect_err("missing row must error");
        assert!(matches!(err, LegionError::WakeAttemptNotFound(_)));
    }

    #[test]
    fn mark_wake_attempt_exit_observed_errors_on_missing_row() {
        let db = test_db();
        let err = db
            .mark_wake_attempt_exit_observed("no-such")
            .expect_err("missing row must error");
        assert!(matches!(err, LegionError::WakeAttemptNotFound(_)));
    }

    // -- card_id linkage + delegated liveness (#778) -------------------------

    #[test]
    fn set_wake_attempt_card_links_and_is_readable_back() {
        let db = test_db();
        let id = wake_id("link");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();

        db.set_wake_attempt_card(&id, "card-1").unwrap();

        let row = db.get_wake_attempt(&id).unwrap().expect("row");
        assert_eq!(row.card_id.as_deref(), Some("card-1"));

        let by_card = db
            .get_wake_attempt_by_card("card-1")
            .unwrap()
            .expect("linked row found by card_id");
        assert_eq!(by_card.attempt_id, id);
    }

    #[test]
    fn set_wake_attempt_card_errors_on_missing_row() {
        let db = test_db();
        let err = db
            .set_wake_attempt_card("no-such", "card-1")
            .expect_err("missing row must error");
        assert!(matches!(err, LegionError::WakeAttemptNotFound(_)));
    }

    #[test]
    fn get_wake_attempt_by_card_returns_none_when_unlinked() {
        let db = test_db();
        let id = wake_id("unlinked");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        assert!(db.get_wake_attempt_by_card("card-1").unwrap().is_none());
    }

    #[test]
    fn find_live_wake_attempt_for_repo_finds_unclaimed_in_flight_row() {
        let db = test_db();
        let id = wake_id("find");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();

        let found = db
            .find_live_wake_attempt_for_repo("legion")
            .unwrap()
            .expect("in-flight, unclaimed-by-card row must be found");
        assert_eq!(found.attempt_id, id);
    }

    #[test]
    fn find_live_wake_attempt_for_repo_skips_rows_already_linked_to_a_card() {
        let db = test_db();
        let id = wake_id("already-linked");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        db.set_wake_attempt_card(&id, "card-1").unwrap();

        assert!(
            db.find_live_wake_attempt_for_repo("legion")
                .unwrap()
                .is_none(),
            "a row already claimed by a card must not be handed out again"
        );
    }

    #[test]
    fn find_live_wake_attempt_for_repo_skips_terminal_rows() {
        let db = test_db();
        let id = wake_id("terminal");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        db.record_wake_attempt_outcome(&id, "ok", "productive")
            .unwrap();

        assert!(
            db.find_live_wake_attempt_for_repo("legion")
                .unwrap()
                .is_none(),
            "a terminal row is not live"
        );
    }

    #[test]
    fn delegated_card_is_live_false_when_daemon_heartbeat_absent() {
        let db = test_db();
        let id = wake_id("no-heartbeat");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        db.set_wake_attempt_card(&id, "card-1").unwrap();

        // Attempt is genuinely in-flight, but no watch daemon has ever
        // beaten -- the predicate must still read false (fail closed):
        // nothing is left to keep the row honest.
        assert!(!db.delegated_card_is_live("card-1", 120).unwrap());
    }

    #[test]
    fn delegated_card_is_live_true_when_both_halves_hold() {
        let db = test_db();
        db.upsert_watch_heartbeat("host-a", 1, "0.1.0", 1, None)
            .unwrap();
        let id = wake_id("live");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        db.set_wake_attempt_card(&id, "card-1").unwrap();

        assert!(db.delegated_card_is_live("card-1", 120).unwrap());
    }

    #[test]
    fn delegated_card_is_live_false_when_attempt_went_terminal() {
        let db = test_db();
        db.upsert_watch_heartbeat("host-a", 1, "0.1.0", 1, None)
            .unwrap();
        let id = wake_id("died");
        db.enqueue_wake_attempt(&id, "legion", "legion", &[])
            .unwrap();
        db.try_claim_wake_attempt(&id, "host-a").unwrap();
        db.set_wake_attempt_card(&id, "card-1").unwrap();
        db.record_wake_attempt_outcome(&id, "error", "died")
            .unwrap();

        assert!(
            !db.delegated_card_is_live("card-1", 120).unwrap(),
            "a heartbeat-alive daemon with a terminal attempt is still not live"
        );
    }

    #[test]
    fn delegated_card_is_live_false_when_card_has_no_linked_attempt() {
        let db = test_db();
        db.upsert_watch_heartbeat("host-a", 1, "0.1.0", 1, None)
            .unwrap();
        assert!(!db.delegated_card_is_live("no-such-card", 120).unwrap());
    }

    #[test]
    fn recent_wake_attempts_returns_latest_first() {
        let db = test_db();

        // Seed three wake attempts with different updated_at values.
        let now = chrono::Utc::now();
        let ts1 = (now - chrono::Duration::minutes(10)).to_rfc3339();
        let ts2 = (now - chrono::Duration::minutes(5)).to_rfc3339();
        let ts3 = now.to_rfc3339();

        for (id, ts) in [("att-a", &ts1), ("att-b", &ts2), ("att-c", &ts3)] {
            db.conn
                .execute(
                    "INSERT INTO wake_attempts \
                     (attempt_id, persona_id, repo_name, signal_ids, state, updated_at) \
                     VALUES (?1, 'p', 'r', '[]', 'queued', ?2)",
                    rusqlite::params![id, ts],
                )
                .unwrap();
        }

        let recent = db.recent_wake_attempts(2).unwrap();
        assert_eq!(recent.len(), 2, "limit=2 returns 2 rows");
        assert_eq!(recent[0].attempt_id, "att-c", "newest first");
        assert_eq!(recent[1].attempt_id, "att-b");
    }
}
