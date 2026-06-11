//! `legion document` handlers (carved from main.rs, #610).

use std::path::PathBuf;

use clap::Subcommand;

use crate::cli::util::open_db;
use crate::{db, documents, error};

#[derive(Subcommand)]
pub(crate) enum DocumentAction {
    /// Insert a new document. Payload is read from --from (file path)
    /// or stdin when --from is omitted. Meta fields are passed
    /// explicitly to keep the storage layer type-agnostic; the schema
    /// registry (sibling child of #455) will validate payloads against
    /// the meta.type's schema once it ships.
    Create {
        /// Document type (e.g. "requirement", "nfr", "persona").
        #[arg(long, value_name = "TYPE")]
        doc_type: String,
        /// Owner agent id.
        #[arg(long)]
        owner: String,
        /// Optional explicit id. Defaults to a UUIDv7 when omitted; for
        /// canonical typed ids like FR-EMAIL-003, pass them here.
        #[arg(long)]
        id: Option<String>,
        /// Functional surface (omit for cross-cutting / type=blueprint).
        #[arg(long)]
        surface: Option<String>,
        /// Initial lifecycle status (default: "draft").
        #[arg(long)]
        status: Option<String>,
        /// RFC 2119 conformance level (requirement only): SHALL|SHOULD|MAY.
        #[arg(long)]
        priority: Option<String>,
        /// Read payload from this file. When omitted, reads from stdin.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// View a document by id.
    View {
        id: String,
        /// Emit full row as JSON (default: human-friendly summary).
        #[arg(long)]
        json: bool,
    },
    /// List documents, optionally filtered.
    List {
        #[arg(long, value_name = "TYPE")]
        doc_type: Option<String>,
        #[arg(long)]
        surface: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        owner: Option<String>,
        /// Include archived documents (default: hot only).
        #[arg(long)]
        archived: bool,
        /// Emit full result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Mark a document archived.
    Archive { id: String },
    /// Validate a JSON instance against a landed schema document (#526).
    /// Checks the dependency-free subset: type, required, properties,
    /// items, enum. Exits non-zero with one error per violation.
    Validate {
        /// Id of the schema document (doc_type=schema) to validate against.
        #[arg(long)]
        schema: String,
        /// Read the instance from this file. When omitted, reads stdin.
        #[arg(long)]
        file: Option<PathBuf>,
    },
}

pub(crate) fn handle(action: DocumentAction) -> error::Result<()> {
    // Payload/instance text from --from/--file path or stdin.
    fn read_json_arg(path: Option<PathBuf>) -> error::Result<String> {
        match path {
            Some(p) => Ok(std::fs::read_to_string(&p)?),
            None => {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                Ok(buf)
            }
        }
    }
    let database = open_db()?;
    match action {
        DocumentAction::Create {
            doc_type,
            owner,
            id,
            surface,
            status,
            priority,
            from,
        } => {
            let payload = read_json_arg(from)?;
            // Sanity-validate JSON shape so a typo doesn't land
            // in the table as a string blob.
            serde_json::from_str::<serde_json::Value>(&payload).map_err(|e| {
                error::LegionError::WorkSource(format!("payload is not valid JSON: {e}"))
            })?;
            // Schema documents get a structural gate (#526): a
            // malformed JSON Schema is rejected at create, and a
            // valid one is summarized for the recall pointer below.
            let schema_summary = if doc_type == "schema" {
                Some(documents::validate_schema_payload(&payload)?)
            } else {
                None
            };
            let meta = documents::DocumentMeta {
                id: id.as_deref(),
                doc_type: &doc_type,
                surface: surface.as_deref(),
                status: status.as_deref(),
                priority: priority.as_deref(),
                owner: &owner,
            };
            let doc = database.insert_document(&meta, &payload)?;
            // Dual-write a pointer reflection (domain=schema) so
            // `legion recall --domain schema` surfaces the schema
            // (#526, option A: documents hold the canonical payload,
            // reflections hold the searchable prose pointer). Repo
            // scoping follows the owning agent.
            if let Some(summary) = schema_summary {
                let text = documents::schema_pointer_text(&doc.id, &summary);
                let meta = db::ReflectionMeta {
                    domain: Some("schema".to_string()),
                    tags: Some("schema,document-pointer".to_string()),
                    parent_id: None,
                };
                database
                    .insert_reflection_with_meta(&owner, &text, "self", &meta)
                    .map_err(|e| {
                        error::LegionError::WorkSource(format!(
                            "document {} created, but the schema pointer reflection \
                             failed: {e}. Re-create the pointer with: legion reflect \
                             --repo {} --text '{}'",
                            doc.id, owner, text
                        ))
                    })?;
            }
            println!("{}", doc.id);
        }
        DocumentAction::View { id, json } => {
            let doc = database.get_document(&id)?.ok_or_else(|| {
                error::LegionError::WorkSource(format!("document '{id}' not found"))
            })?;
            if json {
                println!("{}", serde_json::to_string(&doc)?);
            } else {
                println!("id:       {}", doc.id);
                println!("type:     {}", doc.doc_type);
                if let Some(s) = &doc.surface {
                    println!("surface:  {s}");
                }
                println!("status:   {}", doc.status);
                if let Some(p) = &doc.priority {
                    println!("priority: {p}");
                }
                println!("owner:    {}", doc.owner);
                if let Some(a) = &doc.archived_at {
                    println!("archived: {a}");
                }
                println!("created:  {}", doc.created_at);
                println!("updated:  {}", doc.updated_at);
                println!("--- payload ---");
                println!("{}", doc.payload);
            }
        }
        DocumentAction::List {
            doc_type,
            surface,
            status,
            owner,
            archived,
            json,
        } => {
            let filter = documents::DocumentFilter {
                doc_type: doc_type.as_deref(),
                surface: surface.as_deref(),
                status: status.as_deref(),
                owner: owner.as_deref(),
                archived: if archived { Some(true) } else { None },
            };
            let docs = database.list_documents(&filter)?;
            if json {
                println!("{}", serde_json::to_string(&docs)?);
            } else if docs.is_empty() {
                println!("[legion] no documents match filter");
            } else {
                use std::io::Write;
                let stdout = std::io::stdout();
                let mut out = stdout.lock();
                writeln!(
                    out,
                    "{:<24} {:<14} {:<14} {:<14} {:<10}",
                    "id", "type", "surface", "owner", "status"
                )?;
                for d in &docs {
                    writeln!(
                        out,
                        "{:<24} {:<14} {:<14} {:<14} {:<10}",
                        d.id,
                        d.doc_type,
                        d.surface.as_deref().unwrap_or("-"),
                        d.owner,
                        d.status,
                    )?;
                }
            }
        }
        DocumentAction::Archive { id } => {
            let doc = database.archive_document(&id)?;
            println!(
                "archived {} at {}",
                doc.id,
                doc.archived_at.as_deref().unwrap_or("?")
            );
        }
        DocumentAction::Validate { schema, file } => {
            let doc = database.get_document(&schema)?.ok_or_else(|| {
                error::LegionError::WorkSource(format!("schema document '{schema}' not found"))
            })?;
            if doc.doc_type != "schema" {
                return Err(error::LegionError::WorkSource(format!(
                    "document '{schema}' has doc_type '{}', not 'schema'",
                    doc.doc_type
                )));
            }
            let schema_value: serde_json::Value =
                serde_json::from_str(&doc.payload).map_err(|e| {
                    error::LegionError::WorkSource(format!(
                        "stored schema payload is not valid JSON: {e}"
                    ))
                })?;
            let instance_text = read_json_arg(file)?;
            let instance: serde_json::Value =
                serde_json::from_str(&instance_text).map_err(|e| {
                    error::LegionError::WorkSource(format!("instance is not valid JSON: {e}"))
                })?;
            let errors = documents::validate_instance(&schema_value, &instance);
            if errors.is_empty() {
                println!("valid against schema {}", doc.id);
            } else {
                for e in &errors {
                    eprintln!("{e}");
                }
                return Err(error::LegionError::WorkSource(format!(
                    "instance failed validation against '{}': {} error(s)",
                    doc.id,
                    errors.len()
                )));
            }
        }
    }
    Ok(())
}
