//! Reflection memory handlers: reflect, forget, recall, similar, consult,
//! boost, resolve, chain, backfill (carved from main.rs, #610).

use std::path::PathBuf;

use crate::cli::datadir::data_dir;
use crate::cli::index_cmd::run_consult_symbol;
use crate::cli::util::{audit, open_db, open_db_and_index};
use crate::{db, embed, error, recall, reflect, search};

/// Controls near-duplicate detection behavior on `legion reflect`.
#[derive(clap::ValueEnum, Clone, Debug, Default)]
pub(crate) enum DedupeMode {
    /// Warn on stderr when a near-duplicate is found, but still store the reflection.
    #[default]
    Warn,
    /// Refuse to store the reflection and exit non-zero when a near-duplicate is found.
    Strict,
    /// Skip the near-duplicate check entirely.
    Off,
}

/// Run a compound command (text or transcript) across multiple repos with metadata.
///
/// Prints each stored ID to stdout (one per repo) so callers and scripts
/// can capture them. Returns an error if any repo fails.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_compound_command_with_meta(
    db: &db::Database,
    index: &search::SearchIndex,
    repos: &[String],
    text: &Option<String>,
    transcript: &Option<PathBuf>,
    meta: &db::ReflectionMeta,
    from_text: fn(
        &db::Database,
        &search::SearchIndex,
        &str,
        &str,
        &db::ReflectionMeta,
    ) -> error::Result<String>,
    from_transcript: fn(
        &db::Database,
        &search::SearchIndex,
        &str,
        &std::path::Path,
        &db::ReflectionMeta,
    ) -> error::Result<String>,
    label: &str,
) -> error::Result<()> {
    if text.is_none() && transcript.is_none() {
        return Err(error::LegionError::NoReflectionInput);
    }

    let mut had_error = false;
    for r in repos {
        let result = match (text, transcript) {
            (Some(t), None) => from_text(db, index, r, t, meta),
            (None, Some(path)) => from_transcript(db, index, r, path, meta),
            (Some(_), Some(_)) => return Err(error::LegionError::NoReflectionInput),
            (None, None) => unreachable!("guarded by early return above"),
        };
        match result {
            Ok(id) => {
                info!("[legion] {label} for {r} ({id})");
                println!("{id}");
            }
            Err(e) => {
                eprintln!("[legion] error {label} for {r}: {e}");
                had_error = true;
            }
        }
    }
    if had_error {
        return Err(error::LegionError::ReflectPartialFailure);
    }
    Ok(())
}

/// Try to load the embedding model. Returns None if not available.
///
/// Logs a warning via `info!` on failure so degraded hybrid search is visible
/// in `--verbose` mode without spamming default-quiet runs (which otherwise
/// fail integration tests asserting `stderr.is_empty()` whenever the model
/// fetch hits a transient network error like HuggingFace 429).
pub(crate) fn try_load_embed_model() -> Option<embed::EmbedModel> {
    match embed::EmbedModel::load() {
        Ok(model) => Some(model),
        Err(e) => {
            info!("[legion] embedding model unavailable, falling back to BM25: {e}");
            None
        }
    }
}

/// Compute and store embeddings for all reflections that are missing them.
pub(crate) fn backfill_embeddings(
    db: &db::Database,
    model: &embed::EmbedModel,
) -> error::Result<usize> {
    let missing = db.get_ids_without_embeddings()?;
    let mut count: usize = 0;

    for (id, text) in &missing {
        match model.encode_one(text) {
            Ok(embedding) => {
                let bytes = embed::embedding_to_bytes(&embedding);
                if db.store_embedding(id, &bytes)? {
                    count += 1;
                }
            }
            Err(e) => {
                eprintln!("[legion] warning: failed to embed {}: {}", id, e);
            }
        }
    }

    Ok(count)
}

