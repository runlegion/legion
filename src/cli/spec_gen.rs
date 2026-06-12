//! `legion spec-gen` handler (#527): generate requirement documents from
//! service-design artifacts on a surface.

use crate::cli::util::open_db;
use crate::documents::DocumentFilter;
use crate::error::Result;
use crate::spec_gen::{self, SERVICE_DESIGN_TYPES};

/// Run spec-gen for a surface: read all non-archived service-design docs,
/// run the pipeline, and print a summary.
pub(crate) fn handle(surface: &str) -> Result<()> {
    let db = open_db()?;

    // Collect all non-archived service-design documents on this surface.
    let mut docs = Vec::new();
    for doc_type in SERVICE_DESIGN_TYPES {
        let filter = DocumentFilter {
            doc_type: Some(doc_type),
            surface: Some(surface),
            archived: None, // hot only (None = not archived)
            ..Default::default()
        };
        docs.extend(db.list_documents(&filter)?);
    }

    if docs.is_empty() {
        println!(
            "[spec-gen] no service-design documents found on surface '{surface}' \
             (types: {})",
            SERVICE_DESIGN_TYPES.join(", ")
        );
        return Ok(());
    }

    let outcome = spec_gen::generate_requirements(&docs)?;
    let persist = spec_gen::persist_requirements(&db, surface, outcome)?;

    println!(
        "[spec-gen] surface={surface}: created {created}, skipped {skipped} existing, \
         skipped {mismatch} surface-mismatch, rejected {rejected}",
        created = persist.created,
        skipped = persist.skipped_existing,
        mismatch = persist.skipped_mismatch,
        rejected = persist.rejected.len(),
    );

    for r in &persist.rejected {
        println!(
            "[spec-gen] rejected '{title}' (traces_to={traces_to}): {reason}",
            title = r.title,
            traces_to = r.traces_to,
            reason = r.reason,
        );
    }

    Ok(())
}
