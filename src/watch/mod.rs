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
pub use locks::{
    CooldownTracker, PidLockGuard, SessionLockTracker, acquire_index_lock, acquire_pid_lock,
};
pub(crate) use locks::{process_alive, terminate_process};
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
pub use locks::release_pid_lock;
#[allow(unused_imports)]
pub use signals::{is_wake_worthy, resolves_pending_ask};
#[allow(unused_imports)]
pub use spawn::{SpawnedChild, spawn_agent};
#[allow(unused_imports)]
pub use tracker::TrackedChild;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::db::{Database, RedeliveryOutcome, ReflectionMeta};
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

            // #900: a daemon that has just started owns no live sessions, so any
            // persona lease still attributed to this host is a leftover from a
            // previous daemon lifetime -- and after a power loss there is no
            // graceful shutdown to release them. Without this, those rows are
            // refreshed forever by the heartbeat and the TTL never reclaims them.
            // Runs before the poll loop so the first tick sees a clean table.
            // Non-fatal: a failure here must not stop the daemon from starting.
            match db.release_persona_leases_by_host(&host) {
                Ok(0) => {}
                Ok(n) => eprintln!("{log_prefix} released {n} stale persona lease(s) for {host}"),
                Err(e) => eprintln!("{log_prefix} stale lease release failed: {e}"),
            }

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
        self.tracker.reap_finished(
            Some(&self.db),
            Some(&self.session_locks),
            Duration::from_secs(self.config.session_budget_secs),
            self.config.max_redelivery_attempts,
        );
        // #673 fix 4: reap `running` wake_attempts whose backing pid is dead.
        // These rows accumulate after crash/restart (pid-alive check only runs
        // on the AgentTracker's live children; a row whose pid was never
        // tracked by *this* daemon instance is invisible to the tracker).
        reap_dead_pid_attempts(
            &self.db,
            &self.host,
            self.log_prefix,
            self.config.max_redelivery_attempts,
        );
        // #778: auto-revert delegated work whose attempt is no longer live.
        // Runs AFTER the two reapers above so an attempt that just went
        // terminal this tick is already reflected in wake_attempts.state --
        // reap-before-mutate, the same ordering lesson #679's own reaper
        // finalization fix (f45e7e5) encoded for lease release vs heartbeat.
        reap_delegated_work(&self.db, self.log_prefix);
        // #934: wake card-independent deferred work items whose wake_at has
        // passed. #931 removed the card surface (and its own defer sweep,
        // `reap_deferred_cards`) entirely -- this is the only defer sweep
        // left.
        reap_deferred_work_items(&self.db, &self.host, self.log_prefix);
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
        if let Err(e) = self.db.prune_watch_redelivery(&cutoff) {
            eprintln!("{} watch_redelivery prune error: {e}", self.log_prefix);
        }
    }
}

/// Reap `running` wake_attempts whose backing pid is dead (#673 fix 4).
///
/// `list_local_orphans` returns every in-flight row owned by this host.
/// For each one that has a `spawned_pid`, we probe liveness with
/// `process_alive`. A dead pid means the agent exited without the stop
/// hook firing (crash, OOM, operator kill) -- mark the row `failed` via
/// `record_wake_attempt_outcome` so it does not persist indefinitely.
///
/// Rows without a `spawned_pid` (queued/claimed/spawning without a
/// recorded pid yet) are left alone -- they are still transitioning and
/// the pid has not been stamped yet.
///
/// An error from the DB scan is logged and swallowed; the reaper must
/// never abort the health tick.
///
/// #948: once `record_wake_attempt_outcome` itself returns `Ok(())` (this
/// call won the terminal-state write), the signals this attempt carried
/// are re-armed or abandoned via [`rearm_or_abandon`] -- a losing call
/// (another site already settled the row) never reaches this branch,
/// which is what keeps the redelivery accounting exactly-once.
fn reap_dead_pid_attempts(
    db: &Database,
    host: &str,
    log_prefix: &str,
    max_redelivery_attempts: u32,
) {
    let orphans = match db.list_local_orphans(host) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{log_prefix} dead-pid reaper: scan error: {e}");
            return;
        }
    };

    for attempt in orphans {
        let pid = match attempt.spawned_pid {
            Some(p) => p,
            None => continue, // no pid yet; skip
        };

        if process_alive(pid) {
            continue; // still running; leave it alone
        }

        eprintln!(
            "{log_prefix} dead-pid reaper: attempt {} (repo {}, pid {}) is not alive -- marking failed",
            attempt.attempt_id, attempt.repo_name, pid
        );

        match db.record_wake_attempt_outcome(
            &attempt.attempt_id,
            "error",
            "dead-pid: process not found at reaper scan",
        ) {
            Ok(()) => {
                rearm_or_abandon(
                    db,
                    &attempt.attempt_id,
                    &attempt.repo_name,
                    &attempt.signal_ids,
                    max_redelivery_attempts,
                    log_prefix,
                );
            }
            Err(e) => {
                eprintln!(
                    "{log_prefix} dead-pid reaper: failed to mark {} as failed: {e}",
                    attempt.attempt_id
                );
            }
        }
    }
}

