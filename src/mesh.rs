//! Mesh-aware host ranking. Ranks hosts by machine load from `host_status`,
//! the one-row-per-host table every daemon upserts on each health sample
//! (see `src/watch/mod.rs`). Cluster sync gossips those rows, so any node
//! can ask the store "which host has the most room right now?".
//!
//! Consumer surface: `legion mesh headroom` prints the full ranked table,
//! `legion mesh pick` emits the top hostname. The ranker is separated from
//! the CLI so a future `legion mesh pick --for-task <id>` can apply
//! task-specific weighting without duplicating the scoring math.

use chrono::{DateTime, Utc};
use std::time::Duration;

use crate::db::Database;
use crate::error::Result;
use crate::health::HostStatus;

/// Upper bound on the age of a `host_status` row before the host counts as
/// "stale" (daemon down or partitioned). A running daemon rewrites its row
/// every health tick and gossip carries it within one heartbeat, so ten
/// minutes of silence means the host is not reporting. Configurable via
/// `LEGION_MESH_STALE_SECS` in the CLI layer; this is the default.
pub const DEFAULT_STALE_SECS: u64 = 600;

/// One host's slot in the ranked table: the raw load (so the CLI can render
/// it), the derived score, and a `stale` flag so a caller can include or
/// exclude without re-checking the age.
#[derive(Debug, Clone)]
pub struct HostRanking {
    pub hostname: String,
    pub score: f64,
    pub cpu_pct: f64,
    pub mem_pct: f64,
    pub agents_active: i32,
    pub updated_at: String,
    pub age: Option<Duration>,
    pub stale: bool,
}

/// Headroom on a 0..100 scale: 100 minus the busier of CPU and memory.
///
/// The max, not an average: a host pinned on memory has no room for another
/// agent however idle its CPU is.
pub fn score_host(cpu_pct: f64, mem_pct: f64) -> f64 {
    100.0 - cpu_pct.max(mem_pct).clamp(0.0, 100.0)
}

/// Rank hosts by score. Stale hosts are included (sorted last) but flagged
/// so callers can filter them.
///
/// `now` and `stale_cutoff` are parameters so tests can pin the clock.
pub fn rank_hosts(
    statuses: &[HostStatus],
    now: DateTime<Utc>,
    stale_cutoff: Duration,
) -> Vec<HostRanking> {
    let mut out: Vec<HostRanking> = statuses
        .iter()
        .map(|s| {
            let age = row_age(&s.updated_at, now);
            HostRanking {
                hostname: s.hostname.clone(),
                score: score_host(s.cpu_pct, s.mem_pct),
                cpu_pct: s.cpu_pct,
                mem_pct: s.mem_pct,
                agents_active: s.agents_active,
                updated_at: s.updated_at.clone(),
                age,
                stale: age.is_none_or(|a| a > stale_cutoff),
            }
        })
        .collect();

    // Fresh hosts first (highest score wins), stale hosts last
    // (preserve their score-order for operator legibility).
    out.sort_by(|a, b| match (a.stale, b.stale) {
        (false, true) => std::cmp::Ordering::Less,
        (true, false) => std::cmp::Ordering::Greater,
        _ => b
            .score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal),
    });
    out
}

/// Maximum permitted clock skew before a peer's row is treated as unusable.
/// A row stamped more than this far in the future means the peer's clock is
/// too far off to rank against the cluster; treat it as stale rather than
/// preferring it forever.
const MAX_FUTURE_SKEW_SECS: i64 = 60;

/// Age of a row stamped `ts`, or `None` when the stamp cannot be ranked
/// (unparseable, or further in the future than the skew grace).
fn row_age(ts: &str, now: DateTime<Utc>) -> Option<Duration> {
    let parsed = match DateTime::parse_from_rfc3339(ts) {
        Ok(p) => p,
        Err(e) => {
            // Do not silently swallow: a corrupted updated_at (schema drift
            // or a broken peer) would otherwise mark the host stale with
            // zero diagnostic.
            eprintln!(
                "[legion] warning: unparseable updated_at '{}': {} -- host will be marked stale",
                ts, e
            );
            return None;
        }
    };
    let secs = now
        .signed_duration_since(parsed.with_timezone(&Utc))
        .num_seconds();
    if secs < -MAX_FUTURE_SKEW_SECS {
        eprintln!(
            "[legion] warning: host row timestamped {}s in the future -- peer clock skew beyond grace, marking stale",
            -secs
        );
        None
    } else if secs < 0 {
        // Small forward skew is normal; clamp to zero so the host is fresh.
        Some(Duration::from_secs(0))
    } else {
        Some(Duration::from_secs(secs.unsigned_abs()))
    }
}

