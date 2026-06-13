//! Watch: signal polling and auto-wake, split by domain (#611).
//!
//! This file owns the loop infrastructure only: [`WatchLoop`] with its
//! `bootstrap` / `tick_health` / `tick_poll`, the standalone `run` driver,
//! and the spawn-gate evaluation shared by both drivers. Domain files own
//! their types and tests; the re-exports below keep every pre-split
//! `watch::` path compiling unchanged.
#![allow(clippy::manual_is_multiple_of)] // Use modulo for MSRV compatibility

mod config;
mod gates;
mod locks;
mod signals;
mod spawn;
mod tracker;

pub use config::{
    WatchConfig, WatchRepoConfig, add_repo_to_config, default_session_lock_ttl_secs,
    list_repos_in_config, load_config, remove_repo_from_config, rename_in_config,
};
pub use gates::{PersonaLeaseGate, QuotaPanicGate, poll_cycle};
pub(crate) use locks::process_alive;
pub use locks::{CooldownTracker, PidLockGuard, SessionLockTracker, acquire_pid_lock};
pub use signals::{
    build_wake_prompt, directed_verb_will_not_wake, find_pending_signals, signal_requires_reply,
};
pub use spawn::{SpawnMode, record_session_end};
pub use tracker::AgentTracker;

// Re-exports with no caller outside this module tree today. Kept addressable
// at their pre-split `watch::` paths: Recipient/BroadcastTarget are the
// deliberate #585/#586 routing API contract, and the rest are public watch
// surface that tests and follow-on issues reach through `crate::watch::`.
#[allow(unused_imports)]
pub use config::{BroadcastTarget, Recipient};
#[allow(unused_imports)]
pub use gates::check_auto_unblock;
#[allow(unused_imports)]
pub use locks::release_pid_lock;
#[allow(unused_imports)]
pub use signals::is_wake_worthy;
#[allow(unused_imports)]
pub use spawn::{SpawnedChild, spawn_agent};
#[allow(unused_imports)]
pub use tracker::TrackedChild;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::db::Database;
use crate::error::Result;
use crate::health::HealthSampler;

// -- Main Loop ---------------------------------------------------------------

/// Resolve this host's identity for persona wake lease ownership. Falls back
/// to `"unknown"` when the system hostname is not available.
pub fn resolve_host_id() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "unknown".to_owned())
}

/// How many health ticks between heartbeat INFO log lines.
///
/// One every ten ticks keeps an idle watcher visible in the log without
/// flooding it. Shared by both watch modes so the throttle is identical.
pub const HEARTBEAT_LOG_CADENCE: u64 = 10;

/// Shared per-iteration state for the watch poll loop.
///
/// Both the standalone `watch::run` (sync, `std::thread::sleep`) and the
/// daemon's `run_watch_task` (async, `tokio::time::sleep`) own one of these
/// and call `tick_health` / `tick_poll` on their respective intervals.
///
/// The things that stay loop-specific are the sleep mechanism and the timer
/// types (`std::time::Instant` vs `tokio::time::Instant`); every pre-loop
/// side effect, including the cluster sync-actor spawn, lives in
/// [`WatchLoop::bootstrap`] so it cannot fork between the drivers again.
///
/// Having one shared body means a safety gate can never be present in one
/// loop and absent in the other -- the #578 bug that motivated this (#582).
pub struct WatchLoop {
    /// Watch configuration owned by the loop for the duration of the run.
    pub config: WatchConfig,
    /// Database handle owned by the loop for the duration of the run.
    pub db: Database,
    /// Per-repo spawn cooldown tracker.
    pub cooldown: CooldownTracker,
    /// Tracks live spawned child processes.
    pub tracker: AgentTracker,
    /// Per-repo session-lock gate.
    pub session_locks: SessionLockTracker,
    /// Rolling health-pressure window.
    pub sampler: HealthSampler,
    /// Subscription-quota panic-stop gate.
    pub quota_gate: QuotaPanicGate,
    /// Host identity for lease ownership and heartbeat attribution.
    pub host: String,
    /// Persona wake-lease TTL.
    pub lease_ttl: Duration,
    /// RFC3339 lower-bound for signal lookback (prevents historical flood on restart).
    pub lookback: String,
    /// Which spawn backend to use (print vs PTY).
    pub spawn_mode: SpawnMode,
    /// How long to retain health samples and watch_handled rows.
    pub retention_cutoff: chrono::Duration,
    /// Running count of health ticks elapsed; drives the throttled heartbeat log.
    pub health_tick_count: u64,
    /// PID of the running watch process (daemon or standalone).
    pub pid: u32,
    /// Cargo package version string for heartbeat rows.
    pub version: &'static str,
    /// Log prefix: `"[legion daemon]"` or `"[legion watch]"`.
    pub log_prefix: &'static str,
}

