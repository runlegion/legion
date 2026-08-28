//! CLI surface for the hook-side delivery drain (#941): `legion deliver
//! drain`. Domain logic lives in `crate::deliver`; this module is the thin
//! clap wiring + stdout formatting layer, following the `TaskAction` /
//! `handle_task` convention in `cli/misc.rs`.

use std::io::Write;

use chrono::Utc;
use clap::Subcommand;

use crate::cli::util::open_db;
use crate::db::Reflection;
use crate::{board, deliver, error, telemetry};

#[derive(Subcommand)]
pub(crate) enum DeliverAction {
    /// Drain undelivered bullpen posts/signals for the hook-side delivery
    /// lane
    Drain {
        /// Repository name (the hook-drain cursor's reader identity)
        #[arg(long)]
        repo: String,

        /// Print musings first, then a separator line, then the directed
        /// (REQUIRES A REPLY) set -- so `delivery-drain.sh` can build its
        /// result block without parsing posts to sort them (#1020).
        #[arg(long)]
        split: bool,
    },
}

pub(crate) fn handle(action: DeliverAction) -> error::Result<()> {
    match action {
        DeliverAction::Drain { repo, split } => handle_deliver_drain(repo, split)?,
    }
    Ok(())
}

/// `legion deliver drain --repo <REPO> [--split]`. This is the command
/// `plugin/hooks/delivery-drain.sh` shells out to, the same way
/// `identity-chain-load.sh` shells out to `legion chain --id`.
///
/// Without `--split`, prints `board::format_bullpen` of the drained posts.
/// With `--split` (#1020), prints musings then a separator then the
/// directed set via `emit_drained_split` -- see that function.  Either way,
/// nothing is printed when there is nothing new.
///
/// Telemetry ordering (#941 review): the `lane = "hook"` `DeliveryRecord`
/// rows are written only AFTER the drained text has been printed and
/// flushed -- emission first, record second, so a failed write yields no
/// row. This mirrors the MCP lane's recording point (after `write_ok`),
/// making the two lanes' rows comparable: each asserts "the bytes left the
/// last stage this process controls," and neither can see the harness-side
/// tail beyond it. `--split` records the same rows for the same reason:
/// every drained post is emitted either way, just into a different bucket.
pub(crate) fn handle_deliver_drain(repo: String, split: bool) -> error::Result<()> {
    let database = open_db()?;
    let posts = deliver::drain_for_hook(&database, &repo)?;
    if split {
        emit_drained_split(&repo, &posts, &mut std::io::stdout())?;
    } else {
        emit_drained(&posts, &mut std::io::stdout())?;
    }
    record_hook_telemetry(&repo, &posts);
    Ok(())
}

/// Write the drained posts to `out` and flush. Nothing is written for an
/// empty batch. On any write/flush error the caller propagates and must
/// NOT record telemetry -- an unemitted post keeps no delivery row (its
/// cursor claim stands; the post remains readable via `legion bullpen`).
fn emit_drained(posts: &[Reflection], out: &mut impl Write) -> error::Result<()> {
    if posts.is_empty() {
        return Ok(());
    }
    write!(out, "{}", board::format_bullpen(posts))?;
    out.flush()?;
    Ok(())
}

/// Fixed line separating the musings section from the directed section in
/// `--split` output. Present only when both sections render -- there is
/// nothing to separate when either is empty, and a leading or trailing
/// separator with nothing on one side would misread as its own section.
const SPLIT_SEPARATOR: &str = "---";

/// Write `posts` split into musings then a separator then the directed
/// (REQUIRES A REPLY) set -- `legion deliver drain --split` (#1020).
///
/// `posts` is split via `deliver::split_drained`; musings render through
/// `board::format_bullpen` (the existing lighter header), and the directed
/// set renders through `board::format_pending_replies` -- the same
/// formatter `legion pending-replies` uses, so this bucket's text can
/// never drift from what boot/post-compact would show for the same
/// signal. Nothing is written for an empty batch.
fn emit_drained_split(repo: &str, posts: &[Reflection], out: &mut impl Write) -> error::Result<()> {
    let (musings, directed) = deliver::split_drained(posts);

    let musings_txt = board::format_bullpen(&musings);
    let directed_tuples: Vec<(String, String, String)> = directed
        .iter()
        .map(|r| (r.id.clone(), r.text.clone(), r.repo.clone()))
        .collect();
    let directed_txt = board::format_pending_replies(repo, &directed_tuples);

    if musings_txt.is_empty() && directed_txt.is_empty() {
        return Ok(());
    }

    if !musings_txt.is_empty() {
        write!(out, "{musings_txt}")?;
    }
    if !musings_txt.is_empty() && !directed_txt.is_empty() {
        writeln!(out, "{SPLIT_SEPARATOR}")?;
    }
    if !directed_txt.is_empty() {
        write!(out, "{directed_txt}")?;
    }
    out.flush()?;
    Ok(())
}