/// Re-arm or abandon the signals a wake attempt carried once its terminal
/// failure state has committed (#948). Shared by the three
/// `record_wake_attempt_outcome`-adjacent call sites (the dead-pid reaper
/// above, and the two `reap_finished` failure branches in tracker.rs) so
/// the loud-abandonment behavior cannot drift between them.
///
/// Callers must only invoke this after `record_wake_attempt_outcome`
/// itself returned `Ok(())` -- calling it on a losing write would
/// double-account a redelivery attempt for a terminal state this call
/// never actually settled.
///
/// A DB error re-arming is logged and swallowed, matching every other
/// reaper convention in this file. An `Exhausted` outcome is loud: an
/// `eprintln!` naming every identifying field, plus a best-effort bullpen
/// post to `repo_name` (log-and-swallow on post failure, mirroring
/// `QuotaPanicGate::check_and_post`) -- there is no future retry point for
/// this event since `watch_handled` is deliberately left in place.
pub(crate) fn rearm_or_abandon(
    db: &Database,
    attempt_id: &str,
    repo_name: &str,
    signal_ids: &[String],
    max_attempts: u32,
    log_prefix: &str,
) {
    let outcomes = match db.rearm_or_abandon_signals(signal_ids, repo_name, max_attempts) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{log_prefix} rearm/abandon error for attempt {attempt_id}: {e}");
            return;
        }
    };

    for (signal_id, outcome) in outcomes {
        let RedeliveryOutcome::Exhausted { attempts } = outcome else {
            continue;
        };
        // The post body carries no log prefix -- it is a bullpen message
        // for a human/agent reader, not a log line (mirrors
        // `QuotaPanicGate::check_and_post`, whose post text is likewise
        // prefix-free while its own eprintln! calls carry `self.host`).
        let post_text = format!(
            "redelivery ABANDONED: signal {signal_id} for {repo_name} exhausted {attempts} \
             delivery attempts (cap {max_attempts}) -- wake_attempt {attempt_id} settled failed; \
             signal stays permanently handled for this repo"
        );
        eprintln!("{log_prefix} {post_text}");
        if let Err(e) = db.insert_reflection_with_meta(
            repo_name,
            &post_text,
            "team",
            &ReflectionMeta::default(),
        ) {
            eprintln!(
                "{log_prefix} redelivery abandonment bullpen post failed for signal {signal_id}: {e}"
            );
        }
    }
}

/// Auto-revert delegated work whose linked wake attempt is no longer live
/// (#778, discovery generalized off card status #934): a delegation is
/// sound only while an unfakeable liveness signal backs it, so it must not
/// be able to outlive that signal. Scans every repo (not just this
/// daemon's own spawns) because delegated work can be linked to an attempt
/// claimed by any host in the cluster -- the point of the check is "is the
/// work still happening anywhere," not "did I personally spawn it."
///
/// Discovery (#934) reads `wake_attempts` directly via
/// `Database::live_linked_wake_attempts` -- the sweep needs only the
/// work-item link itself, never any card/task table. Liveness runs through
/// `Database::work_item_is_live`, the same predicate the stop.sh gate 1b
/// subcommand reads, so the two agree by construction (the #679 "one
/// predicate" lesson). The revert is telemetried: an INFO line is logged,
/// so a silent abandonment never has zero trace -- consistent with the
/// dead-pid reaper immediately above.
///
/// #931 removed the card surface, including `kanban::undelegate_card` (the
/// old revert step, which kept a card's visible status in sync with the
/// cleared link). There is no card status left to sync: reverting now is
/// exactly clearing the wake_attempts link, nothing else.
///
/// An error from the DB scan or a single row's revert is logged and
/// swallowed; the health tick must never abort over one bad row.
///
/// The `DELEGATION_STALE_AFTER_SECS` window matches `legion watch
/// status`'s own staleness default (`cli::watch::WatchAction::Status`), so
/// an operator reading "alive" from that command and this sweep agree on
/// the same window. `pub(crate)` so `legion delegated-needs-attention`
/// (the stop.sh gate 1b backing command) uses the identical window this
/// sweep does.
pub(crate) const DELEGATION_STALE_AFTER_SECS: u64 = 120;

