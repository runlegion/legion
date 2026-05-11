//! Documents table -- the coordination substrate (#456, child of #455).
//!
//! Stores spec / NFR / blueprint / persona / journey / etc as rows with
//! a JSON payload + indexed meta columns hoisted from the payload's
//! `meta` block. Hot pool by default; `archived_at` populates when work
//! referencing the doc completes.
//!
//! The substrate is type-agnostic at the storage layer: any document
//! type whose payload carries the canonical `meta` shape lands here.
//! Type-specific schema validation belongs in a sibling issue once
//! vault ships the schemas.

use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{LegionError, Result};

/// A document row. The `payload` field is the raw validated JSON; the
/// indexed columns are hoisted from `payload.meta` at insert time so
/// SQL queries can filter without parsing JSON.
///
/// `id` is sourced from `payload.meta.id` when present (typed ids like
/// `FR-EMAIL-003`), or generated as a UUIDv7 otherwise. Caller-supplied
/// ids must be globally unique across the table; collision is a hard
/// error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Document {
    pub id: String,
    pub doc_type: String,
    pub surface: Option<String>,
    pub status: String,
    pub priority: Option<String>,
    pub owner: String,
    /// Raw payload JSON. Callers parse as needed for the specific type.
    pub payload: String,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Fields hoisted from `payload.meta` at insert time. Provided by the
/// caller; the storage layer does not parse the payload itself, keeping
/// this module type-agnostic. Schema validation lives in a sibling
/// child issue under #455.
#[derive(Debug, Clone)]
pub struct DocumentMeta<'a> {
    /// Optional caller-supplied id. When None, a UUIDv7 is generated.
    pub id: Option<&'a str>,
    pub doc_type: &'a str,
    pub surface: Option<&'a str>,
    /// Initial lifecycle status. Defaults to "draft" when None.
    pub status: Option<&'a str>,
    pub priority: Option<&'a str>,
    pub owner: &'a str,
}

/// Filter for `list_documents`. None on every field returns all rows.
/// Empty struct (all None) is the default broad query.
#[derive(Debug, Clone, Default)]
pub struct DocumentFilter<'a> {
    pub doc_type: Option<&'a str>,
    pub surface: Option<&'a str>,
    pub status: Option<&'a str>,
    pub owner: Option<&'a str>,
    /// When None, returns hot rows only. Some(true) returns archived only.
    /// Some(false) returns hot only (explicit).
    pub archived: Option<bool>,
}

