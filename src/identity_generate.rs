//! `legion whoami --generate` (#784): rebuild an agent's identity chain
//! from its own bylined writing (the claimed half) and cross-agent
//! reflections about it (the given half), through a repeatable,
//! invariant-enforcing command instead of the by-hand process used to
//! rebuild legion's own `whoami` on 2026-07-14.
//!
//! `legion` has no LLM-calling capability in the binary -- this module
//! does not author banner prose. Gather mode (`gather`) deterministically
//! retrieves and packages source material; apply mode (`apply`) performs
//! the guarded, invariant-checked swap once that material has been
//! synthesized into an [`IdentityManifest`] by the calling agent.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::{Database, Reflection};
use crate::embed::EmbedModel;
use crate::error::{LegionError, Result};
use crate::recall;
use crate::search::SearchIndex;

/// Backup/retirement scan reads at most this many domain=identity rows
/// per repo -- well beyond the whoami default root limit (50) and any
/// plausible chain length, so the pre-apply backup never silently
/// truncates the corpus it is meant to protect.
pub const IDENTITY_BACKUP_LIMIT: usize = 500;

/// How many given-half (cross-agent) reflections `gather` pulls via
/// hybrid consult. Not exposed as a CLI flag -- keeps the surface
/// minimal; raise if a real run proves too shallow.
const GIVEN_HALF_LIMIT: usize = 15;

/// `gather` over-fetches by this factor before filtering out self-repo
/// rows, so a query where self-authored reflections rank highly still
/// leaves room for `GIVEN_HALF_LIMIT` genuine cross-agent hits rather
/// than silently under-filling.
const GIVEN_HALF_FETCH_MULTIPLIER: usize = 3;

/// File extensions `gather` treats as frontmatter-capable. Enumeration
/// is never narrowed further -- byline filtering happens after
/// frontmatter extraction, not before (see `gather`).
const CLAIMED_HALF_EXTENSIONS: [&str; 3] = ["md", "mdx", "astro"];

/// One claimed-half source: a file in `vault_repo` whose frontmatter
/// `author` field (not filename) matched one of the requested bylines.
/// `body` is the full file content, not an excerpt -- gather's whole
/// purpose is putting the actual musing/story/concept text, unsanitized,
/// in front of the synthesizing agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimedSource {
    pub repo: String,
    pub path: String,
    pub byline: String,
    pub body: String,
}

/// One given-half source: a cross-agent reflection (repo != the target
/// repo) surfaced by hybrid consult against `"<repo> <bylines...>"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GivenSource {
    pub id: String,
    pub repo: String,
    pub text: String,
    pub score: f32,
}

/// Full gather-mode output. Printed as JSON to stdout by the CLI
/// handler; the calling agent reads it and authors an `IdentityManifest`
/// by hand -- `gather` performs no synthesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatherBundle {
    pub repo: String,
    pub vault_repo: String,
    pub bylines: Vec<String>,
    pub claimed: Vec<ClaimedSource>,
    pub given: Vec<GivenSource>,
}

/// An authored identity replacement: one root plus an ordered chain of
/// `--follows` children (empty chain is valid -- a root-only identity is
/// a legitimate outcome).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityManifest {
    pub root: String,
    #[serde(default)]
    pub chain: Vec<String>,
}

/// Ids and backup location from a completed (non-dry-run) apply.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyOutcome {
    /// New chain ids, root first.
    pub new_ids: Vec<String>,
    pub backup_path: PathBuf,
    /// Old domain=identity ids retired (deleted) as part of this apply.
    pub retired_ids: Vec<String>,
}

/// What a `--dry-run` apply would do, computed without any write.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyPlan {
    pub would_retire: Vec<String>,
    pub would_create: usize,
    pub backup_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ApplyResult {
    Applied(ApplyOutcome),
    Planned(ApplyPlan),
}

/// Gather claimed-half + given-half source material for `repo`'s
/// identity rebuild. Enumerates every frontmatter-capable file
/// (`.md`/`.mdx`/`.astro`) in `vault_repo`'s file inventory and extracts
/// `author` from each -- enumeration is never narrowed by byline, since
/// the byline is a filename heuristic the frontmatter check exists to
/// replace. Returns `LegionError::WhoamiGenerate` if zero files match
/// after filtering.
pub fn gather(
    db: &Database,
    index: &SearchIndex,
    embed_model: Option<&EmbedModel>,
    repo: &str,
    vault_repo: &str,
    bylines: &[String],
) -> Result<GatherBundle> {
    let workdir = vault_repo_workdir(vault_repo)?;
    let claimed = gather_claimed_half(db, vault_repo, &workdir, bylines)?;
    let given = gather_given_half(db, index, embed_model, repo, bylines)?;

    Ok(GatherBundle {
        repo: repo.to_owned(),
        vault_repo: vault_repo.to_owned(),
        bylines: bylines.to_vec(),
        claimed,
        given,
    })
}

/// Resolve `vault_repo`'s workdir from watch.toml, the same
/// repo-validation error `sym etc find-content`/`find-file` already use
/// for an unknown repo. Impure shell around `resolve_vault_repo_workdir`
/// (the pure, unit-testable lookup) -- `data_dir()` caches its result in
/// a process-wide `OnceLock`, so exercising this wrapper's `LEGION_DATA_DIR`
/// resolution itself needs a fresh subprocess (covered by the
/// `tests/integration/whoami_generate.rs` CLI tests), not a unit test.
fn vault_repo_workdir(vault_repo: &str) -> Result<PathBuf> {
    let watch_path = crate::data_dir()?.join("watch.toml");
    let repos = crate::watch::list_repos_in_config(&watch_path)?;
    resolve_vault_repo_workdir(&repos, vault_repo)
}