impl WatchLoop {
    /// Build the shared loop state from a loaded config and an open db.
    ///
    /// Both `daemon::run_watch_task` and `watch::run` construct the loop this
    /// way, so the field wiring (cooldown, session locks, quota gate, lookback
    /// window, heartbeat identity) lives in exactly one place -- the same
    /// anti-fork principle that motivated unifying the loop body itself. The
    /// caller passes its own `log_prefix` (`"[legion daemon]"` vs
    /// `"[legion watch]"`) and `spawn_mode`.
    pub fn new(
        config: WatchConfig,
        db: Database,
        data_dir: &Path,
        host: String,
        spawn_mode: SpawnMode,
        log_prefix: &'static str,
    ) -> Self {
        // Attribute the healthy<->panic bullpen edge posts to the first watched
        // repo, falling back to "legion" so the alert still lands.
        let quota_post_repo: String = config
            .repos
            .first()
            .map(|r| r.name.clone())
            .unwrap_or_else(|| "legion".to_string());
        // On startup, only look back 24 hours for unhandled signals, so a watch
        // that restarts after downtime does not flood agents with stale ones.
        let lookback: String = (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339();

        WatchLoop {
            cooldown: CooldownTracker::new(
                config.cooldown_secs,
                config.work_hours_start,
                config.work_hours_end,
            ),
            tracker: AgentTracker::new(),
            session_locks: SessionLockTracker::new(data_dir, config.session_lock_ttl_secs),
            sampler: HealthSampler::new(config.health_window_size),
            quota_gate: QuotaPanicGate::new(
                config.quota_panic_threshold_pct,
                host.clone(),
                quota_post_repo,
            ),
            host,
            lease_ttl: Duration::from_secs(config.persona_lease_ttl_secs),
            lookback,
            spawn_mode,
            retention_cutoff: chrono::Duration::days(config.retention_days as i64),
            health_tick_count: 0,
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION"),
            log_prefix,
            config,
            db,
        }
    }

    /// Shared pre-loop bootstrap for both drivers (`watch::run` and the
    /// daemon's `run_watch_task`).
    ///
    /// Owns every pre-loop side effect the two drivers used to duplicate:
    /// the cluster sync-actor spawn, the watch.toml/watch.pid/legion.db
    /// path joins, config load, pid-lock acquire (returned as an RAII
    /// guard), database open, and host-id resolution. The #578/#582 lesson
    /// is that anything living in two driver bodies eventually forks -- the
    /// sync-actor spawn already re-forked once (#536). Drivers keep only
    /// their sleep mechanism and timer types.
    ///
    /// Decision (#611, from the PR #624 review): the sync actor spawns
    /// FIRST, before any watch-config-dependent step. It depends only on
    /// cluster.toml, so it must not sit behind the watch.toml / pid-lock /
    /// db-open failure points -- a node with cluster sync enabled but a
    /// broken watch config still syncs. Callers receive the handle even
    /// when `watch` is `Err` and decide how long to keep it alive.
    pub fn bootstrap(
        data_dir: &Path,
        spawn_mode: SpawnMode,
        log_prefix: &'static str,
    ) -> WatchBootstrap {
        let sync = crate::sync_actor::spawn_sync_if_enabled(data_dir, log_prefix);

        let watch = (|| {
            let config_path: PathBuf = data_dir.join("watch.toml");
            let lock_path: PathBuf = data_dir.join("watch.pid");
            let db_path: PathBuf = data_dir.join("legion.db");

            let config = load_config(&config_path)?;

            eprintln!(
                "{log_prefix} config loaded: {} repo(s), poll every {}s, cooldown {}s, \
                 stagger {}s, health threshold {}%, spawn_mode={}",
                config.repos.len(),
                config.poll_interval_secs,
                config.cooldown_secs,
                config.stagger_secs,
                config.health_threshold_pct,
                spawn_mode.as_str(),
            );

            acquire_pid_lock(&lock_path)?;
            eprintln!("{log_prefix} acquired lock (pid {})", std::process::id());
            let guard = PidLockGuard(lock_path);

            let db = Database::open(&db_path)?;
            let host = resolve_host_id();

            Ok((
                WatchLoop::new(config, db, data_dir, host, spawn_mode, log_prefix),
                guard,
            ))
        })();

        WatchBootstrap { sync, watch }
    }

    /// Run one health-tick iteration.
    ///
    /// Samples system pressure, reaps finished children, heartbeats persona
    /// leases, persists a health sample, and upserts the liveness heartbeat
    /// row so `legion watch status` can report alive/stale/absent.  Also
    /// emits a throttled INFO line every `HEARTBEAT_LOG_CADENCE` ticks.
    ///
    /// This runs in BOTH the daemon and the standalone watch loop so
    /// `legion watch status` can observe either mode.
    pub fn tick_health(&mut self) {
        self.sampler.sample();
        // #649: drive the submit-confirmation protocol BEFORE reaping. A
        // PTY wake's prompt is bracketed-pasted at spawn but not submitted
        // until this loop retries Enter and observes a turn start; a child
        // that never confirms is failed here and reaped on the same tick.
        self.tracker
            .drive_submit_confirmation(&self.db, &self.config);
        self.tracker
            .reap_finished(Some(&self.db), Some(&self.session_locks));
        if let Err(e) = self.db.heartbeat_persona_leases(&self.host, self.lease_ttl) {
            eprintln!("{} lease heartbeat error: {e}", self.log_prefix);
        }

        match self.sampler.to_health_sample(self.tracker.active_count()) {
            Ok(sample) => {
                if let Err(e) = self.db.insert_health_sample(&sample) {
                    eprintln!("{} health persist error: {e}", self.log_prefix);
                }
            }
            Err(e) => {
                eprintln!("{} health sample error: {e}", self.log_prefix);
            }
        }

        // Persist the liveness heartbeat so `legion watch status` can report
        // alive/stale/absent without requiring ps or log inspection.
        let repo_count: u32 = self.config.repos.len() as u32;
        if let Err(e) =
            self.db
                .upsert_watch_heartbeat(&self.host, self.pid, self.version, repo_count, None)
        {
            eprintln!("{} heartbeat persist error: {e}", self.log_prefix);
        }

        // Throttled INFO line: once every HEARTBEAT_LOG_CADENCE ticks so an
        // idle watcher is silent most of the time but still proves liveness.
        self.health_tick_count += 1;
        if self.health_tick_count % HEARTBEAT_LOG_CADENCE == 1 {
            eprintln!(
                "{} heartbeat tick={} repos={} pid={}",
                self.log_prefix, self.health_tick_count, repo_count, self.pid
            );
        }
    }

    /// Run one poll-tick iteration.
    ///
    /// Evaluates the quota-panic and health-pressure spawn gates (in that
    /// priority order), runs `poll_cycle` when the gates are clear, then
    /// prunes stale health samples and watch_handled rows.
    ///
    /// Both gates are always evaluated here -- the #578 bug was the daemon
    /// copy calling `poll_cycle` directly, bypassing the quota-panic gate
    /// entirely. The unified body makes that class of omission impossible.
    pub fn tick_poll(&mut self) {
        let lease_gate = PersonaLeaseGate {
            db: &self.db,
            host: &self.host,
            ttl: self.lease_ttl,
        };

        match evaluate_spawn_gate(
            &mut self.quota_gate,
            &self.sampler,
            &self.db,
            self.config.health_threshold_pct,
        ) {
            SpawnGate::Proceed => {
                match poll_cycle(
                    &self.db,
                    &self.config,
                    &mut self.cooldown,
                    &mut self.tracker,
                    Some(&self.session_locks),
                    Some(&lease_gate),
                    Some(&self.lookback),
                    self.spawn_mode,
                ) {
                    Ok(n) if n > 0 => {
                        eprintln!("{} watch: {} agent(s) spawned", self.log_prefix, n);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("{} watch poll error: {e}", self.log_prefix);
                    }
                }
            }
            SpawnGate::QuotaPanic => {
                eprintln!(
                    "{} quota panic active (>= {:.1}%) -- skipping spawn cycle",
                    self.log_prefix, self.config.quota_panic_threshold_pct
                );
            }
            SpawnGate::Pressure(pressure) => {
                eprintln!(
                    "{} pressure {:.1}% >= threshold {:.0}% -- skipping spawn cycle",
                    self.log_prefix, pressure, self.config.health_threshold_pct
                );
            }
        }

        let cutoff = (chrono::Utc::now() - self.retention_cutoff).to_rfc3339();
        if let Err(e) = self.db.prune_health_samples(&cutoff) {
            eprintln!("{} health prune error: {e}", self.log_prefix);
        }
        if let Err(e) = self.db.prune_watch_handled(&cutoff) {
            eprintln!("{} watch_handled prune error: {e}", self.log_prefix);
        }
    }
}

/// Outcome of evaluating the per-poll spawn gates. Shared by `watch::run` and
/// the daemon's `run_watch_task` so the two loops cannot drift on which gates
/// guard a spawn cycle (#578 -- the daemon copy had silently dropped the quota
/// panic-stop gate after the Bun->Rust port).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpawnGate {
    /// Every gate is clear; run the spawn cycle.
    Proceed,
    /// Subscription-quota panic is active; skip the cycle.
    QuotaPanic,
    /// System pressure is at or over the health threshold; skip the cycle.
    /// Carries the measured pressure for the skip log.
    Pressure(f64),
}