/// Append one `lane = "hook"` row per emitted post. Best-effort: a
/// telemetry write failure is logged, never propagated -- the delivery
/// already happened.
fn record_hook_telemetry(repo: &str, posts: &[Reflection]) {
    for post in posts {
        let record = telemetry::DeliveryRecord {
            ts: Utc::now().to_rfc3339(),
            lane: telemetry::DeliveryLane::Hook,
            repo: repo.to_string(),
            reflection_id: post.id.clone(),
        };
        if let Err(e) = telemetry::append_delivery(&record) {
            eprintln!(
                "[legion deliver] telemetry write failed for post {}: {e}",
                post.id
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    /// A sink that refuses every write -- shared by the emit_drained and
    /// emit_drained_split write-failure tests below.
    struct FailingSink;
    impl Write for FailingSink {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("sink refused"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("sink refused"))
        }
    }

    #[test]
    fn emit_drained_prints_seeded_post_then_empty_on_rerun() {
        let db = test_db();

        // Prime past cold start: the first-ever drain against a nonempty
        // board seeds from the current watermark rather than replaying
        // history, so it delivers nothing.
        db.insert_reflection("seed", "sentinel", "team").unwrap();
        let mut out = Vec::new();
        emit_drained(&deliver::drain_for_hook(&db, "legion").unwrap(), &mut out).unwrap();
        assert!(out.is_empty(), "cold-start drain must emit nothing");

        db.insert_reflection("rafters", "hello team", "team")
            .unwrap();

        let mut out = Vec::new();
        emit_drained(&deliver::drain_for_hook(&db, "legion").unwrap(), &mut out).unwrap();
        let first = String::from_utf8(out).unwrap();
        assert!(
            first.contains("hello team"),
            "expected seeded post text in output, got: {first}"
        );

        let mut out = Vec::new();
        emit_drained(&deliver::drain_for_hook(&db, "legion").unwrap(), &mut out).unwrap();
        assert!(out.is_empty(), "expected empty output on immediate rerun");
    }

    #[test]
    fn emit_drained_failure_precedes_telemetry_recording() {
        // emit_drained must propagate a refused write, and
        // handle_deliver_drain's ordering (emit before
        // record_hook_telemetry) means no DeliveryRecord is written for an
        // unemitted post. The ordering itself is structural -- this test
        // pins the propagation half.
        let db = test_db();
        db.insert_reflection("seed", "sentinel", "team").unwrap();
        assert!(deliver::drain_for_hook(&db, "legion").unwrap().is_empty());
        db.insert_reflection("rafters", "doomed post", "team")
            .unwrap();

        let posts = deliver::drain_for_hook(&db, "legion").unwrap();
        assert_eq!(posts.len(), 1);
        assert!(
            emit_drained(&posts, &mut FailingSink).is_err(),
            "a refused write must propagate, not be swallowed"
        );
    }

    #[test]
    fn emit_drained_split_puts_the_directed_set_after_the_musings_with_a_separator() {
        let db = test_db();
        db.insert_reflection("seed", "sentinel", "team").unwrap();
        assert!(deliver::drain_for_hook(&db, "legion").unwrap().is_empty());

        db.insert_reflection("rafters", "just a musing for the team", "team")
            .unwrap();
        db.insert_reflection("kelex", "@legion question: which lane owns retries", "team")
            .unwrap();

        let posts = deliver::drain_for_hook(&db, "legion").unwrap();
        assert_eq!(posts.len(), 2);

        let mut out = Vec::new();
        emit_drained_split("legion", &posts, &mut out).unwrap();
        let rendered = String::from_utf8(out).unwrap();

        let musing_pos = rendered
            .find("just a musing for the team")
            .expect("musing text present");
        let separator_pos = rendered.find(SPLIT_SEPARATOR).expect("separator present");
        let directed_pos = rendered
            .find("REQUIRES A REPLY")
            .expect("directed section present");
        assert!(
            musing_pos < separator_pos && separator_pos < directed_pos,
            "expected musing, then separator, then the directed set; got:\n{rendered}"
        );
        assert!(
            rendered.contains("which lane owns retries"),
            "directed entry must carry the signal text verbatim, got:\n{rendered}"
        );
    }

    #[test]
    fn emit_drained_split_omits_the_separator_when_only_one_bucket_is_nonempty() {
        let db = test_db();
        db.insert_reflection("seed", "sentinel", "team").unwrap();
        assert!(deliver::drain_for_hook(&db, "legion").unwrap().is_empty());

        db.insert_reflection("rafters", "just a musing for the team", "team")
            .unwrap();
        let posts = deliver::drain_for_hook(&db, "legion").unwrap();

        let mut out = Vec::new();
        emit_drained_split("legion", &posts, &mut out).unwrap();
        let rendered = String::from_utf8(out).unwrap();

        assert!(rendered.contains("just a musing for the team"));
        assert!(
            !rendered.contains(SPLIT_SEPARATOR),
            "no separator when there is nothing on the other side, got:\n{rendered}"
        );
        assert!(!rendered.contains("REQUIRES A REPLY"));
    }

    #[test]
    fn emit_drained_split_empty_batch_emits_nothing() {
        let mut out = Vec::new();
        emit_drained_split("legion", &[], &mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn emit_drained_split_directed_only_batch_renders_no_musings_header() {
        let db = test_db();
        db.insert_reflection("seed", "sentinel", "team").unwrap();
        assert!(deliver::drain_for_hook(&db, "legion").unwrap().is_empty());

        db.insert_reflection("kelex", "@legion question: which lane owns retries", "team")
            .unwrap();
        let posts = deliver::drain_for_hook(&db, "legion").unwrap();
        assert_eq!(posts.len(), 1);

        let mut out = Vec::new();
        emit_drained_split("legion", &posts, &mut out).unwrap();
        let rendered = String::from_utf8(out).unwrap();

        assert!(
            rendered.contains("REQUIRES A REPLY"),
            "the directed set must still render with no musings present, got:\n{rendered}"
        );
        assert!(
            !rendered.contains("[Legion] Bullpen ("),
            "the musings header must not appear when there are no musings, got:\n{rendered}"
        );
        assert!(
            !rendered.contains(SPLIT_SEPARATOR),
            "no separator when there is nothing on the other side, got:\n{rendered}"
        );
    }

    #[test]
    fn emit_drained_split_failure_propagates() {
        let db = test_db();
        db.insert_reflection("seed", "sentinel", "team").unwrap();
        assert!(deliver::drain_for_hook(&db, "legion").unwrap().is_empty());
        db.insert_reflection("rafters", "doomed musing", "team")
            .unwrap();

        let posts = deliver::drain_for_hook(&db, "legion").unwrap();
        assert_eq!(posts.len(), 1);
        assert!(
            emit_drained_split("legion", &posts, &mut FailingSink).is_err(),
            "a refused write must propagate, not be swallowed"
        );
    }

    /// #1020 review (correcting the original board.rs placement of this
    /// test): `legion pending-replies` and the hook drain's `--split`
    /// directed bucket must render the SAME signal identically. This
    /// drives BOTH real production call paths -- `cli::signal::
    /// pending_reply_signals` (what `handle_pending_replies` calls) and
    /// the actual private `emit_drained_split` (what the hook shells out
    /// to via `--split`) -- rather than re-deriving either path's steps,
    /// so a regression in either caller's wiring, not just in
    /// `format_pending_replies` itself, would fail this test.
    #[test]
    fn directed_bucket_is_byte_identical_to_pending_replies_for_the_same_signal() {
        use crate::cli::signal::pending_reply_signals;

        let db = test_db();
        db.insert_reflection("seed", "sentinel", "team").unwrap();
        assert!(deliver::drain_for_hook(&db, "legion").unwrap().is_empty());

        db.insert_reflection(
            "rafters",
            "@legion question: which lane owns retries",
            "team",
        )
        .unwrap();

        // Path A: legion pending-replies's own query and formatting.
        let reply_required = pending_reply_signals(&db, "legion", false).unwrap();
        let pending_replies_output = board::format_pending_replies("legion", &reply_required);
        assert!(
            !pending_replies_output.is_empty(),
            "expected a non-empty REQUIRES A REPLY block from the pending-replies path"
        );

        // Path B: the hook drain's --split directed bucket, via the real
        // (private) emit_drained_split.
        let drained = deliver::drain_for_hook(&db, "legion").unwrap();
        let mut out = Vec::new();
        emit_drained_split("legion", &drained, &mut out).unwrap();
        let split_output = String::from_utf8(out).unwrap();

        assert!(
            split_output.contains(&pending_replies_output),
            "the directed section of --split output must equal legion pending-replies' \
             rendering for the same signal verbatim; split output was:\n{split_output}\n\
             expected to find:\n{pending_replies_output}"
        );
    }
}