/// Find `vault_repo` among already-loaded watch.toml entries and return
/// its workdir, or `LegionError::WatchConfig` naming the repo -- the same
/// error `sym etc find-content`/`find-file` return for an unknown repo.
fn resolve_vault_repo_workdir(
    repos: &[crate::watch::WatchRepoConfig],
    vault_repo: &str,
) -> Result<PathBuf> {
    let entry = repos.iter().find(|r| r.name == vault_repo).ok_or_else(|| {
        LegionError::WatchConfig(format!(
            "repo '{vault_repo}' not in watch.toml. Add it with `legion watch add {vault_repo} <path>`."
        ))
    })?;
    Ok(PathBuf::from(&entry.workdir))
}

/// Enumerate `vault_repo`'s frontmatter-capable files, extract `author`
/// from each, and keep the ones matching a requested byline.
fn gather_claimed_half(
    db: &Database,
    vault_repo: &str,
    workdir: &Path,
    bylines: &[String],
) -> Result<Vec<ClaimedSource>> {
    let filter = crate::db::inventory::InventoryFilter {
        repo: Some(vault_repo),
        ext: None,
        lang: None,
    };
    let entries = db.list_file_inventory(&filter)?;

    let mut claimed = Vec::new();
    for entry in &entries {
        if !CLAIMED_HALF_EXTENSIONS.contains(&entry.ext.as_deref().unwrap_or("")) {
            continue;
        }
        let abs_path = workdir.join(&entry.path);

        // A candidate whose extract_field call fails (no frontmatter,
        // missing `author` field, unparseable YAML) is silently skipped,
        // not treated as an error.
        let Ok(author_value) = crate::etc::extract_field(&abs_path, "author") else {
            continue;
        };
        let Some(matched_byline) = matching_byline(&author_value, bylines) else {
            continue;
        };

        let body = std::fs::read_to_string(&abs_path)?;
        claimed.push(ClaimedSource {
            repo: vault_repo.to_owned(),
            path: entry.path.clone(),
            byline: matched_byline,
            body,
        });
    }

    if claimed.is_empty() {
        return Err(LegionError::WhoamiGenerate(format!(
            "no claimed-half sources found in vault_repo '{vault_repo}' for bylines: {}",
            bylines.join(", ")
        )));
    }

    Ok(claimed)
}

/// Check whether a `serde_json::Value` extracted from a frontmatter
/// `author` field (a string, or an array of strings) contains one of
/// `bylines` (case-sensitive exact match). Returns the matched byline.
fn matching_byline(value: &serde_json::Value, bylines: &[String]) -> Option<String> {
    match value {
        serde_json::Value::String(s) => bylines.iter().find(|b| b.as_str() == s).cloned(),
        serde_json::Value::Array(items) => items.iter().find_map(|item| match item {
            serde_json::Value::String(s) => bylines.iter().find(|b| b.as_str() == s).cloned(),
            _ => None,
        }),
        _ => None,
    }
}

/// Populate the given-half via a hybrid-consult-style query (BM25 +
/// cosine when an embed model is available, BM25-only otherwise --
/// precedent `try_load_embed_model`'s Some/None branch in
/// `src/cli/memory.rs`) against `"<repo> <bylines...>"`, excluding
/// self-authored rows (`entry.repo == repo`) since those are claimed-half
/// even if they happen to live outside `vault_repo`. An empty result is
/// not an error.
fn gather_given_half(
    db: &Database,
    index: &SearchIndex,
    embed_model: Option<&EmbedModel>,
    repo: &str,
    bylines: &[String],
) -> Result<Vec<GivenSource>> {
    let query = format!("{repo} {}", bylines.join(" "));
    let fetch_limit = GIVEN_HALF_LIMIT * GIVEN_HALF_FETCH_MULTIPLIER;

    // Identity generation gathers cross-agent source material regardless
    // of when it was written -- unbounded, matching this consult's
    // pre-#786 behavior.
    let range = crate::timerange::TimeRange::default();
    let recalled = match embed_model {
        Some(model) => recall::consult(db, index, model, &query, fetch_limit, &range)?,
        None => recall::consult_bm25(db, index, &query, fetch_limit, &range)?,
    };

    Ok(recalled
        .reflections
        .into_iter()
        .filter(|r| r.repo != repo)
        .take(GIVEN_HALF_LIMIT)
        .map(|r| GivenSource {
            id: r.id,
            repo: r.repo,
            text: r.text,
            score: r.score,
        })
        .collect())
}