/// Decide whether the concurrent-wake cap has been reached and `poll_cycle`
/// should stop spawning for the rest of this cycle (#598).
///
/// `active` is the total number of auto-wake agents in flight, read straight
/// from `AgentTracker::active_count()`. That counter is the single source of
/// truth: `track()` runs synchronously at each spawn site BEFORE the cycle's
/// own `spawned` tally is bumped, so `active_count()` already includes every
/// child launched earlier in this same cycle. Adding the cycle's `spawned`
/// count on top would double-count those children and halve the effective cap.
/// `cap` is `WatchConfig::max_concurrent_wakes`; 0 disables the gate. Negative
/// `active` (the tracker counter is an `i32`) clamps to 0.
///
/// Pulled out of `poll_cycle` so the decision is unit-testable without
/// spawning real agents, mirroring the `evaluate_spawn_gate` pattern.
fn wake_cap_reached(active: i32, cap: u32) -> bool {
    cap > 0 && (active.max(0) as u32) >= cap
}

/// Evaluate the gates guarding a spawn cycle, in priority order: the
/// subscription-quota panic-stop first (it protects the operator's rate-limit
/// cap and must never be skipped), then system-health pressure.
/// `quota_gate.check_and_post` advances the panic edge and emits the
/// healthy<->panic bullpen transition post as a side effect.
pub fn evaluate_spawn_gate(
    quota_gate: &mut QuotaPanicGate,
    sampler: &crate::health::HealthSampler,
    db: &Database,
    health_threshold_pct: f64,
) -> SpawnGate {
    if quota_gate.check_and_post(db) {
        SpawnGate::QuotaPanic
    } else if sampler.can_spawn(health_threshold_pct) {
        SpawnGate::Proceed
    } else {
        SpawnGate::Pressure(sampler.pressure())
    }
}

