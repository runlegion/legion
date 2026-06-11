use std::collections::HashMap;

use chrono::Utc;

use crate::db::Database;
use crate::embed::{self, EmbedModel};
use crate::error::Result;
use crate::search::{SearchIndex, SearchResult};

/// Minimum cosine similarity for a cosine-only candidate (no BM25 match).
/// Prevents noise from weak semantic matches when BM25 found nothing.
const COSINE_MIN_THRESHOLD: f32 = 0.3;

/// Banner that wraps `legion whoami` output. Identity is the first thing an
/// agent sees on session start, and the banner is what makes it impossible
/// to skim past.
pub const WHOAMI_BANNER_OPEN: &str = "=== WHO YOU ARE -- READ THIS ===";
pub const WHOAMI_BANNER_CLOSE: &str = "=== END IDENTITY ===";

/// Soft byte budget for `legion whoami` output. Sized for the SessionStart
/// inline context window -- larger payloads are truncated by the harness to
/// a 2KB preview, which silently drops doctrines past the cutoff. Keeping
/// the whole banner under this budget guarantees nothing is dropped.
pub const WHOAMI_BYTE_CAP: usize = 2048;

/// Banner that wraps `legion whatami` output -- the operating contract (how I
/// operate), distinct from whoami (who I am). Lands right after identity at
/// SessionStart: WHO YOU ARE, then HOW YOU OPERATE.
pub const WHATAMI_BANNER_OPEN: &str = "=== HOW YOU OPERATE -- READ THIS ===";
pub const WHATAMI_BANNER_CLOSE: &str = "=== END OPERATING CONTRACT ===";

/// Soft byte budget for `legion whatami` output. Same rationale and size as
/// `WHOAMI_BYTE_CAP`: keep the SessionStart block under the harness's 2KB
/// inline-context cutoff so nothing is silently dropped.
pub const WHATAMI_BYTE_CAP: usize = 2048;

/// A single entry passed to the banner formatters. The flag indicates whether
/// the reflection has chain context worth pointing the reader at. Shared by
/// `format_whoami` (identity roots) and `format_whatami` (workflow roots).
pub struct WhoamiEntry {
    pub id: String,
    pub text: String,
    pub in_chain: bool,
}

/// Render the whoami banner from the supplied identity reflections, capped at
/// `WHOAMI_BYTE_CAP` bytes total. Iterates entries in order; each entry is
/// emitted in full if its addition keeps the buffer (plus footer) under the
/// cap. The first entry is always emitted regardless of size to avoid an
/// empty banner -- a single oversized root is preferable to silent absence.
/// Remaining entries are summarized with a recall pointer.
/// Render a byte-capped boot banner from root reflections. Shared by
/// `format_whoami` and `format_whatami`. Emits each entry in full while it fits
/// under `cap`; the first entry always emits even if oversized (a single large
/// root beats an empty banner), and the remainder is summarized with a recall
/// pointer to `--domain {recall_domain}`.
#[allow(clippy::too_many_arguments)]
fn format_capped_banner(
    open: &str,
    close: &str,
    header_line: &str,
    truncation_noun: &str,
    recall_domain: &str,
    repo: &str,
    cap: usize,
    entries: &[WhoamiEntry],
) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let header = format!("{open}\n{header_line}\n");
    let footer = format!("{close}\n");
    let mut buf = header;
    let mut emitted = 0usize;
    for entry in entries {
        let chain_line = if entry.in_chain {
            format!("  \u{21b3} chain context: legion chain --id {}\n", entry.id)
        } else {
            String::new()
        };
        let body = format!("- {} (id: {})\n{}", entry.text, entry.id, chain_line);
        if buf.len() + body.len() + footer.len() > cap && emitted > 0 {
            break;
        }
        buf.push_str(&body);
        emitted += 1;
    }
    let remaining = entries.len().saturating_sub(emitted);
    if remaining > 0 {
        buf.push_str(&format!(
            "- ({remaining} more {truncation_noun} truncated; recall via `legion recall --repo {repo} --domain {recall_domain}`)\n"
        ));
    }
    buf.push_str(&footer);
    buf
}

/// Render the whoami banner (identity roots), capped at `WHOAMI_BYTE_CAP`.
pub fn format_whoami(repo: &str, entries: &[WhoamiEntry]) -> String {
    format_capped_banner(
        WHOAMI_BANNER_OPEN,
        WHOAMI_BANNER_CLOSE,
        &format!("[Legion] Identity for {repo}:"),
        "identity reflections",
        "identity",
        repo,
        WHOAMI_BYTE_CAP,
        entries,
    )
}

/// Render the whatami banner (operating-contract / workflow roots), capped at
/// `WHATAMI_BYTE_CAP`. This is HOW the agent operates, distinct from whoami.
pub fn format_whatami(repo: &str, entries: &[WhoamiEntry]) -> String {
    format_capped_banner(
        WHATAMI_BANNER_OPEN,
        WHATAMI_BANNER_CLOSE,
        &format!("[Legion] How {repo} operates:"),
        "operating-contract reflections",
        "workflow",
        repo,
        WHATAMI_BYTE_CAP,
        entries,
    )
}

/// A set of recalled reflections matching a query, optionally scoped to a single repo.
#[derive(Debug, serde::Serialize)]
pub struct RecallResult {
    pub reflections: Vec<RecalledReflection>,
    pub query: String,
    pub repo: String,
}

/// A single recalled reflection with its BM25 relevance score.
#[derive(Debug, serde::Serialize)]
pub struct RecalledReflection {
    pub id: String,
    pub repo: String,
    pub text: String,
    pub score: f32,
    pub created_at: String,
}