/// Validate `manifest` against this command's own structural invariants
/// (NOT the global orphan-root guard -- that lives in
/// `insert_reflection_with_meta`/`IdentityRootExists` and is unaffected
/// by this check). Checks: `root` is non-empty after trimming; `root`'s
/// byte length does not exceed `recall::WHOAMI_BYTE_CAP`; neither `root`
/// nor any `chain` entry contains the case-insensitive substring "what i
/// am". Returns `LegionError::WhoamiGenerate` naming the specific field
/// and reason on the first violation found.
pub fn validate_manifest(manifest: &IdentityManifest) -> Result<()> {
    if manifest.root.trim().is_empty() {
        return Err(LegionError::WhoamiGenerate(
            "manifest root is empty or all-whitespace".to_owned(),
        ));
    }
    if manifest.root.len() > recall::WHOAMI_BYTE_CAP {
        return Err(LegionError::WhoamiGenerate(format!(
            "manifest root is {} bytes, exceeding the {}-byte whoami cap",
            manifest.root.len(),
            recall::WHOAMI_BYTE_CAP
        )));
    }
    if contains_forbidden_phrase(&manifest.root) {
        return Err(LegionError::WhoamiGenerate(
            "manifest root contains the forbidden phrase \"what i am\"".to_owned(),
        ));
    }
    for (i, entry) in manifest.chain.iter().enumerate() {
        if contains_forbidden_phrase(entry) {
            return Err(LegionError::WhoamiGenerate(format!(
                "manifest chain[{i}] contains the forbidden phrase \"what i am\""
            )));
        }
    }
    Ok(())
}

fn contains_forbidden_phrase(text: &str) -> bool {
    text.to_lowercase().contains("what i am")
}

