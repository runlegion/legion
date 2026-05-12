//! Delta serialization for multi-node sync.
//!
//! This module provides types and functions for serializing database rows
//! into delta packets that can be transmitted between legion nodes via
//! smugglr-core's broadcast system.
//!
//! Key design decisions:
//! - Embedding BLOBs are excluded (each node computes its own)
//! - Soft-deleted rows are included (tombstone propagation)
//! - updated_at drives Last-Write-Wins conflict resolution
//!
//! Delta types mirror their source structs but:
//! - Add `deleted_at` for tombstone propagation
//! - Exclude computed/local-only fields (embeddings)
//! - Use String for status enums (serde across nodes)

use serde::{Deserialize, Serialize};

use crate::db::Reflection;

/// A reflection row serialized for sync transmission.
///
/// Mirrors the Reflection struct but excludes the embedding BLOB since
/// each node computes its own embeddings locally. The embedding field
/// would add significant bandwidth for data that cannot be meaningfully
/// merged across nodes anyway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionDelta {
    pub id: String,
    pub repo: String,
    pub text: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub deleted_at: Option<String>,
    pub audience: String,
    pub domain: Option<String>,
    pub tags: Option<String>,
    pub recall_count: i64,
    pub last_recalled_at: Option<String>,
    pub parent_id: Option<String>,
}

impl ReflectionDelta {
    /// Convert a Reflection row (without embedding) to a delta.
    #[allow(dead_code)] // Used by db::get_reflection_deltas_since
    pub fn from_reflection(r: &Reflection, deleted_at: Option<String>) -> Self {
        Self {
            id: r.id.clone(),
            repo: r.repo.clone(),
            text: r.text.clone(),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
            deleted_at,
            audience: r.audience.clone(),
            domain: r.domain.clone(),
            tags: r.tags.clone(),
            recall_count: r.recall_count,
            last_recalled_at: r.last_recalled_at.clone(),
            parent_id: r.parent_id.clone(),
        }
    }
}

/// A kanban card row serialized for sync transmission.
///
/// Cards (tasks table) track work items delegated between agents.
/// Status is serialized as String for cross-node compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardDelta {
    pub id: String,
    pub from_repo: String,
    pub to_repo: String,
    pub text: String,
    pub context: Option<String>,
    pub priority: String,
    pub status: String, // String, not CardStatus, for serde compatibility
    pub note: Option<String>,
    pub labels: Option<String>,
    pub parent_card_id: Option<String>,
    pub source_url: Option<String>,
    pub source_type: Option<String>,
    pub sort_order: i32,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    pub assigned_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub problem: Option<String>,
    pub solution: Option<String>,
    pub acceptance: Option<String>,
}

/// A rate-limit sample serialized for sync transmission.
///
/// Written by `legion statusline` on every Claude Code render. Synced
/// cluster-wide so any node can read the latest account-level headroom
/// when the budget gate decides whether to pick up a card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitDelta {
    pub id: String,
    pub hostname: String,
    pub session_id: String,
    pub sampled_at: String,
    pub five_hour_pct: Option<f64>,
    pub five_hour_resets_at: Option<i64>,
    pub seven_day_pct: Option<f64>,
    pub seven_day_resets_at: Option<i64>,
    pub model: Option<String>,
    pub updated_at: Option<String>,
    pub deleted_at: Option<String>,
}

impl RateLimitDelta {
    /// Convert a RateLimitSample into its sync delta. Callers that need
    /// a tombstone pass the deletion timestamp through `deleted_at`.
    #[allow(dead_code)] // Consumed by the sync actor once #276 wires transport.
    pub fn from_sample(s: &crate::statusline::RateLimitSample, deleted_at: Option<String>) -> Self {
        Self {
            id: s.id.clone(),
            hostname: s.hostname.clone(),
            session_id: s.session_id.clone(),
            sampled_at: s.sampled_at.clone(),
            five_hour_pct: s.five_hour_pct,
            five_hour_resets_at: s.five_hour_resets_at,
            seven_day_pct: s.seven_day_pct,
            seven_day_resets_at: s.seven_day_resets_at,
            model: s.model.clone(),
            updated_at: Some(s.sampled_at.clone()),
            deleted_at,
        }
    }
}

