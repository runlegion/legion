use std::collections::HashMap;

use chrono::Utc;

use crate::db::Database;
use crate::embed::{self, EmbedModel};
use crate::error::Result;
use crate::search::{SearchIndex, SearchResult};

/// Minimum cosine similarity for a cosine-only candidate (no BM25 match).
/// Prevents noise from weak semantic matches when BM25 found nothing.
const COSINE_MIN_THRESHOLD: f32 = 0.3;

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

/// Join search results with the database to produce full reflections.
///
/// Looks up each search hit in SQLite to retrieve the full reflection
/// data (text, repo, created_at). Applies weighted scoring using
/// recall_count and decay_factor. Missing entries (index/DB desync)
/// are logged as warnings to stderr.
fn join_search_results(
    db: &Database,
    search_results: &[SearchResult],
) -> Result<Vec<RecalledReflection>> {
    let mut reflections = Vec::with_capacity(search_results.len());

    for sr in search_results {
        if let Some(reflection) = db.get_reflection_by_id(&sr.id)? {
            let score = weighted_score(
                sr.score,
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
        } else {
            eprintln!(
                "[legion] warning: reflection {} found in index but missing from database",
                sr.id
            );
        }
    }

    // Re-sort by weighted score since ordering may have changed
    reflections.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(reflections)
}

/// Query reflections relevant to the given context.
///
/// Searches the Tantivy index filtered by `repo` and ranked by BM25,
/// then joins each result with the SQLite database to retrieve full
/// reflection data (text, created_at). Missing reflections in the DB
/// (index/DB desync) are logged as warnings to stderr.
///
/// Returns results ordered by descending relevance score.
pub fn recall_bm25(
    db: &Database,
    index: &SearchIndex,
    repo: &str,
    context: &str,
    limit: usize,
) -> Result<RecallResult> {
    let search_results = index.search(repo, context, limit)?;
    let reflections = join_search_results(db, &search_results)?;

    Ok(RecallResult {
        reflections,
        query: context.to_owned(),
        repo: repo.to_owned(),
    })
}

/// Merge BM25 and cosine scores into ranked hybrid results.
///
/// Shared logic for `recall_hybrid` and `consult_hybrid`. Normalizes BM25
/// scores, applies the formula `0.6 * bm25_norm + 0.4 * cosine`, then
/// applies boost/decay via `weighted_score`. Skips cosine-only candidates
/// below `COSINE_MIN_THRESHOLD`.
fn merge_hybrid_scores(
    db: &Database,
    bm25_results: &[SearchResult],
    embeddings: &[(String, Vec<u8>)],
    query_embedding: &[f32],
    limit: usize,
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

        if let Some(reflection) = db.get_reflection_by_id(id)? {
            let bm25_normalized = bm25_raw / bm25_norm_factor;
            let hybrid = 0.6 * bm25_normalized + 0.4 * cosine;
            let score = weighted_score(
                hybrid,
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
        } else {
            eprintln!(
                "[legion] warning: reflection {} found in search but missing from database",
                id
            );
        }
    }

    reflections.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    reflections.truncate(limit);
    Ok(reflections)
}

/// Hybrid recall: BM25 + cosine similarity scoring.
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
) -> Result<RecallResult> {
    let bm25_results = index.search(repo, context, limit * 3)?;
    let query_embedding = embed_model.encode_one(context)?;
    let embeddings = db.get_embeddings(Some(repo))?;
    let reflections = merge_hybrid_scores(db, &bm25_results, &embeddings, &query_embedding, limit)?;

    Ok(RecallResult {
        reflections,
        query: context.to_owned(),
        repo: repo.to_owned(),
    })
}

/// Consult: BM25 + cosine similarity across all repos.
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
    let reflections = merge_hybrid_scores(db, &bm25_results, &embeddings, &query_embedding, limit)?;

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
    let reflections = join_search_results(db, &search_results)?;

    Ok(RecallResult {
        reflections,
        query: context.to_owned(),
        repo: "(all)".to_owned(),
    })
}

/// Format recall results for Claude Code hook injection.
///
/// Produces concise, human-readable output. Returns an empty string
/// when there are no results. When `preview` is `Some(n)`, each reflection
/// text is truncated to the first `n` characters (UTF-8 safe) via
/// [`card_parse::truncate_chars`], keeping session-start and PreToolUse
/// injections small. When preview is None, the reflection text is borrowed
/// in place -- no per-reflection allocation.
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

        let result = recall_bm25(&db, &index, "kelex", "Zod type mapping", 5).expect("recall");
        assert!(result.reflections.len() >= 2);
        assert!(result.reflections[0].score >= result.reflections[1].score);
    }

    #[test]
    fn recall_empty_context_returns_empty() {
        let (db, index, _dir) = test_storage();
        reflect_from_text(&db, &index, "kelex", "some reflection").expect("reflect");

        let result = recall_bm25(&db, &index, "kelex", "", 5).expect("recall");
        assert!(result.reflections.is_empty());
    }

    #[test]
    fn recall_respects_limit() {
        let (db, index, _dir) = test_storage();
        for i in 0..10 {
            reflect_from_text(&db, &index, "test", &format!("testing reflection {i}"))
                .expect("reflect");
        }

        let result = recall_bm25(&db, &index, "test", "testing", 3).expect("recall");
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

        let result = recall_bm25(&db, &index, "kelex", "reflection", 10).expect("recall");

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

        let result = recall_bm25(&db, &index, "kelex", "Zod", 10).expect("recall");
        assert_eq!(result.reflections.len(), 1);
        assert!(result.reflections[0].text.contains("mapping"));
    }

    #[test]
    fn recall_populates_metadata() {
        let (db, index, _dir) = test_storage();
        reflect_from_text(&db, &index, "kelex", "test reflection").expect("reflect");

        let result = recall_bm25(&db, &index, "kelex", "test", 5).expect("recall");
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

        let result = recall_bm25(&db, &index, "kelex", "Zod", 5).expect("recall");
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
}
