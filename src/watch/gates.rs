//! Per-cycle spawn gates and the poll cycle itself: the persona wake-lease
//! gate and the subscription-quota panic stop.
//!
//! Card auto-unblock (`check_auto_unblock`) used to run here too: it
//! advanced state (unblocking a `Blocked` card on a matching completion
//! announcement) but never refused anything, so it was not itself a gate.
//! #931 removed it along with the rest of the card surface -- `Blocked`
//! died with `CardStatus`, and #934's classification never carried
//! "blocking" forward as legion-only state the way it did defer,
//! delegated-wake-liveness, and sanction/autonomy.

use std::time::Duration;

use crate::db::Database;
use crate::error::Result;

use super::config::WatchConfig;
use super::locks::{CooldownTracker, SessionLockTracker};
use super::signals::{
    build_wake_prompt, find_pending_signals, is_wake_worthy, resolved_ask_id,
    resolved_ask_is_authentic,
};
use super::spawn::{SpawnMode, spawn_agent};
use super::tracker::AgentTracker;
use super::wake_cap_reached;

/// Whether this signal wakes `repo_name`: the verb-driven gate (#404), plus
/// the conditional answer-wake (#949).
///
/// An `answer` is `Record`-shaped and never wake-worthy on its own -- a
/// fire-and-forget answer to an agent with no tracked ask stays silent. But
/// the asker is the one party known to be BLOCKED on the reply, and today it
/// is the only party the wake gate cannot page. So an `answer` wakes when it
/// carries a send-time `resolves:<ask-id>` stamp naming a wake-worthy ask
/// this repo itself authored. That bounds answer-wakes by the questions the
/// sleeper asked.
///
/// Kept out of the manifest deliberately: `status.rs` buckets the "WHAT
/// CHANGED" list by exact equality against `VerbShape::Record`, so moving
/// `answer` off `Record` would silently drop every answer from that view.
/// Precedent for a single-verb special case here is `check_auto_unblock`
/// above (`announce` + a `completed:` marker).
///
/// `repo_name` is the watch entry's own name rather than `recipient()`: this
/// runs after the delegated-entry skip, past which `is_delegated()` false
/// makes the two equal by definition (`src/watch/config.rs`), and the ask
/// row's `repo` column is the author repo's own name.
fn signal_wakes_repo(db: &Database, repo_name: &str, text: &str) -> bool {
    if is_wake_worthy(text) {
        return true;
    }
    let Some(ask_id) = resolved_ask_id(text) else {
        return false;
    };
    // Fail CLOSED: an uncertain authenticity check must never spawn. A
    // missed wake costs latency; an unauthenticated one costs a session
    // any sender could conjure with a hand-typed --details.
    resolved_ask_is_authentic(db, &ask_id, repo_name).unwrap_or(false)
}

/// Cluster-wide persona wake lease gate. When present, watch will try to
/// acquire a lease for each signal before spawning; if every lease is already
/// held, the spawn is skipped (another node or session is handling it).
pub struct PersonaLeaseGate<'a> {
    pub db: &'a Database,
    pub host: &'a str,
    pub ttl: Duration,
}

