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
    },
}

pub(crate) fn handle(action: DeliverAction) -> error::Result<()> {
    match action {
        DeliverAction::Drain { repo } => handle_deliver_drain(repo)?,
    }
    Ok(())
}

/// `legion deliver drain --repo <REPO>`. Prints `board::format_bullpen` of
/// the drained posts to stdout; prints nothing when there is nothing new.
/// This is the command `plugin/hooks/delivery-drain.sh` shells out to, the
/// same way `identity-chain-load.sh` shells out to `legion chain --id`.
///
/// Telemetry ordering (#941 review): the `lane = "hook"` `DeliveryRecord`
/// rows are written only AFTER the drained text has been printed and
/// flushed -- emission first, record second, so a failed write yields no
/// row. This mirrors the MCP lane's recording point (after `write_ok`),
/// making the two lanes' rows comparable: each asserts "the bytes left the
/// last stage this process controls," and neither can see the harness-side
/// tail beyond it.
pub(crate) fn handle_deliver_drain(repo: String) -> error::Result<()> {
    let database = open_db()?;
    let posts = deliver::drain_for_hook(&database, &repo)?;
    emit_drained(&posts, &mut std::io::stdout())?;
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
        // A sink that refuses every write: emit_drained must propagate the
        // error, and handle_deliver_drain's ordering (emit before
        // record_hook_telemetry) means no DeliveryRecord is written for an
        // unemitted post. The ordering itself is structural -- this test
        // pins the propagation half.
        struct FailingSink;
        impl Write for FailingSink {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("sink refused"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("sink refused"))
            }
        }

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
}