/// Reduce an arbitrary string to a safe single filename component:
/// every char outside `[A-Za-z0-9._-]` becomes `-`. `repo` flows into
/// the backup filename, and `Path::join` would interpret a `/` (or a
/// `..` segment) in it as path structure, silently relocating the
/// recovery file outside `backup_dir`.
fn sanitize_filename_component(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Apply mode: replace `repo`'s identity chain with `manifest`.
///
/// 1. `validate_manifest(manifest)` -- first gate, before any read or
///    write, so an invalid manifest is a pure no-op.
/// 2. Capture the full pre-existing `domain=identity` rows for `repo`
///    (`get_reflections_by_domain`, `IDENTITY_BACKUP_LIMIT`) -- read-only.
/// 3. If `dry_run`, return `ApplyResult::Planned` here. Nothing below
///    this line runs.
/// 4. Write the captured rows as JSON to a backup file under
///    `backup_dir`, before anything is written or deleted.
/// 5. Perform the swap via `Database::swap_identity_root` (#785): delete
///    every live identity root, insert the new root and chained
///    children, all inside one transaction. Either the whole thing
///    commits or rusqlite's automatic rollback on a dropped, uncommitted
///    transaction restores the old identity -- no two-live-roots and no
///    observable zero-root window. This replaces the
///    insert-then-verify-then-delete sequence an earlier draft of this
///    function used, which the insert-time `IdentityRootExists` guard
///    (#785) refuses outright: the guard sees the old root still live
///    and rejects the new insert before the swap can even begin, with no
///    `--force` escape by design.
/// 6. `swap_identity_root` only clears root-level rows (deliberately
///    non-cascading -- see its own doc comment); a replaced root's old
///    `--follows` children are left dangling. This command's own
///    retirement contract is the full pre-existing corpus (matching the
///    backup captured in step 2), not just the root(s), so any backed-up
///    id the swap did not already remove is deleted here explicitly.
///    Each leftover delete is best-effort and mirrors `handle_forget`'s
///    partial-failure handling: the swap has already committed by this
///    point, so a cleanup failure must not report the whole apply as
///    failed -- the backup file written in step 4 is the recovery path.
/// 7. Sync the search index for the new root, new children, and every
///    retired id (`swap_identity_root` only touches the database, the
///    same split-write `reflect_from_text_with_meta` already has). All
///    of step 7 is best-effort warn-not-fail: the swap committed in
///    step 5, so from here on the database is the truth and `legion
///    reindex` is the recovery path for any index straggler.
pub fn apply(
    db: &Database,
    index: &SearchIndex,
    repo: &str,
    manifest: &IdentityManifest,
    backup_dir: &Path,
    dry_run: bool,
) -> Result<ApplyResult> {
    validate_manifest(manifest)?;

    let old_rows = db.get_reflections_by_domain(
        repo,
        "identity",
        IDENTITY_BACKUP_LIMIT,
        crate::recall::ArchiveMode::Both,
        &crate::timerange::TimeRange::default(),
    )?;
    if old_rows.len() == IDENTITY_BACKUP_LIMIT {
        // A corpus at exactly the cap almost certainly means rows beyond
        // it were cut off by the SQL LIMIT -- and both the backup and the
        // retirement pass below operate only on what was captured. Make
        // the boundary loud instead of silently protecting (and retiring)
        // a truncated set.
        eprintln!(
            "[legion whoami --generate --apply] WARNING: identity corpus for '{repo}' hit the \
             backup scan cap ({IDENTITY_BACKUP_LIMIT} rows) -- rows beyond the cap are neither \
             backed up nor retired by this apply. Audit `legion recall --repo {repo} --domain \
             identity` before trusting this run's backup."
        );
    }
    let would_retire: Vec<String> = old_rows.iter().map(|r| r.id.clone()).collect();
    let backup_path = backup_dir.join(format!(
        "identity-backup-{}-{}.json",
        sanitize_filename_component(repo),
        Uuid::now_v7()
    ));

    if dry_run {
        return Ok(ApplyResult::Planned(ApplyPlan {
            would_retire,
            would_create: 1 + manifest.chain.len(),
            backup_path,
        }));
    }

    std::fs::create_dir_all(backup_dir)?;
    std::fs::write(&backup_path, serde_json::to_vec_pretty(&old_rows)?)?;

    let chained_texts: Vec<&str> = manifest.chain.iter().map(String::as_str).collect();
    let swap = db.swap_identity_root(repo, manifest.root.trim(), &chained_texts, "self")?;

    // Post-commit index writes are best-effort, same as the retirement
    // pass below: the DB swap has already committed, so an index-side
    // failure here must warn (recovery: `legion reindex`) rather than
    // report the whole apply as failed -- a hard error would tell the
    // calling agent the apply did not happen when `whoami` already shows
    // the new root, inviting a re-run that churns identity ids.
    warn_on_index_add_failure(
        index,
        &swap.root.id,
        repo,
        &swap.root.text,
        &swap.root.created_at,
    );
    for child in &swap.children {
        warn_on_index_add_failure(index, &child.id, repo, &child.text, &child.created_at);
    }

    let retired_ids =
        retire_old_identity_rows(db, index, &old_rows, &swap.deleted_ids, &backup_path);

    let new_ids: Vec<String> = std::iter::once(swap.root.id.clone())
        .chain(swap.children.iter().map(|c| c.id.clone()))
        .collect();

    Ok(ApplyResult::Applied(ApplyOutcome {
        new_ids,
        backup_path,
        retired_ids,
    }))
}

/// Best-effort tantivy add for a row already committed to the database
/// by `swap_identity_root`, warning (not failing) on error -- the new
/// identity is already live in the DB, so an index straggler must not
/// make `apply` report failure. `legion reindex` is the recovery path.
fn warn_on_index_add_failure(
    index: &SearchIndex,
    id: &str,
    repo: &str,
    text: &str,
    created_at: &str,
) {
    if let Err(e) = index.add(id, repo, text, created_at) {
        eprintln!(
            "[legion whoami --generate --apply] WARNING: the identity swap committed but the \
             tantivy index add failed for new row {id}.\n\
             The new identity is live in the database (whoami shows it) but will not surface in \
             BM25 recall results\n\
             until the index is rebuilt. Run `legion reindex` to reconcile. Do NOT re-run apply.\n\
             Underlying error: {e}"
        );
    }
}

/// Best-effort tantivy delete for an id already removed from the
/// database, warning (not failing) on a per-id error -- mirrors
/// `handle_forget`'s partial-failure handling: the caller's own write
/// already committed, so an index-side straggler must not report the
/// whole apply as failed. `legion reindex` is the stated recovery path.
fn warn_on_index_delete_failure(index: &SearchIndex, id: &str) {
    if let Err(e) = index.delete(id) {
        eprintln!(
            "[legion whoami --generate --apply] WARNING: SQLite delete succeeded but tantivy index delete failed for {id}.\n\
             The reflection is gone from the database but may still appear in BM25 recall results\n\
             as a ghost document until the index is rebuilt. Run `legion reindex` to reconcile.\n\
             Underlying error: {e}"
        );
    }
}

/// Retire every id captured in `old_rows` (the full pre-swap backup),
/// so this command's own retirement contract -- the whole pre-existing
/// corpus, not just the root(s) -- holds regardless of
/// `swap_identity_root`'s narrower, deliberately non-cascading delete.
/// Two cases per id:
///
/// - In `swap_deleted_ids` (the root(s) `swap_identity_root` already
///   removed from the database, inside its own transaction): only the
///   search index needs to catch up, since `swap_identity_root` never
///   touches it (database-only, like every other write in that
///   module).
/// - Not in `swap_deleted_ids` (a surviving `--follows` chain child --
///   `swap_identity_root` deliberately does not cascade to these, see
///   its own doc comment): delete from both stores here.
///
/// Every delete is best-effort and warns rather than fails -- see
/// `apply`'s step 6 for why. Returns the full set of ids retired: the
/// swap's own database deletions plus whatever this pass additionally
/// removed.
fn retire_old_identity_rows(
    db: &Database,
    index: &SearchIndex,
    old_rows: &[Reflection],
    swap_deleted_ids: &[String],
    backup_path: &Path,
) -> Vec<String> {
    let swap_deleted: std::collections::HashSet<&str> =
        swap_deleted_ids.iter().map(String::as_str).collect();
    let mut retired_ids: Vec<String> = swap_deleted_ids.to_vec();

    for id in swap_deleted_ids {
        warn_on_index_delete_failure(index, id);
    }

    for row in old_rows {
        if swap_deleted.contains(row.id.as_str()) {
            continue;
        }
        match db.delete_reflection(&row.id) {
            Ok(_) => {
                retired_ids.push(row.id.clone());
                warn_on_index_delete_failure(index, &row.id);
            }
            Err(e) => {
                eprintln!(
                    "[legion whoami --generate --apply] WARNING: could not retire leftover old identity row {}: {e}.\n\
                     It remains in the database; recover the pre-apply state from {} if needed.",
                    row.id,
                    backup_path.display()
                );
            }
        }
    }

    retired_ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ReflectionMeta;
    use crate::testutil::test_storage;

    // -- matching_byline -----------------------------------------------

    #[test]
    fn matching_byline_string_exact_match() {
        let value = serde_json::json!("legion");
        let bylines = vec!["legion".to_string(), "other".to_string()];
        assert_eq!(
            matching_byline(&value, &bylines),
            Some("legion".to_string())
        );
    }

    #[test]
    fn matching_byline_string_no_match() {
        let value = serde_json::json!("someone-else");
        let bylines = vec!["legion".to_string()];
        assert_eq!(matching_byline(&value, &bylines), None);
    }

    #[test]
    fn matching_byline_array_match() {
        let value = serde_json::json!(["someone-else", "legion"]);
        let bylines = vec!["legion".to_string()];
        assert_eq!(
            matching_byline(&value, &bylines),
            Some("legion".to_string())
        );
    }

    #[test]
    fn matching_byline_case_sensitive() {
        let value = serde_json::json!("Legion");
        let bylines = vec!["legion".to_string()];
        assert_eq!(matching_byline(&value, &bylines), None);
    }

    // -- gather: claimed half --------------------------------------------

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn seed_inventory_entry(
        db: &Database,
        repo: &str,
        path: &str,
        ext: &str,
    ) -> crate::db::inventory::FileInventoryEntry {
        let entry = crate::db::inventory::FileInventoryEntry {
            repo: repo.to_owned(),
            path: path.to_owned(),
            ext: Some(ext.to_owned()),
            lang: None,
            size: 0,
            mtime: chrono::Utc::now().to_rfc3339(),
            symbol_count: 0,
        };
        db.upsert_file_inventory(std::slice::from_ref(&entry))
            .unwrap();
        entry
    }

    fn fake_watch_repo(name: &str, workdir: &str) -> crate::watch::WatchRepoConfig {
        crate::watch::WatchRepoConfig {
            name: name.to_owned(),
            workdir: workdir.to_owned(),
            agent: None,
            broadcast_tags: vec![],
            extra: toml::Table::new(),
        }
    }

    #[test]
    fn gather_claimed_half_finds_date_titled_file_by_frontmatter_not_filename() {
        let (db, _index, dir) = test_storage();
        let vault_dir = dir.path().join("vault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        // No byline substring anywhere in the filename -- the exact case a
        // filename-glob approach would silently miss.
        write_file(
            &vault_dir,
            "2026-07-01-persistence.md",
            "---\nauthor: legion\n---\n\nPersistence is a discipline, not a feature.\n",
        );
        seed_inventory_entry(&db, "vault-repo", "2026-07-01-persistence.md", "md");

        let bylines = vec!["legion".to_string()];
        let claimed =
            gather_claimed_half(&db, "vault-repo", &vault_dir, &bylines).expect("gather ok");

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].byline, "legion");
        assert_eq!(claimed[0].path, "2026-07-01-persistence.md");
    }

    #[test]
    fn gather_claimed_half_filters_by_frontmatter_author_not_all_files() {
        let (db, _index, dir) = test_storage();
        let vault_dir = dir.path().join("vault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        write_file(&vault_dir, "mine.md", "---\nauthor: legion\n---\n\nmine\n");
        write_file(
            &vault_dir,
            "theirs.md",
            "---\nauthor: someone-else\n---\n\ntheirs\n",
        );
        seed_inventory_entry(&db, "vault-repo", "mine.md", "md");
        seed_inventory_entry(&db, "vault-repo", "theirs.md", "md");

        let bylines = vec!["legion".to_string()];
        let claimed =
            gather_claimed_half(&db, "vault-repo", &vault_dir, &bylines).expect("gather ok");

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].byline, "legion");
        assert_eq!(claimed[0].path, "mine.md");
    }

    #[test]
    fn gather_claimed_half_skips_file_with_no_frontmatter() {
        let (db, _index, dir) = test_storage();
        let vault_dir = dir.path().join("vault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        write_file(&vault_dir, "mine.md", "---\nauthor: legion\n---\n\nmine\n");
        write_file(
            &vault_dir,
            "plain.md",
            "just a plain markdown file, no frontmatter\n",
        );
        seed_inventory_entry(&db, "vault-repo", "mine.md", "md");
        seed_inventory_entry(&db, "vault-repo", "plain.md", "md");

        let bylines = vec!["legion".to_string()];
        let claimed =
            gather_claimed_half(&db, "vault-repo", &vault_dir, &bylines).expect("gather ok");

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].path, "mine.md");
    }

    #[test]
    fn gather_claimed_half_body_is_byte_identical_to_fixture() {
        let (db, _index, dir) = test_storage();
        let vault_dir = dir.path().join("vault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        let body =
            "---\nauthor: legion\n---\n\nParagraph one, with detail.\n\nParagraph two, distinct.\n";
        write_file(&vault_dir, "mine.md", body);
        seed_inventory_entry(&db, "vault-repo", "mine.md", "md");

        let bylines = vec!["legion".to_string()];
        let claimed =
            gather_claimed_half(&db, "vault-repo", &vault_dir, &bylines).expect("gather ok");

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].body, body);
    }

    #[test]
    fn gather_claimed_half_empty_returns_whoami_generate_error() {
        let (db, _index, dir) = test_storage();
        let vault_dir = dir.path().join("vault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        write_file(
            &vault_dir,
            "theirs.md",
            "---\nauthor: someone-else\n---\n\ntheirs\n",
        );
        seed_inventory_entry(&db, "vault-repo", "theirs.md", "md");

        let bylines = vec!["legion".to_string()];
        let err = gather_claimed_half(&db, "vault-repo", &vault_dir, &bylines).unwrap_err();
        match err {
            LegionError::WhoamiGenerate(msg) => {
                assert!(msg.contains("vault-repo"));
                assert!(msg.contains("legion"));
            }
            other => panic!("expected WhoamiGenerate, got {other:?}"),
        }
    }

    // -- gather: given half -----------------------------------------------

    #[test]
    fn gather_given_half_excludes_self_repo() {
        let (db, index, _dir) = test_storage();
        crate::reflect::reflect_from_text(&db, &index, "legion", "legion talks about itself")
            .unwrap();
        crate::reflect::reflect_from_text(&db, &index, "rafters", "legion is a careful engineer")
            .unwrap();

        let given =
            gather_given_half(&db, &index, None, "legion", &["legion".to_string()]).unwrap();

        assert_eq!(given.len(), 1);
        assert_eq!(given[0].repo, "rafters");
    }

    #[test]
    fn gather_given_half_empty_is_not_an_error() {
        let (db, index, _dir) = test_storage();
        crate::reflect::reflect_from_text(&db, &index, "legion", "unrelated content").unwrap();

        let given = gather_given_half(
            &db,
            &index,
            None,
            "legion",
            &["nonexistent-byline".to_string()],
        )
        .unwrap();

        assert!(given.is_empty());
    }

    // -- resolve_vault_repo_workdir -------------------------------------------
    //
    // `gather`'s own watch.toml resolution (via `data_dir()`, cached in a
    // process-wide `OnceLock`) is exercised end-to-end by
    // `tests/integration/whoami_generate.rs`, which spawns a fresh `legion`
    // subprocess per `common::legion_cmd`'s documented pattern. These tests
    // cover the pure lookup `vault_repo_workdir` delegates to.

    #[test]
    fn resolve_vault_repo_workdir_unknown_repo_returns_watch_config_error() {
        let repos = vec![fake_watch_repo("known-repo", "/tmp/known-repo")];
        let err = resolve_vault_repo_workdir(&repos, "ghost-vault-repo").unwrap_err();
        match err {
            LegionError::WatchConfig(msg) => assert!(msg.contains("ghost-vault-repo")),
            other => panic!("expected WatchConfig, got {other:?}"),
        }
    }

    #[test]
    fn resolve_vault_repo_workdir_known_repo_returns_its_workdir() {
        let repos = vec![
            fake_watch_repo("other-repo", "/tmp/other-repo"),
            fake_watch_repo("vault-repo", "/tmp/vault-repo"),
        ];
        let workdir = resolve_vault_repo_workdir(&repos, "vault-repo").unwrap();
        assert_eq!(workdir, PathBuf::from("/tmp/vault-repo"));
    }

    // -- validate_manifest --------------------------------------------------

    #[test]
    fn validate_manifest_rejects_empty_root() {
        let manifest = IdentityManifest {
            root: "   ".to_string(),
            chain: vec![],
        };
        let err = validate_manifest(&manifest).unwrap_err();
        assert!(matches!(err, LegionError::WhoamiGenerate(_)));
    }

    #[test]
    fn validate_manifest_rejects_oversized_root() {
        let manifest = IdentityManifest {
            root: "x".repeat(recall::WHOAMI_BYTE_CAP + 1),
            chain: vec![],
        };
        let err = validate_manifest(&manifest).unwrap_err();
        match err {
            LegionError::WhoamiGenerate(msg) => {
                assert!(msg.contains(&(recall::WHOAMI_BYTE_CAP + 1).to_string()));
                assert!(msg.contains(&recall::WHOAMI_BYTE_CAP.to_string()));
            }
            other => panic!("expected WhoamiGenerate, got {other:?}"),
        }
    }

    #[test]
    fn validate_manifest_rejects_forbidden_phrase_in_chain_names_index() {
        let manifest = IdentityManifest {
            root: "a careful builder".to_string(),
            chain: vec![
                "an early chapter".to_string(),
                "knows What I Am deeply".to_string(),
            ],
        };
        let err = validate_manifest(&manifest).unwrap_err();
        match err {
            LegionError::WhoamiGenerate(msg) => {
                assert!(msg.contains("chain[1]"));
                assert!(!msg.contains("root"));
            }
            other => panic!("expected WhoamiGenerate, got {other:?}"),
        }
    }

    #[test]
    fn validate_manifest_accepts_empty_chain() {
        let manifest = IdentityManifest {
            root: "a careful builder".to_string(),
            chain: vec![],
        };
        assert!(validate_manifest(&manifest).is_ok());
    }

    // -- apply ----------------------------------------------------------------

    #[test]
    fn apply_invalid_manifest_writes_nothing_and_changes_nothing() {
        let (db, index, dir) = test_storage();
        db.insert_reflection_with_meta(
            "legion",
            "old identity root",
            "self",
            &ReflectionMeta {
                domain: Some("identity".to_string()),
                tags: None,
                parent_id: None,
            },
        )
        .unwrap();
        let before = db
            .get_reflections_by_domain(
                "legion",
                "identity",
                500,
                crate::recall::ArchiveMode::Both,
                &crate::timerange::TimeRange::default(),
            )
            .unwrap();

        let backup_dir = dir.path().join("backups");
        let manifest = IdentityManifest {
            root: "".to_string(),
            chain: vec![],
        };
        let err = apply(&db, &index, "legion", &manifest, &backup_dir, false).unwrap_err();
        assert!(matches!(err, LegionError::WhoamiGenerate(_)));

        assert!(!backup_dir.exists());
        let after = db
            .get_reflections_by_domain(
                "legion",
                "identity",
                500,
                crate::recall::ArchiveMode::Both,
                &crate::timerange::TimeRange::default(),
            )
            .unwrap();
        assert_eq!(before.len(), after.len());
    }

    #[test]
    fn apply_writes_backup_containing_root_and_chain_child_before_any_change() {
        let (db, index, dir) = test_storage();
        let root = db
            .insert_reflection_with_meta(
                "legion",
                "old root",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".to_string()),
                    tags: None,
                    parent_id: None,
                },
            )
            .unwrap();
        db.insert_reflection_with_meta(
            "legion",
            "old chain child",
            "self",
            &ReflectionMeta {
                domain: Some("identity".to_string()),
                tags: None,
                parent_id: Some(root.id.clone()),
            },
        )
        .unwrap();

        let backup_dir = dir.path().join("backups");
        let manifest = IdentityManifest {
            root: "new root".to_string(),
            chain: vec![],
        };
        let result = apply(&db, &index, "legion", &manifest, &backup_dir, false).unwrap();
        let outcome = match result {
            ApplyResult::Applied(o) => o,
            ApplyResult::Planned(_) => panic!("expected Applied"),
        };

        let backup_content = std::fs::read_to_string(&outcome.backup_path).unwrap();
        let backed_up: Vec<Reflection> = serde_json::from_str(&backup_content).unwrap();
        assert_eq!(backed_up.len(), 2);
    }

    #[test]
    fn apply_replaces_root_and_retires_every_old_id_including_dangling_children() {
        let (db, index, dir) = test_storage();
        let root = db
            .insert_reflection_with_meta(
                "legion",
                "old root",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".to_string()),
                    tags: None,
                    parent_id: None,
                },
            )
            .unwrap();
        let child = db
            .insert_reflection_with_meta(
                "legion",
                "old chain child",
                "self",
                &ReflectionMeta {
                    domain: Some("identity".to_string()),
                    tags: None,
                    parent_id: Some(root.id.clone()),
                },
            )
            .unwrap();

        let backup_dir = dir.path().join("backups");
        let manifest = IdentityManifest {
            root: "new root".to_string(),
            chain: vec!["new chain entry".to_string()],
        };
        let result = apply(&db, &index, "legion", &manifest, &backup_dir, false).unwrap();
        let outcome = match result {
            ApplyResult::Applied(o) => o,
            ApplyResult::Planned(_) => panic!("expected Applied"),
        };

        assert_eq!(outcome.new_ids.len(), 2);
        assert!(outcome.retired_ids.contains(&root.id));
        assert!(outcome.retired_ids.contains(&child.id));

        let roots = db.get_identity_roots("legion", 50).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].text, "new root");

        assert!(db.get_reflection_by_id(&root.id).unwrap().is_none());
        assert!(db.get_reflection_by_id(&child.id).unwrap().is_none());
    }

    #[test]
    fn apply_removes_all_retired_ids_from_search_index_not_just_the_database() {
        // Seed via reflect_from_text_with_meta (not insert_reflection_with_meta
        // directly) so the old rows actually land in the search index too,
        // the same as every real `legion reflect --whoami` write -- a
        // DB-only fixture would never exercise the index-sync path at all.
        // Covers BOTH retirement branches: the swap-deleted root (index-only
        // sync -- swap_identity_root never touches tantivy) and the leftover
        // chain child (full db+index delete in retire_old_identity_rows).
        let (db, index, dir) = test_storage();
        let root_id = crate::reflect::reflect_from_text_with_meta(
            &db,
            &index,
            "legion",
            "an old distinctive root about careful engineering",
            &ReflectionMeta {
                domain: Some("identity".to_string()),
                tags: None,
                parent_id: None,
            },
        )
        .unwrap();
        let child_id = crate::reflect::reflect_from_text_with_meta(
            &db,
            &index,
            "legion",
            "an old distinctive chapter about zanzibar gateways",
            &ReflectionMeta {
                domain: Some("identity".to_string()),
                tags: None,
                parent_id: Some(root_id.clone()),
            },
        )
        .unwrap();

        let backup_dir = dir.path().join("backups");
        let manifest = IdentityManifest {
            root: "new root".to_string(),
            chain: vec![],
        };
        apply(&db, &index, "legion", &manifest, &backup_dir, false).unwrap();

        let root_hits = index
            .search(
                "legion",
                "distinctive careful engineering",
                10,
                &crate::timerange::TimeRange::default(),
            )
            .unwrap();
        let root_hit_ids: Vec<&str> = root_hits.iter().map(|h| h.id.as_str()).collect();
        assert!(
            !root_hit_ids.contains(&root_id.as_str()),
            "swap-deleted old root must not still be findable in the search index: {root_hit_ids:?}"
        );

        let child_hits = index
            .search(
                "legion",
                "distinctive zanzibar gateways",
                10,
                &crate::timerange::TimeRange::default(),
            )
            .unwrap();
        let child_hit_ids: Vec<&str> = child_hits.iter().map(|h| h.id.as_str()).collect();
        assert!(
            !child_hit_ids.contains(&child_id.as_str()),
            "retired leftover chain child must not still be findable in the search index: {child_hit_ids:?}"
        );
    }

    #[test]
    fn apply_with_no_preexisting_identity_rows_bootstraps_cleanly() {
        // Plausible first-run state: no prior identity at all. The apply
        // must bootstrap (swap_identity_root handles the no-prior-root
        // case), retire nothing, and still write a (empty-corpus) backup.
        let (db, index, dir) = test_storage();
        let backup_dir = dir.path().join("backups");
        let manifest = IdentityManifest {
            root: "first ever root".to_string(),
            chain: vec!["first chapter".to_string()],
        };
        let result = apply(&db, &index, "legion", &manifest, &backup_dir, false).unwrap();
        let outcome = match result {
            ApplyResult::Applied(o) => o,
            ApplyResult::Planned(_) => panic!("expected Applied"),
        };

        assert_eq!(outcome.new_ids.len(), 2);
        assert!(outcome.retired_ids.is_empty());

        let backed_up: Vec<Reflection> =
            serde_json::from_str(&std::fs::read_to_string(&outcome.backup_path).unwrap()).unwrap();
        assert!(backed_up.is_empty());

        let roots = db.get_identity_roots("legion", 50).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].text, "first ever root");
    }

    #[test]
    fn apply_backup_path_stays_inside_backup_dir_for_hostile_repo_name() {
        // `repo` flows into the backup filename; a value carrying path
        // separators must not relocate the recovery file outside
        // backup_dir (Path::join would treat `/` as structure).
        let (db, index, dir) = test_storage();
        let backup_dir = dir.path().join("backups");
        let manifest = IdentityManifest {
            root: "new root".to_string(),
            chain: vec![],
        };
        let result = apply(
            &db,
            &index,
            "../../escape/attempt",
            &manifest,
            &backup_dir,
            false,
        )
        .unwrap();
        let outcome = match result {
            ApplyResult::Applied(o) => o,
            ApplyResult::Planned(_) => panic!("expected Applied"),
        };

        assert!(
            outcome.backup_path.parent() == Some(backup_dir.as_path()),
            "backup must be a direct child of backup_dir, got {}",
            outcome.backup_path.display()
        );
        assert!(outcome.backup_path.exists());
    }

    #[test]
    fn sanitize_filename_component_neutralizes_separators() {
        assert_eq!(
            sanitize_filename_component("../../etc/passwd"),
            "..-..-etc-passwd"
        );
        assert_eq!(sanitize_filename_component("legion"), "legion");
        assert_eq!(sanitize_filename_component("my repo\\x"), "my-repo-x");
    }

    #[test]
    fn apply_chain_order_matches_manifest() {
        let (db, index, dir) = test_storage();
        let backup_dir = dir.path().join("backups");
        let manifest = IdentityManifest {
            root: "new root".to_string(),
            chain: vec!["first".to_string(), "second".to_string()],
        };
        let result = apply(&db, &index, "legion", &manifest, &backup_dir, false).unwrap();
        let outcome = match result {
            ApplyResult::Applied(o) => o,
            ApplyResult::Planned(_) => panic!("expected Applied"),
        };

        let chain = db.get_chain(&outcome.new_ids[0]).unwrap();
        let texts: Vec<&str> = chain.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, vec!["new root", "first", "second"]);
    }

    #[test]
    fn apply_dry_run_changes_nothing_and_writes_no_file() {
        let (db, index, dir) = test_storage();
        db.insert_reflection_with_meta(
            "legion",
            "old root",
            "self",
            &ReflectionMeta {
                domain: Some("identity".to_string()),
                tags: None,
                parent_id: None,
            },
        )
        .unwrap();
        let before = db
            .get_reflections_by_domain(
                "legion",
                "identity",
                500,
                crate::recall::ArchiveMode::Both,
                &crate::timerange::TimeRange::default(),
            )
            .unwrap();

        let backup_dir = dir.path().join("backups");
        let manifest = IdentityManifest {
            root: "new root".to_string(),
            chain: vec!["child".to_string()],
        };
        let result = apply(&db, &index, "legion", &manifest, &backup_dir, true).unwrap();
        let plan = match result {
            ApplyResult::Planned(p) => p,
            ApplyResult::Applied(_) => panic!("expected Planned"),
        };

        assert_eq!(plan.would_retire, vec![before[0].id.clone()]);
        assert_eq!(plan.would_create, 2);
        assert!(!plan.backup_path.exists());

        let after = db
            .get_reflections_by_domain(
                "legion",
                "identity",
                500,
                crate::recall::ArchiveMode::Both,
                &crate::timerange::TimeRange::default(),
            )
            .unwrap();
        assert_eq!(before.len(), after.len());
    }

    // -- retire_old_identity_rows: db-delete failure is warn-not-fail -------

    #[test]
    fn retire_old_identity_rows_warns_and_continues_when_a_db_delete_fails() {
        // Exercises the `Err` branch of `db.delete_reflection` inside
        // `retire_old_identity_rows` directly, rather than through `apply`
        // -- there is no in-process way to force that specific race (the
        // row vanishing between apply's initial capture and this cleanup
        // pass) through the public API. A synthetic `Reflection` whose id
        // was never actually inserted reproduces the same `Err` path
        // deterministically: `delete_reflection` returns
        // `LegionError::ReflectionNotFound` for any id it can't find,
        // which is exactly the shape a genuine race would also produce.
        let (db, index, dir) = test_storage();
        let backup_path = dir.path().join("backups/unused.json");

        let ghost_row = Reflection {
            id: "00000000-0000-7000-8000-000000000000".to_string(),
            repo: "legion".to_string(),
            text: "never actually inserted".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: None,
            audience: "self".to_string(),
            domain: Some("identity".to_string()),
            tags: None,
            recall_count: 0,
            last_recalled_at: None,
            parent_id: None,
        };

        // Must not panic despite the delete failing, and must not report
        // the ghost id as retired -- it was never actually removed.
        let retired_ids = retire_old_identity_rows(
            &db,
            &index,
            std::slice::from_ref(&ghost_row),
            &[],
            &backup_path,
        );

        assert!(
            !retired_ids.contains(&ghost_row.id),
            "a row whose delete failed must not be reported as retired: {retired_ids:?}"
        );
    }
}
