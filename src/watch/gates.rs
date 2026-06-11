//! Per-cycle spawn gates and the poll cycle itself: card auto-unblock,
//! the persona wake-lease gate, and the subscription-quota panic stop.

use std::time::Duration;

use crate::db::Database;
use crate::error::Result;
use crate::signal;

use super::config::WatchConfig;
use super::locks::{CooldownTracker, SessionLockTracker};
use super::signals::{build_wake_prompt, find_pending_signals, is_wake_worthy};
use super::spawn::{spawn_agent, SpawnMode};
use super::tracker::AgentTracker;
use super::wake_cap_reached;

/// Check for blocked cards that might be unblocked by recent announce signals.
///
/// When an agent announces completion ("@all announce -- repo completed: ..."),
/// check if any cards blocked on that repo can be auto-unblocked.
pub fn check_auto_unblock(db: &Database, signals: &[(String, String, String)]) -> u32 {
    let mut unblocked = 0u32;
    for (_id, text, _from_repo) in signals {
        // Completion announcements only: the verb slot must parse as
        // `announce` (structural, via signal::parse_signal -- the same
        // parser the wake gate uses) and the text must carry a
        // `completed:` marker. The old check substring-fished for
        // "announce" anywhere in the text, so e.g. a question whose note
        // mentioned "announce ... completed:" could trigger an unblock
        // scan; requiring the parsed verb is the intended tightening (#611).
        let is_announce =
            signal::parse_signal(text).is_some_and(|sig| sig.verb.eq_ignore_ascii_case("announce"));
        if !is_announce || !text.contains("completed:") {
            continue;
        }

        // Extract the repo that completed work
        let completing_repo = text.split("completed:").next().and_then(|prefix| {
            // Pattern: "@all announce from REPO -- REPO completed:" or "REPO completed:"
            prefix.split_whitespace().rev().find(|w| {
                !w.starts_with('@') && !w.starts_with('-') && *w != "announce" && *w != "from"
            })
        });

        let Some(source_repo) = completing_repo else {
            continue;
        };

        // Find blocked cards whose note mentions the completing repo
        let blocked_cards = match db.get_all_cards() {
            Ok(cards) => cards,
            Err(_) => continue,
        };

        for card in &blocked_cards {
            if card.status != crate::kanban::CardStatus::Blocked {
                continue;
            }
            let mentions_source = card
                .note
                .as_deref()
                .map(|n| n.contains(source_repo))
                .unwrap_or(false);
            if !mentions_source {
                continue;
            }

            // Auto-unblock
            if crate::kanban::transition_card(db, &card.id, crate::kanban::Action::Unblock, None)
                .is_ok()
            {
                eprintln!(
                    "[legion watch] auto-unblocked card {} for {} (blocker {} completed)",
                    card.id, card.to_repo, source_repo
                );
                unblocked += 1;
            }
        }
    }
    unblocked
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

        // Auto-unblock runs on every repo's pending signals BEFORE the
        // cooldown / session-lock spawn gates below: unblocking a card is
        // not a spawn, so it must not be deferred by spawn gating. This
        // used to be a separate prepass that re-queried every repo; folding
        // it here gives exactly one find_pending_signals call per repo per
        // cycle (#611). Repos after a concurrent-wake-cap `break` defer
        // their unblock check to the next poll -- the signals stay pending.
        check_auto_unblock(db, &signals);

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

        // Verb-driven wake gate (#404). Only spawn when at least one signal
        // carries a wake-worthy verb. Informational signals targeting this
        // repo (announce/ack/info/answer/review-without-request) are marked
        // handled here so they do not re-poll forever; they remain visible
        // via `legion bullpen` and were already delivered to live sessions
        // by the channel push.
        if !signals.iter().any(|(_, text, _)| is_wake_worthy(text)) {
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
                    if let Err(e) = db.transition_wake_attempt(aid, Spawning, Running) {
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

    // -- check_auto_unblock (#611) ---------------------------------------------

    /// Create a card and walk it to Blocked with the given note.
    fn blocked_card(db: &Database, note: &str) -> String {
        let id = crate::kanban::create_card(
            db,
            "kelex",
            "rafters",
            "do the thing",
            None,
            "medium",
            None,
            None,
            None,
            None,
            None,
        )
        .expect("create card");
        crate::kanban::transition_card(db, &id, crate::kanban::Action::Assign, None)
            .expect("assign");
        crate::kanban::transition_card(db, &id, crate::kanban::Action::Accept, None)
            .expect("accept");
        crate::kanban::transition_card(db, &id, crate::kanban::Action::Block, Some(note))
            .expect("block");
        id
    }

    #[test]
    fn check_auto_unblock_unblocks_card_on_announce_completion() {
        let (db, _index, _dir) = test_storage();
        let id = blocked_card(&db, "blocked on legion #611");
        let signals = vec![(
            "sig-1".to_string(),
            "@all announce -- legion completed: watch split".to_string(),
            "legion".to_string(),
        )];
        assert_eq!(check_auto_unblock(&db, &signals), 1);
        let card = db.get_card_by_id(&id).expect("get").expect("card");
        assert_eq!(
            card.status,
            crate::kanban::CardStatus::Accepted,
            "Unblock transitions Blocked -> Accepted"
        );
    }

    #[test]
    fn check_auto_unblock_requires_announce_verb_not_substring() {
        let (db, _index, _dir) = test_storage();
        let id = blocked_card(&db, "blocked on legion #611");
        // The old substring fishing matched this text ("announce" and
        // "completed:" both appear) even though the verb slot is `question`.
        // The structural gate via signal::parse_signal must skip it.
        let signals = vec![(
            "sig-2".to_string(),
            "@rafters question -- will announce once legion completed: the split?".to_string(),
            "kelex".to_string(),
        )];
        assert_eq!(
            check_auto_unblock(&db, &signals),
            0,
            "non-announce verb must not trigger an unblock scan"
        );
        let card = db.get_card_by_id(&id).expect("get").expect("card");
        assert_eq!(card.status, crate::kanban::CardStatus::Blocked);
    }

    #[test]
    fn check_auto_unblock_ignores_announce_without_completion_marker() {
        let (db, _index, _dir) = test_storage();
        let id = blocked_card(&db, "blocked on legion #611");
        let signals = vec![(
            "sig-3".to_string(),
            "@all announce -- legion shipped v0.17".to_string(),
            "legion".to_string(),
        )];
        assert_eq!(check_auto_unblock(&db, &signals), 0);
        let card = db.get_card_by_id(&id).expect("get").expect("card");
        assert_eq!(card.status, crate::kanban::CardStatus::Blocked);
    }

    /// Auto-unblock is deliberately NOT gated by cooldown: the fold into the
    /// main poll loop (#611, one find_pending_signals per repo per cycle)
    /// runs check_auto_unblock before the cooldown / session-lock spawn
    /// gates, preserving the old prepass semantics.
    #[test]
    fn poll_cycle_runs_auto_unblock_even_for_cooling_repos() {
        let (db, _index, _dir) = test_storage();
        let config = WatchConfig {
            repos: vec![WatchRepoConfig {
                name: "rafters".to_string(),
                workdir: "/tmp".to_string(),
                agent: None,
                broadcast_tags: Vec::new(),
                extra: toml::Table::new(),
            }],
            ..WatchConfig::default()
        };
        let id = blocked_card(&db, "blocked on legion #611");
        db.insert_reflection(
            "legion",
            "@all announce -- legion completed: watch split",
            "team",
        )
        .expect("insert announce");

        let mut cooldown = CooldownTracker::new(300, None, None);
        cooldown.record_wake("rafters"); // repo is cooling; spawn path must skip
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
        assert_eq!(spawned, 0, "cooling repo must not spawn");
        let card = db.get_card_by_id(&id).expect("get").expect("card");
        assert_eq!(
            card.status,
            crate::kanban::CardStatus::Accepted,
            "auto-unblock must run before the cooldown spawn gate"
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
