//! Multi-node sync registry: every `get_*_deltas_since` getter, every
//! `apply_*_delta` merge, the effective-timestamp rule, and tombstone
//! cleanup. This file owns the which-tables-sync list -- adding a synced
//! table means adding its getter and apply here.

use chrono::Utc;
use rusqlite::OptionalExtension;

use super::Database;
use crate::error::{LegionError, Result};
use crate::sync::{
    CardDelta, ReflectionDelta, ScheduleDelta, UncertaintyCalibrationSnapshotDelta,
    UncertaintyPredictionDelta,
};

/// Result of tombstone cleanup operation.
#[derive(Debug, Default)]
pub struct TombstoneCleanupResult {
    pub reflections: u64,
    pub tasks: u64,
    pub schedules: u64,
    pub uncertainty_predictions: u64,
    pub uncertainty_calibration_snapshots: u64,
}

impl TombstoneCleanupResult {
    pub fn total(&self) -> u64 {
        self.reflections
            + self.tasks
            + self.schedules
            + self.uncertainty_predictions
            + self.uncertainty_calibration_snapshots
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

impl Database {
    /// Get reflection deltas for multi-node sync.
    ///
    /// Returns all reflections that have been modified or soft-deleted since
    /// the given timestamp. Used for delta synchronization between legion nodes.
    ///
    /// The query includes:
    /// - Live rows where updated_at > since (modifications)
    /// - Soft-deleted rows where deleted_at > since (tombstones)
    ///
    /// Excludes embedding column since each node computes its own embeddings.
    #[allow(dead_code)] // Used by sync broadcast in #248
    pub fn get_reflection_deltas_since(&self, since: &str) -> Result<Vec<ReflectionDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, text, created_at, updated_at, deleted_at, audience, domain, tags, \
             recall_count, last_recalled_at, parent_id \
             FROM reflections \
             WHERE updated_at > ?1 OR deleted_at > ?1 \
             ORDER BY COALESCE(updated_at, deleted_at) ASC",
        )?;

        let rows = stmt.query_map([since], |row| {
            Ok(ReflectionDelta {
                id: row.get(0)?,
                repo: row.get(1)?,
                text: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                deleted_at: row.get(5)?,
                audience: row.get(6)?,
                domain: row.get(7)?,
                tags: row.get(8)?,
                recall_count: row.get(9)?,
                last_recalled_at: row.get(10)?,
                parent_id: row.get(11)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get card deltas for multi-node sync.
    ///
    /// Returns all cards (tasks table) that have been modified or soft-deleted
    /// since the given timestamp. Used for delta synchronization between nodes.
    #[allow(dead_code)] // Used by sync broadcast in #249
    pub fn get_card_deltas_since(&self, since: &str) -> Result<Vec<CardDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_repo, to_repo, text, context, priority, status, note, \
             labels, parent_card_id, source_url, source_type, sort_order, \
             created_at, updated_at, deleted_at, assigned_at, started_at, completed_at, \
             problem, solution, acceptance \
             FROM tasks \
             WHERE updated_at > ?1 OR deleted_at > ?1 \
             ORDER BY COALESCE(updated_at, deleted_at) ASC",
        )?;

        let rows = stmt.query_map([since], |row| {
            Ok(CardDelta {
                id: row.get(0)?,
                from_repo: row.get(1)?,
                to_repo: row.get(2)?,
                text: row.get(3)?,
                context: row.get(4)?,
                priority: row.get(5)?,
                status: row.get(6)?,
                note: row.get(7)?,
                labels: row.get(8)?,
                parent_card_id: row.get(9)?,
                source_url: row.get(10)?,
                source_type: row.get(11)?,
                sort_order: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
                deleted_at: row.get(15)?,
                assigned_at: row.get(16)?,
                started_at: row.get(17)?,
                completed_at: row.get(18)?,
                problem: row.get(19)?,
                solution: row.get(20)?,
                acceptance: row.get(21)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get schedule deltas for multi-node sync.
    ///
    /// Returns all schedules that have been modified or soft-deleted since
    /// the given timestamp. Used for delta synchronization between nodes.
    #[allow(dead_code)] // Used by sync broadcast in #249
    pub fn get_schedule_deltas_since(&self, since: &str) -> Result<Vec<ScheduleDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, cron, command, repo, enabled, last_run, next_run, \
             created_at, updated_at, deleted_at, active_start, active_end \
             FROM schedules \
             WHERE updated_at > ?1 OR deleted_at > ?1 \
             ORDER BY COALESCE(updated_at, deleted_at) ASC",
        )?;

        let rows = stmt.query_map([since], |row| {
            let enabled_int: i32 = row.get(5)?;
            Ok(ScheduleDelta {
                id: row.get(0)?,
                name: row.get(1)?,
                cron: row.get(2)?,
                command: row.get(3)?,
                repo: row.get(4)?,
                enabled: enabled_int != 0,
                last_run: row.get(6)?,
                next_run: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                deleted_at: row.get(10)?,
                active_start: row.get(11)?,
                active_end: row.get(12)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get uncertainty prediction deltas for multi-node sync.
    ///
    /// Returns all uncertainty_prediction rows modified or soft-deleted since
    /// the given timestamp. Predictions transition through emitted -> witnessed
    /// -> calibrated -> orphaned -> retired, so updated_at advances on every
    /// state change.
    #[allow(dead_code)] // Wired into sync broadcast once #358 hooks land.
    pub fn get_uncertainty_prediction_deltas_since(
        &self,
        since: &str,
    ) -> Result<Vec<UncertaintyPredictionDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, surface, feature_key, input_fingerprint, model, model_version, \
             claimed_confidence, prediction_payload, state, outcome_label, outcome_payload, \
             outcome_correctness, cohort_key, created_at, updated_at, witnessed_at, \
             orphan_after, deleted_at \
             FROM uncertainty_prediction \
             WHERE updated_at > ?1 OR deleted_at > ?1 \
             ORDER BY COALESCE(updated_at, deleted_at) ASC",
        )?;

        let rows = stmt.query_map([since], |row| {
            Ok(UncertaintyPredictionDelta {
                id: row.get(0)?,
                surface: row.get(1)?,
                feature_key: row.get(2)?,
                input_fingerprint: row.get(3)?,
                model: row.get(4)?,
                model_version: row.get(5)?,
                claimed_confidence: row.get(6)?,
                prediction_payload: row.get(7)?,
                state: row.get(8)?,
                outcome_label: row.get(9)?,
                outcome_payload: row.get(10)?,
                outcome_correctness: row.get(11)?,
                cohort_key: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
                witnessed_at: row.get(15)?,
                orphan_after: row.get(16)?,
                deleted_at: row.get(17)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Get uncertainty calibration snapshot deltas for multi-node sync.
    ///
    /// Each node computes its own snapshots from synced predictions; sync
    /// propagates rows so peers can compare drift across the cluster without
    /// re-running the roller. LWW keyed by id with updated_at as tiebreaker.
    #[allow(dead_code)] // Wired into sync broadcast once #359 daemon roller lands.
    pub fn get_uncertainty_calibration_snapshot_deltas_since(
        &self,
        since: &str,
    ) -> Result<Vec<UncertaintyCalibrationSnapshotDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, cohort_key, bucket_lower, bucket_upper, claimed_confidence, \
             actual_correctness, actual_correctness_raw, prediction_count, orphan_count, \
             brier_score, computed_at, updated_at, deleted_at \
             FROM uncertainty_calibration_snapshot \
             WHERE updated_at > ?1 OR deleted_at > ?1 \
             ORDER BY COALESCE(updated_at, deleted_at) ASC",
        )?;

        let rows = stmt.query_map([since], |row| {
            Ok(UncertaintyCalibrationSnapshotDelta {
                id: row.get(0)?,
                cohort_key: row.get(1)?,
                bucket_lower: row.get(2)?,
                bucket_upper: row.get(3)?,
                claimed_confidence: row.get(4)?,
                actual_correctness: row.get(5)?,
                actual_correctness_raw: row.get(6)?,
                prediction_count: row.get(7)?,
                orphan_count: row.get(8)?,
                brier_score: row.get(9)?,
                computed_at: row.get(10)?,
                updated_at: row.get(11)?,
                deleted_at: row.get(12)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Delta query for cluster sync. Returns every lease row (including
    /// tombstones) whose `updated_at > since`. Wire transport is not yet live;
    /// `sync_actor` reads this today so the count shows up in broadcast logs,
    /// and late-loser resolution is ready to engage when transport lands.
    #[allow(dead_code)] // wired when broadcast transport ships
    pub fn get_persona_wake_lease_deltas_since(
        &self,
        since: &str,
    ) -> Result<Vec<crate::sync::PersonaWakeLeaseDelta>> {
        let mut stmt = self.conn.prepare(
            "SELECT persona_id, signal_id, acquired_by_host, acquired_at, \
                    heartbeat_at, expires_at, deleted_at, updated_at \
             FROM persona_wake_leases \
             WHERE updated_at > ?1 \
             ORDER BY updated_at ASC",
        )?;
        let deltas = stmt
            .query_map(rusqlite::params![since], |row| {
                Ok(crate::sync::PersonaWakeLeaseDelta {
                    persona_id: row.get(0)?,
                    signal_id: row.get(1)?,
                    acquired_by_host: row.get(2)?,
                    acquired_at: row.get(3)?,
                    heartbeat_at: row.get(4)?,
                    expires_at: row.get(5)?,
                    deleted_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(deltas)
    }

    /// Hard-delete tombstones older than the given number of days.
    ///
    /// Removes soft-deleted rows (where deleted_at IS NOT NULL) that are older
    /// than `retention_days`. Returns a struct with counts of deleted rows per table.
    ///
    /// This is the housekeeper cleanup for multi-node sync. Once tombstones have
    /// propagated to all nodes (typically within hours), they can be permanently
    /// removed to reclaim space. A 30-day retention is recommended.
    pub fn cleanup_tombstones(&self, retention_days: i64) -> Result<TombstoneCleanupResult> {
        let cutoff = (Utc::now() - chrono::Duration::days(retention_days)).to_rfc3339();

        let reflections = self.conn.execute(
            "DELETE FROM reflections WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let tasks = self.conn.execute(
            "DELETE FROM tasks WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let schedules = self.conn.execute(
            "DELETE FROM schedules WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let uncertainty_predictions = self.conn.execute(
            "DELETE FROM uncertainty_prediction WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        let uncertainty_calibration_snapshots = self.conn.execute(
            "DELETE FROM uncertainty_calibration_snapshot \
             WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            [&cutoff],
        )? as u64;

        Ok(TombstoneCleanupResult {
            reflections,
            tasks,
            schedules,
            uncertainty_predictions,
            uncertainty_calibration_snapshots,
        })
    }

    /// Apply an incoming lease delta from a peer. Resolution rules:
    ///
    /// - Tombstone (`deleted_at` set): LWW on `updated_at`. Newer wins.
    /// - Live lease vs. live lease for the same (persona, signal):
    ///   earlier `acquired_at` wins. The late-loser releases its local lease
    ///   so the spawned child is the only handler.
    /// - Live lease vs. no local row: insert the peer's lease as-is.
    ///
    /// Returns `Some(released)` with the locally-held `acquired_by_host` when
    /// this node was the late loser and its lease was downgraded to a
    /// tombstone. Callers can use this to stop the losing spawn.
    #[allow(dead_code)] // wired when broadcast transport ships
    pub fn apply_persona_wake_lease_delta(
        &self,
        delta: &crate::sync::PersonaWakeLeaseDelta,
    ) -> Result<Option<String>> {
        let tx = self.conn.unchecked_transaction()?;

        let local: Option<(String, String, Option<String>, String)> = tx
            .query_row(
                "SELECT acquired_by_host, acquired_at, deleted_at, updated_at \
                 FROM persona_wake_leases \
                 WHERE persona_id = ?1 AND signal_id = ?2",
                rusqlite::params![&delta.persona_id, &delta.signal_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;

        let mut late_loser: Option<String> = None;

        match local {
            None => {
                tx.execute(
                    "INSERT INTO persona_wake_leases \
                     (persona_id, signal_id, acquired_by_host, acquired_at, heartbeat_at, \
                      expires_at, updated_at, deleted_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        &delta.persona_id,
                        &delta.signal_id,
                        &delta.acquired_by_host,
                        &delta.acquired_at,
                        &delta.heartbeat_at,
                        &delta.expires_at,
                        &delta.updated_at,
                        &delta.deleted_at,
                    ],
                )?;
            }
            Some((local_host, local_acquired, local_deleted, local_updated)) => {
                let delta_deleted = delta.deleted_at.is_some();
                let local_is_deleted = local_deleted.is_some();

                if delta_deleted || local_is_deleted {
                    // Tombstone involved: plain LWW on updated_at.
                    if delta.updated_at > local_updated {
                        tx.execute(
                            "UPDATE persona_wake_leases \
                             SET acquired_by_host = ?1, acquired_at = ?2, heartbeat_at = ?3, \
                                 expires_at = ?4, updated_at = ?5, deleted_at = ?6 \
                             WHERE persona_id = ?7 AND signal_id = ?8",
                            rusqlite::params![
                                &delta.acquired_by_host,
                                &delta.acquired_at,
                                &delta.heartbeat_at,
                                &delta.expires_at,
                                &delta.updated_at,
                                &delta.deleted_at,
                                &delta.persona_id,
                                &delta.signal_id,
                            ],
                        )?;
                    }
                } else if delta.acquired_at < local_acquired {
                    // Two live leases -- earlier acquired_at wins, regardless
                    // of updated_at ordering. Local is the late loser.
                    let now = Utc::now().to_rfc3339();
                    tx.execute(
                        "UPDATE persona_wake_leases \
                         SET acquired_by_host = ?1, acquired_at = ?2, heartbeat_at = ?3, \
                             expires_at = ?4, updated_at = ?5, deleted_at = NULL \
                         WHERE persona_id = ?6 AND signal_id = ?7",
                        rusqlite::params![
                            &delta.acquired_by_host,
                            &delta.acquired_at,
                            &delta.heartbeat_at,
                            &delta.expires_at,
                            &now,
                            &delta.persona_id,
                            &delta.signal_id,
                        ],
                    )?;
                    late_loser = Some(local_host);
                }
            }
        }

        tx.commit()?;
        Ok(late_loser)
    }

    /// Apply a peer's reflection delta with last-write-wins merge (#536).
    ///
    /// No local row: INSERT (embedding stays NULL -- each node computes its
    /// own embeddings, see the ReflectionDelta doc). Local row exists: the
    /// delta wins only when its effective timestamp (max of updated_at /
    /// deleted_at, falling back to created_at) is strictly newer than the
    /// local row's. Tombstones ride the same comparison.
    pub fn apply_reflection_delta(&self, delta: &crate::sync::ReflectionDelta) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        let local: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT created_at, updated_at, deleted_at FROM reflections WHERE id = ?1",
                rusqlite::params![&delta.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let delta_ts = effective_sync_ts(&delta.created_at, &delta.updated_at, &delta.deleted_at);
        match local {
            None => {
                tx.execute(
                    "INSERT INTO reflections \
                     (id, repo, text, created_at, updated_at, deleted_at, audience, \
                      domain, tags, recall_count, last_recalled_at, parent_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    rusqlite::params![
                        &delta.id,
                        &delta.repo,
                        &delta.text,
                        &delta.created_at,
                        &delta.updated_at,
                        &delta.deleted_at,
                        &delta.audience,
                        &delta.domain,
                        &delta.tags,
                        &delta.recall_count,
                        &delta.last_recalled_at,
                        &delta.parent_id,
                    ],
                )?;
            }
            Some((local_created, local_updated, local_deleted)) => {
                let local_ts = effective_sync_ts(&local_created, &local_updated, &local_deleted);
                if delta_ts > local_ts {
                    tx.execute(
                        "UPDATE reflections SET repo = ?2, text = ?3, created_at = ?4, \
                         updated_at = ?5, deleted_at = ?6, audience = ?7, domain = ?8, \
                         tags = ?9, recall_count = ?10, last_recalled_at = ?11, parent_id = ?12 \
                         WHERE id = ?1",
                        rusqlite::params![
                            &delta.id,
                            &delta.repo,
                            &delta.text,
                            &delta.created_at,
                            &delta.updated_at,
                            &delta.deleted_at,
                            &delta.audience,
                            &delta.domain,
                            &delta.tags,
                            &delta.recall_count,
                            &delta.last_recalled_at,
                            &delta.parent_id,
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Apply a peer's card delta with last-write-wins merge (#536). Same
    /// LWW rule as [`Self::apply_reflection_delta`].
    pub fn apply_card_delta(&self, delta: &crate::sync::CardDelta) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        let local: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT created_at, updated_at, deleted_at FROM tasks WHERE id = ?1",
                rusqlite::params![&delta.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let updated = Some(delta.updated_at.clone());
        let delta_ts = effective_sync_ts(&delta.created_at, &updated, &delta.deleted_at);
        match local {
            None => {
                tx.execute(
                    "INSERT INTO tasks \
                     (id, from_repo, to_repo, text, context, priority, status, note, labels, \
                      parent_card_id, source_url, source_type, sort_order, created_at, \
                      updated_at, deleted_at, assigned_at, started_at, completed_at, \
                      problem, solution, acceptance) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                             ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
                    rusqlite::params![
                        &delta.id,
                        &delta.from_repo,
                        &delta.to_repo,
                        &delta.text,
                        &delta.context,
                        &delta.priority,
                        &delta.status,
                        &delta.note,
                        &delta.labels,
                        &delta.parent_card_id,
                        &delta.source_url,
                        &delta.source_type,
                        &delta.sort_order,
                        &delta.created_at,
                        &delta.updated_at,
                        &delta.deleted_at,
                        &delta.assigned_at,
                        &delta.started_at,
                        &delta.completed_at,
                        &delta.problem,
                        &delta.solution,
                        &delta.acceptance,
                    ],
                )?;
            }
            Some((local_created, local_updated, local_deleted)) => {
                let local_ts = effective_sync_ts(&local_created, &local_updated, &local_deleted);
                if delta_ts > local_ts {
                    tx.execute(
                        "UPDATE tasks SET from_repo = ?2, to_repo = ?3, text = ?4, context = ?5, \
                         priority = ?6, status = ?7, note = ?8, labels = ?9, parent_card_id = ?10, \
                         source_url = ?11, source_type = ?12, sort_order = ?13, created_at = ?14, \
                         updated_at = ?15, deleted_at = ?16, assigned_at = ?17, started_at = ?18, \
                         completed_at = ?19, problem = ?20, solution = ?21, acceptance = ?22 \
                         WHERE id = ?1",
                        rusqlite::params![
                            &delta.id,
                            &delta.from_repo,
                            &delta.to_repo,
                            &delta.text,
                            &delta.context,
                            &delta.priority,
                            &delta.status,
                            &delta.note,
                            &delta.labels,
                            &delta.parent_card_id,
                            &delta.source_url,
                            &delta.source_type,
                            &delta.sort_order,
                            &delta.created_at,
                            &delta.updated_at,
                            &delta.deleted_at,
                            &delta.assigned_at,
                            &delta.started_at,
                            &delta.completed_at,
                            &delta.problem,
                            &delta.solution,
                            &delta.acceptance,
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Apply a peer's schedule delta with last-write-wins merge (#536). Same
    /// LWW rule as [`Self::apply_reflection_delta`].
    pub fn apply_schedule_delta(&self, delta: &crate::sync::ScheduleDelta) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        let local: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT created_at, updated_at, deleted_at FROM schedules WHERE id = ?1",
                rusqlite::params![&delta.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let delta_ts = effective_sync_ts(&delta.created_at, &delta.updated_at, &delta.deleted_at);
        match local {
            None => {
                tx.execute(
                    "INSERT INTO schedules \
                     (id, name, cron, command, repo, enabled, last_run, next_run, created_at, \
                      updated_at, deleted_at, active_start, active_end) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    rusqlite::params![
                        &delta.id,
                        &delta.name,
                        &delta.cron,
                        &delta.command,
                        &delta.repo,
                        &delta.enabled,
                        &delta.last_run,
                        &delta.next_run,
                        &delta.created_at,
                        &delta.updated_at,
                        &delta.deleted_at,
                        &delta.active_start,
                        &delta.active_end,
                    ],
                )?;
            }
            Some((local_created, local_updated, local_deleted)) => {
                let local_ts = effective_sync_ts(&local_created, &local_updated, &local_deleted);
                if delta_ts > local_ts {
                    tx.execute(
                        "UPDATE schedules SET name = ?2, cron = ?3, command = ?4, repo = ?5, \
                         enabled = ?6, last_run = ?7, next_run = ?8, created_at = ?9, \
                         updated_at = ?10, deleted_at = ?11, active_start = ?12, active_end = ?13 \
                         WHERE id = ?1",
                        rusqlite::params![
                            &delta.id,
                            &delta.name,
                            &delta.cron,
                            &delta.command,
                            &delta.repo,
                            &delta.enabled,
                            &delta.last_run,
                            &delta.next_run,
                            &delta.created_at,
                            &delta.updated_at,
                            &delta.deleted_at,
                            &delta.active_start,
                            &delta.active_end,
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Apply an incoming sync delta with state-aware conflict resolution
    /// (#488). Returns `Ok(true)` when the delta was applied or inserted,
    /// `Ok(false)` when rejected (no-op / regression / forward-incompat).
    ///
    /// Conflict rule (in priority order):
    ///
    /// 1. Unknown state literal -> log + reject (forward-incompat from a
    ///    newer peer; do not panic).
    /// 2. No local row -> insert.
    /// 3. Tombstone involved (either side has `deleted_at`) -> plain LWW
    ///    on `updated_at`.
    /// 4. Local terminal + incoming non-terminal -> REJECT. Terminal is
    ///    sticky; a peer observing an earlier state must not regress us.
    /// 5. Both terminal but disagree on state -> keep the row with the
    ///    later `exited_at`; on tie, deterministic tiebreak on
    ///    `acquired_by_host` (lexicographic, lower wins).
    /// 6. Otherwise -> LWW on `updated_at`.
    #[allow(dead_code)] // wired by #488 / #489 / #490
    pub fn apply_wake_attempt_delta(&self, delta: &crate::sync::WakeAttemptDelta) -> Result<bool> {
        // Forward-incompat guard: unknown state literal does not panic.
        let delta_state = match crate::wake_attempts::WakeAttemptState::parse_state(&delta.state) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "[legion sync] wake_attempt_delta rejected: unknown state {:?} \
                         (attempt_id={}, err={})",
                    delta.state, delta.attempt_id, e
                );
                return Ok(false);
            }
        };

        let tx = self.conn.unchecked_transaction()?;
        let local = tx
            .query_row(
                "SELECT state, exited_at, updated_at, deleted_at, acquired_by_host \
                 FROM wake_attempts WHERE attempt_id = ?1",
                rusqlite::params![&delta.attempt_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;

        let signal_ids_json = serde_json::to_string(&delta.signal_ids)?;

        let applied = match local {
            None => {
                tx.execute(
                    "INSERT INTO wake_attempts \
                     (attempt_id, persona_id, repo_name, signal_ids, state, \
                      acquired_by_host, acquired_at, spawned_pid, spawned_at, \
                      exit_observed_at, exited_at, exit_status, outcome, \
                      deleted_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    rusqlite::params![
                        &delta.attempt_id,
                        &delta.persona_id,
                        &delta.repo_name,
                        &signal_ids_json,
                        &delta.state,
                        &delta.acquired_by_host,
                        &delta.acquired_at,
                        delta.spawned_pid.map(|v| v as i64),
                        &delta.spawned_at,
                        &delta.exit_observed_at,
                        &delta.exited_at,
                        &delta.exit_status,
                        &delta.outcome,
                        &delta.deleted_at,
                        &delta.updated_at,
                    ],
                )?;
                true
            }
            Some((local_state_str, local_exited, local_updated, local_deleted, local_host)) => {
                let local_state =
                    match crate::wake_attempts::WakeAttemptState::parse_state(&local_state_str) {
                        Ok(s) => s,
                        Err(_) => {
                            // Local row is corrupt -- treat any incoming
                            // well-formed delta as the truth (forward-only
                            // schema recovery). LWW still applies.
                            if delta.updated_at <= local_updated {
                                tx.commit()?;
                                return Ok(false);
                            }
                            self.upsert_wake_attempt_overwrite(&tx, delta, &signal_ids_json)?;
                            tx.commit()?;
                            return Ok(true);
                        }
                    };

                let tombstone_involved = local_deleted.is_some() || delta.deleted_at.is_some();
                let local_terminal = local_state.is_terminal();
                let delta_terminal = delta_state.is_terminal();

                if tombstone_involved {
                    // Plain LWW on updated_at for tombstones.
                    if delta.updated_at > local_updated {
                        self.upsert_wake_attempt_overwrite(&tx, delta, &signal_ids_json)?;
                        true
                    } else {
                        false
                    }
                } else if local_terminal && !delta_terminal {
                    // Terminal-is-sticky: reject the regression. The
                    // local row keeps its terminal state regardless of
                    // updated_at -- a newer non-terminal write is a
                    // happens-before violation by definition.
                    false
                } else if local_terminal && delta_terminal {
                    if local_state == delta_state {
                        // Both same terminal -- LWW on updated_at to
                        // accept fresher metadata (outcome label, etc).
                        if delta.updated_at > local_updated {
                            self.upsert_wake_attempt_overwrite(&tx, delta, &signal_ids_json)?;
                            true
                        } else {
                            false
                        }
                    } else {
                        // Both terminal but disagree -- keep the later
                        // exited_at; on tie, deterministic tiebreak on
                        // acquired_by_host (lower lexicographic wins).
                        let delta_wins = match (&delta.exited_at, &local_exited) {
                            (Some(d), Some(l)) if d != l => d > l,
                            (Some(_), None) => true,
                            (None, Some(_)) => false,
                            _ => {
                                // exited_at tie -- break on host id.
                                match (&delta.acquired_by_host, &local_host) {
                                    (Some(d), Some(l)) => d < l,
                                    (Some(_), None) => true,
                                    _ => false,
                                }
                            }
                        };
                        if delta_wins {
                            self.upsert_wake_attempt_overwrite(&tx, delta, &signal_ids_json)?;
                            true
                        } else {
                            false
                        }
                    }
                } else if delta.updated_at > local_updated {
                    // Live row, neither terminal -- plain LWW.
                    self.upsert_wake_attempt_overwrite(&tx, delta, &signal_ids_json)?;
                    true
                } else {
                    false
                }
            }
        };

        tx.commit()?;
        Ok(applied)
    }

    #[allow(dead_code)] // wired by #488 / #489 / #490
    fn upsert_wake_attempt_overwrite(
        &self,
        tx: &rusqlite::Transaction<'_>,
        delta: &crate::sync::WakeAttemptDelta,
        signal_ids_json: &str,
    ) -> Result<()> {
        tx.execute(
            "UPDATE wake_attempts \
             SET persona_id = ?1, repo_name = ?2, signal_ids = ?3, state = ?4, \
                 acquired_by_host = ?5, acquired_at = ?6, spawned_pid = ?7, \
                 spawned_at = ?8, exit_observed_at = ?9, exited_at = ?10, \
                 exit_status = ?11, outcome = ?12, deleted_at = ?13, updated_at = ?14 \
             WHERE attempt_id = ?15",
            rusqlite::params![
                &delta.persona_id,
                &delta.repo_name,
                signal_ids_json,
                &delta.state,
                &delta.acquired_by_host,
                &delta.acquired_at,
                delta.spawned_pid.map(|v| v as i64),
                &delta.spawned_at,
                &delta.exit_observed_at,
                &delta.exited_at,
                &delta.exit_status,
                &delta.outcome,
                &delta.deleted_at,
                &delta.updated_at,
                &delta.attempt_id,
            ],
        )?;
        Ok(())
    }
}

/// Effective timestamp for sync LWW comparisons (#536): the latest of
/// updated_at / deleted_at, falling back to created_at when neither is set.
///
/// Precondition: all compared timestamps share legion's uniform format
/// (`Utc::now().to_rfc3339()`, +00:00 offset, fixed precision), under which
/// lexicographic order equals time order. Mixed formats (e.g. a 'Z' suffix)
/// would misorder; every writer in the sync path is legion itself.
/// Exact ties keep the local row (strict >): no flip-flop, but concurrent
/// same-instant writes on two nodes stay divergent -- changing that
/// tiebreaker is a conflict-policy change, out of #536's scope.
fn effective_sync_ts<'a>(
    created_at: &'a str,
    updated_at: &'a Option<String>,
    deleted_at: &'a Option<String>,
) -> &'a str {
    let mut best = created_at;
    if let Some(u) = updated_at.as_deref()
        && u > best
    {
        best = u;
    }
    if let Some(d) = deleted_at.as_deref()
        && d > best
    {
        best = d;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;
    use std::time::Duration;

    #[test]
    fn get_reflection_deltas_since_returns_modified_rows() {
        let db = test_db();

        // Insert two reflections.
        let r1 = db.insert_reflection("kelex", "first", "self").unwrap();
        let r2 = db.insert_reflection("kelex", "second", "self").unwrap();

        // Use a cutoff before both were created -- both should appear.
        let old_cutoff = "2020-01-01T00:00:00Z";
        let deltas = db.get_reflection_deltas_since(old_cutoff).unwrap();
        assert_eq!(deltas.len(), 2);

        // Use a cutoff after r1 but before r2 -- only r2 should appear.
        // (updated_at == created_at on insert, so r1.updated_at < r2.updated_at)
        let deltas_after_r1 = db
            .get_reflection_deltas_since(&r1.updated_at.unwrap())
            .unwrap();
        assert_eq!(deltas_after_r1.len(), 1);
        assert_eq!(deltas_after_r1[0].id, r2.id);

        // Boost r1 to bump its updated_at.
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.boost_reflection(&r1.id).unwrap();
        let boosted = db.get_reflection_by_id(&r1.id).unwrap().unwrap();

        // Use r2's updated_at as cutoff -- now r1 should appear (it was boosted after).
        let deltas_after_r2 = db
            .get_reflection_deltas_since(&r2.updated_at.unwrap())
            .unwrap();
        assert_eq!(deltas_after_r2.len(), 1);
        assert_eq!(deltas_after_r2[0].id, r1.id);
        assert_eq!(deltas_after_r2[0].updated_at, boosted.updated_at);
    }

    #[test]
    fn get_reflection_deltas_since_includes_soft_deleted() {
        let db = test_db();

        let r = db
            .insert_reflection("kelex", "will delete", "self")
            .unwrap();
        let created_at = r.created_at.clone();

        // Soft delete the reflection.
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.soft_delete_reflection(&r.id).unwrap();

        // Query with cutoff before creation -- should include the soft-deleted row.
        let deltas = db
            .get_reflection_deltas_since("2020-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].id, r.id);
        assert!(deltas[0].deleted_at.is_some(), "deleted_at should be set");

        // Query with cutoff after creation but before deletion -- should still include.
        let deltas_after_create = db.get_reflection_deltas_since(&created_at).unwrap();
        assert_eq!(deltas_after_create.len(), 1);
        assert!(deltas_after_create[0].deleted_at.is_some());
    }

    #[test]
    fn get_reflection_deltas_since_excludes_unchanged() {
        let db = test_db();

        let r = db.insert_reflection("kelex", "old", "self").unwrap();

        // Use a cutoff after the reflection was created -- should return empty.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let future_cutoff = chrono::Utc::now().to_rfc3339();
        let deltas = db.get_reflection_deltas_since(&future_cutoff).unwrap();
        assert!(deltas.is_empty());

        // Verify the reflection still exists but wasn't returned.
        assert!(db.get_reflection_by_id(&r.id).unwrap().is_some());
    }

    #[test]
    fn get_card_deltas_since_returns_modified_cards() {
        let db = test_db();

        // Insert two cards.
        let id1 = db
            .insert_card(
                "kelex",
                "legion",
                "task 1",
                None,
                crate::kanban::Priority::Med,
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Pending,
            )
            .unwrap();
        let _id2 = db
            .insert_card(
                "kelex",
                "legion",
                "task 2",
                None,
                crate::kanban::Priority::High,
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Pending,
            )
            .unwrap();

        // Use an old cutoff -- both should appear.
        let old_cutoff = "2020-01-01T00:00:00Z";
        let deltas = db.get_card_deltas_since(old_cutoff).unwrap();
        assert_eq!(deltas.len(), 2);

        // Verify fields are populated.
        let delta1 = deltas.iter().find(|d| d.id == id1).unwrap();
        assert_eq!(delta1.from_repo, "kelex");
        assert_eq!(delta1.to_repo, "legion");
        assert_eq!(delta1.text, "task 1");
        assert_eq!(delta1.priority, "med");
        assert_eq!(delta1.status, "pending");
        assert!(delta1.deleted_at.is_none());
    }

    #[test]
    fn get_card_deltas_since_includes_soft_deleted() {
        let db = test_db();

        let id = db
            .insert_card(
                "kelex",
                "legion",
                "will delete",
                None,
                crate::kanban::Priority::Low,
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Backlog,
            )
            .unwrap();

        // Soft delete the card.
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.soft_delete_card(&id).unwrap();

        // Should still appear in deltas with deleted_at set.
        let deltas = db.get_card_deltas_since("2020-01-01T00:00:00Z").unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].id, id);
        assert!(deltas[0].deleted_at.is_some());
    }

    #[test]
    fn get_schedule_deltas_since_returns_modified_schedules() {
        let db = test_db();

        // Insert a schedule.
        let id = db
            .insert_schedule("test-sched", "*/30m", "echo hello", "legion", None, None)
            .unwrap();

        // Use an old cutoff -- should appear.
        let deltas = db
            .get_schedule_deltas_since("2020-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].id, id);
        assert_eq!(deltas[0].name, "test-sched");
        assert_eq!(deltas[0].cron, "*/30m");
        assert_eq!(deltas[0].command, "echo hello");
        assert!(deltas[0].enabled);
        assert!(deltas[0].deleted_at.is_none());
    }

    #[test]
    fn get_schedule_deltas_since_includes_soft_deleted() {
        let db = test_db();

        let id = db
            .insert_schedule("to-delete", "*/5m", "echo bye", "legion", None, None)
            .unwrap();

        // Soft delete.
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.soft_delete_schedule(&id).unwrap();

        // Should appear with deleted_at set.
        let deltas = db
            .get_schedule_deltas_since("2020-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(deltas.len(), 1);
        assert!(deltas[0].deleted_at.is_some());
    }

    #[test]
    fn cleanup_tombstones_removes_old_soft_deleted_rows() {
        let db = test_db();

        // Insert and soft delete a reflection.
        let r = db.insert_reflection("kelex", "to delete", "self").unwrap();
        db.soft_delete_reflection(&r.id).unwrap();

        // Insert and soft delete a card.
        let card_id = db
            .insert_card(
                "kelex",
                "legion",
                "to delete",
                None,
                crate::kanban::Priority::Med,
                None,
                None,
                None,
                None,
                None,
                crate::kanban::CardStatus::Backlog,
            )
            .unwrap();
        db.soft_delete_card(&card_id).unwrap();

        // Insert and soft delete a schedule.
        let sched_id = db
            .insert_schedule("to-delete", "*/5m", "echo bye", "legion", None, None)
            .unwrap();
        db.soft_delete_schedule(&sched_id).unwrap();

        // Cleanup with 0-day retention should remove all tombstones.
        let result = db.cleanup_tombstones(0).unwrap();
        assert_eq!(result.reflections, 1);
        assert_eq!(result.tasks, 1);
        assert_eq!(result.schedules, 1);
        assert_eq!(result.total(), 3);

        // Running again should return zeros.
        let result2 = db.cleanup_tombstones(0).unwrap();
        assert!(result2.is_empty());
    }

    #[test]
    fn cleanup_tombstones_respects_retention_period() {
        let db = test_db();

        // Insert and soft delete a reflection.
        let r = db
            .insert_reflection("kelex", "recent delete", "self")
            .unwrap();
        db.soft_delete_reflection(&r.id).unwrap();

        // Cleanup with 30-day retention should NOT remove the freshly deleted row.
        let result = db.cleanup_tombstones(30).unwrap();
        assert!(
            result.is_empty(),
            "fresh tombstone should not be cleaned up"
        );
    }

    #[test]
    fn persona_lease_apply_delta_inserts_new() {
        let db = test_db();
        let delta = crate::sync::PersonaWakeLeaseDelta {
            persona_id: "legion".into(),
            signal_id: "sig-1".into(),
            acquired_by_host: "peer".into(),
            acquired_at: "2026-04-24T00:00:00Z".into(),
            heartbeat_at: "2026-04-24T00:00:00Z".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            updated_at: "2026-04-24T00:00:00Z".into(),
            deleted_at: None,
        };
        let late = db.apply_persona_wake_lease_delta(&delta).unwrap();
        assert!(late.is_none(), "no local row means no late loser");
        let listed = db.list_persona_leases(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].acquired_by_host, "peer");
    }

    #[test]
    fn persona_lease_apply_delta_earlier_acquired_at_wins() {
        // Two real acquires against separate databases, 50ms apart, then
        // sync-apply the earlier one onto the later's database and assert
        // the earlier wins. Uses realistic clock deltas rather than hardcoded
        // ancient timestamps so the test exercises actual RFC3339 ordering
        // at sub-second precision.
        let peer_db = test_db();
        assert!(
            peer_db
                .try_acquire_persona_lease("legion", "sig-1", "peer", Duration::from_secs(3600))
                .unwrap()
        );
        let peer_row = peer_db.list_persona_leases(None).unwrap().remove(0);

        std::thread::sleep(Duration::from_millis(50));

        let local_db = test_db();
        assert!(
            local_db
                .try_acquire_persona_lease("legion", "sig-1", "local", Duration::from_secs(3600))
                .unwrap()
        );

        // Peer's lease is older; when its delta reaches local, local is the
        // late loser.
        let delta = crate::sync::PersonaWakeLeaseDelta {
            persona_id: peer_row.persona_id,
            signal_id: peer_row.signal_id,
            acquired_by_host: peer_row.acquired_by_host,
            acquired_at: peer_row.acquired_at.clone(),
            heartbeat_at: peer_row.heartbeat_at,
            expires_at: peer_row.expires_at,
            updated_at: peer_row.updated_at,
            deleted_at: peer_row.deleted_at,
        };
        let late = local_db.apply_persona_wake_lease_delta(&delta).unwrap();
        assert_eq!(
            late.as_deref(),
            Some("local"),
            "local node is the late loser; its host identity must surface"
        );

        let listed = local_db.list_persona_leases(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].acquired_by_host, "peer",
            "peer's earlier lease must win"
        );
        assert_eq!(
            listed[0].acquired_at, delta.acquired_at,
            "winning acquired_at must be peer's, not local's"
        );
    }

    #[test]
    fn persona_lease_apply_delta_tombstone_wins_by_lww() {
        let db = test_db();
        db.try_acquire_persona_lease("legion", "sig-1", "local", Duration::from_secs(60))
            .unwrap();

        // Incoming tombstone with a later updated_at.
        let delta = crate::sync::PersonaWakeLeaseDelta {
            persona_id: "legion".into(),
            signal_id: "sig-1".into(),
            acquired_by_host: "local".into(),
            acquired_at: "2026-04-24T00:00:00Z".into(),
            heartbeat_at: "2026-04-24T00:00:00Z".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            updated_at: "2099-01-01T00:00:00Z".into(),
            deleted_at: Some("2099-01-01T00:00:00Z".into()),
        };
        db.apply_persona_wake_lease_delta(&delta).unwrap();

        let listed = db.list_persona_leases(None).unwrap();
        assert!(
            listed.is_empty(),
            "incoming tombstone with newer updated_at must eclipse local live lease"
        );
    }

    // -- apply_*_delta LWW (#536) ------------------------------------------

    fn reflection_delta(id: &str, text: &str, updated_at: &str) -> crate::sync::ReflectionDelta {
        crate::sync::ReflectionDelta {
            id: id.into(),
            repo: "legion".into(),
            text: text.into(),
            created_at: "2026-06-01T00:00:00Z".into(),
            updated_at: Some(updated_at.into()),
            deleted_at: None,
            audience: "self".into(),
            domain: None,
            tags: None,
            recall_count: 0,
            last_recalled_at: None,
            parent_id: None,
        }
    }

    fn read_reflection_text(db: &Database, id: &str) -> Option<String> {
        db.conn
            .query_row("SELECT text FROM reflections WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .unwrap()
    }

    #[test]
    fn apply_reflection_delta_inserts_when_missing() {
        let db = test_db();
        db.apply_reflection_delta(&reflection_delta(
            "r-1",
            "from peer",
            "2026-06-02T00:00:00Z",
        ))
        .unwrap();
        assert_eq!(
            read_reflection_text(&db, "r-1").as_deref(),
            Some("from peer")
        );
    }

    #[test]
    fn apply_reflection_delta_newer_overwrites_older_is_noop() {
        let db = test_db();
        db.apply_reflection_delta(&reflection_delta("r-2", "v1", "2026-06-02T00:00:00Z"))
            .unwrap();
        // Newer delta wins.
        db.apply_reflection_delta(&reflection_delta("r-2", "v2", "2026-06-03T00:00:00Z"))
            .unwrap();
        assert_eq!(read_reflection_text(&db, "r-2").as_deref(), Some("v2"));
        // Older delta is a no-op.
        db.apply_reflection_delta(&reflection_delta("r-2", "stale", "2026-06-01T00:00:00Z"))
            .unwrap();
        assert_eq!(read_reflection_text(&db, "r-2").as_deref(), Some("v2"));
    }

    #[test]
    fn apply_reflection_delta_tombstone_wins_by_lww() {
        let db = test_db();
        db.apply_reflection_delta(&reflection_delta("r-3", "alive", "2026-06-02T00:00:00Z"))
            .unwrap();
        let mut tomb = reflection_delta("r-3", "alive", "2026-06-02T00:00:00Z");
        tomb.deleted_at = Some("2026-06-04T00:00:00Z".into());
        db.apply_reflection_delta(&tomb).unwrap();
        let deleted: Option<String> = db
            .conn
            .query_row(
                "SELECT deleted_at FROM reflections WHERE id = ?1",
                ["r-3"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(deleted.as_deref(), Some("2026-06-04T00:00:00Z"));
    }

    #[test]
    fn apply_card_delta_insert_then_lww() {
        let db = test_db();
        let mut delta = crate::sync::CardDelta {
            id: "c-1".into(),
            from_repo: "legion".into(),
            to_repo: "legion".into(),
            text: "card v1".into(),
            context: None,
            priority: "med".into(),
            status: "pending".into(),
            note: None,
            labels: None,
            parent_card_id: None,
            source_url: None,
            source_type: None,
            sort_order: 0,
            created_at: "2026-06-01T00:00:00Z".into(),
            updated_at: "2026-06-02T00:00:00Z".into(),
            deleted_at: None,
            assigned_at: None,
            started_at: None,
            completed_at: None,
            problem: None,
            solution: None,
            acceptance: None,
        };
        db.apply_card_delta(&delta).unwrap();

        delta.text = "card v2".into();
        delta.updated_at = "2026-06-03T00:00:00Z".into();
        db.apply_card_delta(&delta).unwrap();

        // Stale write loses.
        delta.text = "card stale".into();
        delta.updated_at = "2026-06-01T12:00:00Z".into();
        db.apply_card_delta(&delta).unwrap();

        let text: String = db
            .conn
            .query_row("SELECT text FROM tasks WHERE id = ?1", ["c-1"], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(text, "card v2");
    }

    #[test]
    fn apply_schedule_delta_insert_then_lww() {
        let db = test_db();
        let mut delta = crate::sync::ScheduleDelta {
            id: "s-1".into(),
            name: "nightly".into(),
            cron: "0 2 * * *".into(),
            command: "echo one".into(),
            repo: "legion".into(),
            enabled: true,
            last_run: None,
            next_run: "2026-06-02T02:00:00Z".into(),
            created_at: "2026-06-01T00:00:00Z".into(),
            updated_at: Some("2026-06-02T00:00:00Z".into()),
            deleted_at: None,
            active_start: None,
            active_end: None,
        };
        db.apply_schedule_delta(&delta).unwrap();

        delta.command = "echo two".into();
        delta.updated_at = Some("2026-06-03T00:00:00Z".into());
        db.apply_schedule_delta(&delta).unwrap();

        delta.command = "echo stale".into();
        delta.updated_at = Some("2026-06-01T06:00:00Z".into());
        db.apply_schedule_delta(&delta).unwrap();

        let cmd: String = db
            .conn
            .query_row(
                "SELECT command FROM schedules WHERE id = ?1",
                ["s-1"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cmd, "echo two");
    }

    #[test]
    fn effective_sync_ts_picks_latest_of_three() {
        assert_eq!(
            effective_sync_ts("2026-01-01T00:00:00Z", &None, &None),
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(
            effective_sync_ts(
                "2026-01-01T00:00:00Z",
                &Some("2026-02-01T00:00:00Z".into()),
                &None
            ),
            "2026-02-01T00:00:00Z"
        );
        assert_eq!(
            effective_sync_ts(
                "2026-01-01T00:00:00Z",
                &Some("2026-02-01T00:00:00Z".into()),
                &Some("2026-03-01T00:00:00Z".into())
            ),
            "2026-03-01T00:00:00Z"
        );
    }

    fn insert_prediction_row(db: &Database, id: &str, updated_at: &str) {
        db.conn
            .execute(
                "INSERT INTO uncertainty_prediction \
                 (id, surface, feature_key, input_fingerprint, model, model_version, \
                  claimed_confidence, prediction_payload, state, cohort_key, \
                  created_at, updated_at) \
                 VALUES (?1, 'legion.task', 'scip.refactor', 'fp-1', 'claude-opus-4-7', \
                         '4.7', 0.7, '{\"predicted_tokens\":1000}', 'emitted', \
                         'legion:claude-opus-4-7:scip.refactor:0.7', ?2, ?3)",
                rusqlite::params![id, updated_at, updated_at],
            )
            .unwrap();
    }

    fn insert_snapshot_row(db: &Database, id: &str, updated_at: &str) {
        db.conn
            .execute(
                "INSERT INTO uncertainty_calibration_snapshot \
                 (id, cohort_key, bucket_lower, bucket_upper, claimed_confidence, \
                  actual_correctness, actual_correctness_raw, prediction_count, \
                  orphan_count, brier_score, computed_at, updated_at) \
                 VALUES (?1, 'legion:claude-opus-4-7:scip.refactor:0.7', 0.6, 0.8, 0.7, \
                         0.68, 0.65, 42, 3, 0.09, ?2, ?2)",
                rusqlite::params![id, updated_at],
            )
            .unwrap();
    }

    #[test]
    fn get_uncertainty_prediction_deltas_returns_modified_rows() {
        let db = test_db();
        insert_prediction_row(&db, "p-1", "2026-05-12T00:00:00+00:00");
        insert_prediction_row(&db, "p-2", "2026-05-12T01:00:00+00:00");

        let deltas = db
            .get_uncertainty_prediction_deltas_since("2026-05-12T00:30:00+00:00")
            .unwrap();
        assert_eq!(deltas.len(), 1, "only p-2 is newer than the cutoff");
        assert_eq!(deltas[0].id, "p-2");
        assert_eq!(deltas[0].surface, "legion.task");
        assert_eq!(deltas[0].claimed_confidence, 0.7);
        assert_eq!(deltas[0].state, "emitted");
    }

    #[test]
    fn get_uncertainty_prediction_deltas_includes_soft_deleted() {
        let db = test_db();
        insert_prediction_row(&db, "p-1", "2026-05-12T00:00:00+00:00");
        db.conn
            .execute(
                "UPDATE uncertainty_prediction SET deleted_at = ?1 WHERE id = 'p-1'",
                ["2026-05-12T02:00:00+00:00"],
            )
            .unwrap();
        let deltas = db
            .get_uncertainty_prediction_deltas_since("2026-05-12T01:00:00+00:00")
            .unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(
            deltas[0].deleted_at.as_deref(),
            Some("2026-05-12T02:00:00+00:00")
        );
    }

    #[test]
    fn get_uncertainty_calibration_snapshot_deltas_round_trip() {
        let db = test_db();
        insert_snapshot_row(&db, "s-1", "2026-05-12T00:00:00+00:00");
        insert_snapshot_row(&db, "s-2", "2026-05-12T01:00:00+00:00");

        let deltas = db
            .get_uncertainty_calibration_snapshot_deltas_since("2026-05-12T00:30:00+00:00")
            .unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].id, "s-2");
        assert_eq!(deltas[0].prediction_count, 42);
        assert_eq!(deltas[0].orphan_count, 3);
        assert!((deltas[0].actual_correctness - 0.68).abs() < 1e-9);
        assert!((deltas[0].actual_correctness_raw - 0.65).abs() < 1e-9);
    }

    #[test]
    fn cleanup_tombstones_includes_uncertainty_tables() {
        let db = test_db();
        let old = (Utc::now() - chrono::Duration::days(100)).to_rfc3339();
        insert_prediction_row(&db, "p-old", &old);
        db.conn
            .execute(
                "UPDATE uncertainty_prediction SET deleted_at = ?1 WHERE id = 'p-old'",
                [&old],
            )
            .unwrap();
        insert_snapshot_row(&db, "s-old", &old);
        db.conn
            .execute(
                "UPDATE uncertainty_calibration_snapshot SET deleted_at = ?1 WHERE id = 's-old'",
                [&old],
            )
            .unwrap();

        let result = db.cleanup_tombstones(30).unwrap();
        assert_eq!(result.uncertainty_predictions, 1);
        assert_eq!(result.uncertainty_calibration_snapshots, 1);
    }

    // -- WakeAttemptDelta + apply_wake_attempt_delta (#488) ------------------

    fn delta_for(attempt_id: &str, state: &str, updated_at: &str) -> crate::sync::WakeAttemptDelta {
        crate::sync::WakeAttemptDelta {
            attempt_id: attempt_id.to_string(),
            persona_id: "legion".to_string(),
            repo_name: "legion".to_string(),
            signal_ids: vec!["sig-1".to_string()],
            state: state.to_string(),
            acquired_by_host: Some("peer-host".to_string()),
            acquired_at: Some("2026-05-23T10:00:00Z".to_string()),
            spawned_pid: None,
            spawned_at: None,
            exit_observed_at: None,
            exited_at: None,
            exit_status: None,
            outcome: None,
            deleted_at: None,
            updated_at: updated_at.to_string(),
        }
    }

    fn wake_id(tag: &str) -> String {
        format!("attempt-{}-{}", tag, uuid::Uuid::now_v7())
    }

    #[test]
    fn delta_from_attempt_roundtrips() {
        use crate::wake_attempts::WakeAttemptState;
        let db = test_db();
        let id = wake_id("delta-rt");
        db.enqueue_wake_attempt(&id, "legion", "legion", &["a".into(), "b".into()])
            .unwrap();
        let attempt = db.get_wake_attempt(&id).unwrap().expect("row");
        let delta = crate::sync::WakeAttemptDelta::from_attempt(&attempt);

        assert_eq!(delta.attempt_id, id);
        assert_eq!(delta.state, "queued");
        assert_eq!(delta.signal_ids, vec!["a".to_string(), "b".to_string()]);
        // Round-trip back through serde to confirm the state literal
        // parses on the other side.
        let json = serde_json::to_string(&delta).unwrap();
        let back: crate::sync::WakeAttemptDelta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.state, "queued");
        assert!(matches!(
            WakeAttemptState::parse_state(&back.state).unwrap(),
            WakeAttemptState::Queued
        ));
    }

    #[test]
    fn apply_delta_inserts_when_no_local_row() {
        let db = test_db();
        let delta = delta_for("new-id", "queued", "2026-05-23T10:00:00Z");
        let applied = db.apply_wake_attempt_delta(&delta).unwrap();
        assert!(applied);
        let row = db.get_wake_attempt("new-id").unwrap().expect("row");
        assert_eq!(row.state.as_str(), "queued");
    }

    #[test]
    fn apply_delta_lww_older_is_noop() {
        let db = test_db();
        // Local row is newer.
        let new_delta = delta_for("lww-id", "claimed", "2026-05-23T12:00:00Z");
        assert!(db.apply_wake_attempt_delta(&new_delta).unwrap());
        // Older incoming -> rejected.
        let old_delta = delta_for("lww-id", "queued", "2026-05-23T10:00:00Z");
        assert!(!db.apply_wake_attempt_delta(&old_delta).unwrap());
        let row = db.get_wake_attempt("lww-id").unwrap().expect("row");
        assert_eq!(
            row.state.as_str(),
            "claimed",
            "older delta must not regress"
        );
    }

    #[test]
    fn apply_delta_lww_newer_overwrites() {
        let db = test_db();
        let old = delta_for("over-id", "queued", "2026-05-23T10:00:00Z");
        assert!(db.apply_wake_attempt_delta(&old).unwrap());
        let new = delta_for("over-id", "claimed", "2026-05-23T11:00:00Z");
        assert!(db.apply_wake_attempt_delta(&new).unwrap());
        let row = db.get_wake_attempt("over-id").unwrap().expect("row");
        assert_eq!(row.state.as_str(), "claimed");
    }

    #[test]
    fn apply_delta_terminal_is_sticky_against_non_terminal() {
        // Local row has reached Done; peer's non-terminal delta with a
        // later updated_at must NOT regress us. This is the load-bearing
        // happens-before guard that distinguishes wake_attempts from
        // plain LWW rows.
        let db = test_db();
        let mut done = delta_for("sticky-id", "done", "2026-05-23T11:00:00Z");
        done.exited_at = Some("2026-05-23T11:00:00Z".to_string());
        done.exit_status = Some("ok".to_string());
        done.outcome = Some("productive".to_string());
        assert!(db.apply_wake_attempt_delta(&done).unwrap());

        // Newer updated_at, but state regression: rejected.
        let regress = delta_for("sticky-id", "running", "2026-05-23T12:00:00Z");
        assert!(!db.apply_wake_attempt_delta(&regress).unwrap());

        let row = db.get_wake_attempt("sticky-id").unwrap().expect("row");
        assert_eq!(
            row.state.as_str(),
            "done",
            "terminal must survive a newer non-terminal delta"
        );
    }

    #[test]
    fn apply_delta_both_terminal_disagree_keeps_later_exited_at() {
        let db = test_db();
        let mut early = delta_for("term-id", "done", "2026-05-23T11:00:00Z");
        early.exited_at = Some("2026-05-23T11:00:00Z".to_string());
        early.exit_status = Some("ok".to_string());
        assert!(db.apply_wake_attempt_delta(&early).unwrap());

        // Peer's terminal disagrees but exited later -> wins.
        let mut later_failed = delta_for("term-id", "failed", "2026-05-23T11:30:00Z");
        later_failed.exited_at = Some("2026-05-23T12:00:00Z".to_string());
        later_failed.exit_status = Some("error".to_string());
        assert!(db.apply_wake_attempt_delta(&later_failed).unwrap());

        let row = db.get_wake_attempt("term-id").unwrap().expect("row");
        assert_eq!(row.state.as_str(), "failed");
    }

    #[test]
    fn apply_delta_both_terminal_tie_breaks_on_host() {
        // Local "done" on host-b vs incoming "failed" on host-a. Equal
        // exited_at; deterministic tiebreak picks lower lexicographic
        // host (host-a < host-b).
        let db = test_db();
        let mut local = delta_for("tie-id", "done", "2026-05-23T11:00:00Z");
        local.exited_at = Some("2026-05-23T12:00:00Z".to_string());
        local.exit_status = Some("ok".to_string());
        local.acquired_by_host = Some("host-b".to_string());
        assert!(db.apply_wake_attempt_delta(&local).unwrap());

        let mut peer = delta_for("tie-id", "failed", "2026-05-23T11:00:00Z");
        peer.exited_at = Some("2026-05-23T12:00:00Z".to_string());
        peer.exit_status = Some("error".to_string());
        peer.acquired_by_host = Some("host-a".to_string());
        assert!(db.apply_wake_attempt_delta(&peer).unwrap());

        let row = db.get_wake_attempt("tie-id").unwrap().expect("row");
        assert_eq!(
            row.state.as_str(),
            "failed",
            "host-a wins the lexicographic tiebreak"
        );
    }

    #[test]
    fn apply_delta_unknown_state_is_rejected_no_panic() {
        let db = test_db();
        let delta = delta_for("unknown-id", "frobnicated", "2026-05-23T10:00:00Z");
        let applied = db.apply_wake_attempt_delta(&delta).unwrap();
        assert!(!applied, "forward-incompat state must be rejected");
        assert!(
            db.get_wake_attempt("unknown-id").unwrap().is_none(),
            "rejected delta must not insert"
        );
    }

    #[test]
    fn apply_delta_tombstone_lww() {
        let db = test_db();
        // Local live row.
        let live = delta_for("tomb-id", "running", "2026-05-23T10:00:00Z");
        assert!(db.apply_wake_attempt_delta(&live).unwrap());
        // Incoming tombstone with newer updated_at wins.
        let mut tomb = delta_for("tomb-id", "running", "2026-05-23T11:00:00Z");
        tomb.deleted_at = Some("2026-05-23T11:00:00Z".to_string());
        assert!(db.apply_wake_attempt_delta(&tomb).unwrap());
        let row = db.get_wake_attempt("tomb-id").unwrap().expect("row");
        assert!(row.deleted_at.is_some());
    }
}
