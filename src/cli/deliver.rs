//! CLI surface for the hook-side delivery drain (#941): `legion deliver
//! drain`. Domain logic lives in `crate::deliver`; this module is the thin
//! clap wiring + stdout formatting layer, following the `TaskAction` /
//! `handle_task` convention in `cli/misc.rs`.

use clap::Subcommand;

use crate::cli::util::open_db;
use crate::db::Database;
use crate::{board, deliver, error};

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
pub(crate) fn handle_deliver_drain(repo: String) -> error::Result<()> {
    let database = open_db()?;
    let output = deliver_drain_output(&database, &repo)?;
    if !output.is_empty() {
        print!("{output}");
    }
    Ok(())
}

/// Testable core of `handle_deliver_drain`, split out so tests can exercise
/// it against a temp database without going through `open_db`'s canonical
/// data-dir resolution.
fn deliver_drain_output(database: &Database, repo: &str) -> error::Result<String> {
    let posts = deliver::drain_for_hook(database, repo)?;
    Ok(board::format_bullpen(&posts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    #[test]
    fn deliver_drain_output_prints_seeded_post_then_empty_on_rerun() {
        let db = test_db();

        // Prime past cold start: the first-ever drain against a nonempty
        // board seeds from the current watermark rather than replaying
        // history, so it delivers nothing.
        db.insert_reflection("seed", "sentinel", "team").unwrap();
        assert_eq!(deliver_drain_output(&db, "legion").unwrap(), "");

        db.insert_reflection("rafters", "hello team", "team")
            .unwrap();

        let first = deliver_drain_output(&db, "legion").unwrap();
        assert!(
            first.contains("hello team"),
            "expected seeded post text in output, got: {first}"
        );

        let second = deliver_drain_output(&db, "legion").unwrap();
        assert_eq!(second, "", "expected empty stdout on immediate rerun");
    }
}