/// Compute a decay factor based on how recently a reflection was recalled.
///
/// Returns 1.0 for reflections recalled in the last 7 days, decaying to
/// 0.5 at 30 days and 0.25 at 90 days. Never returns less than 0.1 so
/// old wisdom remains findable. Returns 1.0 when last_recalled_at is None
/// (never recalled -- no penalty, boost factor handles this).
fn decay_factor(last_recalled_at: &Option<String>) -> f32 {
    let last = match last_recalled_at {
        Some(ts) => match ts.parse::<chrono::DateTime<Utc>>() {
            Ok(dt) => dt,
            Err(_) => return 1.0,
        },
        None => return 1.0,
    };

    let days = (Utc::now() - last).num_days().max(0) as f32;

    if days <= 7.0 {
        1.0
    } else if days <= 30.0 {
        // Linear interpolation from 1.0 at 7d to 0.5 at 30d
        1.0 - 0.5 * (days - 7.0) / 23.0
    } else if days <= 90.0 {
        // Linear interpolation from 0.5 at 30d to 0.25 at 90d
        0.5 - 0.25 * (days - 30.0) / 60.0
    } else {
        // Floor at 0.1 for very old reflections
        (0.25 - 0.15 * ((days - 90.0) / 180.0).min(1.0)).max(0.1)
    }
}

/// Apply weighted scoring: boost by recall_count, decay by recency.
///
/// Formula: bm25_score * (1.0 + 0.1 * recall_count) * decay_factor
fn weighted_score(bm25_score: f32, recall_count: i64, last_recalled_at: &Option<String>) -> f32 {
    let boost = 1.0 + 0.1 * recall_count as f32;
    let decay = decay_factor(last_recalled_at);
    bm25_score * boost * decay
}

/// Archive-mode filter for recall queries (#457). Hot is the default
/// (exclude archived rows); Cold returns ONLY archived rows (the deep-
/// dive); Both includes everything. Mutually exclusive at the CLI layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArchiveMode {
    #[default]
    Hot,
    Cold,
    Both,
}

/// Join one search hit with the database and apply boost/decay weighting.
///
/// Returns `None` when the archive-mode filter rejects the row: the search
/// index returns ids regardless of archive state, so a miss here is an
/// expected silent skip, not a desync warning. Shared by the BM25 and
/// hybrid paths so their join, weighted-score and skip behavior cannot
/// drift apart.
fn join_and_score(
    db: &Database,
    id: &str,
    base_score: f32,
    mode: ArchiveMode,
) -> Result<Option<RecalledReflection>> {
    Ok(db
        .get_reflection_by_id_in_mode(id, mode)?
        .map(|reflection| {
            let score = weighted_score(
                base_score,
                reflection.recall_count,
                &reflection.last_recalled_at,
            );
            RecalledReflection {
                id: reflection.id,
                repo: reflection.repo,
                text: reflection.text,
                score,
                created_at: reflection.created_at,
            }
        }))
}