/// Convenience entry point for the CLI: load `host_status` and return the
/// ranked table in one call.
pub fn ranked_hosts_from_db(
    db: &Database,
    now: DateTime<Utc>,
    stale_cutoff: Duration,
) -> Result<Vec<HostRanking>> {
    let statuses = db.list_host_status()?;
    Ok(rank_hosts(&statuses, now, stale_cutoff))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(host: &str, updated_at: &str, cpu: f64, mem: f64) -> HostStatus {
        HostStatus {
            hostname: host.into(),
            cpu_pct: cpu,
            mem_pct: mem,
            load_1m: None,
            agents_active: 0,
            created_at: updated_at.into(),
            updated_at: updated_at.into(),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-04-22T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn score_host_is_100_minus_the_busier_resource() {
        assert!((score_host(30.0, 70.0) - 30.0).abs() < 1e-9);
        assert!((score_host(90.0, 10.0) - 10.0).abs() < 1e-9);
        assert!((score_host(0.0, 0.0) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn score_host_clamps_out_of_range_load() {
        assert!((score_host(150.0, 0.0) - 0.0).abs() < 1e-9);
        assert!((score_host(-5.0, -5.0) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn rank_hosts_orders_by_lowest_max_load() {
        // Laptop has the lower CPU but the higher memory; Puck's max is lower.
        let statuses = vec![
            status("laptop", "2026-04-22T11:59:00Z", 10.0, 80.0),
            status("puck", "2026-04-22T11:59:00Z", 40.0, 50.0),
        ];
        let ranked = rank_hosts(&statuses, now(), Duration::from_secs(600));
        assert_eq!(ranked[0].hostname, "puck");
        assert_eq!(ranked[1].hostname, "laptop");
    }

    #[test]
    fn rank_hosts_pushes_stale_to_bottom_regardless_of_score() {
        let statuses = vec![
            status("stale-host", "2026-04-22T00:00:00Z", 1.0, 1.0),
            status("fresh-host", "2026-04-22T11:59:00Z", 50.0, 50.0),
        ];
        let ranked = rank_hosts(&statuses, now(), Duration::from_secs(600));
        assert_eq!(ranked[0].hostname, "fresh-host");
        assert!(!ranked[0].stale);
        assert_eq!(ranked[1].hostname, "stale-host");
        assert!(ranked[1].stale);
    }

    #[test]
    fn rank_hosts_empty_returns_empty() {
        assert!(rank_hosts(&[], now(), Duration::from_secs(600)).is_empty());
    }

    #[test]
    fn rank_hosts_clock_skew_does_not_stale_a_future_row() {
        let statuses = vec![status("skewed", "2026-04-22T12:00:30Z", 30.0, 50.0)];
        let ranked = rank_hosts(&statuses, now(), Duration::from_secs(600));
        assert!(!ranked[0].stale);
        assert_eq!(ranked[0].age, Some(Duration::from_secs(0)));
    }

    #[test]
    fn rank_hosts_huge_future_skew_marks_host_stale() {
        let statuses = vec![status("broken-clock", "2026-04-22T14:00:00Z", 5.0, 5.0)];
        let ranked = rank_hosts(&statuses, now(), Duration::from_secs(600));
        assert!(ranked[0].stale, "2h-future row must be stale, not winning");
        assert!(ranked[0].age.is_none());
    }

    #[test]
    fn rank_hosts_malformed_updated_at_marks_host_stale() {
        let statuses = vec![status("garbled", "not-a-timestamp", 5.0, 5.0)];
        let ranked = rank_hosts(&statuses, now(), Duration::from_secs(600));
        assert!(ranked[0].stale);
        assert!(ranked[0].age.is_none());
    }

    #[test]
    fn ranked_hosts_from_db_reads_host_status() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("mesh.db")).unwrap();
        db.upsert_host_status(&status("puck", "2026-04-22T11:59:00Z", 20.0, 30.0))
            .unwrap();
        db.upsert_host_status(&status("laptop", "2026-04-22T11:59:30Z", 60.0, 10.0))
            .unwrap();
        db.upsert_host_status(&status("forgotten", "2026-04-21T12:00:00Z", 0.0, 0.0))
            .unwrap();

        let ranked = ranked_hosts_from_db(&db, now(), Duration::from_secs(600)).unwrap();
        let order: Vec<&str> = ranked.iter().map(|h| h.hostname.as_str()).collect();
        assert_eq!(order, vec!["puck", "laptop", "forgotten"]);
        assert!(ranked[2].stale);
    }
}
