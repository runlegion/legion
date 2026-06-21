//! Team-board handlers: post, signal, pending-replies, bullpen
//! (carved from main.rs, #610).

use std::path::PathBuf;

use crate::cli::datadir::data_dir;
use crate::cli::memory::{
    backfill_embeddings, run_compound_command_with_meta, try_load_embed_model,
};
use crate::cli::util::{open_db, open_db_and_index};
use crate::{board, db, error, reflect, signal, task, verbs, watch};

pub(crate) fn handle_post(
    repo: Vec<String>,
    text: Option<String>,
    transcript: Option<PathBuf>,
    domain: Option<String>,
    tags: Option<String>,
    follows: Option<String>,
) -> error::Result<()> {
    // Redirect @self posts to reflect -- they're private, not for the team
    let is_self_post = text.as_deref().is_some_and(|t| {
        let lower = t.trim_start().to_lowercase();
        lower.starts_with("@self ") || lower.starts_with("@self\t") || lower == "@self"
    });
    if is_self_post {
        eprintln!("[legion] @self posts are private -- redirecting to reflect");
    }

    let (database, index) = open_db_and_index()?;
    let meta = db::ReflectionMeta {
        domain,
        tags,
        parent_id: follows,
    };

    if is_self_post {
        run_compound_command_with_meta(
            &database,
            &index,
            &repo,
            &text,
            &transcript,
            &meta,
            reflect::reflect_from_text_with_meta,
            reflect::reflect_from_transcript_with_meta,
            "reflecting",
        )?;
    } else {
        run_compound_command_with_meta(
            &database,
            &index,
            &repo,
            &text,
            &transcript,
            &meta,
            board::post_from_text_with_meta,
            board::post_from_transcript_with_meta,
            "posting",
        )?;
    }

    // Compute embeddings for new posts
    if let Some(model) = try_load_embed_model() {
        let n = backfill_embeddings(&database, &model)?;
        if n > 0 {
            info!("[legion] embedded {} posts", n);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_signal(
    repo: Vec<String>,
    to: String,
    verb: String,
    status: Option<String>,
    note: Option<String>,
    details: Option<String>,
    follows: Option<String>,
    domain: Option<String>,
    tags: Option<String>,
) -> error::Result<()> {
    // Guard: --repo is the authoring context; --to is the routing target.
    // Sending a signal where author == recipient is silently dropped by the
    // poll query (src/db/board.rs: `AND r.repo != ?{repo_param}`), so the
    // daemon never sees it. Broadcasts (@all, @everyone) are exempt -- they
    // route through a separate fan-out path that ignores the author filter.
    // `is_self_address` handles case normalization.
    if is_self_address(&repo, &to) {
        // Find the matching author to name it in the error.
        let matched = repo
            .iter()
            .find(|r| r.to_lowercase() == to.to_lowercase())
            .cloned()
            .unwrap_or_else(|| to.clone());
        return Err(error::LegionError::SignalSelfAddressed { repo: matched });
    }

    let (database, index) = open_db_and_index()?;

    // One compose/validate entry point shared with the MCP legion_signal
    // tool (#612): details wire parsing, the #587 required-fields gate,
    // and the note length cap all live in signal::compose.
    let text = signal::compose(
        &to,
        &verb,
        status.as_deref(),
        note.as_deref(),
        details.as_deref(),
        verbs::active_manifest(),
    )?;

    let meta = db::ReflectionMeta {
        domain,
        tags,
        parent_id: follows,
    };

    run_compound_command_with_meta(
        &database,
        &index,
        &repo,
        &Some(text),
        &None,
        &meta,
        board::post_from_text_with_meta,
        board::post_from_transcript_with_meta,
        "sending signal",
    )?;

    // Compute embeddings for new signals
    if let Some(model) = try_load_embed_model() {
        let n = backfill_embeddings(&database, &model)?;
        if n > 0 {
            info!("[legion] embedded {} signals", n);
        }
    }

    // #586: tell the sender when a directed signal will not wake its
    // recipient -- a non-wake-worthy verb delivers to a live session but
    // never pages an asleep agent, so surface it at send time.
    if watch::directed_verb_will_not_wake(&to, &verb) {
        let wake_verbs: Vec<&str> = verbs::active_manifest().wake_verb_names();
        eprintln!(
            "[legion] note: verb '{}' will not wake {} -- it delivers to a live \
             session but does not page an asleep agent. Wake-worthy verbs: {}.",
            verb,
            to,
            wake_verbs.join(", ")
        );
    }
    Ok(())
}

pub(crate) fn handle_pending_replies(repo: String) -> error::Result<()> {
    let database = open_db()?;

    // Build the full addressable name set for this repo via the same
    // wake_addresses() the watch poll cycle uses, so the read path can never
    // disagree with the wake path on which addresses reach this repo. Fall
    // back to [repo] for un-watched callers (no watch.toml, or repo not in it).
    let names: Vec<String> = watch::load_config(&data_dir()?.join("watch.toml"))
        .ok()
        .and_then(|cfg| {
            cfg.repos
                .iter()
                .find(|r| r.name == repo)
                .map(watch::WatchRepoConfig::wake_addresses)
        })
        .unwrap_or_else(|| vec![repo.clone()]);

    let signals = watch::find_pending_signals(&database, &repo, &names, None)?;
    let reply_required: Vec<(String, String, String)> = signals
        .into_iter()
        .filter(|(_, text, _)| watch::signal_requires_reply(text))
        .collect();

    if !reply_required.is_empty() {
        print!("{}", watch::build_wake_prompt(&repo, &reply_required));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_bullpen(
    repo: Option<String>,
    count: bool,
    signals: bool,
    musings: bool,
    archive: bool,
    archived: bool,
    include_stale: bool,
    include_resolved: bool,
) -> error::Result<()> {
    let database = open_db()?;

    if archive {
        let count = board::archive_read_posts(&database)?;
        eprintln!("[legion] archived {count} posts");
    } else if archived {
        let posts = board::bullpen_archived(&database)?;
        let output = board::format_bullpen(&posts);
        if !output.is_empty() {
            print!("{output}");
        }
    } else {
        // repo is guaranteed by clap's required_unless_present_any
        let repo = repo.expect("--repo required for this path");
        if count {
            let post_count = board::bullpen_count(&database, &repo)?;
            let task_count = task::count_pending_inbound(&database, &repo)?;
            let output = board::format_bullpen_count(post_count, task_count);
            if !output.is_empty() {
                println!("{output}");
            }
        } else {
            let filter = if signals {
                board::BullpenFilter::SignalsOnly
            } else if musings {
                board::BullpenFilter::MusingsOnly
            } else {
                board::BullpenFilter::All
            };
            let posts = board::bullpen_filtered_with_decay(
                &database,
                &repo,
                filter,
                include_stale,
                include_resolved,
            )?;
            let mut output = board::format_bullpen(&posts);
            if filter == board::BullpenFilter::All {
                let pending_tasks = task::get_pending_inbound(&database, &repo)?;
                let task_output = task::format_pending_for_surface(&pending_tasks);
                output.push_str(&task_output);
            }
            if !output.is_empty() {
                print!("{output}");
            }
        }
    }
    Ok(())
}

/// Pure helper: decide whether a signal address is a self-address collision.
///
/// Returns `true` when `to` equals any entry in `repos` (case-insensitive)
/// AND is not a broadcast sentinel ("all" / "everyone"). Extracted so the
/// guard logic is unit-testable without a live DB.
fn is_self_address(repos: &[String], to: &str) -> bool {
    let to_lower = to.to_lowercase();
    let is_broadcast = matches!(to_lower.as_str(), "all" | "everyone");
    if is_broadcast {
        return false;
    }
    repos.iter().any(|r| r.to_lowercase() == to_lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- FIX 1: self-address guard ------------------------------------------

    #[test]
    fn self_address_guard_rejects_same_repo() {
        // Exact match: author repo equals recipient.
        assert!(
            is_self_address(&["legion".to_string()], "legion"),
            "legion -> legion must be detected as self-address"
        );
    }

    #[test]
    fn self_address_guard_rejects_case_insensitive() {
        // Case normalization: "Legion" and "legion" are the same.
        assert!(
            is_self_address(&["legion".to_string()], "Legion"),
            "case mismatch must still be caught as self-address"
        );
        assert!(
            is_self_address(&["Legion".to_string()], "legion"),
            "repo-side case mismatch must still be caught"
        );
    }

    #[test]
    fn self_address_guard_allows_different_repo() {
        // Different repos: author=legion, recipient=huttspawn -- this is fine.
        assert!(
            !is_self_address(&["legion".to_string()], "huttspawn"),
            "a signal to a different repo must not be flagged"
        );
    }

    #[test]
    fn self_address_guard_allows_broadcast_all() {
        // Broadcasts are always permitted regardless of author.
        assert!(
            !is_self_address(&["legion".to_string()], "all"),
            "broadcast 'all' must never be flagged as self-address"
        );
        assert!(
            !is_self_address(&["legion".to_string()], "everyone"),
            "broadcast 'everyone' must never be flagged as self-address"
        );
    }

    #[test]
    fn self_address_guard_allows_broadcast_case_insensitive() {
        // Broadcasts with different case must also pass.
        assert!(
            !is_self_address(&["legion".to_string()], "ALL"),
            "broadcast 'ALL' must never be flagged"
        );
        assert!(
            !is_self_address(&["legion".to_string()], "Everyone"),
            "broadcast 'Everyone' must never be flagged"
        );
    }

    #[test]
    fn self_address_error_variant_names_the_repo() {
        // Verify the error variant carries the repo name so the message is useful.
        let err = error::LegionError::SignalSelfAddressed {
            repo: "legion".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("legion"),
            "error message must name the repo: {msg}"
        );
        assert!(
            msg.contains("--repo"),
            "error message must reference --repo flag: {msg}"
        );
        assert!(
            msg.contains("--to"),
            "error message must reference --to flag: {msg}"
        );
    }
}