/// Check new reflection text for near-duplicates in the same repo.
///
/// Computes the embedding of `text`, then compares it against the 100 most
/// recent reflections with embeddings for `repo`. When cosine similarity
/// exceeds 0.95, warns to stderr. In `Strict` mode the function returns an
/// error so the caller aborts before storing. In `Warn` mode it returns Ok
/// and the reflection is stored anyway.
fn run_dedupe_check(
    db: &db::Database,
    model: &embed::EmbedModel,
    repo: &str,
    text: &str,
    mode: &DedupeMode,
) -> error::Result<()> {
    const DEDUPE_THRESHOLD: f32 = 0.95;
    const DEDUPE_LOOKBACK: usize = 100;

    let new_emb = match model.encode_one(text) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[legion] warning: dedupe check skipped (embed failed): {e}");
            return Ok(());
        }
    };

    let recent = db.get_recent_reflections_with_embeddings(repo, DEDUPE_LOOKBACK)?;

    for (id, blob, existing_text, created_at) in &recent {
        let existing_emb = embed::embedding_from_bytes(blob);
        let sim = embed::cosine_similarity(&new_emb, &existing_emb);

        if sim >= DEDUPE_THRESHOLD {
            let preview: String = existing_text.chars().take(80).collect();
            let date = db::format_date(created_at);
            eprintln!(
                "[legion] warning: this reflection is {sim:.2} cosine to reflection {id} \
                 (created {date}): \"{preview}\". \
                 stored anyway (set --dedupe-mode strict to refuse). \
                 Consider `legion reflect --follows {id}` to extend."
            );

            if matches!(mode, DedupeMode::Strict) {
                return Err(error::LegionError::Embedding(format!(
                    "near-duplicate detected (cosine {sim:.2} >= {DEDUPE_THRESHOLD}) \
                     with reflection {id} -- use --force to store anyway"
                )));
            }

            // In Warn mode we warn once for the closest match and stop checking.
            break;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_reflect(
    repo: Vec<String>,
    text: Option<String>,
    transcript: Option<PathBuf>,
    domain: Option<String>,
    whoami: bool,
    tags: Option<String>,
    follows: Option<String>,
    force: bool,
    dedupe_mode: DedupeMode,
) -> error::Result<()> {
    let domain = if whoami {
        Some("identity".to_owned())
    } else {
        domain
    };
    let (database, index) = open_db_and_index()?;

    // Near-duplicate detection before storing (Card C).
    // Skip when --force or --dedupe-mode off.
    let skip_dedupe = force || matches!(dedupe_mode, DedupeMode::Off);
    if !skip_dedupe
        && let Some(ref t) = text
        && let Some(model) = try_load_embed_model()
    {
        // Compound repos (comma-separated) each get their own check.
        for r in &repo {
            run_dedupe_check(&database, &model, r, t, &dedupe_mode)?;
        }
        // If model is unavailable, skip dedupe silently (best-effort).
    }

    let meta = db::ReflectionMeta {
        domain,
        tags,
        parent_id: follows,
    };

    run_compound_command_with_meta(
        &database,
        &index,
        &repo,
        &text,
        &transcript,
        &meta,
        reflect::reflect_from_text_with_meta,
        reflect::reflect_from_transcript_with_meta,
        "storing reflection",
    )?;

    // Compute embeddings for new reflections (silent fail if model unavailable)
    if let Some(model) = try_load_embed_model() {
        let n = backfill_embeddings(&database, &model)?;
        if n > 0 {
            info!("[legion] embedded {} reflections", n);
        }
    }
    Ok(())
}

/// `legion reflect retag --id X --set-domain <name|none>` (#783): move a
/// live reflection between domains in place. The literal `none` clears
/// the domain. Pure DB metadata write -- `domain` has no tantivy
/// presence, so unlike `forget` there is no index side to reconcile and
/// `open_db` suffices.
pub(crate) fn handle_retag(id: String, set_domain: String) -> error::Result<()> {
    let database = open_db()?;

    let new_domain = if set_domain == "none" {
        None
    } else {
        Some(set_domain.as_str())
    };

    // Peek first so the audit row and the confirmation line can name the
    // old domain, matching handle_forget's peek-then-write shape.
    let existing = database
        .get_reflection_by_id(&id)?
        .ok_or_else(|| error::LegionError::ReflectionNotFound(id.clone()))?;
    let old_domain = existing.domain.clone();

    let details = format!(
        "from={} to={}",
        old_domain.as_deref().unwrap_or("none"),
        new_domain.unwrap_or("none")
    );
    let write_audit = |outcome: &str| {
        audit(
            &database,
            &db::AuditInput {
                agent: &existing.repo,
                action: "retag-reflection",
                target_type: "reflection",
                target_ref: &id,
                task_id: None,
                source_type: "legion",
                details: Some(&details),
                outcome,
            },
        );
    };

    let retagged = match database.retag_reflection(&id, new_domain) {
        Ok(r) => r,
        Err(
            e @ (error::LegionError::RetagLastIdentityRoot { .. }
            | error::LegionError::RetagLastWorkflowRoot { .. }
            | error::LegionError::IdentityRootExists { .. }),
        ) => {
            // Guard refusals are forensically relevant for the same
            // reason handle_forget audits its --repo rejections: we want
            // a trace of every attempt to mutate a root, not just the
            // ones that landed.
            write_audit("rejected");
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    write_audit("success");

    let preview: String = retagged.text.chars().take(80).collect();
    let ellipsis = if retagged.text.chars().count() > 80 {
        "..."
    } else {
        ""
    };
    println!(
        "retagged reflection {} ({}): domain {} -> {}: {}{} -- still hot and recallable; id, chain, and recall_count unchanged.",
        id,
        retagged.repo,
        old_domain.as_deref().unwrap_or("none"),
        retagged.domain.as_deref().unwrap_or("none"),
        preview,
        ellipsis
    );
    Ok(())
}

pub(crate) fn handle_forget(id: String, repo: Option<String>, persist: bool) -> error::Result<()> {
    let (database, index) = open_db_and_index()?;

    // Peek at the reflection first so we can run the optional
    // --repo safety check AND print a summary of what is about
    // to be forgotten. The actual write goes through
    // `delete_reflection` / `archive_reflection`, which re-verify the
    // id exists.
    let existing = database
        .get_reflection_by_id(&id)?
        .ok_or_else(|| error::LegionError::ReflectionNotFound(id.clone()))?;

    // Every audit row for this command shares target_type/target_ref/
    // task_id/source_type; only the action differs by disposition
    // (permanent delete vs. #782's archive). Build audit rows through
    // one closure so the three call sites (rejected / success / partial)
    // only name the three things that actually differ.
    let action = if persist {
        "archive-reflection"
    } else {
        "delete-reflection"
    };
    let write_audit = |agent: &str, outcome: &str, details: Option<&str>| {
        audit(
            &database,
            &db::AuditInput {
                agent,
                action,
                target_type: "reflection",
                target_ref: &id,
                task_id: None,
                source_type: "legion",
                details,
                outcome,
            },
        );
    };

    if let Some(ref expected_repo) = repo
        && existing.repo != *expected_repo
    {
        // Audit the rejection BEFORE returning. Destructive-command
        // rejections are forensically relevant -- we want a trace
        // of every attempted forget, not just successful ones.
        let details = format!("expected={} actual={}", expected_repo, existing.repo);
        write_audit(&existing.repo, "rejected", Some(&details));
        return Err(error::LegionError::ReflectionRepoMismatch {
            id: id.clone(),
            actual: existing.repo.clone(),
            expected: expected_repo.clone(),
        });
    }

    if persist {
        // Archive: the row and its tantivy index entry both survive
        // (#782). No index write here -- unlike the permanent-delete
        // path below, there is no index-side counterpart to reconcile;
        // the search index returns ids regardless of archive state, and
        // the DB join step is what applies the archive-mode filter.
        let archived = database.archive_reflection(&id)?;
        write_audit(&archived.repo, "success", None);

        let preview: String = archived.text.chars().take(80).collect();
        let ellipsis = if archived.text.chars().count() > 80 {
            "..."
        } else {
            ""
        };
        println!(
            "persisted reflection {} ({}): {}{} -- archived, out of hot recall/whoami/whatami, \
             still reachable via `recall --archives` / `--include-archives`.",
            id, archived.repo, preview, ellipsis
        );
        return Ok(());
    }

    // Delete from SQLite first. The audit entry is written AFTER
    // the SQLite delete succeeds but BEFORE the index delete,
    // so even if the tantivy side fails we still have a record
    // that the db-side delete happened. The two-store write is
    // not transactional -- if index.delete fails, the next
    // `legion reindex` run will reconcile.
    let deleted = database.delete_reflection(&id)?;
    write_audit(&deleted.repo, "success", None);

    if let Err(e) = index.delete(&id) {
        // SQLite row is gone; tantivy still has the document.
        // Tell the operator exactly what state the system is in
        // and how to recover. Do not silently succeed.
        eprintln!(
            "[legion forget] WARNING: SQLite delete succeeded but tantivy index delete failed for {id}.\n\
             The reflection is gone from the database but may still appear in BM25 recall results\n\
             as a ghost document until the index is rebuilt. Run `legion reindex` to reconcile.\n\
             Underlying error: {e}"
        );
        write_audit(
            &deleted.repo,
            "partial",
            Some("index-orphan: SQLite row deleted, tantivy index delete failed"),
        );
        return Err(e);
    }

    let preview: String = deleted.text.chars().take(80).collect();
    let ellipsis = if deleted.text.chars().count() > 80 {
        "..."
    } else {
        ""
    };
    println!(
        "forgot reflection {} ({}): {}{}",
        id, deleted.repo, preview, ellipsis
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_recall(
    repo: String,
    context: String,
    limit: usize,
    latest: bool,
    preview: Option<usize>,
    cosine_only: bool,
    min_score: Option<f32>,
    domain: Option<String>,
    archives: bool,
    include_archives: bool,
) -> error::Result<()> {
    let database = open_db()?;

    // Resolve archive mode (#457). Mutually-exclusive flags
    // enforced at the clap layer via conflicts_with.
    let mode = if archives {
        recall::ArchiveMode::Cold
    } else if include_archives {
        recall::ArchiveMode::Both
    } else {
        recall::ArchiveMode::Hot
    };

    // #782 closed the --domain / --latest half of the #457 coverage gap:
    // both now thread the resolved archive mode through to their DB
    // backers. --cosine-only remains out of scope -- it ranks purely by
    // embedding similarity against `get_embeddings`, which has no
    // archive-mode parameter, and cosine ranking over a mixed hot+cold
    // corpus is a different feature than this issue's DB-join filter, not
    // a caller-side wiring gap. Warn loudly there so an operator does not
    // silently get hot-only results from --cosine-only --archives.
    //
    // This condition does not need to also check `domain.is_none() &&
    // !latest`: clap's `conflicts_with` on the `domain`/`latest`/
    // `cosine_only` args (src/cli/mod.rs) is enforced bidirectionally, so
    // `cosine_only` can never be `true` here while `domain` is `Some` or
    // `latest` is `true` -- confirmed empirically (`recall --domain x
    // --cosine-only` and `recall --latest --cosine-only` both refuse to
    // parse). If `cosine_only` reaches this point, the `if let`/`else if`
    // chain below is guaranteed to fall through to the `cosine_only`
    // branch, so the warning is never a false alarm for a run that
    // actually takes the --domain or --latest path.
    if (archives || include_archives) && cosine_only {
        eprintln!(
            "[legion] warning: --archives / --include-archives do not apply to --cosine-only, which ranks purely by embedding similarity with no archive-mode filter. This run uses hot-only results."
        );
    }

    let mut result = if let Some(ref dom) = domain {
        recall::recall_by_domain(&database, &repo, dom, limit, mode)?
    } else if latest {
        recall::recall_latest(&database, &repo, limit, mode)?
    } else if cosine_only {
        // --cosine-only requires the embed model; error if unavailable.
        let model = embed::EmbedModel::load().map_err(|e| {
            error::LegionError::Embedding(format!("--cosine-only requires embedding model: {e}"))
        })?;
        recall::recall_cosine_only(&database, &model, &repo, &context, limit, min_score)?
    } else {
        let index = search::SearchIndex::open(&data_dir()?.join("index"))?;
        // Try hybrid (BM25 + cosine) recall, fall back to BM25-only
        match try_load_embed_model() {
            Some(model) => recall::recall(&database, &index, &model, &repo, &context, limit, mode)?,
            None => recall::recall_bm25(&database, &index, &repo, &context, limit, mode)?,
        }
    };
    // Apply min-score filter on hybrid/latest paths (cosine-only applies it inline).
    if !cosine_only && let Some(threshold) = min_score {
        recall::filter_by_min_score(&mut result, threshold);
    }
    let output = recall::format_for_hook(&result, preview);
    if !output.is_empty() {
        print!("{output}");
    }
    Ok(())
}

pub(crate) fn handle_similar(
    id: String,
    limit: usize,
    cross_repo: bool,
    min_score: Option<f32>,
    preview: Option<usize>,
    json: bool,
) -> error::Result<()> {
    let database = open_db()?;
    // Validate the embed model is available; similar needs embeddings to work.
    embed::EmbedModel::load().map_err(|e| {
        error::LegionError::Embedding(format!("legion similar requires embedding model: {e}"))
    })?;
    let result = recall::find_similar_by_id(&database, &id, limit, cross_repo, min_score)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let output = recall::format_for_hook(&result, preview);
        if !output.is_empty() {
            print!("{output}");
        }
    }
    Ok(())
}

pub(crate) fn handle_consult(
    context: Option<String>,
    symbol: Option<String>,
    limit: usize,
    json: bool,
) -> error::Result<()> {
    let database = open_db()?;

    match (context, symbol) {
        (Some(ctx), None) => {
            let index = search::SearchIndex::open(&data_dir()?.join("index"))?;
            let result = match try_load_embed_model() {
                Some(model) => recall::consult(&database, &index, &model, &ctx, limit)?,
                None => recall::consult_bm25(&database, &index, &ctx, limit)?,
            };
            let output = recall::format_for_consult(&result);
            if output.is_empty() {
                info!("[legion] no reflections matched context: \"{}\"", ctx);
            } else {
                print!("{output}");
            }
        }
        (None, Some(name)) => {
            run_consult_symbol(&database, &name, json)?;
        }
        (None, None) => {
            return Err(error::LegionError::WatchConfig(
                "either --context <query> or --symbol <name> is required".to_string(),
            ));
        }
        (Some(_), Some(_)) => {
            // clap's conflicts_with should reject this before we get here.
            return Err(error::LegionError::WatchConfig(
                "--context and --symbol are mutually exclusive".to_string(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn handle_boost(id: String) -> error::Result<()> {
    let database = open_db()?;

    if database.boost_reflection(&id)? {
        info!("[legion] boosted reflection {}", id);
    } else {
        eprintln!("[legion] reflection not found: {}", id);
    }
    Ok(())
}

pub(crate) fn handle_resolve(id: String, reflection: Option<String>) -> error::Result<()> {
    let database = open_db()?;

    if database.resolve_post(&id, reflection.as_deref())? {
        info!("[legion] resolved {}", id);
    } else {
        eprintln!("[legion] post not found: {}", id);
        return Err(error::LegionError::ExitWith(1));
    }
    Ok(())
}

pub(crate) fn handle_chain(id: String, full: bool) -> error::Result<()> {
    let database = open_db()?;

    let chain = database.get_chain(&id)?;
    if chain.is_empty() {
        info!("[legion] no chain found for {}", id);
    } else if full {
        // Boundary marker `--- ` is parsed by the
        // identity-chain-load.sh UserPromptSubmit hook (#345) to
        // count chain links. Do not change the prefix without
        // updating the hook script.
        for r in &chain {
            let date = db::format_date(&r.created_at);
            let domain_tag = r
                .domain
                .as_deref()
                .map(|d| format!(" [{}]", d))
                .unwrap_or_default();
            println!("--- {} {}{} (id: {}) ---", r.repo, date, domain_tag, r.id);
            println!("{}", r.text);
            println!();
        }
    } else {
        for (i, r) in chain.iter().enumerate() {
            let prefix = if i == 0 {
                String::new()
            } else {
                "  ".repeat(i) + "-> "
            };
            let date = db::format_date(&r.created_at);
            let domain_tag = r
                .domain
                .as_deref()
                .map(|d| format!(" [{}]", d))
                .unwrap_or_default();
            let truncated: String = r.text.chars().take(80).collect();
            let ellipsis = if r.text.len() > 80 { "..." } else { "" };
            println!(
                "{}{} {}{}: {}{}",
                prefix, r.repo, date, domain_tag, truncated, ellipsis
            );
        }
    }
    Ok(())
}

pub(crate) fn handle_backfill() -> error::Result<()> {
    let database = open_db()?;

    let model = embed::EmbedModel::load()?;
    let count = backfill_embeddings(&database, &model)?;
    info!("[legion] embedded {} reflections", count);
    Ok(())
}