/// Run a single poll cycle across all configured repos.
///
/// Returns the number of agents spawned in this cycle.
#[allow(clippy::too_many_arguments)]
pub fn poll_cycle(
    db: &Database,
    config: &WatchConfig,
    cooldown: &mut CooldownTracker,
    tracker: &mut AgentTracker,
    session_locks: Option<&SessionLockTracker>,
    lease_gate: Option<&PersonaLeaseGate<'_>>,
    since: Option<&str>,
    spawn_mode: SpawnMode,
) -> Result<u32> {
    let mut spawned: u32 = 0;

    for repo in &config.repos {
        let recipient = repo.recipient();
        let names = repo.wake_addresses();
        let signals = find_pending_signals(db, &repo.name, &names, since)?;
        if signals.is_empty() {
            continue;
        }

        // #849: a delegated entry never spawns. `agent` names the persona that
        // MAINTAINS this repo, and that persona wakes in its OWN workdir -- so
        // this entry is not a wake target at all, whatever the wake source
        // (directed signal, broadcast, or a file change that landed a signal).
        // The skip lands before the lease and wake_attempt work below on
        // purpose: a delegated entry that acquired the persona lease would win
        // the race against the owner's entry and wake the owner's persona in
        // the WRONG workdir. Not competing for the lease is what makes a
        // directed `@owner` signal wake exactly once, in the owner's workdir.
        //
        // The batch IS marked handled under THIS repo's own name, and that is
        // local bookkeeping rather than a cluster decision. `watch_handled` is
        // a HOST-LOCAL table: it carries no `updated_at`/`deleted_at`
        // (src/db/board.rs) and is not one of the four tables on the sync wire
        // (reflections, cards, schedules, persona_wake_leases --
        // src/sync_actor.rs), so a mark written here can never travel to a peer
        // whose watch.toml does not delegate this repo. It is keyed
        // (signal_id, repo_name), and the `find_pending_signals` above joins on
        // that exact pair (via `get_unhandled_signals_for_repo`), so the mark
        // retires ONLY this entry's pending copy: the owner's copy lives under
        // the OWNER's repo_name, untouched, and the owner's wake is unaffected.
        // Retiring it is the point -- nothing else ever drains a delegated
        // entry's copy, so an unmarked row would sit pending until the 7-day
        // signal TTL, re-announcing this skip on every poll. The mark is also
        // anti-replay: if the operator later drops `agent` and un-delegates the
        // repo, week-old signals must not suddenly wake it. Same keying
        // doctrine as the post-spawn mark below -- keyed by `repo.name`
        // (stable) so future `agent=` edits do not replay.
        //
        // The mark is not permanent: the retention sweep in src/watch/mod.rs
        // calls `prune_watch_handled` (src/db/board.rs) to drop rows past
        // `retention_days`. If that is ever tuned below the signal TTL, a
        // pruned mark resurrects this pending copy for exactly one cycle --
        // one log line, then re-marked here. Self-healing, so the prune needs
        // no delegation-aware special case.
        if repo.is_delegated() {
            eprintln!(
                "[legion watch] skipping wake for delegated repo '{}' (owner: {})",
                repo.name, recipient
            );
            for (id, _, _) in &signals {
                if let Err(e) = db.mark_signal_handled_for_repo(id, &repo.name) {
                    eprintln!(
                        "[legion watch] failed to mark delegated-skip signal {} as handled for {}: {}",
                        id, repo.name, e
                    );
                }
            }
            continue;
        }

        if cooldown.is_cooling_down(&repo.name) {
            continue;
        }

        if let Some(locks) = session_locks
            && let Some(pid) = locks.active_pid(&repo.name)
        {
            eprintln!(
                "[legion watch] skipping {}: active session (pid {})",
                repo.name, pid
            );
            continue;
        }

        // Verb-driven wake gate (#404), plus the conditional answer-wake
        // (#949) -- see `signal_wakes_repo`. Only spawn when at least one
        // signal wakes this repo. Informational signals targeting this repo
        // (announce/ack/info/review-without-request, and any answer that
        // resolves nothing this repo asked) are marked handled here so they
        // do not re-poll forever; they remain visible via `legion bullpen`
        // and were already delivered to live sessions by the channel push.
        //
        // The gate stays at this exact point in the cycle so an
        // answer-triggered wake inherits every guard a classic wake already
        // has: the delegated skip, the cooldown, and the live-session lock
        // all run above, and the concurrent-wake cap runs below.
        if !signals
            .iter()
            .any(|(_, text, _)| signal_wakes_repo(db, &repo.name, text))
        {
            for (id, _, _) in &signals {
                if let Err(e) = db.mark_signal_handled_for_repo(id, &repo.name) {
                    eprintln!(
                        "[legion watch] failed to mark informational signal {} as handled for {}: {}",
                        id, repo.name, e
                    );
                }
            }
            continue;
        }

        // #598: concurrent-wake cap. This repo has a wake-worthy signal and
        // would spawn, but a single `@all` broadcast otherwise fans out to
        // every watched repo in one cycle. Once in-flight wakes reach the cap,
        // stop spawning and let later polls drain the rest as running agents
        // finish. We `break` rather than `continue` because every remaining
        // wake-worthy repo would hit the same ceiling. Informational-only repos
        // that appear later in the list are left pending one extra poll cycle --
        // acceptable at the default 30s interval. Deferred wake-worthy repos
        // acquire no lease and are not marked handled, so they re-poll next
        // cycle.
        let active_wakes = tracker.active_count();
        if wake_cap_reached(active_wakes, config.max_concurrent_wakes) {
            eprintln!(
                "[legion watch] concurrent-wake cap reached ({} in flight >= {}); \
                 deferring remaining wakes to next poll",
                active_wakes, config.max_concurrent_wakes
            );
            break;
        }

        // Try to acquire a persona wake lease for each signal. Policy:
        // - All acquires fail  -> another node/session is already handling
        //   every one of these signals; skip this spawn entirely.
        // - Any acquire succeeds -> proceed with the spawn. The held leases
        //   travel with the child and are released on reap.
        let held_leases = match lease_gate {
            Some(gate) => {
                let mut acquired: Vec<(String, String)> = Vec::new();
                let mut skipped: Vec<String> = Vec::new();
                for (id, _, _) in &signals {
                    match gate
                        .db
                        .try_acquire_persona_lease(recipient, id, gate.host, gate.ttl)
                    {
                        Ok(true) => acquired.push((recipient.to_string(), id.clone())),
                        Ok(false) => skipped.push(id.clone()),
                        Err(e) => eprintln!(
                            "[legion watch] lease acquire error for {}/{}: {}",
                            recipient, id, e
                        ),
                    }
                }
                if acquired.is_empty() {
                    eprintln!(
                        "[legion watch] skipping {}: persona {} lease held elsewhere ({} signal(s))",
                        repo.name,
                        recipient,
                        skipped.len()
                    );
                    // Do NOT mark signals handled here. The lease TTL is the
                    // authoritative signal for "is the holder still alive."
                    // If the holder crashes before spawning, its lease ages
                    // out and the next poll on this host can try again. Local
                    // handled-marking would turn a crashed peer into a
                    // permanently-lost signal from our perspective.
                    continue;
                }
                acquired
            }
            None => Vec::new(),
        };

        eprintln!(
            "[legion watch] {} signal(s) for {} -- waking agent",
            signals.len(),
            repo.name
        );

        let prompt = build_wake_prompt(recipient, &signals);

        if spawned > 0 && config.stagger_secs > 0 {
            std::thread::sleep(Duration::from_secs(config.stagger_secs));
        }

        // #491: enqueue + claim a wake_attempts row before spawning so
        // peer nodes have a cluster-visible work item. Enqueue or
        // claim failure does not block the spawn (the lease layer is
        // still the authoritative mutex this session); the attempt
        // row becomes None and the reaper skips outcome recording.
        let signal_ids_vec: Vec<String> = signals.iter().map(|(id, _, _)| id.clone()).collect();
        let track_host_pre = lease_gate.map(|g| g.host.to_string()).unwrap_or_default();
        let attempt_id: Option<String> = {
            let candidate = uuid::Uuid::now_v7().to_string();
            match db.enqueue_wake_attempt(&candidate, recipient, &repo.name, &signal_ids_vec) {
                Ok(()) => match db.try_claim_wake_attempt(&candidate, &track_host_pre) {
                    Ok(true) => Some(candidate),
                    Ok(false) => {
                        eprintln!(
                            "[legion watch] wake_attempt {} not claimed (peer race?); proceeding anyway",
                            candidate
                        );
                        None
                    }
                    Err(e) => {
                        eprintln!(
                            "[legion watch] wake_attempt claim error: {} -- proceeding",
                            e
                        );
                        None
                    }
                },
                Err(e) => {
                    eprintln!(
                        "[legion watch] wake_attempt enqueue error: {} -- proceeding",
                        e
                    );
                    None
                }
            }
        };

        match spawn_agent(&repo.workdir, &prompt, spawn_mode, attempt_id.as_deref()) {
            Ok(child) => {
                let child_pid = child.id();
                // Mark ALL signals as handled for THIS repo (per-repo tracking).
                // This includes @all broadcasts -- each repo marks its own copy,
                // so other repos still see the signal on their next poll.
                // Keyed by `repo.name` (stable) so future `agent=` edits do not replay.
                for (id, _, _) in &signals {
                    if let Err(e) = db.mark_signal_handled_for_repo(id, &repo.name) {
                        eprintln!(
                            "[legion watch] failed to mark signal {} as handled for {}: {}",
                            id, repo.name, e
                        );
                    }
                }
                if let Some(locks) = session_locks
                    && let Err(e) = locks.record_spawn(&repo.name, child_pid)
                {
                    eprintln!(
                        "[legion watch] failed to write session lock for {}: {}",
                        repo.name, e
                    );
                }
                // #490: advance the wake_attempt FSM through the spawn
                // path. Errors logged + not propagated (terminal state
                // recording in reap is the load-bearing write).
                if let Some(ref aid) = attempt_id {
                    use crate::wake_attempts::WakeAttemptState::{Claimed, Running, Spawning};
                    if let Err(e) = db.set_wake_attempt_pid(aid, child_pid) {
                        eprintln!("[legion watch] set_wake_attempt_pid {}: {}", aid, e);
                    }
                    if let Err(e) = db.transition_wake_attempt(aid, Claimed, Spawning) {
                        eprintln!("[legion watch] transition Claimed->Spawning {}: {}", aid, e);
                    }
                    // Print submits its prompt as an argv, so the turn is
                    // already underway -- advance to Running now. Pty defers
                    // Spawning->Running to drive_submit_confirmation (#649),
                    // which fires it only once the ring buffer confirms the
                    // bracketed-paste prompt actually submitted.
                    if spawn_mode == SpawnMode::Print
                        && let Err(e) = db.transition_wake_attempt(aid, Spawning, Running)
                    {
                        eprintln!("[legion watch] transition Spawning->Running {}: {}", aid, e);
                    }
                }
                let track_host = track_host_pre;
                let session_id = uuid::Uuid::now_v7().to_string();
                let spawn_at = chrono::Utc::now().to_rfc3339();
                let signal_ids = signal_ids_vec;
                tracker.track(
                    repo.name.clone(),
                    child,
                    held_leases,
                    track_host,
                    session_id,
                    spawn_at,
                    signal_ids,
                    attempt_id,
                );
                cooldown.record_wake(&repo.name);
                spawned += 1;
                eprintln!("[legion watch] spawned agent for {}", repo.name);
            }
            Err(e) => {
                // Spawn failed -- release any leases we acquired so another
                // node/poll cycle can retry. Missing release would block
                // re-wakes for a full TTL.
                if let Some(gate) = lease_gate {
                    for (persona, sig_id) in &held_leases {
                        if let Err(re) = gate.db.release_persona_lease(persona, sig_id) {
                            eprintln!(
                                "[legion watch] failed to release lease {}/{} after spawn failure: {}",
                                persona, sig_id, re
                            );
                        }
                    }
                }
                // #490: mark the claimed wake_attempt as abandoned so the
                // FSM does not show a leaked Claimed row. Best-effort.
                if let Some(ref aid) = attempt_id {
                    use crate::wake_attempts::WakeAttemptState::{Abandoned, Claimed};
                    if let Err(re) = db.transition_wake_attempt(aid, Claimed, Abandoned) {
                        eprintln!(
                            "[legion watch] failed to abandon wake_attempt {} after spawn failure: {}",
                            aid, re
                        );
                    }
                }
                eprintln!("[legion watch] spawn failed for {}: {}", repo.name, e);
            }
        }
    }

    Ok(spawned)
}

