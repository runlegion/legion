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
}