impl Database {
    /// Insert a new document. Returns the inserted Document.
    ///
    /// `id` is taken from `meta.id` when supplied, else generated as
    /// UUIDv7. Conflict on id is a hard error.
    pub fn insert_document(&self, meta: &DocumentMeta<'_>, payload: &str) -> Result<Document> {
        let now = Utc::now().to_rfc3339();
        let id = match meta.id {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => Uuid::now_v7().to_string(),
        };
        let status = meta.status.unwrap_or("draft");

        self.conn.execute(
            "INSERT INTO documents (id, type, surface, status, priority, owner, payload, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                &id,
                meta.doc_type,
                meta.surface,
                status,
                meta.priority,
                meta.owner,
                payload,
                &now,
            ],
        ).map_err(|e| match e {
            rusqlite::Error::SqliteFailure(ref err, _)
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                LegionError::WorkSource(format!("document id '{id}' already exists"))
            }
            other => LegionError::Database(other),
        })?;

        Ok(Document {
            id,
            doc_type: meta.doc_type.to_string(),
            surface: meta.surface.map(str::to_string),
            status: status.to_string(),
            priority: meta.priority.map(str::to_string),
            owner: meta.owner.to_string(),
            payload: payload.to_string(),
            archived_at: None,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Read a document by id. Returns None if the row is soft-deleted
    /// or does not exist. Does NOT filter archived rows -- caller can
    /// check `archived_at` on the returned Document.
    pub fn get_document(&self, id: &str) -> Result<Option<Document>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, type, surface, status, priority, owner, payload, archived_at, created_at, updated_at \
             FROM documents WHERE id = ?1 AND deleted_at IS NULL",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(map_document_row(row)?))
        } else {
            Ok(None)
        }
    }

    /// List documents matching the filter. Returns rows ordered by
    /// updated_at DESC (most recently touched first).
    pub fn list_documents(&self, filter: &DocumentFilter<'_>) -> Result<Vec<Document>> {
        // Build the WHERE clause dynamically. Each filter clause adds a
        // bind parameter; archived clause is fixed SQL.
        let mut clauses: Vec<String> = vec!["deleted_at IS NULL".to_string()];
        let mut binds: Vec<String> = Vec::new();

        match filter.archived {
            None | Some(false) => clauses.push("archived_at IS NULL".to_string()),
            Some(true) => clauses.push("archived_at IS NOT NULL".to_string()),
        }

        if let Some(t) = filter.doc_type {
            clauses.push(format!("type = ?{}", binds.len() + 1));
            binds.push(t.to_string());
        }
        if let Some(s) = filter.surface {
            clauses.push(format!("surface = ?{}", binds.len() + 1));
            binds.push(s.to_string());
        }
        if let Some(st) = filter.status {
            clauses.push(format!("status = ?{}", binds.len() + 1));
            binds.push(st.to_string());
        }
        if let Some(o) = filter.owner {
            clauses.push(format!("owner = ?{}", binds.len() + 1));
            binds.push(o.to_string());
        }

        let sql = format!(
            "SELECT id, type, surface, status, priority, owner, payload, archived_at, created_at, updated_at \
             FROM documents WHERE {} ORDER BY updated_at DESC",
            clauses.join(" AND ")
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let bind_refs: Vec<&dyn rusqlite::ToSql> =
            binds.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(bind_refs.as_slice(), map_document_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Mark a document archived. Sets `archived_at = now`, updates
    /// `updated_at`. Idempotent: archiving an already-archived doc is
    /// a no-op success. Returns the updated Document.
    pub fn archive_document(&self, id: &str) -> Result<Document> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE documents \
             SET archived_at = COALESCE(archived_at, ?1), updated_at = ?1 \
             WHERE id = ?2 AND deleted_at IS NULL",
            params![&now, id],
        )?;
        if rows == 0 {
            return Err(LegionError::WorkSource(format!(
                "document '{id}' not found"
            )));
        }
        self.get_document(id)?.ok_or_else(|| {
            LegionError::WorkSource(format!("document '{id}' vanished after archive"))
        })
    }
}

