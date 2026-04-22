//! Mesh-aware task placement. Ranks hosts by headroom using the statusline
//! samples that every node writes on every Claude Code turn (see
//! `src/statusline.rs`). Cluster sync makes those samples visible to peers,
//! so any node can ask the store "who has the most runway right now?".
//!
//! Consumer surface: `legion mesh headroom` prints the full ranked table,
//! `legion mesh pick` emits the top hostname. The ranker itself is
//! separated from the CLI so a future `legion mesh pick --for-task <id>`
//! can apply task-specific weighting without duplicating the scoring math.

use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::db::Database;
use crate::error::Result;
use crate::statusline::{RateLimitSample, UsageSample};

/// Upper bound on the age of a statusline sample before a host counts as
/// "stale" (effectively idle/offline). Statusline writes on every
/// assistant turn, so minutes of silence usually means "no session is
/// live there right now". Configurable via `LEGION_MESH_STALE_SECS` in
/// the CLI layer; this constant is the compile-time default.
pub const DEFAULT_STALE_SECS: u64 = 600;

/// One host's slot in the ranked table. Carries both raw inputs (so the
/// CLI can render them) and the derived score, plus a `stale` flag so a
/// caller can decide to include vs exclude without re-checking the age.
#[derive(Debug, Clone)]
pub struct HostRanking {
    pub hostname: String,
    pub score: f64,
    pub five_hour_pct: Option<f64>,
    pub seven_day_pct: Option<f64>,
    pub last_effective_tokens: Option<i64>,
    pub sampled_at: Option<String>,
    pub age: Option<Duration>,
    pub stale: bool,
}

/// Score formula weights. Five-hour headroom dominates (it's the binding
/// constraint in practice); seven-day is secondary; recent token burn is
/// a small tiebreaker. Exposed so tests can reason about the shape.
pub const WEIGHT_FIVE_HOUR: f64 = 0.5;
pub const WEIGHT_SEVEN_DAY: f64 = 0.4;
pub const WEIGHT_BURN: f64 = 0.1;

/// Neutral default for an absent rate-limit % (50% used). A new node that
/// has not yet written a rate-limit sample must not win the picker for
/// free: we have no evidence it has headroom, and defaulting to 0% (full
/// headroom) would trivially outrank every host with actual data. Neutral
/// means "treat unknown as cluster-median, not best-case".
const UNKNOWN_PCT_NEUTRAL: f64 = 50.0;

/// Combine a host's rate-limit + usage samples into a single 0..100 score.
///
/// Semantics of the inputs:
/// - `five_hour_pct` / `seven_day_pct` are USED-%; `None` means "we don't
///   have a rate-limit sample yet" and is treated as cluster-neutral
///   (50% used) rather than best-case.
/// - `burn_component` is a 0..100 value representing "how few tokens this
///   host is burning relative to the cluster median" (higher == less
///   burning). Derived by `compute_burn_components`.
pub fn score_host(
    five_hour_pct: Option<f64>,
    seven_day_pct: Option<f64>,
    burn_component: f64,
) -> f64 {
    let h5 = 100.0
        - five_hour_pct
            .unwrap_or(UNKNOWN_PCT_NEUTRAL)
            .clamp(0.0, 100.0);
    let h7 = 100.0
        - seven_day_pct
            .unwrap_or(UNKNOWN_PCT_NEUTRAL)
            .clamp(0.0, 100.0);
    WEIGHT_FIVE_HOUR * h5 + WEIGHT_SEVEN_DAY * h7 + WEIGHT_BURN * burn_component
}