/// Pure predicate: does this sample cross the panic threshold? Missing
/// pct values are treated as below-threshold so a partially-populated
/// sample (e.g. one window not yet known) cannot fire panic on its own.
fn sample_crosses_threshold(
    sample: &crate::statusline::RateLimitSample,
    threshold_pct: f64,
) -> bool {
    sample.five_hour_pct.is_some_and(|p| p >= threshold_pct)
        || sample.seven_day_pct.is_some_and(|p| p >= threshold_pct)
}

/// Gate for subscription-quota panic-stop. When the most recent rate-limit
/// sample for this host shows either the 5-hour or 7-day window at or above
/// the configured threshold, watch enters panic mode and skips spawn
/// cycles. A single bullpen post fires on the healthy -> panic edge; a
/// matching post fires on the panic -> healthy edge.
///
/// Per-host (not cluster-wide): a peer node burning its cap should not
/// gate this node's spawns. Each watch instance reads its own samples.
///
/// DB read failures return `false` (do not halt watch). A transient query
/// error is not a reason to enter panic, and a stuck panic state would
/// itself be a denial-of-service against the operator's mesh.
pub struct QuotaPanicGate {
    threshold_pct: f64,
    host: String,
    post_repo: String,
    in_panic: bool,
}

impl QuotaPanicGate {
    pub fn new(threshold_pct: f64, host: String, post_repo: String) -> Self {
        Self {
            threshold_pct,
            host,
            post_repo,
            in_panic: false,
        }
    }

    /// True when the most recent sample for this host crosses the
    /// threshold. DB errors and absent samples both return `false`.
    pub fn quota_panic_active(&self, db: &Database) -> bool {
        match db.latest_rate_limit_sample_for_host(&self.host) {
            Ok(Some(sample)) => sample_crosses_threshold(&sample, self.threshold_pct),
            Ok(None) => false,
            Err(e) => {
                eprintln!(
                    "[legion watch] quota panic check error: {} -- treating as healthy",
                    e
                );
                false
            }
        }
    }

