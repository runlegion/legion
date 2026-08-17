//! Hook-side delivery drain (#941): the live-session delivery lane. Gives
//! interactive sessions the same no-inference-roundtrip delivery path the
//! watch daemon already has for sleeping/spawned agents, driven by plugin
//! hook events. It ran alongside the MCP subprocess's
//! `notifications/claude/channel` push through a dual-lane parity window
//! and became the sole live-session lane when that push was retired
//! (#947). Reuses the `board_reads` cursor primitives that lane also used
//! -- no new table.
//!
//! The hook lane's cursor row is keyed by a distinct `reader_repo` string
//! (`hook_reader_key`) so it cannot collide with manual `legion bullpen`'s
//! cursor (`Database::mark_board_read`, keyed by the plain repo name).
//! Independent cursor rows on the same table are what the parity
//! comparison needed -- each lane had to observe every eligible post on
//! its own rather than race the others for one shared cursor -- and the
//! separation still earns its keep: a hook drain must not move the unread
//! count that manual `bullpen` drives.
//!
//! Reusing `mark_board_read`/`get_and_mark_unread_board_posts` directly
//! was considered and rejected: those also drive `archive_read_posts`'s
//! "all known readers have read" gate, so a hook drain that touched them
//! would let an agent who only ever receives posts via hooks silently
//! satisfy archival without anyone running `legion bullpen` by hand.

use crate::db::{Database, HOOK_DRAIN_CURSOR_SUFFIX, Reflection};
use crate::error::Result;
use crate::mcp::notifier;

/// Batch size cap for a single drain call. Half the 100-row cap the
/// retired MCP notifier used: drains fire per hook event, far more often
/// than that lane's poll ticks, so each call can afford a smaller bite.
/// Overflow is safe -- anything beyond the cap is picked up on the next
/// drain because the cursor advances to the last fetched row.
const DRAIN_BATCH_LIMIT: usize = 50;

/// Cursor key the hook-drain lane writes to `board_reads`, namespaced
/// apart from the plain `repo` key manual `legion bullpen` uses (and the
/// retired MCP notifier used). Built from `db::HOOK_DRAIN_CURSOR_SUFFIX`, the
/// same constant `archive_read_posts` uses to exclude these rows from its
/// aggregate -- the key scheme and the exclusion cannot drift apart.
pub fn hook_reader_key(repo: &str) -> String {
    format!("{repo}{HOOK_DRAIN_CURSOR_SUFFIX}")
}

