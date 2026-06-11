//! DDL for the documents table (#456). The document methods live in
//! src/documents.rs, which carries its own `impl Database` block.

use rusqlite::Connection;

use crate::error::Result;

/// `documents` table (#456).
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    // Migration 21: Documents table for the coordination substrate
    // (#456, child of #455).
    //
    // Stores spec / NFR / blueprint / persona / journey / etc as rows
    // with structured meta columns hoisted from a JSON payload. Hot
    // pool by default; archived_at populates when work referencing
    // the doc completes (kanban-spec linkage in a later child issue).
    //
    // The payload column holds the validated JSON for the document
    // type. Type-specific schema validation at INSERT happens in a
    // sibling child issue under #455 once vault's schemas land. For
    // #456 the foundation ships; validator is a stub that accepts
    // any JSON.
    conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS documents (
                id TEXT PRIMARY KEY,
                type TEXT NOT NULL,
                surface TEXT,
                status TEXT NOT NULL DEFAULT 'draft',
                priority TEXT,
                owner TEXT NOT NULL,
                payload TEXT NOT NULL,
                archived_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_documents_type
                ON documents(type) WHERE archived_at IS NULL AND deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_documents_surface
                ON documents(surface) WHERE archived_at IS NULL AND deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_documents_owner
                ON documents(owner) WHERE archived_at IS NULL AND deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_documents_status
                ON documents(status) WHERE archived_at IS NULL AND deleted_at IS NULL;
            CREATE INDEX IF NOT EXISTS idx_documents_archived
                ON documents(archived_at, type) WHERE archived_at IS NOT NULL AND deleted_at IS NULL;",
        )?;
    Ok(())
}