/// Everything `WatchLoop::bootstrap` produces for a driver.
///
/// The sync handle is a separate field rather than part of the `watch`
/// result because the two are deliberately independent: cluster sync is
/// spawned BEFORE any watch-config-dependent step, so a broken watch.toml
/// (or a lost pid-lock race, or a db-open failure) cannot silently kill
/// cluster sync on a node that has sync enabled (#611, dispositioned from
/// the PR #624 review). Drivers must keep the handle alive for the
/// lifetime of their loop -- dropping it stops the sync actor.
pub struct WatchBootstrap {
    /// Cluster sync actor handle when cluster.toml enables sync.
    pub sync: Option<crate::sync_actor::SyncHandle>,
    /// The shared loop state plus the pid-lock guard, or the reason the
    /// watch loop cannot start. Sync (above) runs either way.
    pub watch: Result<(WatchLoop, PidLockGuard)>,
}

/// Run the watch daemon main loop.
///
/// Uses a dual-interval loop: health sampling every `health_poll_secs`
/// (default 5s) and spawn checks every `poll_interval_secs` (default 30s).
/// Spawning is gated on system health -- if pressure exceeds the threshold,
/// the spawn cycle is skipped.
pub fn run(data_dir: &Path) -> Result<()> {
    let spawn_mode = SpawnMode::from_env();
    let boot = WatchLoop::bootstrap(data_dir, spawn_mode, "[legion watch]");

    // Keep the sync actor alive for the whole loop. In this foreground
    // driver a bootstrap failure propagates below and exits the process,
    // dropping (stopping) sync with it -- fail loud is correct for a
    // foreground command.
    let _sync_handle = boot.sync;
    let (mut state, _guard) = boot.watch?;

    // Timer intervals are read from the loop-owned config.
    let poll_interval: Duration = Duration::from_secs(state.config.poll_interval_secs);
    let health_interval: Duration = Duration::from_secs(state.config.health_poll_secs);

    // Use checked_sub so a near-zero system clock cannot overflow and panic.
    let mut poll_timer: Instant = Instant::now()
        .checked_sub(poll_interval)
        .unwrap_or_else(Instant::now);
    let mut health_timer: Instant = Instant::now()
        .checked_sub(health_interval)
        .unwrap_or_else(Instant::now);

    eprintln!(
        "[legion watch] watching repos: {}",
        state
            .config
            .repos
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    loop {
        // Health sample on its own interval.
        if health_timer.elapsed() >= health_interval {
            state.tick_health();
            health_timer = Instant::now();
        }

        // Spawn check on the poll interval.
        if poll_timer.elapsed() >= poll_interval {
            state.tick_poll();
            poll_timer = Instant::now();
        }

        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_storage;

    fn rate_limit_sample(
        hostname: &str,
        five_hour_pct: Option<f64>,
    ) -> crate::statusline::RateLimitSample {
        crate::statusline::RateLimitSample {
            id: "test-id".to_string(),
            hostname: hostname.to_string(),
            session_id: "test-session".to_string(),
            sampled_at: "2026-06-09T00:00:00Z".to_string(),
            five_hour_pct,
            five_hour_resets_at: None,
            seven_day_pct: None,
            seven_day_resets_at: None,
            model: None,
        }
    }

    #[test]
    fn evaluate_spawn_gate_proceeds_when_all_clear() {
        let (db, _index, _dir) = test_storage();
        let mut quota = QuotaPanicGate::new(90.0, "host-a".to_string(), "legion".to_string());
        // Empty window -> can_spawn true; no rate-limit sample -> no panic.
        let sampler = crate::health::HealthSampler::new(6);
        assert_eq!(
            evaluate_spawn_gate(&mut quota, &sampler, &db, 80.0),
            SpawnGate::Proceed
        );
    }

    #[test]
    fn evaluate_spawn_gate_quota_panic_takes_priority_over_healthy_system() {
        let (db, _index, _dir) = test_storage();
        // A sample for this host crosses the 90% panic threshold.
        db.insert_rate_limit_sample(&rate_limit_sample("host-a", Some(99.0)))
            .expect("insert rate-limit sample");
        let mut quota = QuotaPanicGate::new(90.0, "host-a".to_string(), "legion".to_string());
        // Fresh sampler would otherwise allow spawning -- proves the quota gate
        // wins. This is the regression #578 fixes: the daemon loop never even
        // consulted this gate.
        let sampler = crate::health::HealthSampler::new(6);
        assert_eq!(
            evaluate_spawn_gate(&mut quota, &sampler, &db, 80.0),
            SpawnGate::QuotaPanic
        );
    }

    #[test]
    fn evaluate_spawn_gate_reports_pressure_when_over_threshold() {
        let (db, _index, _dir) = test_storage();
        let mut quota = QuotaPanicGate::new(90.0, "host-a".to_string(), "legion".to_string());
        let mut sampler = crate::health::HealthSampler::new(6);
        sampler.push_pressure_for_test(95.0); // over the 80.0 threshold
        match evaluate_spawn_gate(&mut quota, &sampler, &db, 80.0) {
            SpawnGate::Pressure(p) => assert!(p >= 80.0, "pressure {p} should exceed threshold"),
            other => panic!("expected Pressure, got {other:?}"),
        }
    }

    #[test]
    fn wake_cap_reached_disabled_when_cap_zero() {
        // cap 0 disables the gate regardless of in-flight count.
        assert!(!wake_cap_reached(100, 0));
    }

    #[test]
    fn wake_cap_reached_true_at_and_over_cap() {
        // active == cap -> reached.
        assert!(wake_cap_reached(4, 4));
        // active > cap -> reached.
        assert!(wake_cap_reached(5, 4));
    }

    #[test]
    fn wake_cap_reached_false_under_cap() {
        assert!(!wake_cap_reached(3, 4));
        assert!(!wake_cap_reached(0, 4));
    }

    #[test]
    fn wake_cap_reached_clamps_negative_active() {
        // A negative tracker count must not wrap to a huge u32.
        assert!(!wake_cap_reached(-3, 4));
        assert!(!wake_cap_reached(-100, 1));
    }

    // -- WatchLoop unification (#582) ----------------------------------------

    /// Build a minimal WatchLoop for unit tests. Uses /tmp as the workdir
    /// (always exists) and the supplied db. The log_prefix and spawn_mode
    /// are caller-supplied so tests can exercise either mode's prefix.
    fn test_watch_loop(db: crate::db::Database, log_prefix: &'static str) -> WatchLoop {
        let config = WatchConfig {
            repos: vec![WatchRepoConfig {
                name: "test-repo".to_string(),
                workdir: "/tmp".to_string(),
                agent: None,
                broadcast_tags: Vec::new(),
                extra: toml::Table::new(),
            }],
            ..WatchConfig::default()
        };
        WatchLoop {
            cooldown: CooldownTracker::new(
                config.cooldown_secs,
                config.work_hours_start,
                config.work_hours_end,
            ),
            tracker: AgentTracker::new(),
            // Use a temp-style path; we don't need locks to actually fire in
            // these tests (no signals are wake-worthy enough to spawn).
            session_locks: SessionLockTracker::new(std::path::Path::new("/tmp"), 3600),
            sampler: HealthSampler::new(config.health_window_size),
            quota_gate: QuotaPanicGate::new(
                config.quota_panic_threshold_pct,
                "test-host".to_string(),
                "test-repo".to_string(),
            ),
            host: "test-host".to_string(),
            lease_ttl: std::time::Duration::from_secs(600),
            lookback: (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339(),
            spawn_mode: SpawnMode::Print,
            retention_cutoff: chrono::Duration::days(7),
            health_tick_count: 0,
            pid: std::process::id(),
            version: "0.0.0-test",
            log_prefix,
            config,
            db,
        }
    }

    /// `tick_poll` runs `poll_cycle` when all gates are clear (Proceed path).
    ///
    /// Inserts an informational (non-wake-worthy) signal so `poll_cycle` has
    /// something to process. An informational signal causes `poll_cycle` to
    /// mark it as handled (without spawning). After `tick_poll`, the signal
    /// should no longer appear in `find_pending_signals`.
    ///
    /// This test exercises the same code path the daemon and standalone watch
    /// both take -- the unification means there is only ONE path to test.
    #[test]
    fn watch_loop_tick_poll_proceed_marks_informational_signal_handled() {
        let (db, _index, _dir) = test_storage();
        // Insert an informational signal (verb=announce, not wake-worthy).
        db.insert_reflection("other-agent", "@test-repo announce -- hello", "team")
            .expect("insert signal");

        let mut state = test_watch_loop(db, "[legion test]");

        // Before the tick: signal should be pending.
        let before = find_pending_signals(&state.db, "test-repo", &["test-repo".to_string()], None)
            .expect("find pending");
        assert!(!before.is_empty(), "signal should be pending before tick");

        state.tick_poll();

        // After the tick: poll_cycle ran (Proceed gate), marked the
        // non-wake-worthy signal as handled, so the pending list is empty.
        let after = find_pending_signals(&state.db, "test-repo", &["test-repo".to_string()], None)
            .expect("find pending after");
        assert!(
            after.is_empty(),
            "informational signal should be handled after tick_poll with Proceed gate"
        );
    }

    /// `tick_poll` skips `poll_cycle` when the quota-panic gate is active.
    ///
    /// Inserts a rate-limit sample that trips the panic threshold. An
    /// informational signal is also inserted; after `tick_poll`, the signal
    /// must still be pending (poll_cycle was NOT called because the quota gate
    /// fired). This is the exact regression from #578 -- the daemon copy
    /// bypassed this gate entirely.
    #[test]
    fn watch_loop_tick_poll_quota_panic_skips_poll_cycle() {
        let (db, _index, _dir) = test_storage();
        // Rate-limit sample that crosses the 99% default threshold.
        db.insert_rate_limit_sample(&rate_limit_sample("test-host", Some(99.5)))
            .expect("insert rate-limit sample");
        // An informational signal that would be consumed if poll_cycle ran.
        db.insert_reflection(
            "other-agent",
            "@test-repo announce -- should not be consumed",
            "team",
        )
        .expect("insert signal");

        let mut state = test_watch_loop(db, "[legion test]");

        state.tick_poll();

        // Signal must still be pending: poll_cycle was skipped due to QuotaPanic.
        let pending =
            find_pending_signals(&state.db, "test-repo", &["test-repo".to_string()], None)
                .expect("find pending after quota panic tick");
        assert!(
            !pending.is_empty(),
            "signal must remain pending when quota panic gate fires (poll_cycle must not run)"
        );
    }

    /// `tick_poll` skips `poll_cycle` when system pressure exceeds the threshold.
    ///
    /// Uses `push_pressure_for_test` to simulate a fully-loaded system. An
    /// informational signal should remain pending after the tick because the
    /// pressure gate fired before `poll_cycle` was reached.
    #[test]
    fn watch_loop_tick_poll_pressure_gate_skips_poll_cycle() {
        let (db, _index, _dir) = test_storage();
        // An informational signal that would be consumed if poll_cycle ran.
        db.insert_reflection(
            "other-agent",
            "@test-repo announce -- should not be consumed",
            "team",
        )
        .expect("insert signal");

        let mut state = test_watch_loop(db, "[legion test]");
        // Push pressure above the default 80% threshold.
        state.sampler.push_pressure_for_test(95.0);

        state.tick_poll();

        // Signal must still be pending: poll_cycle was skipped due to Pressure.
        let pending =
            find_pending_signals(&state.db, "test-repo", &["test-repo".to_string()], None)
                .expect("find pending after pressure gate tick");
        assert!(
            !pending.is_empty(),
            "signal must remain pending when pressure gate fires (poll_cycle must not run)"
        );
    }

    /// WatchLoop-level guard for the #600 concurrent-wake cap: `tick_poll`
    /// must not spawn past `max_concurrent_wakes` even when a wake-worthy
    /// signal is pending, and must leave that signal pending so a later
    /// cycle can pick it up once a slot frees.
    ///
    /// `poll_cycle_caps_concurrent_wakes` pins the same behavior at the
    /// `poll_cycle` seam; this test drives it through the unified loop body
    /// both the daemon and standalone watch run, so a future refactor of
    /// `tick_poll` cannot drop the cap from one path (#578 class of bug).
    #[test]
    fn watch_loop_tick_poll_respects_concurrent_wake_cap() {
        let (db, _index, _dir) = test_storage();
        // A wake-worthy directed signal that would spawn if the cap allowed.
        db.insert_reflection("other-agent", "@test-repo request -- wake up", "team")
            .expect("insert signal");

        let mut state = test_watch_loop(db, "[legion test]");
        state.config.max_concurrent_wakes = 1;
        state.config.stagger_secs = 0;

        // Pre-seed one in-flight wake so the cap of 1 is already met. The
        // dummy child stands in for an already-running agent; tick_poll's
        // reap runs in tick_health, not tick_poll, so active_count() stays
        // at 1 for the whole call.
        state.tracker.track(
            "filler".to_string(),
            SpawnedChild::Print(
                std::process::Command::new("true")
                    .spawn()
                    .expect("spawn dummy child"),
            ),
            Vec::new(),
            String::new(),
            "filler-session".to_string(),
            "now".to_string(),
            Vec::new(),
            None,
        );
        assert_eq!(state.tracker.active_count(), 1);

        state.tick_poll();

        assert_eq!(
            state.tracker.active_count(),
            1,
            "tick_poll must not spawn past the concurrent-wake cap"
        );
        let pending =
            find_pending_signals(&state.db, "test-repo", &["test-repo".to_string()], None)
                .expect("find pending after capped tick");
        assert_eq!(
            pending.len(),
            1,
            "deferred wake-worthy signal must stay pending for re-poll"
        );
    }

    // -- WatchLoop::bootstrap (#611) ------------------------------------------

    /// Bootstrap with no watch.toml fails the watch half loudly and spawns
    /// no sync actor when cluster.toml is absent. The pid lock must not be
    /// touched -- config load fails before the lock step.
    #[test]
    fn bootstrap_missing_watch_toml_fails_watch_without_sync() {
        let dir = tempfile::tempdir().expect("tempdir");
        let boot = WatchLoop::bootstrap(dir.path(), SpawnMode::Print, "[legion test]");
        assert!(boot.sync.is_none(), "no cluster.toml -> no sync actor");
        let err = boot
            .watch
            .err()
            .expect("missing watch.toml must fail bootstrap");
        assert!(
            err.to_string().contains("config file not found"),
            "unexpected error: {err}"
        );
        assert!(
            !dir.path().join("watch.pid").exists(),
            "pid lock must not be acquired when config load fails"
        );
    }

    /// A successful bootstrap loads the config, acquires the pid lock, and
    /// the returned guard releases the lock on drop. A second bootstrap
    /// while the lock is held loses the pid-lock race.
    #[cfg(unix)]
    #[test]
    fn bootstrap_acquires_pid_lock_and_guard_releases_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("watch.toml"),
            "[[repos]]\nname = \"t\"\nworkdir = \"/tmp\"\n",
        )
        .expect("write watch.toml");

        let boot = WatchLoop::bootstrap(dir.path(), SpawnMode::Print, "[legion test]");
        let (state, guard) = boot.watch.expect("bootstrap must succeed");
        assert_eq!(state.config.repos.len(), 1);
        assert_eq!(state.spawn_mode, SpawnMode::Print);
        assert_eq!(state.log_prefix, "[legion test]");

        let pid_path = dir.path().join("watch.pid");
        assert!(pid_path.exists(), "bootstrap must acquire the pid lock");

        // Second bootstrap while we hold the lock (our own live pid) must
        // fail the watch half -- the pre-loop side effects cannot double-run.
        let second = WatchLoop::bootstrap(dir.path(), SpawnMode::Print, "[legion test]");
        assert!(
            second.watch.is_err(),
            "second bootstrap must lose the pid-lock race"
        );

        drop(state);
        drop(guard);
        assert!(
            !pid_path.exists(),
            "dropping the guard must release the pid lock"
        );
    }
}