fn reap_delegated_work(db: &Database, log_prefix: &str) {
    let linked = match db.live_linked_wake_attempts(None) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{log_prefix} delegated reaper: scan error: {e}");
            return;
        }
    };

    for attempt in linked {
        let Some(work_item_id) = attempt.work_item_id.clone() else {
            // live_linked_wake_attempts filters on card_id IS NOT NULL;
            // this branch should be unreachable, but stay defensive rather
            // than unwrap.
            continue;
        };

        let live = match db.work_item_is_live(&work_item_id, DELEGATION_STALE_AFTER_SECS) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "{log_prefix} delegated reaper: liveness check error for work item {work_item_id}: {e}"
                );
                continue;
            }
        };
        if live {
            continue;
        }

        eprintln!(
            "{log_prefix} delegated reaper: work item {work_item_id} (repo {}) no longer live -- clearing the link",
            attempt.repo_name
        );

        if let Err(e) = db.clear_wake_attempt_work_item(&work_item_id) {
            eprintln!(
                "{log_prefix} delegated reaper: failed to clear stale link for {work_item_id}: {e}"
            );
        }
    }
}

/// Wake card-independent deferred work items whose `wake_at` has passed
/// (#934), reading `deferrals` (keyed on an opaque work-item id + owning
/// repo). Clears the deferral and posts a wake-worthy `routing` signal
/// naming the work item to its owning repo. #931 removed the card-scoped
/// predecessor (`reap_deferred_cards`) -- this is the only defer sweep now.
///
/// The signal is authored as `host`, not a repo name: the recipient-side
/// self-address filter drops a signal whose author equals its own repo,
/// and a deferred work item's owning repo can legitimately be `"legion"` --
/// a fixed repo-name author would silently never wake legion's own
/// deferrals (#816/#817).
///
/// An error from the DB scan, a single row's clear, or a single row's wake
/// signal is logged and swallowed; the health tick must never abort over one
/// bad row. Liveness caveat: this only fires while `legion watch` is
/// running for the work item's repo.
fn reap_deferred_work_items(db: &Database, host: &str, log_prefix: &str) {
    let now = chrono::Utc::now().to_rfc3339();
    let due = match db.deferrals_due(&now) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{log_prefix} deferred work-item reaper: scan error: {e}");
            return;
        }
    };

    for deferral in due {
        if let Err(e) = db.clear_deferral(&deferral.work_item_id) {
            eprintln!(
                "{log_prefix} deferred work-item reaper: failed to clear {}: {e}",
                deferral.work_item_id
            );
            continue;
        }

        eprintln!(
            "{log_prefix} deferred work-item reaper: {} (repo {}) wake_at reached",
            deferral.work_item_id, deferral.repo
        );

        let note = format!(
            "deferred work item {} is back (wake_at reached)",
            deferral.work_item_id
        );
        let text = crate::signal::format_signal(&deferral.repo, "routing", None, Some(&note), &[]);
        if let Err(e) = db.insert_reflection_with_meta(
            host,
            &text,
            "team",
            &crate::db::ReflectionMeta::default(),
        ) {
            eprintln!(
                "{log_prefix} deferred work-item reaper: wake signal failed for {}: {e}",
                deferral.work_item_id
            );
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

    /// `tick_poll` prunes stale `watch_redelivery` rows alongside
    /// `watch_handled` (#948), on the same retention cutoff.
    #[test]
    fn watch_loop_tick_poll_prunes_watch_redelivery() {
        let (db, _index, _dir) = test_storage();
        db.conn
            .execute(
                "INSERT INTO watch_redelivery (signal_id, repo_name, attempts, last_failed_at) \
                 VALUES ('stale-signal', 'test-repo', 1, '2020-01-01T00:00:00+00:00')",
                [],
            )
            .expect("seed stale row");
        db.conn
            .execute(
                "INSERT INTO watch_redelivery (signal_id, repo_name, attempts, last_failed_at) \
                 VALUES ('fresh-signal', 'test-repo', 1, ?1)",
                [chrono::Utc::now().to_rfc3339()],
            )
            .expect("seed fresh row");

        let mut state = test_watch_loop(db, "[legion test]");
        state.tick_poll();

        let remaining: i64 = state
            .db
            .conn
            .query_row("SELECT COUNT(*) FROM watch_redelivery", [], |r| r.get(0))
            .expect("count remaining");
        assert_eq!(
            remaining, 1,
            "tick_poll must prune the stale watch_redelivery row and keep the fresh one"
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

    // -- FIX 4: dead-pid reaper (#673) ----------------------------------------

    /// A `running` wake_attempt with a dead pid is marked failed by
    /// `reap_dead_pid_attempts`. A live-pid row is left alone.
    #[cfg(unix)]
    #[test]
    fn reap_dead_pid_attempts_marks_dead_pid_failed_and_leaves_live_pid() {
        let (db, _index, _dir) = test_storage();

        // Spawn a real process, record its pid, then wait for it to die.
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let dead_pid = child.id();
        child.wait().expect("wait for child");

        // Insert a running attempt with the dead pid.
        let dead_id = uuid::Uuid::now_v7().to_string();
        db.enqueue_wake_attempt(&dead_id, "test-persona", "test-repo", &[])
            .expect("enqueue dead-pid attempt");
        db.try_claim_wake_attempt(&dead_id, "test-host")
            .expect("claim dead-pid attempt");
        db.transition_wake_attempt(
            &dead_id,
            crate::wake_attempts::WakeAttemptState::Claimed,
            crate::wake_attempts::WakeAttemptState::Spawning,
        )
        .expect("spawning");
        db.set_wake_attempt_pid(&dead_id, dead_pid)
            .expect("set dead pid");
        db.transition_wake_attempt(
            &dead_id,
            crate::wake_attempts::WakeAttemptState::Spawning,
            crate::wake_attempts::WakeAttemptState::Running,
        )
        .expect("running");

        // Insert a second running attempt with our own (live) pid.
        let live_id = uuid::Uuid::now_v7().to_string();
        db.enqueue_wake_attempt(&live_id, "test-persona", "test-repo", &[])
            .expect("enqueue live-pid attempt");
        db.try_claim_wake_attempt(&live_id, "test-host")
            .expect("claim live-pid attempt");
        db.transition_wake_attempt(
            &live_id,
            crate::wake_attempts::WakeAttemptState::Claimed,
            crate::wake_attempts::WakeAttemptState::Spawning,
        )
        .expect("spawning live");
        db.set_wake_attempt_pid(&live_id, std::process::id())
            .expect("set live pid");
        db.transition_wake_attempt(
            &live_id,
            crate::wake_attempts::WakeAttemptState::Spawning,
            crate::wake_attempts::WakeAttemptState::Running,
        )
        .expect("running live");

        // Run the reaper.
        reap_dead_pid_attempts(&db, "test-host", "[test]", 3);

        // Dead-pid row must be in a terminal (failed) state.
        let dead_row = db
            .get_wake_attempt(&dead_id)
            .expect("get dead row")
            .expect("row exists");
        assert_eq!(
            dead_row.state.as_str(),
            "failed",
            "dead-pid attempt must be marked failed; got state: {}",
            dead_row.state.as_str()
        );

        // Live-pid row must still be running.
        let live_row = db
            .get_wake_attempt(&live_id)
            .expect("get live row")
            .expect("row exists");
        assert_eq!(
            live_row.state.as_str(),
            "running",
            "live-pid attempt must remain in running state; got: {}",
            live_row.state.as_str()
        );
    }

    /// A `running` wake_attempt with no spawned_pid is left alone by the reaper
    /// (it is still transitioning and has not been assigned a pid yet).
    #[test]
    fn reap_dead_pid_attempts_skips_rows_without_pid() {
        let (db, _index, _dir) = test_storage();

        // Insert a running attempt without calling set_wake_attempt_pid.
        // Use direct SQL to get to running without a pid (rare in practice,
        // but the reaper must handle it defensively).
        let attempt_id = uuid::Uuid::now_v7().to_string();
        db.enqueue_wake_attempt(&attempt_id, "test-persona", "test-repo", &[])
            .expect("enqueue");
        db.try_claim_wake_attempt(&attempt_id, "test-host")
            .expect("claim");
        // Transition to spawning and then running without setting a pid.
        db.transition_wake_attempt(
            &attempt_id,
            crate::wake_attempts::WakeAttemptState::Claimed,
            crate::wake_attempts::WakeAttemptState::Spawning,
        )
        .expect("spawning");
        db.transition_wake_attempt(
            &attempt_id,
            crate::wake_attempts::WakeAttemptState::Spawning,
            crate::wake_attempts::WakeAttemptState::Running,
        )
        .expect("running");

        // Run the reaper -- must not crash and must leave the row alone.
        reap_dead_pid_attempts(&db, "test-host", "[test]", 3);

        let row = db
            .get_wake_attempt(&attempt_id)
            .expect("get row")
            .expect("row exists");
        assert_eq!(
            row.state.as_str(),
            "running",
            "no-pid running row must be left in running state"
        );
    }

    // -- redelivery on failed wake settle (#948) ------------------------------

    /// A dead-pid attempt that carries a signal already marked
    /// `watch_handled` for its repo must re-arm that signal (delete the
    /// `watch_handled` row) once `record_wake_attempt_outcome` commits the
    /// terminal `failed` state -- the exact incident this issue fixes.
    #[cfg(unix)]
    #[test]
    fn reap_dead_pid_attempts_rearms_signal_below_cap() {
        let (db, _index, _dir) = test_storage();

        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let dead_pid = child.id();
        child.wait().expect("wait for child");

        let signal_id = db
            .insert_reflection("platform", "@smugglr question:reap-me", "team")
            .expect("insert signal")
            .id;
        db.mark_signal_handled_for_repo(&signal_id, "smugglr")
            .expect("mark handled at spawn time");

        let attempt_id = uuid::Uuid::now_v7().to_string();
        db.enqueue_wake_attempt(
            &attempt_id,
            "test-persona",
            "smugglr",
            std::slice::from_ref(&signal_id),
        )
        .expect("enqueue");
        db.try_claim_wake_attempt(&attempt_id, "test-host")
            .expect("claim");
        db.transition_wake_attempt(
            &attempt_id,
            crate::wake_attempts::WakeAttemptState::Claimed,
            crate::wake_attempts::WakeAttemptState::Spawning,
        )
        .expect("spawning");
        db.set_wake_attempt_pid(&attempt_id, dead_pid)
            .expect("set dead pid");
        db.transition_wake_attempt(
            &attempt_id,
            crate::wake_attempts::WakeAttemptState::Spawning,
            crate::wake_attempts::WakeAttemptState::Running,
        )
        .expect("running");

        reap_dead_pid_attempts(&db, "test-host", "[test]", 3);

        let row = db
            .get_wake_attempt(&attempt_id)
            .expect("get row")
            .expect("row exists");
        assert_eq!(row.state.as_str(), "failed");

        let signals = db
            .get_unhandled_signals_for_repo("smugglr", &["smugglr".to_string()], None)
            .expect("query unhandled");
        assert_eq!(
            signals.len(),
            1,
            "the signal must re-surface for smugglr after the dead-pid reap"
        );
    }

    /// Once a (signal_id, repo_name) pair has already failed `max_attempts`
    /// times, the next failure must NOT delete `watch_handled` and must
    /// post a loud abandonment notice to the recipient repo's own bullpen.
    #[cfg(unix)]
    #[test]
    fn reap_dead_pid_attempts_exhausts_and_posts_bullpen_alarm() {
        let (db, _index, _dir) = test_storage();

        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let dead_pid = child.id();
        child.wait().expect("wait for child");

        let signal_id = db
            .insert_reflection("platform", "@smugglr question:crashy", "team")
            .expect("insert signal")
            .id;
        // Pre-seed the counter at the cap so this failure is the one that
        // tips it over.
        db.conn
            .execute(
                "INSERT INTO watch_redelivery (signal_id, repo_name, attempts, last_failed_at) \
                 VALUES (?1, 'smugglr', 3, ?2)",
                rusqlite::params![&signal_id, chrono::Utc::now().to_rfc3339()],
            )
            .expect("seed redelivery counter at cap");
        db.mark_signal_handled_for_repo(&signal_id, "smugglr")
            .expect("mark handled at spawn time");

        let attempt_id = uuid::Uuid::now_v7().to_string();
        db.enqueue_wake_attempt(
            &attempt_id,
            "test-persona",
            "smugglr",
            std::slice::from_ref(&signal_id),
        )
        .expect("enqueue");
        db.try_claim_wake_attempt(&attempt_id, "test-host")
            .expect("claim");
        db.transition_wake_attempt(
            &attempt_id,
            crate::wake_attempts::WakeAttemptState::Claimed,
            crate::wake_attempts::WakeAttemptState::Spawning,
        )
        .expect("spawning");
        db.set_wake_attempt_pid(&attempt_id, dead_pid)
            .expect("set dead pid");
        db.transition_wake_attempt(
            &attempt_id,
            crate::wake_attempts::WakeAttemptState::Spawning,
            crate::wake_attempts::WakeAttemptState::Running,
        )
        .expect("running");

        reap_dead_pid_attempts(&db, "test-host", "[test]", 3);

        // watch_handled must be left in place -- signal stays permanently
        // handled for smugglr.
        let handled: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM watch_handled WHERE signal_id = ?1 AND repo_name = 'smugglr'",
                [&signal_id],
                |r| r.get(0),
            )
            .expect("count watch_handled");
        assert_eq!(handled, 1, "exhausted signal must stay handled");

        // A best-effort bullpen post must land on smugglr's own board.
        let posts = db.get_board_posts().expect("get board posts");
        assert!(
            posts
                .iter()
                .any(|p| p.repo == "smugglr" && p.text.contains("redelivery ABANDONED")),
            "exhausted redelivery must post a loud abandonment notice to smugglr's bullpen; got: {:?}",
            posts.iter().map(|p| (&p.repo, &p.text)).collect::<Vec<_>>()
        );
    }

    // -- delegated-work reaper (#778, card-free since #931) -------------------

    #[test]
    fn reap_delegated_work_clears_link_when_attempt_went_terminal() {
        let (db, _index, _dir) = test_storage();
        db.upsert_watch_heartbeat("test-host", 1, "0.1.0", 1, None)
            .expect("heartbeat");

        let work_item_id = "item-died";
        let attempt_id = uuid::Uuid::now_v7().to_string();
        db.enqueue_wake_attempt(&attempt_id, "test-persona", "test-repo", &[])
            .expect("enqueue");
        db.try_claim_wake_attempt(&attempt_id, "test-host")
            .expect("claim");
        db.set_wake_attempt_work_item(&attempt_id, work_item_id)
            .expect("link");
        db.record_wake_attempt_outcome(&attempt_id, "ok", "productive")
            .expect("terminal");

        reap_delegated_work(&db, "[test]");

        assert!(
            !db.work_item_is_live(work_item_id, DELEGATION_STALE_AFTER_SECS)
                .expect("liveness check"),
            "still not live after the sweep"
        );
        let attempt = db
            .get_wake_attempt(&attempt_id)
            .expect("get")
            .expect("attempt exists");
        assert!(
            attempt.work_item_id.is_none(),
            "a terminal attempt's link must be cleared by the sweep"
        );
    }

    #[test]
    fn reap_delegated_work_leaves_live_delegation_alone() {
        let (db, _index, _dir) = test_storage();
        db.upsert_watch_heartbeat("test-host", 1, "0.1.0", 1, None)
            .expect("heartbeat");

        let work_item_id = "item-running";
        let attempt_id = uuid::Uuid::now_v7().to_string();
        db.enqueue_wake_attempt(&attempt_id, "test-persona", "test-repo", &[])
            .expect("enqueue");
        db.try_claim_wake_attempt(&attempt_id, "test-host")
            .expect("claim");
        db.set_wake_attempt_work_item(&attempt_id, work_item_id)
            .expect("link");

        reap_delegated_work(&db, "[test]");

        let attempt = db
            .get_wake_attempt(&attempt_id)
            .expect("get")
            .expect("attempt exists");
        assert_eq!(
            attempt.work_item_id.as_deref(),
            Some(work_item_id),
            "a live delegation's link must not be touched"
        );
    }

    #[test]
    fn reap_delegated_work_clears_link_when_daemon_heartbeat_is_absent() {
        let (db, _index, _dir) = test_storage();
        // No upsert_watch_heartbeat call.

        let work_item_id = "item-no-heartbeat";
        let attempt_id = uuid::Uuid::now_v7().to_string();
        db.enqueue_wake_attempt(&attempt_id, "test-persona", "test-repo", &[])
            .expect("enqueue");
        db.try_claim_wake_attempt(&attempt_id, "test-host")
            .expect("claim");
        db.set_wake_attempt_work_item(&attempt_id, work_item_id)
            .expect("link");

        reap_delegated_work(&db, "[test]");

        let attempt = db
            .get_wake_attempt(&attempt_id)
            .expect("get")
            .expect("attempt exists");
        assert!(
            attempt.work_item_id.is_none(),
            "an in-flight attempt with no daemon heartbeat is still not verifiably live"
        );
    }

    // -- deferred work-item reaper (#934) ---------------------------------------

    #[test]
    fn reap_deferred_work_items_clears_and_wakes_when_due() {
        let (db, _index, _dir) = test_storage();
        db.upsert_deferral("item-1", "test-repo", "2020-01-01T00:00:00+00:00", None)
            .expect("defer");

        reap_deferred_work_items(&db, "test-host", "[test]");

        assert!(
            db.get_deferral("item-1").unwrap().is_none(),
            "a due deferral must be cleared"
        );

        let pending = find_pending_signals(&db, "test-repo", &["test-repo".to_string()], None)
            .expect("find pending");
        assert_eq!(
            pending.len(),
            1,
            "the work item's owner must have a pending wake signal"
        );
        assert!(
            pending[0].1.contains("item-1"),
            "the wake signal must name the work item: {}",
            pending[0].1
        );
    }

    #[test]
    fn reap_deferred_work_items_leaves_future_wake_at_alone() {
        let (db, _index, _dir) = test_storage();
        db.upsert_deferral("item-1", "test-repo", "2099-01-01T00:00:00+00:00", None)
            .expect("defer");

        reap_deferred_work_items(&db, "test-host", "[test]");

        assert!(
            db.get_deferral("item-1").unwrap().is_some(),
            "a not-yet-due deferral must be left alone"
        );
        let pending = find_pending_signals(&db, "test-repo", &["test-repo".to_string()], None)
            .expect("find pending");
        assert!(
            pending.is_empty(),
            "no wake signal should be sent before wake_at"
        );
    }

    /// Same self-address-author caveat as `reap_deferred_cards_wakes_owner_even_when_to_repo_is_legion`
    /// (#816/#817): the wake signal's author must never collide with the
    /// deferral's own owning repo, since `find_pending_signals` drops any
    /// signal whose author equals the repo being polled.
    #[test]
    fn reap_deferred_work_items_wakes_owner_even_when_repo_is_legion() {
        let (db, _index, _dir) = test_storage();
        db.upsert_deferral("item-1", "legion", "2020-01-01T00:00:00+00:00", None)
            .expect("defer");

        reap_deferred_work_items(&db, "test-host", "[test]");

        let pending = find_pending_signals(&db, "legion", &["legion".to_string()], None)
            .expect("find pending");
        assert_eq!(
            pending.len(),
            1,
            "a deferral owned by 'legion' must still wake its owner; got: {pending:?}"
        );
    }

    /// Full `tick_health` integration, mirroring
    /// `watch_loop_tick_health_wakes_due_deferred_card` for the card-free
    /// path: proves the sweep is actually wired into the health tick.
    #[test]
    fn watch_loop_tick_health_wakes_due_deferred_work_item() {
        let (db, _index, _dir) = test_storage();
        db.upsert_deferral("item-1", "test-repo", "2020-01-01T00:00:00+00:00", None)
            .expect("defer");

        let mut state = test_watch_loop(db, "[legion test]");
        state.tick_health();

        assert!(state.db.get_deferral("item-1").unwrap().is_none());

        let pending =
            find_pending_signals(&state.db, "test-repo", &["test-repo".to_string()], None)
                .expect("find pending");
        assert_eq!(
            pending.len(),
            1,
            "tick_health must wake the deferred work item's owner"
        );
    }
}