/// Sort reflections by descending weighted score.
fn sort_by_score_desc(reflections: &mut [RecalledReflection]) {
    reflections.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Join search results with the database to produce full reflections.
///
/// Looks up each search hit in SQLite to retrieve the full reflection
/// data (text, repo, created_at), scoped by archive mode. Applies
/// weighted scoring using recall_count and decay_factor, then re-sorts
/// by the weighted score.
fn join_search_results(
    db: &Database,
    search_results: &[SearchResult],
    mode: ArchiveMode,
) -> Result<Vec<RecalledReflection>> {
    let mut reflections = Vec::with_capacity(search_results.len());

    for sr in search_results {
        if let Some(reflection) = join_and_score(db, &sr.id, sr.score, mode)? {
            reflections.push(reflection);
        }
    }

    sort_by_score_desc(&mut reflections);

    Ok(reflections)
}

/// Query reflections relevant to the given context.
///
/// Searches the Tantivy index filtered by `repo` and ranked by BM25,
/// then joins each result with the SQLite database to retrieve full
/// reflection data (text, created_at), scoped by archive mode (#457).
/// The search index returns ids regardless of archive state; the mode
/// filter applies at the DB join step so cold-only or both modes return
/// the correct partition. The search index limit is raised when mode is
/// Cold to compensate for hot rows being filtered out, but the final
/// result is still capped at the requested limit.
///
/// Returns results ordered by descending relevance score.
pub fn recall_bm25(
    db: &Database,
    index: &SearchIndex,
    repo: &str,
    context: &str,
    limit: usize,
    mode: ArchiveMode,
) -> Result<RecallResult> {
    // Over-fetch from the search index when filtering archived rows out
    // so a heavily archived corpus does not silently return < limit.
    // Cap at 4x to bound the work; if it's not enough the corpus has
    // unusual distribution and a smaller limit is the right call.
    let index_limit = match mode {
        ArchiveMode::Hot | ArchiveMode::Cold => limit.saturating_mul(4),
        ArchiveMode::Both => limit,
    };
    let search_results = index.search(repo, context, index_limit)?;
    let mut reflections = join_search_results(db, &search_results, mode)?;
    reflections.truncate(limit);

    Ok(RecallResult {
        reflections,
        query: context.to_owned(),
        repo: repo.to_owned(),
    })
}

/// Merge BM25 and cosine scores into ranked hybrid results, scoped by
/// archive mode.
///
/// Shared logic for `recall` and `consult`. Normalizes BM25 scores,
/// applies the formula `0.6 * bm25_norm + 0.4 * cosine`, then applies
/// boost/decay via `weighted_score` (through the shared `join_and_score`,
/// so the join and skip behavior matches the BM25 path). Skips
/// cosine-only candidates below `COSINE_MIN_THRESHOLD`.
fn merge_hybrid_scores(
    db: &Database,
    bm25_results: &[SearchResult],
    embeddings: &[(String, Vec<u8>)],
    query_embedding: &[f32],
    limit: usize,
    mode: ArchiveMode,
) -> Result<Vec<RecalledReflection>> {
    let mut bm25_scores: HashMap<String, f32> = HashMap::new();
    let mut max_bm25: f32 = 0.0;
    for sr in bm25_results {
        bm25_scores.insert(sr.id.clone(), sr.score);
        if sr.score > max_bm25 {
            max_bm25 = sr.score;
        }
    }

    let mut cosine_scores: HashMap<String, f32> = HashMap::new();
    for (id, blob) in embeddings {
        let emb = embed::embedding_from_bytes(blob);
        let sim = embed::cosine_similarity(query_embedding, &emb);
        cosine_scores.insert(id.clone(), sim);
    }

    // Collect all candidate IDs from both sources
    let mut all_ids: Vec<String> = bm25_scores.keys().cloned().collect();
    for id in cosine_scores.keys() {
        if !bm25_scores.contains_key(id) {
            all_ids.push(id.clone());
        }
    }

    let bm25_norm_factor = if max_bm25 > 0.0 { max_bm25 } else { 1.0 };
    let mut reflections = Vec::new();

    for id in &all_ids {
        let bm25_raw = bm25_scores.get(id).copied().unwrap_or(0.0);
        let cosine = cosine_scores.get(id).copied().unwrap_or(0.0);

        if bm25_raw == 0.0 && cosine < COSINE_MIN_THRESHOLD {
            continue;
        }

        let bm25_normalized = bm25_raw / bm25_norm_factor;
        let hybrid = 0.6 * bm25_normalized + 0.4 * cosine;
        if let Some(reflection) = join_and_score(db, id, hybrid, mode)? {
            reflections.push(reflection);
        }
    }

    sort_by_score_desc(&mut reflections);
    reflections.truncate(limit);
    Ok(reflections)
}

/// Hybrid recall: BM25 + cosine similarity scoring, with archive-mode
/// filtering (#457).
///
/// Combines BM25 text search with semantic cosine similarity for better
/// recall on paraphrased or conceptually related queries. Uses the formula:
/// `score = 0.6 * bm25_norm + 0.4 * cosine_sim` (then applies boost/decay).
pub fn recall(
    db: &Database,
    index: &SearchIndex,
    embed_model: &EmbedModel,
    repo: &str,
    context: &str,
    limit: usize,
    mode: ArchiveMode,
) -> Result<RecallResult> {
    let index_limit = match mode {
        ArchiveMode::Hot | ArchiveMode::Cold => limit.saturating_mul(4),
        ArchiveMode::Both => limit * 3,
    };
    let bm25_results = index.search(repo, context, index_limit)?;
    let query_embedding = embed_model.encode_one(context)?;
    let embeddings = db.get_embeddings(Some(repo))?;
    let reflections = merge_hybrid_scores(
        db,
        &bm25_results,
        &embeddings,
        &query_embedding,
        limit,
        mode,
    )?;

    Ok(RecallResult {
        reflections,
        query: context.to_owned(),
        repo: repo.to_owned(),
    })
}

/// Consult: BM25 + cosine similarity across all repos.
///
/// Pre-existing asymmetry, preserved: hybrid consult joins in Hot mode
/// (archived rows excluded) while `consult_bm25` pins Both. Changing the
/// hybrid path's mode is a behavior decision, not a refactor.
pub fn consult(
    db: &Database,
    index: &SearchIndex,
    embed_model: &EmbedModel,
    context: &str,
    limit: usize,
) -> Result<RecallResult> {
    let bm25_results = index.search_all(context, limit * 3)?;
    let query_embedding = embed_model.encode_one(context)?;
    let embeddings = db.get_embeddings(None)?;
    let reflections = merge_hybrid_scores(
        db,
        &bm25_results,
        &embeddings,
        &query_embedding,
        limit,
        ArchiveMode::Hot,
    )?;

    Ok(RecallResult {
        reflections,
        query: context.to_owned(),
        repo: "(all)".to_owned(),
    })
}

/// Return the most recent reflections for a repo, bypassing BM25 search.
///
/// Useful for session-start hooks where no meaningful search context
/// is available yet. Returns results ordered newest first. Uses SQL
/// LIMIT for efficiency instead of fetching all and truncating.
pub fn recall_latest(db: &Database, repo: &str, limit: usize) -> Result<RecallResult> {
    let latest = db.get_latest_self_reflections(repo, limit)?;

    let reflections: Vec<RecalledReflection> = latest
        .into_iter()
        .map(|r| RecalledReflection {
            id: r.id,
            repo: r.repo,
            text: r.text,
            score: 0.0,
            created_at: r.created_at,
        })
        .collect();

    Ok(RecallResult {
        reflections,
        query: "(latest)".to_owned(),
        repo: repo.to_owned(),
    })
}

/// Return reflections matching a specific domain for a repo, bypassing search.
///
/// Used for reserved domains like `identity` and `snooze` that are injected
/// on every session start. Pure SQL lookup, no BM25 or cosine involved.
pub fn recall_by_domain(
    db: &Database,
    repo: &str,
    domain: &str,
    limit: usize,
) -> Result<RecallResult> {
    let matched = db.get_reflections_by_domain(repo, domain, limit)?;

    let reflections: Vec<RecalledReflection> = matched
        .into_iter()
        .map(|r| RecalledReflection {
            id: r.id,
            repo: r.repo,
            text: r.text,
            score: 0.0,
            created_at: r.created_at,
        })
        .collect();

    Ok(RecallResult {
        reflections,
        query: format!("(domain:{domain})"),
        repo: repo.to_owned(),
    })
}

/// Search reflections across all repositories for cross-agent consultation.
///
/// Uses `index.search_all()` (no repo filter) and joins with the database
/// to retrieve full reflection data including the originating repo.
/// Returns a `RecallResult` with `repo` set to "(all)".
pub fn consult_bm25(
    db: &Database,
    index: &SearchIndex,
    context: &str,
    limit: usize,
) -> Result<RecallResult> {
    let search_results = index.search_all(context, limit)?;
    // consult searches across the whole corpus regardless of archive
    // state -- a question asked of "all reflections" should find
    // archived bullpen posts the same as fresh ones. Pin Both so the
    // archive-mode default of Hot does not narrow consult's surface
    // when it should not.
    let reflections = join_search_results(db, &search_results, ArchiveMode::Both)?;

    Ok(RecallResult {
        reflections,
        query: context.to_owned(),
        repo: "(all)".to_owned(),
    })
}

/// Rank all reflections for a repo purely by cosine similarity to a query.
///
/// Skips BM25 entirely. Used when the caller knows BM25 will miss paraphrased
/// queries, or when debugging hybrid weight tuning. Requires the embed model;
/// returns an error if unavailable. Applies the same boost/decay weighting as
/// the hybrid path so results are comparable.
pub fn recall_cosine_only(
    db: &Database,
    embed_model: &EmbedModel,
    repo: &str,
    context: &str,
    limit: usize,
    min_score: Option<f32>,
) -> Result<RecallResult> {
    let query_embedding = embed_model.encode_one(context)?;
    let embeddings = db.get_embeddings(Some(repo))?;

    let mut reflections: Vec<RecalledReflection> = Vec::new();

    for (id, blob) in &embeddings {
        let emb = embed::embedding_from_bytes(blob);
        let cosine = embed::cosine_similarity(&query_embedding, &emb);

        if let Some(threshold) = min_score
            && cosine < threshold
        {
            continue;
        }

        if let Some(reflection) = db.get_reflection_by_id(id)? {
            let score = weighted_score(
                cosine,
                reflection.recall_count,
                &reflection.last_recalled_at,
            );
            reflections.push(RecalledReflection {
                id: reflection.id,
                repo: reflection.repo,
                text: reflection.text,
                score,
                created_at: reflection.created_at,
            });
        }
    }

    sort_by_score_desc(&mut reflections);
    reflections.truncate(limit);

    Ok(RecallResult {
        reflections,
        query: context.to_owned(),
        repo: repo.to_owned(),
    })
}

/// Find the nearest neighbors of a reflection by cosine similarity.
///
/// Fetches the source reflection's stored embedding from the database, then
/// scores all other embeddings for the same repo (or all repos if `cross_repo`
/// is true) against it. The source reflection itself is excluded from results.
/// Results are ranked by the same boost/decay weighted scoring used by hybrid
/// recall for consistency. The caller must ensure an embed model is available
/// before calling this function (model availability is checked in main.rs).
pub fn find_similar_by_id(
    db: &Database,
    id: &str,
    limit: usize,
    cross_repo: bool,
    min_score: Option<f32>,
) -> Result<RecallResult> {
    // Fetch the source reflection and its embedding.
    let source = db.get_reflection_by_id(id)?.ok_or_else(|| {
        crate::error::LegionError::Embedding(format!("reflection not found: {id}"))
    })?;

    let source_blob = db.get_embedding(id)?.ok_or_else(|| {
        crate::error::LegionError::Embedding(
            "reflection has no embedding -- run `legion reindex` to backfill".to_string(),
        )
    })?;

    let source_emb = embed::embedding_from_bytes(&source_blob);

    // Load candidate embeddings (repo-scoped or cross-repo).
    let repo_filter = if cross_repo {
        None
    } else {
        Some(source.repo.as_str())
    };
    let embeddings = db.get_embeddings(repo_filter)?;

    let mut reflections: Vec<RecalledReflection> = Vec::new();

    for (cand_id, blob) in &embeddings {
        if cand_id == id {
            continue; // exclude the source itself
        }

        let emb = embed::embedding_from_bytes(blob);
        let cosine = embed::cosine_similarity(&source_emb, &emb);

        if let Some(threshold) = min_score
            && cosine < threshold
        {
            continue;
        }

        if let Some(reflection) = db.get_reflection_by_id(cand_id)? {
            let score = weighted_score(
                cosine,
                reflection.recall_count,
                &reflection.last_recalled_at,
            );
            reflections.push(RecalledReflection {
                id: reflection.id,
                repo: reflection.repo,
                text: reflection.text,
                score,
                created_at: reflection.created_at,
            });
        }
    }

    sort_by_score_desc(&mut reflections);
    reflections.truncate(limit);

    let query_label = format!("similar:{id}");
    let result_repo = if cross_repo {
        "(all)".to_owned()
    } else {
        source.repo.clone()
    };

    Ok(RecallResult {
        reflections,
        query: query_label,
        repo: result_repo,
    })
}

/// Apply a min-score filter to an existing RecallResult.
///
/// Removes reflections whose score falls below the given threshold.
/// Used by the `--min-score` flag in the hybrid recall path to trim
/// weak matches that pollute context.
pub fn filter_by_min_score(result: &mut RecallResult, min_score: f32) {
    result.reflections.retain(|r| r.score >= min_score);
}

/// Format recall results for Claude Code hook injection.
///
/// Produces concise, human-readable output. Returns an empty string
/// when there are no results. When `preview` is `Some(n)`, each reflection
/// text is truncated to the first `n` characters (UTF-8 safe) via
/// [`card_parse::truncate_chars`], keeping session-start and PreToolUse
/// injections small. When preview is None, the reflection text is borrowed
/// rather than cloned (the per-line `format!` still allocates the output
/// chunk).
pub fn format_for_hook(result: &RecallResult, preview: Option<usize>) -> String {
    if result.reflections.is_empty() {
        return String::new();
    }

    let mut output = format!("[Legion] Relevant reflections for {}:\n", result.repo);

    for r in &result.reflections {
        let text: std::borrow::Cow<'_, str> = match preview {
            Some(n) if r.text.chars().count() > n => {
                std::borrow::Cow::Owned(crate::card_parse::truncate_chars(&r.text, n))
            }
            _ => std::borrow::Cow::Borrowed(&r.text),
        };
        output.push_str(&format!(
            "- {} (id: {}, score: {:.2})\n",
            text, r.id, r.score
        ));
    }

    output
}

