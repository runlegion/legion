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
    // daemon never sees it. Broadcasts (bare "all", "everyone", or the
    // "@"-prefixed forms) are exempt -- they route through a separate fan-out
    // path that ignores the author filter. `signal::is_self_address` handles
    // case normalization and the leading-@ strip.
    if crate::signal::is_self_address(&repo, &to) {
        // Find the matching author to name it in the error. Strip a leading '@'
        // from `to` before comparison so "@legion" matches "legion" in the repo
        // list -- matching is_self_address's own normalization.
        let bare_to = to.strip_prefix('@').unwrap_or(&to);
        let matched = repo
            .iter()
            .find(|r| r.to_lowercase() == bare_to.to_lowercase())
            .cloned()
            .unwrap_or_else(|| bare_to.to_string());
        return Err(error::LegionError::SignalSelfAddressed { repo: matched });
    }

    let (database, index) = open_db_and_index()?;

    // #949: an `answer` is Record-shaped and never wakes on its own, but an
    // answer that RESOLVES a pending ask must page the asker -- the one
    // party known to be blocked on the reply. The resolved ask id is
    // computed HERE, at send time, because the #919 retire below destroys
    // the pending-ask evidence synchronously in this same invocation; a
    // watch daemon polling later (possibly on another host) would find
    // nothing left to re-derive. Stamping it into `details` makes the fact
    // travel with the synced reflections row.
    let resolves_id: Option<String> = if verb.eq_ignore_ascii_case("answer") {
        match answered_ask_id(&database, &repo, &to) {
            Ok(id) => id,
            // Logged, never propagated. The stamp is an optimization on top
            // of a send that must succeed regardless -- same guarantee the
            // #919 retire call gets by running after the send, enforced here
            // on the earlier side of compose.
            Err(e) => {
                eprintln!("[legion] could not check for a pending ask from {to}: {e}");
                None
            }
        }
    } else {
        None
    };
    let details: Option<String> = stamp_resolves(details.as_deref(), resolves_id.as_deref());

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

    // #919: replying retires what it answers. Runs after the send so a
    // failure here can never cost the signal itself.
    match retire_answered_signals(&database, &repo, &to) {
        Ok(n) if n > 0 => {
            eprintln!(
                "[legion] retired {n} answered ask(s) from {to} -- they will not re-surface at next session start"
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("[legion] could not retire answered asks from {to}: {e}"),
    }

    // #586: tell the sender when a directed signal will not wake its
    // recipient -- a non-wake-worthy verb delivers to a live session but
    // never pages an asleep agent, so surface it at send time.
    //
    // Suppressed when this send stamped a `resolves` marker (#949): that
    // answer DOES wake its recipient, and the manifest-based warning would
    // state the exact opposite of what just happened.
    if resolves_id.is_none() && watch::directed_verb_will_not_wake(&to, &verb) {
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

/// Addressable name set for `repo`, via the same `wake_addresses()` the watch
/// poll cycle uses, so the read paths can never disagree with the wake path on
/// which addresses reach this repo. Falls back to `[repo]` for un-watched
/// callers (no watch.toml, or repo not listed in it).
fn wake_names_for(repo: &str) -> error::Result<Vec<String>> {
    Ok(watch::load_config(&data_dir()?.join("watch.toml"))
        .ok()
        .and_then(|cfg| {
            cfg.repos
                .iter()
                .find(|r| r.name == repo)
                .map(watch::WatchRepoConfig::wake_addresses)
        })
        .unwrap_or_else(|| vec![repo.to_string()]))
}

/// Retire `author`'s pending asks from `recipient` after `author` replies to
/// them (#919).
///
/// Replying to a signal did not previously clear it. Nothing on the CLI path
/// ever wrote `watch_handled` -- only the watch spawn path did
/// (src/watch/gates.rs) -- so an agent that answered every ask still woke to
/// the identical queue next session, with nothing distinguishing "unanswered"
/// from "answered but unresolved". The honest response to that is to answer
/// again, which is how a converged thread becomes an infinite one.
///
/// Retires on REPLY rather than on render. Marking at render was the obvious
/// alternative and is wrong here: `legion pending-replies` backs both the
/// SessionStart banner and post-compact.sh (lib/boot-sections.sh drives both),
/// so retiring what it printed would mean a compacted session silently loses
/// obligations it has not answered yet -- compaction being exactly when the
/// agent has forgotten them. Replying is the first point at which the ask is
/// demonstrably handled rather than merely delivered.
///
/// Scoped to `recipient`: only asks sent BY the agent being replied to are
/// retired. Everything retired was rendered in the same banner the reply
/// answers, so this never retires an ask the author has not seen.
///
/// Host-local by construction. `watch_handled` is not on the sync wire (the
/// four synced tables are pinned at src/sync_actor.rs:45-48), so this clears
/// THIS host's inbox copy and never touches a peer's queue or the team's
/// bullpen. That is deliberately NOT `resolve_post`, which writes `resolved_at`
/// on a synced reflections row and hides the thread from everyone's default
/// `legion bullpen` -- a per-inbox intent with a team-wide effect.
fn retire_answered_signals(
    database: &db::Database,
    authors: &[String],
    recipient: &str,
) -> error::Result<usize> {
    let mut retired = 0;
    for author in authors {
        let names = wake_names_for(author)?;
        retired += retire_answered_for_author(database, author, &names, recipient)?;
    }
    Ok(retired)
}

/// The pending wake-worthy ask ids from `recipient` that a reply from
/// `author` would resolve -- the read-only half of
/// [`retire_answered_for_author`], marking nothing handled.
///
/// Split out for #949: the send path needs the same match to STAMP the
/// resolved ask onto an outgoing `answer` before the retire consumes it,
/// and one shared matcher is what keeps the stamp and the retirement from
/// ever disagreeing about which ask a reply answered.
///
/// Broadcasts are not replies to anyone in particular; matching every
/// pending ask because the author addressed the room would sweep in
/// unrelated threads from unrelated senders.
fn matching_pending_ask_ids(
    database: &db::Database,
    author: &str,
    names: &[String],
    recipient: &str,
) -> error::Result<Vec<String>> {
    if crate::signal::is_broadcast_address(recipient) {
        return Ok(Vec::new());
    }
    let bare = recipient.strip_prefix('@').unwrap_or(recipient);
    let pending = watch::find_pending_signals(database, author, names, None)?;
    Ok(pending
        .into_iter()
        // Only the set `pending-replies` actually renders. A pending signal
        // that does not require a reply never reaches the banner, so it is
        // not part of the reported pain and is left alone.
        .filter(|(_, text, sender)| {
            watch::signal_requires_reply(text) && sender.eq_ignore_ascii_case(bare)
        })
        .map(|(id, _, _)| id)
        .collect())
}

/// The id of the first still-pending wake-worthy ask (across `authors`) that
/// `to` sent, if any -- the ask a `--verb answer` send resolves (#949).
///
/// `None` means this looks like a fire-and-forget answer: nothing tracked as
/// asked, so the caller must not stamp `resolves` and the send must not wake
/// anyone. Best-effort by contract -- see the call site in [`handle_signal`]
/// for why a DB error here is logged rather than propagated.
fn answered_ask_id(
    database: &db::Database,
    authors: &[String],
    to: &str,
) -> error::Result<Option<String>> {
    for author in authors {
        let names = wake_names_for(author)?;
        if let Some(id) = matching_pending_ask_ids(database, author, &names, to)?
            .into_iter()
            .next()
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Merge the system-computed `resolves:<ask-id>` marker into the user's
/// `--details` wire string, keeping every pair the sender typed (#949).
///
/// The stamp is appended LAST on purpose: `format_signal` preserves this
/// order and `parse_signal` reads the braced block into a HashMap, so a
/// sender who hand-types their own `resolves` key is overridden by the
/// computed one rather than able to shadow it.
fn stamp_resolves(details: Option<&str>, ask_id: Option<&str>) -> Option<String> {
    match (details, ask_id) {
        (Some(d), Some(id)) => Some(format!("{d}, resolves: {id}")),
        (Some(d), None) => Some(d.to_string()),
        (None, Some(id)) => Some(format!("resolves: {id}")),
        (None, None) => None,
    }
}

/// Single-author half of [`retire_answered_signals`], with the addressable
/// name set injected so it is testable without a watch.toml on disk.
fn retire_answered_for_author(
    database: &db::Database,
    author: &str,
    names: &[String],
    recipient: &str,
) -> error::Result<usize> {
    let mut retired = 0;
    for id in matching_pending_ask_ids(database, author, names, recipient)? {
        match database.mark_signal_handled_for_repo(&id, author) {
            Ok(true) => retired += 1,
            Ok(false) => {}
            // Logged, not propagated: the reply itself already landed, and a
            // mark that fails costs one duplicate render next session --
            // strictly better than failing a command whose primary effect
            // succeeded.
            Err(e) => eprintln!("[legion] failed to retire answered signal {id}: {e}"),
        }
    }
    Ok(retired)
}

pub(crate) fn handle_pending_replies(repo: String) -> error::Result<()> {
    let database = open_db()?;

    let names = wake_names_for(&repo)?;

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
    since: Option<String>,
    until: Option<String>,
    on: Option<String>,
) -> error::Result<()> {
    let database = open_db()?;

    // #786: applies to the --repo listing path only, per --since/--until/
    // --on's help text; --count/--archive/--archived ignore it.
    let range =
        crate::timerange::TimeRange::parse(since.as_deref(), until.as_deref(), on.as_deref())?;

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
                &range,
            )?;
            let mut output = board::format_bullpen(&posts);
            if filter == board::BullpenFilter::All {
                let pending_tasks = task::get_pending_inbound(&database, &repo, &range)?;
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

// is_self_address was extracted from this module and now lives in
// crate::signal (src/signal.rs) so both the CLI and MCP signal guards
// share one implementation. See that module's tests for the full suite.

#[cfg(test)]
mod tests {
    use super::*;

    // -- self-address guard (delegates to crate::signal::is_self_address) ---
    //
    // These tests exercise the shared guard from the CLI surface's perspective.
    // The full sentinel + case + @-strip coverage lives in signal::tests.

    #[test]
    fn self_address_guard_rejects_same_repo() {
        assert!(
            crate::signal::is_self_address(&["legion".to_string()], "legion"),
            "exact match must be detected as self-address"
        );
    }

    #[test]
    fn self_address_guard_allows_broadcast_all() {
        // Bare broadcast sentinels.
        assert!(
            !crate::signal::is_self_address(&["legion".to_string()], "all"),
            "broadcast 'all' must never be flagged as self-address"
        );
        // @-prefixed broadcast: callers passing "@all" must be treated as a
        // broadcast, not a self-address, after the leading-@ strip.
        assert!(
            !crate::signal::is_self_address(&["legion".to_string()], "@all"),
            "@all with leading @ must be treated as a broadcast"
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

    // -- retire-on-reply (#919) --------------------------------------------

    /// Pending reply-required asks visible to `author`, as
    /// `pending-replies` would render them.
    fn rendered(database: &db::Database, author: &str) -> Vec<String> {
        watch::find_pending_signals(database, author, &[author.to_string()], None)
            .expect("find pending")
            .into_iter()
            .filter(|(_, text, _)| watch::signal_requires_reply(text))
            .map(|(id, _, _)| id)
            .collect()
    }

    /// The exact failure this change closes: six asks from one agent, all
    /// answered by one reply, previously re-rendered forever because nothing
    /// on the reply path ever wrote `watch_handled`.
    #[test]
    fn replying_retires_every_ask_from_that_sender() {
        let (database, _index, _dir) = crate::testutil::test_storage();
        for _ in 0..6 {
            database
                .insert_reflection("smugglr", "@legion question:help -- blast radius", "team")
                .expect("insert ask");
        }
        assert_eq!(rendered(&database, "legion").len(), 6, "all six pending");

        let n = retire_answered_for_author(&database, "legion", &["legion".to_string()], "smugglr")
            .expect("retire");

        assert_eq!(n, 6, "one reply retires the whole thread from that sender");
        assert!(
            rendered(&database, "legion").is_empty(),
            "answered asks must not re-surface at next session start"
        );
    }

    /// Scoping guard: replying to one agent must not silently drop another
    /// agent's unanswered ask.
    #[test]
    fn replying_does_not_retire_a_different_senders_ask() {
        let (database, _index, _dir) = crate::testutil::test_storage();
        database
            .insert_reflection("smugglr", "@legion question:help -- mine", "team")
            .expect("insert smugglr ask");
        database
            .insert_reflection("rafters", "@legion question:help -- theirs", "team")
            .expect("insert rafters ask");

        let n = retire_answered_for_author(&database, "legion", &["legion".to_string()], "smugglr")
            .expect("retire");

        assert_eq!(n, 1, "only the replied-to sender's ask is retired");
        let left = rendered(&database, "legion");
        assert_eq!(left.len(), 1, "rafters' ask must survive");
    }

    /// Addressing the room is not a reply to anyone, so it must retire
    /// nothing -- otherwise one broadcast clears every unanswered thread.
    #[test]
    fn broadcast_reply_retires_nothing() {
        let (database, _index, _dir) = crate::testutil::test_storage();
        database
            .insert_reflection("smugglr", "@legion question:help -- unanswered", "team")
            .expect("insert ask");

        for addr in ["all", "@all", "everyone", "@everyone"] {
            let n = retire_answered_for_author(&database, "legion", &["legion".to_string()], addr)
                .expect("retire");
            assert_eq!(n, 0, "broadcast address {addr} must retire nothing");
        }
        assert_eq!(
            rendered(&database, "legion").len(),
            1,
            "the unanswered ask must survive every broadcast form"
        );
    }

    /// A leading `@` on the recipient is decoration the send path tolerates,
    /// so the retire path must match it or replies to "@smugglr" silently
    /// stop clearing the queue.
    #[test]
    fn recipient_at_prefix_and_case_are_tolerated() {
        for addr in ["@smugglr", "SMUGGLR", "Smugglr"] {
            let (database, _index, _dir) = crate::testutil::test_storage();
            database
                .insert_reflection("smugglr", "@legion question:help -- ask", "team")
                .expect("insert ask");

            let n = retire_answered_for_author(&database, "legion", &["legion".to_string()], addr)
                .expect("retire");
            assert_eq!(n, 1, "recipient form {addr} must match sender 'smugglr'");
        }
    }

    /// Informational signals never reach the wake banner, so they are not the
    /// reported pain and must be left alone -- the watch poll owns retiring
    /// those (src/watch/gates.rs).
    #[test]
    fn informational_signals_are_left_for_the_watch_path() {
        let (database, _index, _dir) = crate::testutil::test_storage();
        database
            .insert_reflection("smugglr", "@legion announce -- shipped 0.5.0", "team")
            .expect("insert announce");

        let n = retire_answered_for_author(&database, "legion", &["legion".to_string()], "smugglr")
            .expect("retire");

        assert_eq!(n, 0, "informational signals are not retired on reply");
    }

    // -- send-time resolves stamping (#949) ---------------------------------

    /// The composition `handle_signal` performs for a `--verb answer` send,
    /// with the addressable name set injected (the production path reaches
    /// it through `wake_names_for`, which needs a watch.toml on disk).
    fn compose_answer(
        database: &db::Database,
        author: &str,
        to: &str,
        details: Option<&str>,
    ) -> String {
        let ask_id: Option<String> =
            matching_pending_ask_ids(database, author, &[author.to_string()], to)
                .expect("match pending asks")
                .into_iter()
                .next();
        let stamped: Option<String> = stamp_resolves(details, ask_id.as_deref());
        signal::compose(
            to,
            "answer",
            Some("resolved"),
            Some("here you go"),
            stamped.as_deref(),
            verbs::active_manifest(),
        )
        .expect("compose")
    }

    /// Read the `resolves` value back off a composed signal, through the same
    /// parser the wake gate uses -- asserting on the wire text alone would
    /// not catch a duplicate-key ordering that parses the wrong way.
    fn parsed_resolves(text: &str) -> Option<String> {
        crate::signal::parse_signal(text).and_then(|sig| sig.details.get("resolves").cloned())
    }

    /// The send-time stamp is the whole mechanism: #919's retire (which runs
    /// later in this same invocation) destroys the pending-ask evidence, so
    /// a watch daemon polling afterwards can only learn the fact if it rides
    /// on the signal.
    #[test]
    fn answer_with_pending_ask_stamps_resolves_detail() {
        let (database, _index, _dir) = crate::testutil::test_storage();
        let ask = database
            .insert_reflection("veneer", "@rafters question:help -- need X", "team")
            .expect("insert ask");

        let text = compose_answer(&database, "rafters", "veneer", None);

        assert_eq!(
            parsed_resolves(&text).as_deref(),
            Some(ask.id.as_str()),
            "the outgoing answer must name the ask it resolves: {text}"
        );
        assert!(
            watch::resolves_pending_ask(&text),
            "the composed text must satisfy the watch-side predicate: {text}"
        );
    }

    /// A fire-and-forget answer to an agent with no tracked ask must compose
    /// exactly as it does today -- no marker, and therefore no wake.
    #[test]
    fn answer_with_no_pending_ask_composes_unchanged() {
        let (database, _index, _dir) = crate::testutil::test_storage();

        let text = compose_answer(&database, "rafters", "veneer", None);

        let unchanged = signal::compose(
            "veneer",
            "answer",
            Some("resolved"),
            Some("here you go"),
            None,
            verbs::active_manifest(),
        )
        .expect("compose baseline");
        assert_eq!(
            text, unchanged,
            "with nothing pending the composed signal must be byte-for-byte today's"
        );
        assert!(!watch::resolves_pending_ask(&text));
    }

    /// The stamp merges into the sender's own `--details`, and wins over a
    /// hand-typed `resolves` key. Ordering is what enforces that (the stamp
    /// is appended last, and the braced block parses into a HashMap), so it
    /// is pinned here rather than left to a `format_signal` refactor.
    #[test]
    fn answer_stamps_resolves_alongside_user_supplied_details() {
        let (database, _index, _dir) = crate::testutil::test_storage();
        let ask = database
            .insert_reflection("veneer", "@rafters question:help -- need X", "team")
            .expect("insert ask");

        let text = compose_answer(&database, "rafters", "veneer", Some("pr: 949"));
        assert!(
            text.contains("pr: 949"),
            "the sender's own details must survive the stamp: {text}"
        );
        assert_eq!(parsed_resolves(&text).as_deref(), Some(ask.id.as_str()));

        let forged = compose_answer(
            &database,
            "rafters",
            "veneer",
            Some("resolves: 01a0-forged-id"),
        );
        assert_eq!(
            parsed_resolves(&forged).as_deref(),
            Some(ask.id.as_str()),
            "a hand-typed resolves key must not shadow the computed one: {forged}"
        );
    }

    /// Retiring is host-local. `watch_handled` is keyed (signal_id,
    /// repo_name) and is not on the sync wire, so clearing legion's inbox
    /// must not clear another repo's copy of the same broadcast.
    #[test]
    fn retiring_is_per_repo_and_does_not_touch_a_peers_queue() {
        let (database, _index, _dir) = crate::testutil::test_storage();
        database
            .insert_reflection("smugglr", "@all question:help -- who owns this", "team")
            .expect("insert broadcast ask");

        let n = retire_answered_for_author(&database, "legion", &["legion".to_string()], "smugglr")
            .expect("retire");
        assert_eq!(n, 1, "legion's own copy is retired");

        assert!(
            rendered(&database, "legion").is_empty(),
            "legion no longer sees it"
        );
        assert_eq!(
            rendered(&database, "rafters").len(),
            1,
            "rafters' copy of the same broadcast must be untouched"
        );
    }
}