/// A per-turn usage sample serialized for sync transmission.
///
/// Feeds the empirical cost estimator: historical `usage_samples` rows
/// grouped by card shape give the p50/p90/p99 distribution used at
/// gate time to predict cost against remaining rate-limit headroom.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageDelta {
    pub id: String,
    pub hostname: String,
    pub session_id: String,
    pub turn_index: Option<i64>,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_read_tokens: i64,
    pub effective_tokens: i64,
    pub error_bytes: i64,
    pub sampled_at: String,
    pub updated_at: Option<String>,
    pub deleted_at: Option<String>,
}

impl UsageDelta {
    /// Convert a UsageSample into its sync delta. Tombstones flow via
    /// `deleted_at`; live rows carry `updated_at = sampled_at`.
    #[allow(dead_code)] // Consumed by the sync actor once #276 wires transport.
    pub fn from_sample(s: &crate::statusline::UsageSample, deleted_at: Option<String>) -> Self {
        Self {
            id: s.id.clone(),
            hostname: s.hostname.clone(),
            session_id: s.session_id.clone(),
            turn_index: s.turn_index,
            model: s.model.clone(),
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            cache_write_tokens: s.cache_write_tokens,
            cache_read_tokens: s.cache_read_tokens,
            effective_tokens: s.effective_tokens,
            error_bytes: s.error_bytes,
            sampled_at: s.sampled_at.clone(),
            updated_at: Some(s.sampled_at.clone()),
            deleted_at,
        }
    }
}

/// A schedule row serialized for sync transmission.
///
/// Schedules define cron-like commands that fire periodically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleDelta {
    pub id: String,
    pub name: String,
    pub cron: String,
    pub command: String,
    pub repo: String,
    pub enabled: bool,
    pub last_run: Option<String>,
    pub next_run: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub deleted_at: Option<String>,
    pub active_start: Option<String>,
    pub active_end: Option<String>,
}

/// An uncertainty prediction row serialized for sync transmission.
///
/// Mirrors the uncertainty_prediction table. The prediction_payload and
/// outcome_payload columns hold JSON blobs that are opaque to sync -- LWW
/// merges happen on the whole row keyed by id with updated_at as the
/// tiebreaker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UncertaintyPredictionDelta {
    pub id: String,
    pub surface: String,
    pub feature_key: String,
    pub input_fingerprint: String,
    pub model: String,
    pub model_version: String,
    pub claimed_confidence: f64,
    pub prediction_payload: String,
    pub state: String,
    pub outcome_label: Option<String>,
    pub outcome_payload: Option<String>,
    pub outcome_correctness: Option<f64>,
    pub cohort_key: String,
    pub created_at: String,
    pub updated_at: String,
    pub witnessed_at: Option<String>,
    pub orphan_after: Option<String>,
    pub deleted_at: Option<String>,
}

/// An uncertainty calibration snapshot row serialized for sync transmission.
///
/// Snapshots are computed independently per node from synced prediction rows,
/// so cross-node merge is rare -- but sync still propagates rows so nodes
/// can compare calibration drift across the cluster without re-running the
/// roller. actual_correctness is the EB-shrunk value; actual_correctness_raw
/// is the unshrunk cell average kept for audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UncertaintyCalibrationSnapshotDelta {
    pub id: String,
    pub cohort_key: String,
    pub bucket_lower: f64,
    pub bucket_upper: f64,
    pub claimed_confidence: f64,
    pub actual_correctness: f64,
    pub actual_correctness_raw: f64,
    pub prediction_count: i64,
    pub orphan_count: i64,
    pub brier_score: f64,
    pub computed_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