/// Format recall results for cross-repo consultation output.
///
/// Includes repository attribution per line so agents can see where
/// each reflection originated. Returns an empty string when there
/// are no results.
pub fn format_for_consult(result: &RecallResult) -> String {
    if result.reflections.is_empty() {
        return String::new();
    }

    let mut output = String::from("[Legion] Cross-repo reflections:\n");

    for r in &result.reflections {
        output.push_str(&format!(
            "- [{}] {} (id: {}, score: {:.2})\n",
            r.repo, r.text, r.id, r.score
        ));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflect::reflect_from_text;
    use crate::testutil::test_storage;

    #[test]
    fn recall_returns_ranked_results() {
        let (db, index, _dir) = test_storage();
        reflect_from_text(
            &db,
            &index,
            "kelex",
            "mapping rules are fragile with Zod types",
        )
        .expect("reflect 1");
        reflect_from_text(&db, &index, "kelex", "the CLI argument parser works fine")
            .expect("reflect 2");
        reflect_from_text(
            &db,
            &index,
            "kelex",
            "Zod schema introspection handles unions",
        )
        .expect("reflect 3");

        let result = recall_bm25(
            &db,
            &index,
            "kelex",
            "Zod type mapping",
            5,
            ArchiveMode::Hot,
        )
        .expect("recall");
        assert!(result.reflections.len() >= 2);
        assert!(result.reflections[0].score >= result.reflections[1].score);
    }

    #[test]
    fn recall_empty_context_returns_empty() {
        let (db, index, _dir) = test_storage();
        reflect_from_text(&db, &index, "kelex", "some reflection").expect("reflect");

        let result = recall_bm25(&db, &index, "kelex", "", 5, ArchiveMode::Hot).expect("recall");
        assert!(result.reflections.is_empty());
    }

    #[test]
    fn recall_respects_limit() {
        let (db, index, _dir) = test_storage();
        for i in 0..10 {
            reflect_from_text(&db, &index, "test", &format!("testing reflection {i}"))
                .expect("reflect");
        }

        let result =
            recall_bm25(&db, &index, "test", "testing", 3, ArchiveMode::Hot).expect("recall");
        assert_eq!(result.reflections.len(), 3);
    }

    #[test]
    fn recall_skips_missing_db_entries() {
        let (db, index, _dir) = test_storage();

        // Add directly to index without DB entry to simulate desync
        index
            .add("orphan-id", "kelex", "orphan reflection text")
            .expect("add to index");

        // Add a proper entry through reflect_from_text
        reflect_from_text(&db, &index, "kelex", "properly stored reflection").expect("reflect");

        let result =
            recall_bm25(&db, &index, "kelex", "reflection", 10, ArchiveMode::Hot).expect("recall");

        // Only the properly stored one should appear
        for r in &result.reflections {
            assert_ne!(r.id, "orphan-id");
        }
    }

    #[test]
    fn recall_filters_by_repo() {
        let (db, index, _dir) = test_storage();
        reflect_from_text(&db, &index, "kelex", "Zod schema mapping").expect("reflect kelex");
        reflect_from_text(&db, &index, "rafters", "Zod token generation").expect("reflect rafters");

        let result =
            recall_bm25(&db, &index, "kelex", "Zod", 10, ArchiveMode::Hot).expect("recall");
        assert_eq!(result.reflections.len(), 1);
        assert!(result.reflections[0].text.contains("mapping"));
    }

    #[test]
    fn recall_populates_metadata() {
        let (db, index, _dir) = test_storage();
        reflect_from_text(&db, &index, "kelex", "test reflection").expect("reflect");

        let result =
            recall_bm25(&db, &index, "kelex", "test", 5, ArchiveMode::Hot).expect("recall");
        assert_eq!(result.repo, "kelex");
        assert_eq!(result.query, "test");
    }

    #[test]
    fn format_for_hook_produces_readable_output() {
        let result = RecallResult {
            query: "Zod mapping".into(),
            repo: "kelex".into(),
            reflections: vec![RecalledReflection {
                id: "test-id".into(),
                repo: "kelex".into(),
                text: "mapping rules are fragile".into(),
                score: 0.87,
                created_at: "2026-03-05T00:00:00Z".into(),
            }],
        };
        let output = format_for_hook(&result, None);
        assert!(output.contains("mapping rules are fragile"));
        assert!(output.contains("kelex"));
        assert!(output.contains("0.87"));
        assert!(output.contains("id: test-id"));
    }

    #[test]
    fn format_for_hook_multiple_results() {
        let result = RecallResult {
            query: "Zod mapping".into(),
            repo: "kelex".into(),
            reflections: vec![
                RecalledReflection {
                    id: "id-1".into(),
                    repo: "kelex".into(),
                    text: "mapping rules are fragile".into(),
                    score: 0.87,
                    created_at: "2026-03-05T00:00:00Z".into(),
                },
                RecalledReflection {
                    id: "id-2".into(),
                    repo: "kelex".into(),
                    text: "discriminated unions hide complexity".into(),
                    score: 0.62,
                    created_at: "2026-03-05T00:00:00Z".into(),
                },
            ],
        };
        let output = format_for_hook(&result, None);
        assert!(output.contains("mapping rules are fragile"));
        assert!(output.contains("discriminated unions hide complexity"));
        assert!(output.contains("[Legion]"));
    }

    #[test]
    fn format_for_hook_empty_results() {
        let result = RecallResult {
            query: "nothing".into(),
            repo: "kelex".into(),
            reflections: vec![],
        };
        let output = format_for_hook(&result, None);
        assert!(output.is_empty() || output.contains("No relevant reflections"));
    }

    #[test]
    fn format_for_hook_truncates_when_preview_set() {
        let result = RecallResult {
            query: "q".into(),
            repo: "legion".into(),
            reflections: vec![RecalledReflection {
                id: "abc".into(),
                repo: "legion".into(),
                text: "a".repeat(500),
                score: 0.9,
                created_at: "2026-04-10".into(),
            }],
        };
        let output = format_for_hook(&result, Some(50));
        // Delegates to card_parse::truncate_chars which appends "..."
        assert!(output.contains("..."));
        assert!(output.len() < 200);
    }

    #[test]
    fn format_for_hook_preview_none_does_not_truncate() {
        let long_text = "a".repeat(500);
        let result = RecallResult {
            query: "q".into(),
            repo: "legion".into(),
            reflections: vec![RecalledReflection {
                id: "abc".into(),
                repo: "legion".into(),
                text: long_text.clone(),
                score: 0.9,
                created_at: "2026-04-10".into(),
            }],
        };
        let output = format_for_hook(&result, None);
        assert!(output.contains(&long_text));
    }

    #[test]
    fn consult_searches_across_repos() {
        let (db, index, _dir) = test_storage();
        reflect_from_text(&db, &index, "kelex", "Zod schema mapping rules").expect("reflect kelex");
        reflect_from_text(&db, &index, "rafters", "token generation pipeline")
            .expect("reflect rafters");
        reflect_from_text(&db, &index, "platform", "Zod validation at the edge")
            .expect("reflect platform");

        let result = consult_bm25(&db, &index, "Zod", 10).expect("consult");
        // Should match kelex and platform but not rafters
        assert!(result.reflections.len() >= 2);
        let repos: Vec<&str> = result.reflections.iter().map(|r| r.repo.as_str()).collect();
        assert!(repos.contains(&"kelex"));
        assert!(repos.contains(&"platform"));
    }

    #[test]
    fn consult_includes_repo_attribution() {
        let (db, index, _dir) = test_storage();
        reflect_from_text(&db, &index, "kelex", "schema introspection logic").expect("reflect");

        let result = consult_bm25(&db, &index, "schema", 5).expect("consult");
        assert_eq!(result.reflections.len(), 1);
        assert_eq!(result.reflections[0].repo, "kelex");
        assert_eq!(result.repo, "(all)");
    }

    #[test]
    fn consult_empty_context_returns_empty() {
        let (db, index, _dir) = test_storage();
        reflect_from_text(&db, &index, "kelex", "some reflection text").expect("reflect");

        let result = consult_bm25(&db, &index, "", 5).expect("consult");
        assert!(result.reflections.is_empty());
    }

    #[test]
    fn format_for_consult_includes_repo_per_line() {
        let result = RecallResult {
            query: "schema".into(),
            repo: "(all)".into(),
            reflections: vec![
                RecalledReflection {
                    id: "id-1".into(),
                    repo: "kelex".into(),
                    text: "schema introspection".into(),
                    score: 0.90,
                    created_at: "2026-03-05T00:00:00Z".into(),
                },
                RecalledReflection {
                    id: "id-2".into(),
                    repo: "platform".into(),
                    text: "schema validation".into(),
                    score: 0.75,
                    created_at: "2026-03-05T00:00:00Z".into(),
                },
            ],
        };
        let output = format_for_consult(&result);
        assert!(output.contains("[Legion] Cross-repo reflections:"));
        assert!(output.contains("[kelex]"));
        assert!(output.contains("[platform]"));
        assert!(output.contains("schema introspection"));
        assert!(output.contains("schema validation"));
        assert!(output.contains("0.90"));
        assert!(output.contains("0.75"));
        assert!(output.contains("id: id-1"));
        assert!(output.contains("id: id-2"));
    }

    #[test]
    fn format_for_consult_empty_results() {
        let result = RecallResult {
            query: "nothing".into(),
            repo: "(all)".into(),
            reflections: vec![],
        };
        let output = format_for_consult(&result);
        assert!(output.is_empty());
    }

    #[test]
    fn decay_factor_none_returns_one() {
        let factor = decay_factor(&None);
        assert!((factor - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn decay_factor_recent_returns_one() {
        let now = Utc::now().to_rfc3339();
        let factor = decay_factor(&Some(now));
        assert!((factor - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn decay_factor_30_days_returns_half() {
        let thirty_days_ago = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let factor = decay_factor(&Some(thirty_days_ago));
        assert!((factor - 0.5).abs() < 0.05, "expected ~0.5, got {factor}");
    }

    #[test]
    fn decay_factor_90_days_returns_quarter() {
        let ninety_days_ago = (Utc::now() - chrono::Duration::days(90)).to_rfc3339();
        let factor = decay_factor(&Some(ninety_days_ago));
        assert!((factor - 0.25).abs() < 0.05, "expected ~0.25, got {factor}");
    }

    #[test]
    fn decay_factor_never_below_minimum() {
        let year_ago = (Utc::now() - chrono::Duration::days(365)).to_rfc3339();
        let factor = decay_factor(&Some(year_ago));
        assert!(factor >= 0.1, "expected >= 0.1, got {factor}");
    }

    #[test]
    fn weighted_score_boost_factor() {
        // recall_count of 5 should give 1.5x boost
        let score = weighted_score(1.0, 5, &None);
        assert!((score - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn weighted_score_zero_recall_no_change() {
        let score = weighted_score(0.8, 0, &None);
        assert!((score - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn recall_reranks_by_weighted_score() {
        let (db, index, _dir) = test_storage();

        // Create two reflections about the same topic
        reflect_from_text(&db, &index, "kelex", "Zod schema mapping is complex")
            .expect("reflect 1");
        reflect_from_text(&db, &index, "kelex", "Zod type validation patterns").expect("reflect 2");

        // Boost the second one
        let all = db.get_reflections_by_repo("kelex").unwrap();
        let second_id = &all
            .iter()
            .find(|r| r.text.contains("validation"))
            .unwrap()
            .id;
        db.boost_reflection(second_id).unwrap();
        db.boost_reflection(second_id).unwrap();
        db.boost_reflection(second_id).unwrap();

        let result = recall_bm25(&db, &index, "kelex", "Zod", 5, ArchiveMode::Hot).expect("recall");
        assert!(result.reflections.len() >= 2);
        // The boosted reflection should have a higher weighted score
        let boosted = result
            .reflections
            .iter()
            .find(|r| r.text.contains("validation"))
            .unwrap();
        let unboosted = result
            .reflections
            .iter()
            .find(|r| r.text.contains("mapping"))
            .unwrap();
        assert!(
            boosted.score >= unboosted.score,
            "boosted ({}) should score >= unboosted ({})",
            boosted.score,
            unboosted.score
        );
    }

    // --- Card B tests: recall_cosine_only and filter_by_min_score ---

    /// Helper: store a reflection and write a synthetic embedding blob directly.
    fn store_with_embedding(
        db: &crate::db::Database,
        index: &crate::search::SearchIndex,
        repo: &str,
        text: &str,
        embedding: &[f32],
    ) -> String {
        let id = reflect_from_text(db, index, repo, text).expect("reflect");
        let blob = crate::embed::embedding_to_bytes(embedding);
        db.store_embedding(&id, &blob).expect("store embedding");
        id
    }

    #[test]
    fn recall_cosine_only_skips_bm25() {
        // Without an embed model we can't call recall_cosine_only in the unit test,
        // so we test filter_by_min_score + find_similar_by_id without the model.
        // The cosine-only model-dependent path is covered by integration tests.
        // This test verifies filter_by_min_score behavior instead.
        let result = RecallResult {
            query: "q".into(),
            repo: "test".into(),
            reflections: vec![
                RecalledReflection {
                    id: "a".into(),
                    repo: "test".into(),
                    text: "high score".into(),
                    score: 0.9,
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
                RecalledReflection {
                    id: "b".into(),
                    repo: "test".into(),
                    text: "low score".into(),
                    score: 0.2,
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
            ],
        };
        let mut filtered = result;
        filter_by_min_score(&mut filtered, 0.5);
        assert_eq!(filtered.reflections.len(), 1);
        assert_eq!(filtered.reflections[0].id, "a");
    }

    #[test]
    fn filter_by_min_score_keeps_all_above_threshold() {
        let mut result = RecallResult {
            query: "q".into(),
            repo: "test".into(),
            reflections: vec![
                RecalledReflection {
                    id: "a".into(),
                    repo: "test".into(),
                    text: "first".into(),
                    score: 0.8,
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
                RecalledReflection {
                    id: "b".into(),
                    repo: "test".into(),
                    text: "second".into(),
                    score: 0.6,
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
            ],
        };
        filter_by_min_score(&mut result, 0.5);
        assert_eq!(result.reflections.len(), 2);
    }

    #[test]
    fn filter_by_min_score_drops_all_below_threshold() {
        let mut result = RecallResult {
            query: "q".into(),
            repo: "test".into(),
            reflections: vec![RecalledReflection {
                id: "a".into(),
                repo: "test".into(),
                text: "weak".into(),
                score: 0.1,
                created_at: "2026-01-01T00:00:00Z".into(),
            }],
        };
        filter_by_min_score(&mut result, 0.5);
        assert!(result.reflections.is_empty());
    }

    // --- Card A tests: find_similar_by_id ---

    #[test]
    fn find_similar_by_id_excludes_source() {
        let (db, index, _dir) = test_storage();
        // Use a 3-dim unit vector for determinism.
        let emb_a: Vec<f32> = vec![1.0, 0.0, 0.0];
        let emb_b: Vec<f32> = vec![0.9, 0.436, 0.0]; // ~25 deg from a
        let id_a = store_with_embedding(&db, &index, "kelex", "reflection a", &emb_a);
        store_with_embedding(&db, &index, "kelex", "reflection b", &emb_b);

        let result = find_similar_by_id(&db, &id_a, 5, false, None).expect("similar");
        // Source itself must not appear in results
        assert!(result.reflections.iter().all(|r| r.id != id_a));
    }

    #[test]
    fn find_similar_by_id_ranks_closer_first() {
        let (db, index, _dir) = test_storage();
        // emb_b is closer to emb_a than emb_c
        let emb_a: Vec<f32> = vec![1.0, 0.0, 0.0];
        let emb_b: Vec<f32> = vec![0.99, 0.14, 0.0]; // ~8 deg
        let emb_c: Vec<f32> = vec![0.0, 1.0, 0.0]; // 90 deg
        let id_a = store_with_embedding(&db, &index, "kelex", "anchor text", &emb_a);
        let id_b = store_with_embedding(&db, &index, "kelex", "close match text", &emb_b);
        let id_c = store_with_embedding(&db, &index, "kelex", "unrelated content here", &emb_c);

        let result = find_similar_by_id(&db, &id_a, 5, false, None).expect("similar");
        assert_eq!(result.reflections.len(), 2);
        // id_b should be ranked first
        assert_eq!(
            result.reflections[0].id, id_b,
            "expected closer reflection first"
        );
        assert_eq!(result.reflections[1].id, id_c);
    }

    #[test]
    fn find_similar_by_id_not_found_returns_error() {
        let (db, index, _dir) = test_storage();
        // Silence unused variable
        let _ = index;
        let err = find_similar_by_id(&db, "nonexistent-uuid", 5, false, None).unwrap_err();
        assert!(
            matches!(err, crate::error::LegionError::Embedding(_)),
            "expected Embedding error, got: {err:?}"
        );
    }

    #[test]
    fn find_similar_by_id_cross_repo_includes_other_repos() {
        let (db, index, _dir) = test_storage();
        let emb_a: Vec<f32> = vec![1.0, 0.0, 0.0];
        let emb_b: Vec<f32> = vec![0.99, 0.14, 0.0];
        let id_a = store_with_embedding(&db, &index, "kelex", "kelex reflection", &emb_a);
        let id_b = store_with_embedding(&db, &index, "rafters", "rafters reflection", &emb_b);

        // Without cross_repo: should not find rafters reflection
        let result = find_similar_by_id(&db, &id_a, 5, false, None).expect("similar");
        assert!(
            result.reflections.iter().all(|r| r.id != id_b),
            "should not cross repos"
        );

        // With cross_repo: should find rafters reflection
        let result_cross = find_similar_by_id(&db, &id_a, 5, true, None).expect("similar cross");
        assert!(
            result_cross.reflections.iter().any(|r| r.id == id_b),
            "cross_repo should include rafters reflection"
        );
    }

    #[test]
    fn find_similar_by_id_min_score_filters() {
        let (db, index, _dir) = test_storage();
        let emb_a: Vec<f32> = vec![1.0, 0.0, 0.0];
        let emb_orthogonal: Vec<f32> = vec![0.0, 1.0, 0.0]; // cosine = 0
        let id_a = store_with_embedding(&db, &index, "kelex", "anchor", &emb_a);
        store_with_embedding(&db, &index, "kelex", "orthogonal", &emb_orthogonal);

        // High threshold: orthogonal vector should be filtered out
        let result = find_similar_by_id(&db, &id_a, 5, false, Some(0.5)).expect("similar");
        assert!(
            result.reflections.is_empty(),
            "orthogonal vector should be filtered by min_score 0.5"
        );
    }

    fn entry(id: &str, text: &str, in_chain: bool) -> WhoamiEntry {
        WhoamiEntry {
            id: id.to_string(),
            text: text.to_string(),
            in_chain,
        }
    }

    #[test]
    fn format_whoami_empty_returns_empty() {
        assert_eq!(format_whoami("legion", &[]), "");
    }

    #[test]
    fn format_whoami_single_short_entry_fits() {
        let out = format_whoami("legion", &[entry("abc", "hi", false)]);
        assert!(out.starts_with(WHOAMI_BANNER_OPEN));
        assert!(out.contains("- hi (id: abc)"));
        assert!(out.trim_end().ends_with(WHOAMI_BANNER_CLOSE));
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn format_whoami_emits_chain_pointer_when_in_chain() {
        let out = format_whoami("legion", &[entry("abc", "hi", true)]);
        assert!(out.contains("legion chain --id abc"));
    }

    #[test]
    fn format_whoami_skips_chain_pointer_when_not_in_chain() {
        let out = format_whoami("legion", &[entry("abc", "hi", false)]);
        assert!(!out.contains("legion chain --id"));
    }

    #[test]
    fn format_whoami_caps_output_under_budget_with_truncation_pointer() {
        let big = "x".repeat(800);
        let entries: Vec<WhoamiEntry> = (0..5)
            .map(|i| entry(&format!("id{i}"), &big, false))
            .collect();
        let out = format_whoami("legion", &entries);
        assert!(out.contains("more identity reflections truncated"));
        assert!(
            out.len() < WHOAMI_BYTE_CAP + 200,
            "output {} bytes should be near the cap",
            out.len()
        );
    }

    #[test]
    fn format_whoami_always_emits_first_entry_even_if_oversized() {
        let huge = "x".repeat(WHOAMI_BYTE_CAP * 2);
        let out = format_whoami("legion", &[entry("solo", &huge, false)]);
        assert!(out.contains("solo"));
        assert!(out.contains(WHOAMI_BANNER_OPEN));
        assert!(out.contains(WHOAMI_BANNER_CLOSE));
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn format_whoami_truncation_pointer_includes_repo() {
        let big = "x".repeat(800);
        let entries: Vec<WhoamiEntry> = (0..5)
            .map(|i| entry(&format!("id{i}"), &big, false))
            .collect();
        let out = format_whoami("kelex", &entries);
        assert!(out.contains("legion recall --repo kelex --domain identity"));
    }

    #[test]
    fn format_whatami_empty_returns_empty() {
        assert_eq!(format_whatami("legion", &[]), "");
    }

    #[test]
    fn format_whatami_wraps_operating_banner() {
        let out = format_whatami("legion", &[entry("w1", "work the board", false)]);
        assert!(out.starts_with(WHATAMI_BANNER_OPEN));
        assert!(out.contains("[Legion] How legion operates:"));
        assert!(out.contains("- work the board (id: w1)"));
        assert!(out.trim_end().ends_with(WHATAMI_BANNER_CLOSE));
    }

    #[test]
    fn format_whatami_truncation_pointer_uses_workflow_domain() {
        let big = "x".repeat(800);
        let entries: Vec<WhoamiEntry> = (0..5)
            .map(|i| entry(&format!("w{i}"), &big, false))
            .collect();
        let out = format_whatami("kelex", &entries);
        assert!(out.contains("more operating-contract reflections truncated"));
        assert!(out.contains("legion recall --repo kelex --domain workflow"));
    }

    #[test]
    fn find_similar_by_id_missing_embedding_returns_error() {
        let (db, index, _dir) = test_storage();
        // Store a reflection but do NOT set its embedding
        let id =
            reflect_from_text(&db, &index, "kelex", "no embedding reflection").expect("reflect");

        let err = find_similar_by_id(&db, &id, 5, false, None).unwrap_err();
        assert!(
            matches!(err, crate::error::LegionError::Embedding(_)),
            "expected Embedding error for missing embedding, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("reindex"),
            "error should suggest reindex, got: {msg}"
        );
    }
}
