//! Hook-side delivery drain (#941): the code-side counterpart to the MCP
//! notification lane's push. Gives interactive sessions the same
//! no-inference-roundtrip delivery path the watch daemon already has for
//! sleeping/spawned agents, driven by hooks instead of the MCP
//! subprocess's `notifications/claude/channel` push. Reuses the exact
//! `board_reads` cursor primitives the MCP notifier already uses -- no
//! new table.
//!
//! The hook lane's cursor row is keyed by a distinct `reader_repo` string
//! (`hook_reader_key`) so it cannot collide with the MCP notifier's
//! cursor (`mcp::notifier`, keyed by the plain repo name) or with manual
//! `legion bullpen`'s cursor (`Database::mark_board_read`, same
//! plain-repo key). Three lanes, three independent cursor rows on the
//! same table -- exactly what a dual-lane parity comparison needs: MCP
//! and hook must each independently observe every eligible post, not
//! race each other for one shared cursor.
//!
//! Reusing `mark_board_read`/`get_and_mark_unread_board_posts` directly
//! was considered and rejected: those also drive `archive_read_posts`'s
//! "all known readers have read" gate, so a hook drain that touched them
//! would let an agent who only ever receives posts via hooks silently
//! satisfy archival without anyone running `legion bullpen` by hand.

use chrono::Utc;

use crate::db::{Database, Reflection};
use crate::error::Result;
use crate::mcp::notifier;
use crate::telemetry;

/// Batch size cap for a single drain call. Mirrors `NOTIFIER_BATCH_LIMIT`
/// (mcp/notifier.rs) -- anything beyond the cap is picked up on the next
/// drain because the cursor advances to the last fetched row.
const DRAIN_BATCH_LIMIT: usize = 50;

/// Cursor key the hook-drain lane writes to `board_reads`, namespaced
/// apart from the plain `repo` key the MCP notifier and manual `legion
/// bullpen` already use.
pub fn hook_reader_key(repo: &str) -> String {
    format!("{repo}::hook-drain")
}

/// Fetch this repo's undelivered bullpen posts/signals via the hook-drain
/// cursor and advance the cursor past the fetched batch in the same call.
///
/// Applies the same delivery decision the MCP lane applies --
/// `notifier::should_notify(text, repo, Some(repo))` -- so the two lanes
/// are judged against an identical filter, not two different notions of
/// "should this post reach this agent."
///
/// The cursor advances unconditionally to the last row's `(created_at,
/// id)` in the fetched batch regardless of how many rows `should_notify`
/// kept, mirroring the MCP notifier's "must happen unconditionally or a
/// suppressed post is re-scanned forever" invariant.
///
/// Cold start (no `board_reads` row for `hook_reader_key(repo)` yet)
/// seeds from `Database::get_board_cursor_watermark` -- the current tail
/// of the board -- rather than replaying full history, mirroring the MCP
/// notifier's unknown-client cursor seed. The seed is persisted
/// immediately, before the first `get_board_posts_since` call: each
/// `drain_for_hook` invocation is a fresh, stateless CLI process, so
/// unlike the long-lived MCP notifier thread (which keeps its seed in a
/// local variable across poll ticks) there is nothing to carry the seed
/// forward in memory. Leaving the cold-start row unwritten would mean a
/// call against a nonempty board -- whose fetched batch is legitimately
/// empty, because the seeded watermark strictly excludes itself -- would
/// recompute the same cold-start seed on every subsequent call, sliding
/// forward with the watermark and never catching up.
///
/// Every reflection returned here appends one `telemetry::DeliveryRecord`
/// tagged `lane = "hook"`. Best-effort: a telemetry write failure is
/// logged, never propagated, and never blocks delivery.
pub fn drain_for_hook(db: &Database, repo: &str) -> Result<Vec<Reflection>> {
    let key = hook_reader_key(repo);

    let (since_at, since_id) = match db.get_board_read_cursor(&key)? {
        Some(cursor) => cursor,
        None => {
            let seed = db
                .get_board_cursor_watermark()?
                .unwrap_or_else(|| (String::new(), String::new()));
            db.advance_board_read_cursor(&key, &seed.0, &seed.1)?;
            seed
        }
    };

    let batch = db.get_board_posts_since(&since_at, &since_id, DRAIN_BATCH_LIMIT)?;

    // Advance unconditionally past the whole fetched batch -- a suppressed
    // row (self-post, signal to a different recipient) must not be
    // re-scanned on the next call.
    if let Some(last) = batch.last() {
        db.advance_board_read_cursor(&key, &last.created_at, &last.id)?;
    }

    let delivered: Vec<Reflection> = batch
        .into_iter()
        .filter(|post| notifier::should_notify(&post.text, &post.repo, Some(repo)))
        .collect();

    for post in &delivered {
        let record = telemetry::DeliveryRecord {
            ts: Utc::now().to_rfc3339(),
            lane: "hook".to_string(),
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

    Ok(delivered)
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

        // Seed the plain "legion" cursor the way both the MCP notifier
        // (advance_board_read_cursor) and manual `legion bullpen`
        // (mark_board_read) would -- both write to the same
        // plain-repo-keyed row in board_reads.
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