/// A persona wake lease row serialized for sync transmission.
///
/// Leases prevent two nodes from waking the same persona for the same signal.
/// See `db::PersonaWakeLease` for acquire / release / heartbeat semantics and
/// `db::apply_persona_wake_lease_delta` for the late-loser conflict rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaWakeLeaseDelta {
    pub persona_id: String,
    pub signal_id: String,
    pub acquired_by_host: String,
    pub acquired_at: String,
    pub heartbeat_at: String,
    pub expires_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_from_reflection_excludes_embedding() {
        let r = Reflection {
            id: "test-id".into(),
            repo: "test-repo".into(),
            text: "test text".into(),
            created_at: "2026-04-15T00:00:00Z".into(),
            updated_at: Some("2026-04-15T00:00:00Z".into()),
            audience: "self".into(),
            domain: Some("test-domain".into()),
            tags: Some("tag1,tag2".into()),
            recall_count: 5,
            last_recalled_at: Some("2026-04-14T00:00:00Z".into()),
            parent_id: None,
        };

        let delta = ReflectionDelta::from_reflection(&r, None);

        assert_eq!(delta.id, "test-id");
        assert_eq!(delta.repo, "test-repo");
        assert_eq!(delta.text, "test text");
        assert_eq!(delta.recall_count, 5);
        assert!(delta.deleted_at.is_none());
        // No embedding field in delta - that's the point
    }

    #[test]
    fn delta_includes_deleted_at_for_tombstones() {
        let r = Reflection {
            id: "deleted-id".into(),
            repo: "test-repo".into(),
            text: "deleted text".into(),
            created_at: "2026-04-15T00:00:00Z".into(),
            updated_at: Some("2026-04-15T01:00:00Z".into()),
            audience: "self".into(),
            domain: None,
            tags: None,
            recall_count: 0,
            last_recalled_at: None,
            parent_id: None,
        };

        let delta = ReflectionDelta::from_reflection(&r, Some("2026-04-15T01:00:00Z".into()));

        assert!(delta.deleted_at.is_some());
        assert_eq!(delta.deleted_at.unwrap(), "2026-04-15T01:00:00Z");
    }

    #[test]
    fn rate_limit_delta_from_sample_roundtrips_fields() {
        let sample = crate::statusline::RateLimitSample {
            id: "rl-1".into(),
            hostname: "puck".into(),
            session_id: "sess-a".into(),
            sampled_at: "2026-04-20T01:00:00Z".into(),
            five_hour_pct: Some(42.5),
            five_hour_resets_at: Some(1714500000),
            seven_day_pct: Some(68.0),
            seven_day_resets_at: Some(1714900000),
            model: Some("claude-opus-4-7".into()),
        };
        let delta = RateLimitDelta::from_sample(&sample, None);
        assert_eq!(delta.id, "rl-1");
        assert_eq!(delta.five_hour_pct, Some(42.5));
        assert_eq!(delta.updated_at.as_deref(), Some("2026-04-20T01:00:00Z"));
        assert!(delta.deleted_at.is_none());
    }

    #[test]
    fn rate_limit_delta_tombstone_carries_deleted_at() {
        let sample = crate::statusline::RateLimitSample {
            id: "rl-2".into(),
            hostname: "puck".into(),
            session_id: "sess-b".into(),
            sampled_at: "2026-04-20T02:00:00Z".into(),
            five_hour_pct: None,
            five_hour_resets_at: None,
            seven_day_pct: None,
            seven_day_resets_at: None,
            model: None,
        };
        let delta = RateLimitDelta::from_sample(&sample, Some("2026-04-20T03:00:00Z".into()));
        assert_eq!(delta.deleted_at.as_deref(), Some("2026-04-20T03:00:00Z"));
    }

    #[test]
    fn usage_delta_from_sample_preserves_token_fields() {
        let sample = crate::statusline::UsageSample {
            id: "us-1".into(),
            hostname: "puck".into(),
            session_id: "sess-a".into(),
            turn_index: Some(12),
            model: Some("claude-sonnet-4-6".into()),
            input_tokens: 100,
            output_tokens: 200,
            cache_write_tokens: 300,
            cache_read_tokens: 400,
            effective_tokens: 640,
            error_bytes: 0,
            sampled_at: "2026-04-20T04:00:00Z".into(),
        };
        let delta = UsageDelta::from_sample(&sample, None);
        assert_eq!(delta.input_tokens, 100);
        assert_eq!(delta.cache_read_tokens, 400);
        assert_eq!(delta.effective_tokens, 640);
        assert_eq!(delta.turn_index, Some(12));
        assert!(delta.deleted_at.is_none());
    }
}