fn map_document_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Document> {
    Ok(Document {
        id: row.get(0)?,
        doc_type: row.get(1)?,
        surface: row.get(2)?,
        status: row.get(3)?,
        priority: row.get(4)?,
        owner: row.get(5)?,
        payload: row.get(6)?,
        archived_at: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_db() -> Database {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.db");
        // Leak the tempdir so the path stays valid for the test's lifetime.
        // Tests run in isolated processes; the OS reclaims at exit.
        std::mem::forget(dir);
        Database::open(&path).expect("open")
    }

    fn sample_meta<'a>(doc_type: &'a str, owner: &'a str) -> DocumentMeta<'a> {
        DocumentMeta {
            id: None,
            doc_type,
            surface: None,
            status: None,
            priority: None,
            owner,
        }
    }

    #[test]
    fn insert_and_get_document_round_trips() {
        let db = open_test_db();
        let meta = DocumentMeta {
            id: Some("FR-EMAIL-003"),
            doc_type: "requirement",
            surface: Some("email"),
            status: Some("specified"),
            priority: Some("SHALL"),
            owner: "mail",
        };
        let payload = r#"{"meta":{"id":"FR-EMAIL-003"},"title":"Thread detail"}"#;
        let inserted = db.insert_document(&meta, payload).expect("insert");
        assert_eq!(inserted.id, "FR-EMAIL-003");
        assert_eq!(inserted.doc_type, "requirement");
        assert_eq!(inserted.surface.as_deref(), Some("email"));
        assert_eq!(inserted.status, "specified");
        assert_eq!(inserted.priority.as_deref(), Some("SHALL"));
        assert_eq!(inserted.owner, "mail");
        assert_eq!(inserted.payload, payload);
        assert!(inserted.archived_at.is_none());

        let fetched = db.get_document("FR-EMAIL-003").expect("get").expect("some");
        assert_eq!(fetched.id, "FR-EMAIL-003");
        assert_eq!(fetched.payload, payload);
    }

    #[test]
    fn insert_without_id_generates_uuidv7() {
        let db = open_test_db();
        let inserted = db
            .insert_document(&sample_meta("persona", "vault"), "{}")
            .expect("insert");
        // UUIDv7 string is 36 chars (8-4-4-4-12 with dashes).
        assert_eq!(inserted.id.len(), 36);
        assert!(inserted.id.contains('-'));
    }

    #[test]
    fn insert_with_duplicate_id_errors() {
        let db = open_test_db();
        let mut meta = sample_meta("requirement", "mail");
        meta.id = Some("FR-TEST-001");
        db.insert_document(&meta, "{}").expect("first");
        let err = db.insert_document(&meta, "{}").unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "expected conflict error, got: {err}"
        );
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let db = open_test_db();
        assert!(db.get_document("FR-NOPE-999").expect("get").is_none());
    }

    #[test]
    fn list_documents_filters_by_type() {
        let db = open_test_db();
        db.insert_document(&sample_meta("requirement", "mail"), "{}")
            .unwrap();
        db.insert_document(&sample_meta("persona", "vault"), "{}")
            .unwrap();
        db.insert_document(&sample_meta("requirement", "platform"), "{}")
            .unwrap();

        let filter = DocumentFilter {
            doc_type: Some("requirement"),
            ..Default::default()
        };
        let rows = db.list_documents(&filter).expect("list");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.doc_type == "requirement"));
    }

    #[test]
    fn list_documents_filters_by_surface_owner_status() {
        let db = open_test_db();
        let mut a = sample_meta("requirement", "mail");
        a.surface = Some("email");
        a.status = Some("specified");
        db.insert_document(&a, "{}").unwrap();

        let mut b = sample_meta("requirement", "platform");
        b.surface = Some("auth");
        b.status = Some("draft");
        db.insert_document(&b, "{}").unwrap();

        let filter = DocumentFilter {
            doc_type: Some("requirement"),
            surface: Some("email"),
            status: Some("specified"),
            owner: Some("mail"),
            ..Default::default()
        };
        let rows = db.list_documents(&filter).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].surface.as_deref(), Some("email"));
    }

    #[test]
    fn list_documents_excludes_archived_by_default() {
        let db = open_test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-A");
        db.insert_document(&m, "{}").unwrap();
        m.id = Some("FR-B");
        db.insert_document(&m, "{}").unwrap();

        db.archive_document("FR-A").expect("archive");

        let hot = db.list_documents(&DocumentFilter::default()).expect("list");
        assert_eq!(hot.len(), 1);
        assert_eq!(hot[0].id, "FR-B");

        let cold = db
            .list_documents(&DocumentFilter {
                archived: Some(true),
                ..Default::default()
            })
            .expect("list");
        assert_eq!(cold.len(), 1);
        assert_eq!(cold[0].id, "FR-A");
    }

    #[test]
    fn archive_document_is_idempotent() {
        let db = open_test_db();
        let mut m = sample_meta("requirement", "mail");
        m.id = Some("FR-IDEM");
        db.insert_document(&m, "{}").unwrap();

        let first = db.archive_document("FR-IDEM").expect("first");
        assert!(first.archived_at.is_some());
        let first_ts = first.archived_at.clone();

        // Second archive does not change archived_at (COALESCE preserves
        // the original timestamp).
        let second = db.archive_document("FR-IDEM").expect("second");
        assert_eq!(second.archived_at, first_ts);
    }

    #[test]
    fn archive_nonexistent_returns_error() {
        let db = open_test_db();
        let err = db.archive_document("FR-NOPE").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