/// Claim this repo's undrained bullpen posts/signals via the hook-drain
/// cursor. The claim -- cursor read, batch fetch, cursor advance, and
/// cold-start watermark seed -- is one atomic operation
/// (`Database::claim_board_posts_for_reader`, an IMMEDIATE transaction),
/// so two concurrent sessions on the same repo cannot double-deliver a
/// post; see that method's docs for the loser's two safe outcomes.
///
/// Applies the delivery decision `notifier::should_notify(text, repo,
/// Some(repo))` -- the filter the retired MCP push lane was also judged
/// against, kept as the single notion of "should this post reach this
/// agent" rather than re-derived for the hook lane.
///
/// This function records NO telemetry: a claimed post is drained, not yet
/// delivered. The `lane = "hook"` `DeliveryRecord` is written by the CLI
/// handler (`cli::deliver`) only after the drained text has been printed
/// and flushed -- the last stage this process controls. What it cannot
/// verify is the harness-side tail (the additionalContext injection);
/// that residual asymmetry is documented on
/// `telemetry::DeliveryRecord`.
pub fn drain_for_hook(db: &Database, repo: &str) -> Result<Vec<Reflection>> {
    let batch = db.claim_board_posts_for_reader(&hook_reader_key(repo), DRAIN_BATCH_LIMIT)?;

    Ok(batch
        .into_iter()
        .filter(|post| notifier::should_notify(&post.text, &post.repo, Some(repo)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    #[test]
    fn drain_for_hook_delivers_each_post_exactly_once_across_two_calls() {
        let db = test_db();

        // Prime past cold start: a fresh cursor seeds from the current
        // watermark, not full history (see
        // drain_for_hook_cold_start_seeds_from_watermark_not_full_history),
        // so the first-ever call against a nonempty board delivers nothing.
        db.insert_reflection("seed", "sentinel", "team").unwrap();
        assert!(drain_for_hook(&db, "legion").unwrap().is_empty());

        db.insert_reflection("rafters", "musing one", "team")
            .unwrap();
        db.insert_reflection("kelex", "musing two", "team").unwrap();

        let first = drain_for_hook(&db, "legion").unwrap();
        assert_eq!(first.len(), 2);

        let second = drain_for_hook(&db, "legion").unwrap();
        assert!(second.is_empty(), "expected empty on second call");

        db.insert_reflection("rafters", "musing three", "team")
            .unwrap();
        let third = drain_for_hook(&db, "legion").unwrap();
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].text, "musing three");
    }

    #[test]
    fn drain_for_hook_leaves_notifier_and_manual_bullpen_cursors_untouched() {
        let db = test_db();
        db.insert_reflection("rafters", "old post", "team").unwrap();

        // Seed the plain "legion" cursor the way manual `legion bullpen`
        // (mark_board_read) would -- the retired MCP notifier wrote the
        // same plain-repo-keyed row in board_reads, which is why the hook
        // lane was namespaced away from it in the first place.
        db.mark_board_read("legion").unwrap();
        let plain_cursor_before = db.get_board_read_cursor("legion").unwrap();
        assert_eq!(db.get_unread_count("legion").unwrap(), 0);

        // Prime the hook lane past cold start (it swallows whatever is
        // already on the board at its first-ever call).
        assert!(drain_for_hook(&db, "legion").unwrap().is_empty());

        db.insert_reflection("kelex", "new post", "team").unwrap();
        assert_eq!(db.get_unread_count("legion").unwrap(), 1);

        let drained = drain_for_hook(&db, "legion").unwrap();
        assert_eq!(drained.len(), 1);

        // The hook drain must not have moved the plain-repo cursor or the
        // unread count it drives.
        assert_eq!(
            db.get_board_read_cursor("legion").unwrap(),
            plain_cursor_before
        );
        assert_eq!(db.get_unread_count("legion").unwrap(), 1);

        // But the hook-drain's own cursor row DID advance.
        assert!(
            db.get_board_read_cursor(&hook_reader_key("legion"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn drain_for_hook_cursor_does_not_affect_archive_read_posts() {
        // archive_read_posts's "all known readers have read" gate takes
        // MIN(last_read_at) over every row in board_reads, with no filter
        // on reader_repo (db/board.rs). A hook-drain cursor row must not
        // participate in that aggregate: an empty/cold cursor would drag
        // the MIN down to "" and stop archival entirely, and any hook-drain
        // row present would only ever make archival more conservative than
        // pre-#941 behavior. Two identically-seeded boards -- one drained
        // via the hook lane, one not -- must archive the same count.
        let without_drain = test_db();
        without_drain
            .insert_reflection("rafters", "old post", "team")
            .unwrap();
        without_drain.mark_board_read("legion").unwrap();
        let archived_without_drain = without_drain.archive_read_posts().unwrap();
        assert_eq!(archived_without_drain, 1);

        let with_drain = test_db();
        with_drain
            .insert_reflection("rafters", "old post", "team")
            .unwrap();
        with_drain.mark_board_read("legion").unwrap();
        // Cold-start hook drain -- persists a cursor row for
        // hook_reader_key("legion") on an otherwise-empty board_reads
        // history for this key.
        assert!(drain_for_hook(&with_drain, "legion").unwrap().is_empty());
        let archived_with_drain = with_drain.archive_read_posts().unwrap();
        assert_eq!(
            archived_with_drain, archived_without_drain,
            "a hook-drain cursor row must not change archive_read_posts's count"
        );
    }

    #[test]
    fn drain_for_hook_cold_start_seeds_from_watermark_not_full_history() {
        let db = test_db();
        // Old history that predates the hook lane's first drain -- must
        // NOT be replayed.
        db.insert_reflection("rafters", "ancient musing", "team")
            .unwrap();
        db.insert_reflection("kelex", "another ancient musing", "team")
            .unwrap();

        // First drain call: no cursor row yet, seeds from the watermark
        // (the current tail), so nothing from before this call is
        // delivered.
        let first = drain_for_hook(&db, "legion").unwrap();
        assert!(first.is_empty(), "cold start must not replay full history");

        // A post created after the cold-start seed IS delivered.
        db.insert_reflection("rafters", "fresh musing", "team")
            .unwrap();
        let second = drain_for_hook(&db, "legion").unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].text, "fresh musing");
    }

    #[test]
    fn drain_for_hook_claims_each_post_once_across_concurrent_sessions() {
        // Regression for the #941 review race: two live sessions on the
        // same repo share one hook-drain cursor row, and the pre-fix
        // read-fetch-advance sequence let both deliver the same post. The
        // IMMEDIATE transaction in claim_board_posts_for_reader serializes
        // the claim: the loser either sees the advanced cursor (empty) or
        // errors with SQLITE_BUSY (counted as zero here -- it retries on
        // its next debounced call). Either way the total delivered across
        // both racers must be exactly one.
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.db");

        let seed_db = Database::open(&path).unwrap();
        seed_db
            .insert_reflection("seed", "sentinel", "team")
            .unwrap();
        assert!(drain_for_hook(&seed_db, "legion").unwrap().is_empty());
        seed_db
            .insert_reflection("rafters", "raced post", "team")
            .unwrap();
        drop(seed_db);

        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let db = Database::open(&path).unwrap();
                    barrier.wait();
                    drain_for_hook(&db, "legion").map(|p| p.len()).unwrap_or(0)
                })
            })
            .collect();

        let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(total, 1, "a raced post must be claimed exactly once");
    }

    #[test]
    fn drain_for_hook_advances_past_suppressed_rows_without_redelivering() {
        let db = test_db();

        // Prime past cold start.
        db.insert_reflection("seed", "sentinel", "team").unwrap();
        assert!(drain_for_hook(&db, "legion").unwrap().is_empty());

        // A post FROM "legion" itself -- should_notify suppresses
        // own-repo musings (no `@` prefix).
        db.insert_reflection("legion", "self musing", "team")
            .unwrap();
        // A signal addressed to a different recipient -- suppressed.
        db.insert_reflection("rafters", "@kelex please review", "team")
            .unwrap();
        // A deliverable musing from a different repo.
        db.insert_reflection("rafters", "team musing", "team")
            .unwrap();

        let first = drain_for_hook(&db, "legion").unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].text, "team musing");

        // The cursor advanced past ALL three rows (not just the delivered
        // one) -- a second call with no new posts returns nothing,
        // proving the suppressed rows were not left behind for re-scan.
        let second = drain_for_hook(&db, "legion").unwrap();
        assert!(second.is_empty());
    }
}