    /// Evaluate the gate, emit a single bullpen post on each state edge,
    /// and return the current panic state. Callers skip the spawn cycle
    /// when this returns `true`.
    ///
    /// Edge state (`in_panic`) only advances after the bullpen post lands.
    /// On post failure the flag stays stale so the next poll retries the
    /// same edge -- otherwise a single dropped post would leave peers
    /// believing this node is still down (or still up) until the inverse
    /// transition forces a re-announce.
    pub fn check_and_post(&mut self, db: &Database) -> bool {
        let active = self.quota_panic_active(db);
        if active != self.in_panic {
            let post = if active {
                format!(
                    "QUOTA PANIC on {}: rate-limit sample crossed {:.1}% threshold. \
                     `legion watch` is halting spawn cycles on this host until usage \
                     drops back below the threshold.",
                    self.host, self.threshold_pct,
                )
            } else {
                format!(
                    "QUOTA RECOVERED on {}: rate-limit sample fell below the {:.1}% \
                     threshold. `legion watch` is resuming spawn cycles.",
                    self.host, self.threshold_pct,
                )
            };
            match db.insert_reflection_with_meta(
                &self.post_repo,
                &post,
                "team",
                &crate::db::ReflectionMeta::default(),
            ) {
                Ok(_) => {
                    self.in_panic = active;
                }
                Err(e) => {
                    eprintln!(
                        "[legion watch] quota panic bullpen post failed (edge {} -> {}): {} \
                         -- will retry on next poll; mesh state may be stale",
                        self.in_panic, active, e
                    );
                }
            }
        }
        active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_storage;
    use crate::watch::{SpawnedChild, WatchRepoConfig};

    #[test]
    fn poll_cycle_skips_when_session_lock_is_active() {
        // Integration-style gate test: with a live session lock in place for a
        // repo, poll_cycle must not spawn a second agent even when the repo
        // has fresh signals waiting and cooldown is idle.
        let (db, _index, data_dir) = test_storage();

        let config = WatchConfig {
            repos: vec![WatchRepoConfig {
                name: "legion".to_string(),
                workdir: "/tmp".to_string(),
                agent: None,
                broadcast_tags: Vec::new(),
                extra: toml::Table::new(),
            }],
            ..WatchConfig::default()
        };

        db.insert_reflection("kelex", "@legion question:help", "team")
            .expect("insert signal");

        let locks = SessionLockTracker::new(data_dir.path(), 3600);
        locks
            .record_spawn("legion", std::process::id())
            .expect("record lock");

        let mut cooldown = CooldownTracker::new(0, None, None);
        let mut tracker = AgentTracker::new();
        let spawned = poll_cycle(
            &db,
            &config,
            &mut cooldown,
            &mut tracker,
            Some(&locks),
            None,
            None,
            SpawnMode::Print,
        )
        .expect("poll");
        assert_eq!(
            spawned, 0,
            "active session lock must block a second wake for the same repo"
        );
    }

    #[test]
    fn poll_cycle_skips_when_persona_lease_is_held() {
        // Cluster-gate test: a peer node has already acquired the lease for
        // the inbound signal. This host's poll_cycle must skip the wake
        // entirely and mark the signal handled so it does not retry.
        let (db, _index, _dir) = test_storage();

        let config = WatchConfig {
            repos: vec![WatchRepoConfig {
                name: "legion".to_string(),
                workdir: "/tmp".to_string(),
                agent: None,
                broadcast_tags: Vec::new(),
                extra: toml::Table::new(),
            }],
            ..WatchConfig::default()
        };

        let signal = db
            .insert_reflection("kelex", "@legion question:help", "team")
            .expect("insert signal");

        // Simulate a peer holding the lease for this (persona, signal).
        let held = db
            .try_acquire_persona_lease("legion", &signal.id, "peer-host", Duration::from_secs(3600))
            .expect("peer acquire");
        assert!(held, "peer must be able to acquire a free lease");

        let gate = PersonaLeaseGate {
            db: &db,
            host: "this-host",
            ttl: Duration::from_secs(3600),
        };
        let mut cooldown = CooldownTracker::new(0, None, None);
        let mut tracker = AgentTracker::new();
        let spawned = poll_cycle(
            &db,
            &config,
            &mut cooldown,
            &mut tracker,
            None,
            Some(&gate),
            None,
            SpawnMode::Print,
        )
        .expect("poll");
        assert_eq!(
            spawned, 0,
            "persona lease held by peer must block this host from spawning"
        );

        // The lease holder (peer) must still own the lease after the skip --
        // the failed local acquire must not have disturbed peer's row.
        let listed = db.list_persona_leases(Some("legion")).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].acquired_by_host, "peer-host");
    }

    // -- Delegated entries never wake (#849) -----------------------------------

    /// A workdir that cannot exist, so `spawn_agent` fails at the `chdir`
    /// instead of launching a real `claude` session.
    ///
    /// Every delegated-entry test below uses it as a safety net. Without it, a
    /// regression that removes the guard does not just fail the test -- it
    /// launches a live billed wake session per test run, in `/tmp`, with the
    /// prompt of whatever signal the test seeded. Verified during development:
    /// disabling the guard with a `/tmp` workdir spawned real `claude --print`
    /// children that had to be killed by hand. The assertions below therefore
    /// do NOT lean on `spawned == 0` (a bogus workdir gives that for free);
    /// they lean on the wake_attempt row, which is enqueued immediately before
    /// `spawn_agent` with nothing in between that could skip.
    const UNSPAWNABLE_WORKDIR: &str = "/nonexistent/legion-849/spawn-must-fail";

    /// A watch.toml entry whose `agent` names another persona.
    fn delegated_repo(name: &str, owner: &str) -> WatchRepoConfig {
        WatchRepoConfig {
            name: name.to_string(),
            workdir: UNSPAWNABLE_WORKDIR.to_string(),
            agent: Some(owner.to_string()),
            broadcast_tags: Vec::new(),
            extra: toml::Table::new(),
        }
    }

    /// Assert the delegated entry was skipped before it could touch any of the
    /// wake machinery: no persona lease taken from `owner`, and no wake_attempt
    /// row enqueued. The attempt table is the tight one -- `enqueue_wake_attempt`
    /// is the last statement before `spawn_agent`, so an empty table means the
    /// loop never reached the spawn call at all.
    ///
    /// Every caller must run `poll_cycle` WITH a `PersonaLeaseGate`. Without one
    /// the lease half of this assertion is vacuous -- no lease can be taken, so
    /// it would still pass against a guard that had been moved below the acquire,
    /// which is precisely the regression #849 is about.
    fn assert_no_wake_machinery_touched(db: &Database, owner: &str) {
        let leases = db.list_persona_leases(Some(owner)).expect("list leases");
        assert!(
            leases.is_empty(),
            "delegated entry must not acquire the persona lease it would steal from {owner}"
        );

        let attempts = db.recent_wake_attempts(50).expect("list attempts");
        assert!(
            attempts.is_empty(),
            "delegated entry must be skipped before the wake_attempt enqueue that \
             immediately precedes spawn_agent"
        );
    }

    /// Assert the delegated skip retired its OWN pending copy and only its own.
    ///
    /// Both halves are load-bearing, and neither is sufficient alone. The first
    /// is the point of the mark: nothing else drains a delegated entry's copy,
    /// so an unmarked row re-announces the skip every poll until the signal's
    /// 7-day TTL. The second is what proves the mark is SCOPED -- the owner's
    /// copy is keyed under the owner's own `repo_name`, and a guard that marked
    /// under `recipient()` instead of `repo.name` would retire it and starve
    /// the real wake. `find_pending_signals` joins `watch_handled` on
    /// (signal_id, repo_name), so only the paired query can see the difference.
    fn assert_delegated_copy_retired(db: &Database, delegated: &str, owner: &str) {
        let names: Vec<String> = vec![owner.to_string()];

        let retired = find_pending_signals(db, delegated, &names, None).expect("delegated pending");
        assert!(
            retired.is_empty(),
            "the delegated skip must retire {delegated}'s pending copy -- nothing else drains it"
        );

        let owner_pending = find_pending_signals(db, owner, &names, None).expect("owner pending");
        assert_eq!(
            owner_pending.len(),
            1,
            "{owner}'s copy is keyed under its own repo_name and must survive the skip"
        );
    }

    #[test]
    fn poll_cycle_skips_wake_for_delegated_entry_with_directed_signal() {
        // `ledger` is maintained by `platform`, so a directed @platform signal
        // must wake platform in PLATFORM's workdir -- never ledger's entry.
        let (db, _index, _dir) = test_storage();

        let config = WatchConfig {
            repos: vec![delegated_repo("ledger", "platform")],
            ..WatchConfig::default()
        };

        db.insert_reflection("kelex", "@platform question:can you look at this", "team")
            .expect("insert signal");

        let gate = PersonaLeaseGate {
            db: &db,
            host: "this-host",
            ttl: Duration::from_secs(3600),
        };
        let mut cooldown = CooldownTracker::new(0, None, None);
        let mut tracker = AgentTracker::new();
        let spawned = poll_cycle(
            &db,
            &config,
            &mut cooldown,
            &mut tracker,
            None,
            Some(&gate),
            None,
            SpawnMode::Print,
        )
        .expect("poll");

        assert_eq!(spawned, 0, "a delegated entry must never spawn a session");

        // The load-bearing assertions: the skip happens BEFORE lease acquisition
        // and before the wake_attempt enqueue. A skip that ran AFTER the lease
        // acquire would still report `spawned == 0` while holding the lease the
        // owner's entry needs -- blocking the real wake for a full TTL. That is
        // the exact bug #849 is about, so it gets its own assertion.
        assert_no_wake_machinery_touched(&db, "platform");

        // The skip retires ledger's own pending copy. `watch_handled` is
        // host-local and keyed (signal_id, repo_name), so nothing else would
        // ever drain this row -- leaving it pending means re-announcing the
        // skip every poll until the 7-day TTL.
        assert_delegated_copy_retired(&db, "ledger", "platform");
    }

    #[test]
    fn poll_cycle_skips_wake_for_delegated_entry_on_broadcast() {
        // Broadcasts reach spawn through the same loop as directed signals, so
        // the guard must cover them too -- an @all fan-out must not wake a
        // delegated entry in the wrong workdir.
        let (db, _index, _dir) = test_storage();

        let config = WatchConfig {
            repos: vec![delegated_repo("ledger", "platform")],
            ..WatchConfig::default()
        };

        db.insert_reflection("kelex", "@all request -- wake everyone", "team")
            .expect("insert broadcast");

        // The lease gate is passed on purpose: without it the lease half of
        // assert_no_wake_machinery_touched could not fail.
        let gate = PersonaLeaseGate {
            db: &db,
            host: "this-host",
            ttl: Duration::from_secs(3600),
        };
        let mut cooldown = CooldownTracker::new(0, None, None);
        let mut tracker = AgentTracker::new();
        let spawned = poll_cycle(
            &db,
            &config,
            &mut cooldown,
            &mut tracker,
            None,
            Some(&gate),
            None,
            SpawnMode::Print,
        )
        .expect("poll");

        assert_eq!(
            spawned, 0,
            "a broadcast must not wake a delegated entry either"
        );
        assert_no_wake_machinery_touched(&db, "platform");
        // A broadcast copy is retired the same way a directed one is: the mark
        // is per (signal_id, repo_name), which is exactly the mechanism that
        // already lets one `@all` wake every non-delegated repo once.
        assert_delegated_copy_retired(&db, "ledger", "platform");
    }

    #[test]
    fn poll_cycle_skips_wake_for_delegated_entry_on_file_change_wake() {
        // A file-change wake is not a separate spawn path: `spawn_agent` has a
        // single call site in this loop, and a file change reaches it by landing
        // a signal that `find_pending_signals` picks up on the next poll. So the
        // file-change case is exercised the same way -- a wake-worthy signal
        // addressed to the owner, arriving at a delegated entry.
        let (db, _index, _dir) = test_storage();

        let config = WatchConfig {
            repos: vec![delegated_repo("ledger", "platform")],
            ..WatchConfig::default()
        };

        db.insert_reflection("watch", "@platform request -- ledger files changed", "team")
            .expect("insert file-change signal");

        // The lease gate is passed on purpose: without it the lease half of
        // assert_no_wake_machinery_touched could not fail.
        let gate = PersonaLeaseGate {
            db: &db,
            host: "this-host",
            ttl: Duration::from_secs(3600),
        };
        let mut cooldown = CooldownTracker::new(0, None, None);
        let mut tracker = AgentTracker::new();
        let spawned = poll_cycle(
            &db,
            &config,
            &mut cooldown,
            &mut tracker,
            None,
            Some(&gate),
            None,
            SpawnMode::Print,
        )
        .expect("poll");

        assert_eq!(
            spawned, 0,
            "a file-change-driven wake must not spawn a delegated entry"
        );
        assert_no_wake_machinery_touched(&db, "platform");
        assert_delegated_copy_retired(&db, "ledger", "platform");
    }

    #[test]
    fn poll_cycle_does_not_skip_a_self_owned_entry() {
        // The negative control for the guard: an entry whose `agent` equals its
        // own name is NOT delegated, so it must run the full wake path.
        //
        // Same UNSPAWNABLE_WORKDIR trick as the delegated tests, so this control
        // does not launch a live session either: `spawn_agent` fails at the
        // chdir and poll_cycle takes its Err arm. `spawned` is therefore 0 for
        // BOTH this entry and a delegated one -- the distinguishing evidence is
        // the wake_attempt row, enqueued only once the delegation guard has let
        // the entry through. This is the mirror of
        // `assert_no_wake_machinery_touched`: there the table must be empty,
        // here it must not be.
        let (db, _index, _dir) = test_storage();

        let config = WatchConfig {
            stagger_secs: 0,
            repos: vec![WatchRepoConfig {
                name: "platform".to_string(),
                workdir: UNSPAWNABLE_WORKDIR.to_string(),
                agent: Some("platform".to_string()),
                broadcast_tags: Vec::new(),
                extra: toml::Table::new(),
            }],
            ..WatchConfig::default()
        };

        db.insert_reflection("kelex", "@platform question:can you look at this", "team")
            .expect("insert signal");

        let mut cooldown = CooldownTracker::new(0, None, None);
        let mut tracker = AgentTracker::new();
        let spawned = poll_cycle(
            &db,
            &config,
            &mut cooldown,
            &mut tracker,
            None,
            None,
            None,
            SpawnMode::Print,
        )
        .expect("poll");
        assert_eq!(spawned, 0, "the bogus workdir must fail the spawn");

        let attempts = db.recent_wake_attempts(50).expect("list attempts");
        assert_eq!(
            attempts.len(),
            1,
            "a self-owned entry must reach the wake_attempt enqueue -- \
             the delegation guard must not swallow it"
        );
        assert_eq!(attempts[0].repo_name, "platform");
    }

    #[test]
    fn poll_cycle_wakes_only_the_owner_when_both_entries_are_configured() {
        // Co-presence: the delegated entry and its owner are BOTH in watch.toml,
        // which is the shape a real host has, and the shape the single-entry
        // tests above cannot express. One directed @platform signal, two entries
        // that both match it, one real lease gate.
        //
        // What this proves: exactly one entry reaches the wake machinery, and it
        // is the owner's. The wake_attempt table carries both halves -- its
        // length says how many entries got through, and its `repo_name` says
        // which one.
        //
        // Which mutations it discriminates:
        // - guard deleted    -> ledger runs the full path first, so there are
        //                       TWO attempt rows (ledger's spawn fails on the
        //                       bogus workdir, which releases its lease and lets
        //                       platform re-acquire and enqueue its own).
        // - predicate inverted -> ONE attempt row, keyed to `ledger` instead of
        //                       `platform`.
        //
        // One caveat, stated because the alternative is a comment that claims
        // more than it checks. This test does NOT discriminate a guard moved
        // BELOW the lease acquire: `try_acquire_persona_lease` returns true for
        // a same-host re-acquire (src/db/wake.rs -- it reads the holder back and
        // compares it to `host`), so ledger taking the lease and skipping does
        // not lock platform out, and the attempt count stays 1. That mutation is
        // caught by `assert_no_wake_machinery_touched` in the single-entry tests,
        // where no second entry can paper over the stolen lease.
        //
        // Nor does it replay the production #849 incident, which was one wake in
        // the WRONG workdir. Reproducing that needs a SUCCESSFUL spawn, and the
        // whole suite deliberately forbids one (see UNSPAWNABLE_WORKDIR) rather
        // than bill a live session per test run.
        let (db, _index, _dir) = test_storage();

        let config = WatchConfig {
            // Explicit: the default is 15s, and a stagger sleep between two
            // entries would make this test pay for it.
            stagger_secs: 0,
            repos: vec![
                delegated_repo("ledger", "platform"),
                WatchRepoConfig {
                    name: "platform".to_string(),
                    workdir: UNSPAWNABLE_WORKDIR.to_string(),
                    agent: Some("platform".to_string()),
                    broadcast_tags: Vec::new(),
                    extra: toml::Table::new(),
                },
            ],
            ..WatchConfig::default()
        };

        db.insert_reflection("kelex", "@platform question:can you look at this", "team")
            .expect("insert signal");

        let gate = PersonaLeaseGate {
            db: &db,
            host: "this-host",
            ttl: Duration::from_secs(3600),
        };
        let mut cooldown = CooldownTracker::new(0, None, None);
        let mut tracker = AgentTracker::new();
        let spawned = poll_cycle(
            &db,
            &config,
            &mut cooldown,
            &mut tracker,
            None,
            Some(&gate),
            None,
            SpawnMode::Print,
        )
        .expect("poll");
        assert_eq!(spawned, 0, "the bogus workdir must fail both spawns");

        let attempts = db.recent_wake_attempts(50).expect("list attempts");
        assert_eq!(
            attempts.len(),
            1,
            "a directed @platform signal must reach the wake machinery exactly once \
             even though two configured entries match it"
        );
        assert_eq!(
            attempts[0].repo_name, "platform",
            "the entry that wakes must be the owner's, not the delegated one"
        );

        // Scoped marking, asserted directly on the table rather than through
        // the pending lookups. The delegated skip retires ledger's copy, so a
        // row must exist under 'ledger'; the row that must NOT exist is one
        // under 'platform'. That is the mark-under-recipient regression, and it
        // is invisible to the pending lookups because each joins watch_handled
        // on its own repo_name -- ledger's query cannot see a platform-keyed
        // row, and a platform-keyed row would silently starve the owner's wake.
        //
        // No other path can have written a platform row in this cycle: the
        // owner's post-spawn marking loop lives on the `Ok(child)` arm of the
        // spawn match, and this spawn fails at UNSPAWNABLE_WORKDIR. The Err arm
        // releases leases and abandons the wake_attempt, marking nothing. So
        // counting by repo_name is exact here, not merely indicative.
        assert_delegated_copy_retired(&db, "ledger", "platform");

        let ledger_marks: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM watch_handled WHERE repo_name = 'ledger'",
                [],
                |row| row.get(0),
            )
            .expect("count ledger marks");
        assert_eq!(
            ledger_marks, 1,
            "the delegated skip must mark the batch under its OWN repo_name"
        );

        let platform_marks: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM watch_handled WHERE repo_name = 'platform'",
                [],
                |row| row.get(0),
            )
            .expect("count platform marks");
        assert_eq!(
            platform_marks, 0,
            "a mark written under the RECIPIENT would retire the owner's copy \
             and starve the wake that is supposed to happen"
        );
    }

    #[test]
    fn poll_cycle_skips_cooling_repos() {
        let (db, _index, _dir) = test_storage();

        let config = WatchConfig {
            repos: vec![WatchRepoConfig {
                name: "legion".to_string(),
                workdir: "/tmp".to_string(),
                agent: None,
                broadcast_tags: Vec::new(),
                extra: toml::Table::new(),
            }],
            ..WatchConfig::default()
        };

        // Insert a signal
        db.insert_reflection("kelex", "@legion review:ready", "team")
            .expect("insert");

        // Pre-cool the repo
        let mut cooldown = CooldownTracker::new(300, None, None);
        cooldown.record_wake("legion");

        let mut tracker = AgentTracker::new();
        let spawned = poll_cycle(
            &db,
            &config,
            &mut cooldown,
            &mut tracker,
            None,
            None,
            None,
            SpawnMode::Print,
        )
        .expect("poll");
        assert_eq!(spawned, 0, "cooling repo should be skipped");
    }

    #[test]
    fn poll_cycle_caps_concurrent_wakes() {
        let (db, _index, _dir) = test_storage();

        // Three repos, each of which would wake on the @all broadcast below.
        let repos: Vec<WatchRepoConfig> = ["legion", "rafters", "smugglr"]
            .iter()
            .map(|name| WatchRepoConfig {
                name: name.to_string(),
                workdir: "/tmp".to_string(),
                agent: None,
                broadcast_tags: vec!["all".to_string()],
                extra: toml::Table::new(),
            })
            .collect();
        let config = WatchConfig {
            max_concurrent_wakes: 1,
            stagger_secs: 0,
            repos,
            ..WatchConfig::default()
        };

        // One wake-worthy @all broadcast every repo would otherwise wake on.
        db.insert_reflection("kelex", "@all request -- wake everyone", "team")
            .expect("insert broadcast");

        // Pre-seed the tracker so in-flight wakes already meet the cap of 1.
        // The dummy stands in for an already-running wake; poll_cycle never
        // reaps mid-cycle, so active_count() stays at 1 for the whole call.
        // With the cap met up front, the loop must break on the first
        // wake-worthy repo BEFORE spawning, so no real agent is launched.
        let mut tracker = AgentTracker::new();
        tracker.track(
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
        assert_eq!(tracker.active_count(), 1);

        let mut cooldown = CooldownTracker::new(300, None, None);
        let spawned = poll_cycle(
            &db,
            &config,
            &mut cooldown,
            &mut tracker,
            None,
            None,
            None,
            SpawnMode::Print,
        )
        .expect("poll");

        assert_eq!(
            spawned, 0,
            "cap already met by in-flight wakes; poll_cycle must spawn nothing this cycle"
        );

        // The deferred broadcast must remain pending (not marked handled) so a
        // later cycle re-polls it once a slot frees.
        let still_pending = find_pending_signals(
            &db,
            "legion",
            &["legion".to_string(), "all".to_string()],
            None,
        )
        .expect("pending lookup");
        assert_eq!(
            still_pending.len(),
            1,
            "deferred wake-worthy signal must stay pending for re-poll"
        );
    }

    // -- Conditional answer-wake (#949) ---------------------------------------

    /// One non-delegated `veneer` entry on the unspawnable workdir. Every
    /// answer-wake test below takes its wake/no-wake evidence from the
    /// `wake_attempts` table rather than from `spawned`, for the reason
    /// spelled out at `UNSPAWNABLE_WORKDIR`: a spawn that SUCCEEDS bills a
    /// live session per test run. `enqueue_wake_attempt` is the last
    /// statement before `spawn_agent` with no gate in between, so a row
    /// there means the wake gate let the batch through, and an empty table
    /// means it did not. `spawned == 0` is still asserted everywhere as the
    /// safety net that the workdir stayed unspawnable.
    fn veneer_config() -> WatchConfig {
        WatchConfig {
            stagger_secs: 0,
            repos: vec![WatchRepoConfig {
                name: "veneer".to_string(),
                workdir: UNSPAWNABLE_WORKDIR.to_string(),
                agent: None,
                broadcast_tags: Vec::new(),
                extra: toml::Table::new(),
            }],
            ..WatchConfig::default()
        }
    }

    fn poll_veneer(db: &Database, locks: Option<&SessionLockTracker>) -> u32 {
        let config = veneer_config();
        let mut cooldown = CooldownTracker::new(0, None, None);
        let mut tracker = AgentTracker::new();
        poll_cycle(
            db,
            &config,
            &mut cooldown,
            &mut tracker,
            locks,
            None,
            None,
            SpawnMode::Print,
        )
        .expect("poll")
    }

    /// The motivating case: veneer asked, slept, and rafters answered. The
    /// answer verb is `Record` and never joins the wake set, so the stamped
    /// `resolves` marker is the only thing that can page the asker.
    #[test]
    fn poll_cycle_wakes_repo_when_authentic_answer_resolves_pending_ask() {
        let (db, _index, _dir) = test_storage();
        let ask = db
            .insert_reflection("veneer", "@rafters question -- need X", "team")
            .expect("insert ask");
        db.insert_reflection(
            "rafters",
            &format!(
                "@veneer answer:resolved {{resolves: {}}} -- X is in tokens.css",
                ask.id
            ),
            "team",
        )
        .expect("insert answer");

        assert_eq!(
            poll_veneer(&db, None),
            0,
            "the bogus workdir must fail the spawn -- a test that really spawns \
             bills a live session per run"
        );

        let attempts = db.recent_wake_attempts(50).expect("list attempts");
        assert_eq!(
            attempts.len(),
            1,
            "an authentic answer must reach the wake machinery -- the asker is \
             the party blocked on it"
        );
        assert_eq!(attempts[0].repo_name, "veneer");
    }

    /// The regression this issue exists to prevent: a fire-and-forget answer
    /// to an agent with no tracked ask stays silent, exactly as any other
    /// Record-shaped signal does today.
    #[test]
    fn poll_cycle_answer_without_matching_ask_does_not_wake() {
        let (db, _index, _dir) = test_storage();
        db.insert_reflection(
            "rafters",
            "@veneer answer:resolved -- fyi, X is done",
            "team",
        )
        .expect("insert answer");

        // Pre-assertion, so an empty pending set after the poll cannot pass
        // vacuously: the answer really does reach veneer's queue first.
        let before =
            find_pending_signals(&db, "veneer", &["veneer".to_string()], None).expect("pending");
        assert_eq!(before.len(), 1, "the answer must be pending for veneer");

        assert_eq!(poll_veneer(&db, None), 0, "nothing may spawn here");

        let attempts = db.recent_wake_attempts(50).expect("list attempts");
        assert!(
            attempts.is_empty(),
            "an answer with no resolves marker must not wake anyone"
        );
        let pending =
            find_pending_signals(&db, "veneer", &["veneer".to_string()], None).expect("pending");
        assert!(
            pending.is_empty(),
            "it must be retired as informational, not left to re-poll forever"
        );
    }

    /// The authenticity check, pinned: a `resolves` id can be typed by hand,
    /// so a real id belonging to someone else's ask must not forge a wake.
    #[test]
    fn poll_cycle_rejects_forged_resolves_id() {
        let (db, _index, _dir) = test_storage();
        // A real, wake-worthy ask -- but smugglr asked it, not veneer.
        let others_ask = db
            .insert_reflection("smugglr", "@rafters question -- need Y", "team")
            .expect("insert third-party ask");
        db.insert_reflection(
            "rafters",
            &format!("@veneer answer:resolved {{resolves: {}}}", others_ask.id),
            "team",
        )
        .expect("insert forged answer");
        db.insert_reflection(
            "rafters",
            "@veneer answer:resolved {resolves: 01a0-no-such-reflection}",
            "team",
        )
        .expect("insert answer naming a nonexistent ask");

        // Pre-assertion, same reason as the sibling test above: an empty
        // attempts table proves the authenticity check REFUSED these two
        // only if they actually reached veneer's queue. Without it, broken
        // address matching would pass this test with
        // `resolved_ask_is_authentic` never having run. (The smugglr ask is
        // addressed @rafters, so it is not one of the two.)
        let before =
            find_pending_signals(&db, "veneer", &["veneer".to_string()], None).expect("pending");
        assert_eq!(
            before.len(),
            2,
            "both forged answers must be pending for veneer before the poll"
        );

        assert_eq!(poll_veneer(&db, None), 0, "nothing may spawn here");

        let attempts = db.recent_wake_attempts(50).expect("list attempts");
        assert!(
            attempts.is_empty(),
            "a hand-typed resolves id must not forge a wake, whether it names a \
             third repo's ask or nothing at all"
        );
    }

    /// The live-session guard needs no new code -- the `session_locks` gate
    /// runs above the wake check -- but an answer-wake must inherit it, so
    /// pin it rather than trust the ordering to survive a refactor.
    ///
    /// Unix-only, and for the same reason the session-lock tests in
    /// `locks.rs` are (see the note above
    /// `session_lock_record_spawn_overwrites_abandoned_lock`): the setup
    /// records a lock holding OUR OWN pid and needs it to read back as
    /// alive. `process_alive` shells out to `kill -0` on unix and returns a
    /// flat `false` on every other platform, so on Windows `active_pid`
    /// reports no holder, `poll_cycle` sails past the session-lock gate into
    /// the wake check, and the authentic answer enqueues the very
    /// wake_attempt row this test asserts is absent.
    ///
    /// The sibling `poll_cycle_skips_when_session_lock_is_active` sets up
    /// the identical scenario ungated, but asserts only `spawned == 0` --
    /// true on Windows whether or not the lock gate held, because the spawn
    /// fails there anyway. It passes vacuously, which is why `gates.rs`
    /// carried no cfg gate before this test asserted on the attempts table.
    #[cfg(unix)]
    #[test]
    fn poll_cycle_authentic_answer_does_not_wake_live_session() {
        let (db, _index, data_dir) = test_storage();
        let ask = db
            .insert_reflection("veneer", "@rafters question -- need X", "team")
            .expect("insert ask");
        db.insert_reflection(
            "rafters",
            &format!("@veneer answer:resolved {{resolves: {}}}", ask.id),
            "team",
        )
        .expect("insert answer");

        let locks = SessionLockTracker::new(data_dir.path(), 3600);
        locks
            .record_spawn("veneer", std::process::id())
            .expect("record lock");

        let spawned = poll_veneer(&db, Some(&locks));

        assert_eq!(spawned, 0, "a live session must not be woken");
        let attempts = db.recent_wake_attempts(50).expect("list attempts");
        assert!(
            attempts.is_empty(),
            "the session-lock gate runs before the wake check, so an authentic \
             answer must not even reach the wake machinery"
        );
        let pending =
            find_pending_signals(&db, "veneer", &["veneer".to_string()], None).expect("pending");
        assert_eq!(
            pending.len(),
            1,
            "the answer skipped for a live session stays pending for the next poll"
        );
    }

    // -- Quota panic gate (#484) ---------------------------------------------

    fn rate_sample(
        id: &str,
        host: &str,
        sampled_at: &str,
        five_hour_pct: Option<f64>,
        seven_day_pct: Option<f64>,
    ) -> crate::statusline::RateLimitSample {
        crate::statusline::RateLimitSample {
            id: id.to_string(),
            hostname: host.to_string(),
            session_id: "sess".to_string(),
            sampled_at: sampled_at.to_string(),
            five_hour_pct,
            five_hour_resets_at: None,
            seven_day_pct,
            seven_day_resets_at: None,
            model: None,
        }
    }

    fn bullpen_panic_post_count(db: &Database) -> usize {
        let posts = crate::board::bullpen(db, "legion").expect("read bullpen");
        posts
            .iter()
            .filter(|p| p.text.contains("QUOTA PANIC") || p.text.contains("QUOTA RECOVERED"))
            .count()
    }

    #[test]
    fn panic_active_when_sample_crosses_threshold() {
        let (db, _idx, _tmp) = test_storage();
        db.insert_rate_limit_sample(&rate_sample(
            "s1",
            "this-host",
            "2026-05-23T10:00:00Z",
            Some(99.5),
            Some(40.0),
        ))
        .unwrap();
        let gate = QuotaPanicGate::new(99.0, "this-host".to_string(), "legion".to_string());
        assert!(gate.quota_panic_active(&db));
    }

    #[test]
    fn panic_inactive_when_sample_below_threshold() {
        let (db, _idx, _tmp) = test_storage();
        db.insert_rate_limit_sample(&rate_sample(
            "s1",
            "this-host",
            "2026-05-23T10:00:00Z",
            Some(50.0),
            Some(60.0),
        ))
        .unwrap();
        let gate = QuotaPanicGate::new(99.0, "this-host".to_string(), "legion".to_string());
        assert!(!gate.quota_panic_active(&db));
    }

    #[test]
    fn panic_inactive_when_no_sample_yet() {
        let (db, _idx, _tmp) = test_storage();
        let gate = QuotaPanicGate::new(99.0, "this-host".to_string(), "legion".to_string());
        assert!(!gate.quota_panic_active(&db));
    }

    #[test]
    fn panic_inactive_for_other_hosts_sample() {
        // A peer node burning its cap must not gate this host.
        let (db, _idx, _tmp) = test_storage();
        db.insert_rate_limit_sample(&rate_sample(
            "s1",
            "other-host",
            "2026-05-23T10:00:00Z",
            Some(99.9),
            Some(99.9),
        ))
        .unwrap();
        let gate = QuotaPanicGate::new(99.0, "this-host".to_string(), "legion".to_string());
        assert!(!gate.quota_panic_active(&db));
    }

    #[test]
    fn panic_seven_day_window_independently_trips_gate() {
        let (db, _idx, _tmp) = test_storage();
        db.insert_rate_limit_sample(&rate_sample(
            "s1",
            "this-host",
            "2026-05-23T10:00:00Z",
            Some(10.0),
            Some(99.5),
        ))
        .unwrap();
        let gate = QuotaPanicGate::new(99.0, "this-host".to_string(), "legion".to_string());
        assert!(gate.quota_panic_active(&db));
    }

    #[test]
    fn panic_five_hour_window_independently_trips_gate() {
        // Symmetric to the 7d test: a single high 5h reading with a quiet
        // 7d window must still trip the gate. Guards against a refactor that
        // drops the 5h disjunct or swaps `||` for `&&` in the predicate.
        let (db, _idx, _tmp) = test_storage();
        db.insert_rate_limit_sample(&rate_sample(
            "s1",
            "this-host",
            "2026-05-23T10:00:00Z",
            Some(99.5),
            Some(10.0),
        ))
        .unwrap();
        let gate = QuotaPanicGate::new(99.0, "this-host".to_string(), "legion".to_string());
        assert!(gate.quota_panic_active(&db));
    }

    #[test]
    fn panic_at_exactly_threshold_is_active() {
        // The contract is "at or above" -- pin the >= boundary so a future
        // refactor to `>` cannot silently let the last 1% slip.
        let s = rate_sample("s1", "this-host", "t", Some(99.0), Some(40.0));
        assert!(sample_crosses_threshold(&s, 99.0));
        let s_below = rate_sample("s2", "this-host", "t", Some(98.999), Some(40.0));
        assert!(!sample_crosses_threshold(&s_below, 99.0));
    }

    #[test]
    fn edge_flip_emits_exactly_one_post_per_transition() {
        let (db, _idx, _tmp) = test_storage();
        let mut gate = QuotaPanicGate::new(99.0, "this-host".to_string(), "legion".to_string());

        // Healthy at start, no post.
        db.insert_rate_limit_sample(&rate_sample(
            "s1",
            "this-host",
            "2026-05-23T10:00:00Z",
            Some(50.0),
            Some(50.0),
        ))
        .unwrap();
        assert!(!gate.check_and_post(&db));
        assert_eq!(bullpen_panic_post_count(&db), 0);

        // Cross into panic -- one post.
        db.insert_rate_limit_sample(&rate_sample(
            "s2",
            "this-host",
            "2026-05-23T11:00:00Z",
            Some(99.5),
            Some(60.0),
        ))
        .unwrap();
        assert!(gate.check_and_post(&db));
        assert_eq!(bullpen_panic_post_count(&db), 1);

        // Still in panic -- no additional post (no spam).
        db.insert_rate_limit_sample(&rate_sample(
            "s3",
            "this-host",
            "2026-05-23T12:00:00Z",
            Some(99.8),
            Some(70.0),
        ))
        .unwrap();
        assert!(gate.check_and_post(&db));
        assert_eq!(bullpen_panic_post_count(&db), 1);

        // Recover -- one more post.
        db.insert_rate_limit_sample(&rate_sample(
            "s4",
            "this-host",
            "2026-05-23T13:00:00Z",
            Some(50.0),
            Some(50.0),
        ))
        .unwrap();
        assert!(!gate.check_and_post(&db));
        assert_eq!(bullpen_panic_post_count(&db), 2);

        // Still healthy -- no additional post.
        assert!(!gate.check_and_post(&db));
        assert_eq!(bullpen_panic_post_count(&db), 2);
    }

    #[test]
    fn db_error_returns_false_does_not_halt_watch() {
        // Dropping the table makes latest_rate_limit_sample_for_host return
        // Err. The gate must treat that as healthy (false) so a transient
        // query failure cannot DoS the mesh by sticking watch in panic.
        let (db, _idx, _tmp) = test_storage();
        db.conn
            .execute("DROP TABLE rate_limit_samples", [])
            .expect("drop table");
        let gate = QuotaPanicGate::new(99.0, "this-host".to_string(), "legion".to_string());
        assert!(!gate.quota_panic_active(&db));
    }

    #[test]
    fn post_failure_does_not_advance_edge() {
        // If the bullpen post fails on an edge, the gate must NOT flip
        // `in_panic`. Otherwise the next tick sees no edge and the mesh
        // is left believing the prior state. Simulate the failure by
        // dropping the reflections table while leaving rate_limit_samples
        // intact: the gate read succeeds, the post write fails.
        let (db, _idx, _tmp) = test_storage();
        let mut gate = QuotaPanicGate::new(99.0, "this-host".to_string(), "legion".to_string());

        db.insert_rate_limit_sample(&rate_sample(
            "s1",
            "this-host",
            "2026-05-23T10:00:00Z",
            Some(99.5),
            Some(40.0),
        ))
        .unwrap();
        db.conn
            .execute("DROP TABLE reflections", [])
            .expect("drop reflections");

        // active=true is observed (the gate read still succeeds), but the
        // post insert fails. The contract is: in_panic stays false so the
        // next poll sees the same edge and retries.
        assert!(gate.check_and_post(&db), "active state still observed");
        assert!(!gate.in_panic, "post failure must not advance in_panic");

        // Second call mirrors the next poll cycle with the same broken
        // write path. The edge must still re-fire (active != in_panic),
        // not silently no-op.
        assert!(gate.check_and_post(&db), "still active");
        assert!(!gate.in_panic, "edge still pending after second failure");
    }

    #[test]
    fn default_threshold_is_ninety_nine_percent() {
        let cfg = WatchConfig::default();
        assert!((cfg.quota_panic_threshold_pct - 99.0).abs() < f64::EPSILON);
    }

    #[test]
    fn missing_pct_does_not_trip_gate() {
        // A sample with both windows None should not be treated as panic.
        let s = rate_sample("s1", "this-host", "2026-05-23T10:00:00Z", None, None);
        assert!(!sample_crosses_threshold(&s, 99.0));
    }
}