/// Map hostname -> burn_component in 0..100, where higher means less
/// burning. Derived from the median effective-token count across the
/// cluster: any host <= median gets 100; any host >= 2x median gets 0;
/// linear interpolation in between. Hosts with no usage sample default
/// to 100 (we don't know they're burning, so don't penalise them).
fn compute_burn_components(usage_by_host: &BTreeMap<String, UsageSample>) -> BTreeMap<String, f64> {
    let mut burns: Vec<i64> = usage_by_host.values().map(|s| s.effective_tokens).collect();
    if burns.is_empty() {
        return BTreeMap::new();
    }
    burns.sort_unstable();
    let median = burns[burns.len() / 2].max(1) as f64;

    let mut out = BTreeMap::new();
    for (host, sample) in usage_by_host {
        let ratio = sample.effective_tokens as f64 / median;
        let score = if ratio <= 1.0 {
            100.0
        } else if ratio >= 2.0 {
            0.0
        } else {
            // Linear: ratio=1 -> 100, ratio=2 -> 0.
            100.0 * (2.0 - ratio)
        };
        out.insert(host.clone(), score);
    }
    out
}

/// Rank every host that has appeared in either the rate-limit or usage
/// tables by the combined score. Stale hosts are included in the output
/// (sorted last) but flagged so callers can filter them.
///
/// `now` and `stale_cutoff` are parameters rather than computed inside so
/// tests can pin the clock.
pub fn rank_hosts(
    rate_samples: &[RateLimitSample],
    usage_samples: &[UsageSample],
    now: DateTime<Utc>,
    stale_cutoff: Duration,
) -> Vec<HostRanking> {
    let mut rate_by_host: BTreeMap<String, RateLimitSample> = BTreeMap::new();
    for s in rate_samples {
        rate_by_host.insert(s.hostname.clone(), s.clone());
    }
    let mut usage_by_host: BTreeMap<String, UsageSample> = BTreeMap::new();
    for s in usage_samples {
        usage_by_host.insert(s.hostname.clone(), s.clone());
    }

    let burn_by_host = compute_burn_components(&usage_by_host);

    let mut hosts: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for k in rate_by_host.keys() {
        hosts.insert(k.clone());
    }
    for k in usage_by_host.keys() {
        hosts.insert(k.clone());
    }

    let mut out: Vec<HostRanking> = hosts
        .into_iter()
        .map(|host| {
            let rate = rate_by_host.get(&host);
            let usage = usage_by_host.get(&host);

            // Age is taken from the newest of the two samples so a host
            // with only a rate-limit sample (or only a usage sample) is
            // not penalised as stale just because one table is empty.
            let age = newest_age(rate, usage, now);
            let stale = age.is_none_or(|a| a > stale_cutoff);

            let burn = burn_by_host.get(&host).copied().unwrap_or(100.0);
            let score = score_host(
                rate.and_then(|s| s.five_hour_pct),
                rate.and_then(|s| s.seven_day_pct),
                burn,
            );

            HostRanking {
                hostname: host,
                score,
                five_hour_pct: rate.and_then(|s| s.five_hour_pct),
                seven_day_pct: rate.and_then(|s| s.seven_day_pct),
                last_effective_tokens: usage.map(|u| u.effective_tokens),
                sampled_at: newest_sampled_at(rate, usage),
                age,
                stale,
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

fn newest_sampled_at(
    rate: Option<&RateLimitSample>,
    usage: Option<&UsageSample>,
) -> Option<String> {
    match (rate, usage) {
        (Some(r), Some(u)) => {
            if r.sampled_at >= u.sampled_at {
                Some(r.sampled_at.clone())
            } else {
                Some(u.sampled_at.clone())
            }
        }
        (Some(r), None) => Some(r.sampled_at.clone()),
        (None, Some(u)) => Some(u.sampled_at.clone()),
        (None, None) => None,
    }
}

fn newest_age(
    rate: Option<&RateLimitSample>,
    usage: Option<&UsageSample>,
    now: DateTime<Utc>,
) -> Option<Duration> {
    let ts = newest_sampled_at(rate, usage)?;
    let parsed = DateTime::parse_from_rfc3339(&ts).ok()?;
    let parsed_utc = parsed.with_timezone(&Utc);
    let delta = now.signed_duration_since(parsed_utc);
    // Negative deltas (sample timestamped in the future) clamp to zero so
    // a clock-skew on a peer cannot silently push that host into stale.
    if delta.num_seconds() < 0 {
        Some(Duration::from_secs(0))
    } else {
        Some(Duration::from_secs(delta.num_seconds() as u64))
    }
}

/// Convenience entry point for the CLI: load samples from the store and
/// return the ranked table in one call.
pub fn ranked_hosts_from_db(
    db: &Database,
    now: DateTime<Utc>,
    stale_cutoff: Duration,
) -> Result<Vec<HostRanking>> {
    let rates = db.latest_rate_limit_samples_per_host()?;
    let usages = db.latest_usage_samples_per_host()?;
    Ok(rank_hosts(&rates, &usages, now, stale_cutoff))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(host: &str, sampled_at: &str, five: f64, seven: f64) -> RateLimitSample {
        RateLimitSample {
            id: format!("r-{host}-{sampled_at}"),
            hostname: host.into(),
            session_id: "sess".into(),
            sampled_at: sampled_at.into(),
            five_hour_pct: Some(five),
            five_hour_resets_at: None,
            seven_day_pct: Some(seven),
            seven_day_resets_at: None,
            model: None,
        }
    }

    fn usage(host: &str, sampled_at: &str, effective: i64) -> UsageSample {
        UsageSample {
            id: format!("u-{host}-{sampled_at}"),
            hostname: host.into(),
            session_id: "sess".into(),
            turn_index: None,
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_write_tokens: 0,
            cache_read_tokens: 0,
            effective_tokens: effective,
            error_bytes: 0,
            sampled_at: sampled_at.into(),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-04-22T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn score_host_prefers_more_headroom() {
        let puck = score_host(Some(30.0), Some(50.0), 100.0);
        let laptop = score_host(Some(70.0), Some(60.0), 100.0);
        assert!(puck > laptop, "lower used-% must score higher");
    }

    #[test]
    fn score_host_handles_none_as_neutral_not_best_case() {
        // Unknown % must not beat a host with actual low-usage data.
        // Otherwise a brand-new node that has not yet written a rate-limit
        // sample wins the picker for free.
        let full = score_host(Some(0.0), Some(0.0), 100.0);
        let missing = score_host(None, None, 100.0);
        let exhausted = score_host(Some(100.0), Some(100.0), 100.0);
        assert!(full > missing, "known-zero-used must outrank unknown");
        assert!(missing > exhausted, "unknown must outrank known-exhausted");
    }

    #[test]
    fn rank_hosts_orders_by_score_descending() {
        let rates = vec![
            rate("laptop", "2026-04-22T11:59:00Z", 70.0, 60.0),
            rate("puck", "2026-04-22T11:59:00Z", 30.0, 50.0),
        ];
        let usages = vec![
            usage("laptop", "2026-04-22T11:59:00Z", 100),
            usage("puck", "2026-04-22T11:59:00Z", 100),
        ];
        let ranked = rank_hosts(&rates, &usages, now(), Duration::from_secs(600));
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].hostname, "puck");
        assert_eq!(ranked[1].hostname, "laptop");
    }

    #[test]
    fn rank_hosts_pushes_stale_to_bottom_regardless_of_score() {
        // stale host has "better" raw headroom, but its age blows the cutoff.
        let rates = vec![
            rate("stale-host", "2026-04-22T00:00:00Z", 5.0, 10.0),
            rate("fresh-host", "2026-04-22T11:59:00Z", 50.0, 50.0),
        ];
        let usages = vec![];
        let ranked = rank_hosts(&rates, &usages, now(), Duration::from_secs(600));
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].hostname, "fresh-host");
        assert!(!ranked[0].stale);
        assert_eq!(ranked[1].hostname, "stale-host");
        assert!(ranked[1].stale);
    }

    #[test]
    fn rank_hosts_empty_returns_empty() {
        let ranked = rank_hosts(&[], &[], now(), Duration::from_secs(600));
        assert!(ranked.is_empty());
    }

    #[test]
    fn rank_hosts_host_with_only_usage_sample_ranks() {
        // usage-only host has no rate %'s -- scores as full headroom,
        // but we still want it to appear in the table.
        let rates = vec![];
        let usages = vec![usage("lone", "2026-04-22T11:59:30Z", 100)];
        let ranked = rank_hosts(&rates, &usages, now(), Duration::from_secs(600));
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].hostname, "lone");
        assert_eq!(ranked[0].five_hour_pct, None);
        assert!(!ranked[0].stale);
    }

    #[test]
    fn rank_hosts_clock_skew_does_not_stale_a_future_sample() {
        // Sample timestamped 30 seconds in the future (peer with slight
        // clock skew). Must NOT be flagged stale.
        let rates = vec![rate("skewed", "2026-04-22T12:00:30Z", 30.0, 50.0)];
        let ranked = rank_hosts(&rates, &[], now(), Duration::from_secs(600));
        assert_eq!(ranked.len(), 1);
        assert!(!ranked[0].stale);
        assert_eq!(ranked[0].age, Some(Duration::from_secs(0)));
    }

    #[test]
    fn ranked_hosts_from_db_roundtrips_multi_host_samples() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("mesh.db")).unwrap();

        // Puck: newer rate sample (should win); laptop: older rate sample;
        // kestrel: only a usage sample, no rate %s.
        db.insert_rate_limit_sample(&rate("puck", "2026-04-22T11:59:00Z", 30.0, 50.0))
            .unwrap();
        db.insert_rate_limit_sample(&rate("puck", "2026-04-22T11:58:00Z", 80.0, 80.0))
            .unwrap();
        db.insert_rate_limit_sample(&rate("laptop", "2026-04-22T11:55:00Z", 70.0, 60.0))
            .unwrap();
        db.insert_usage_sample(&usage("puck", "2026-04-22T11:59:00Z", 100))
            .unwrap();
        db.insert_usage_sample(&usage("laptop", "2026-04-22T11:55:00Z", 120))
            .unwrap();
        db.insert_usage_sample(&usage("kestrel", "2026-04-22T11:59:30Z", 80))
            .unwrap();

        let ranked = ranked_hosts_from_db(&db, now(), Duration::from_secs(600)).unwrap();
        assert_eq!(ranked.len(), 3);
        // Puck wins: low used-% + recent sample. Kestrel has no rate %s so
        // scores as full headroom but with the burn floor. Laptop trails on
        // higher used-%.
        assert_eq!(ranked[0].hostname, "puck");
        assert!(
            !ranked.iter().any(|h| h.stale),
            "all three samples are within the 600s cutoff"
        );
    }

    #[test]
    fn ranked_hosts_from_db_stale_samples_render_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("mesh.db")).unwrap();

        // Sample from a day ago: definitely stale under the default cutoff.
        db.insert_rate_limit_sample(&rate("forgotten", "2026-04-21T12:00:00Z", 10.0, 10.0))
            .unwrap();

        let ranked = ranked_hosts_from_db(&db, now(), Duration::from_secs(600)).unwrap();
        assert_eq!(ranked.len(), 1);
        assert!(ranked[0].stale);
        assert!(ranked[0].age.is_some());
    }

    #[test]
    fn burn_component_penalises_heavy_burner() {
        // Three hosts, median effective = 200. heavy host is 2x, should
        // receive burn=0; light host is <= median, burn=100.
        let rates = vec![
            rate("light", "2026-04-22T11:59:00Z", 30.0, 30.0),
            rate("medium", "2026-04-22T11:59:00Z", 30.0, 30.0),
            rate("heavy", "2026-04-22T11:59:00Z", 30.0, 30.0),
        ];
        let usages = vec![
            usage("light", "2026-04-22T11:59:00Z", 100),
            usage("medium", "2026-04-22T11:59:00Z", 200),
            usage("heavy", "2026-04-22T11:59:00Z", 400),
        ];
        let ranked = rank_hosts(&rates, &usages, now(), Duration::from_secs(600));
        let by_host: std::collections::HashMap<_, _> = ranked
            .into_iter()
            .map(|h| (h.hostname.clone(), h))
            .collect();
        // All three share identical headroom; burn is the sole differentiator.
        assert!(by_host["light"].score > by_host["heavy"].score);
    }
}
