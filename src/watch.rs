use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use chrono::Timelike;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::{LegionError, Result};
use crate::health::HealthSampler;
use crate::signal;

// -- Config ------------------------------------------------------------------

/// A watched repository entry from watch.toml.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatchRepoConfig {
    pub name: String,
    pub workdir: String,
    /// The recipient name used for signal matching. When set, signals addressed
    /// to this value (e.g., `@my-agent`) wake this repo. Falls back to `name`
    /// when absent, preserving backward compatibility with existing watch.toml files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl WatchRepoConfig {
    /// Recipient name used for signal matching. Prefers `agent` when set, falls
    /// back to `name`. Centralizes the fallback rule so callers never recompute it.
    pub fn recipient(&self) -> &str {
        self.agent.as_deref().unwrap_or(&self.name)
    }
}

/// Top-level watch configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatchConfig {
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,

    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,

    /// Seconds to wait between spawning agents. Prevents startup storms
    /// where simultaneous I/O on the same drive causes overheating.
    #[serde(default = "default_stagger_secs")]
    pub stagger_secs: u64,

    /// Work hours start (0-23, local time). No cooldown during work hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_hours_start: Option<u8>,

    /// Work hours end (0-23, local time). No cooldown during work hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_hours_end: Option<u8>,

    /// Pressure threshold (0-100). Spawning is skipped when exceeded.
    #[serde(default = "default_health_threshold")]
    pub health_threshold_pct: f64,

    /// Seconds between health samples.
    #[serde(default = "default_health_poll_secs")]
    pub health_poll_secs: u64,

    /// Number of samples in the rolling pressure window.
    #[serde(default = "default_health_window_size")]
    pub health_window_size: usize,

    /// Days to retain health samples before pruning.
    #[serde(default = "default_retention_days")]
    pub retention_days: u64,

    /// Whether this node serves the web dashboard.
    /// Only one node per network should have this set to true.
    #[serde(default)]
    pub serve: bool,

    #[serde(default)]
    pub repos: Vec<WatchRepoConfig>,
}

fn default_poll_interval() -> u64 {
    30
}

fn default_cooldown_secs() -> u64 {
    300
}

fn default_stagger_secs() -> u64 {
    15
}

fn default_health_threshold() -> f64 {
    80.0
}

fn default_health_poll_secs() -> u64 {
    5
}

fn default_health_window_size() -> usize {
    6
}

fn default_retention_days() -> u64 {
    7
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval(),
            cooldown_secs: default_cooldown_secs(),
            stagger_secs: default_stagger_secs(),
            work_hours_start: None,
            work_hours_end: None,
            health_threshold_pct: default_health_threshold(),
            health_poll_secs: default_health_poll_secs(),
            health_window_size: default_health_window_size(),
            retention_days: default_retention_days(),
            serve: false,
            repos: Vec::new(),
        }
    }
}

/// Rename a repo in watch.toml. Does string replacement to preserve comments
/// and formatting. Returns true if the file was modified.
pub fn rename_in_config(path: &Path, from: &str, to: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }

    let contents = std::fs::read_to_string(path)?;
    let needle = format!("name = \"{}\"", from);
    let replacement = format!("name = \"{}\"", to);

    if !contents.contains(&needle) {
        return Ok(false);
    }

    let updated = contents.replace(&needle, &replacement);
    std::fs::write(path, updated)?;
    Ok(true)
}

/// Read the current config from disk, or return a fresh default if the file
/// does not exist. Used by add/remove to do a read-modify-write.
fn read_or_default(path: &Path) -> Result<WatchConfig> {
    if !path.exists() {
        return Ok(WatchConfig::default());
    }
    let contents = std::fs::read_to_string(path)?;
    let config: WatchConfig = toml::from_str(&contents)?;
    Ok(config)
}

/// Write a `WatchConfig` to disk as TOML.
///
/// Creates parent directories if they do not exist.
fn write_config(path: &Path, config: &WatchConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let toml_str =
        toml::to_string_pretty(config).map_err(|e| LegionError::WatchConfig(e.to_string()))?;
    std::fs::write(path, toml_str)?;
    Ok(())
}

/// Add a repo entry to watch.toml. Idempotent by canonicalized path.
///
/// Returns `true` if a new entry was written, `false` if the path was already
/// present (matched by canonicalized workdir). Errors if `workdir` is not an
/// existing directory.
pub fn add_repo_to_config(
    path: &Path,
    name: &str,
    workdir: &Path,
    agent: Option<&str>,
) -> Result<bool> {
    if !workdir.is_dir() {
        return Err(LegionError::WatchConfig(format!(
            "workdir is not a directory: {}",
            workdir.display()
        )));
    }

    let canonical = std::fs::canonicalize(workdir)?;
    let canonical_str = canonical.to_string_lossy().into_owned();

    let mut config = read_or_default(path)?;

    // Idempotent by canonicalized path. A repeated add for the same path is a no-op.
    if config.repos.iter().any(|r| r.workdir == canonical_str) {
        return Ok(false);
    }

    // Name collision with a different path is an error, not a silent no-op,
    // so the caller sees the conflict instead of a misleading "already present".
    if let Some(existing) = config.repos.iter().find(|r| r.name == name) {
        return Err(LegionError::WatchConfig(format!(
            "name '{}' already in use by {}",
            name, existing.workdir
        )));
    }

    config.repos.push(WatchRepoConfig {
        name: name.to_string(),
        workdir: canonical_str,
        agent: agent.map(|s| s.to_string()),
    });

    write_config(path, &config)?;
    Ok(true)
}

/// Remove a repo entry from watch.toml by name.
///
/// Returns `true` if an entry was removed, `false` if no entry matched.
pub fn remove_repo_from_config(path: &Path, name: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }

    let mut config = read_or_default(path)?;
    let before = config.repos.len();
    config.repos.retain(|r| r.name != name);
    let removed = config.repos.len() < before;

    if removed {
        write_config(path, &config)?;
    }
    Ok(removed)
}

/// List all repo entries currently in watch.toml.
///
/// Returns an empty vec if the file does not exist.
pub fn list_repos_in_config(path: &Path) -> Result<Vec<WatchRepoConfig>> {
    Ok(read_or_default(path)?.repos)
}

/// Load watch config from the given path. Returns a default config if the
/// file does not exist.
pub fn load_config(path: &Path) -> Result<WatchConfig> {
    if !path.exists() {
        return Err(LegionError::WatchConfig(format!(
            "config file not found: {}. Create it with watched repos.",
            path.display()
        )));
    }

    let contents = std::fs::read_to_string(path)?;
    let config: WatchConfig = toml::from_str(&contents)?;

    if config.repos.is_empty() {
        return Err(LegionError::WatchConfig(
            "no repos configured in watch.toml".to_string(),
        ));
    }

    // Validate workdirs exist
    for repo in &config.repos {
        if !Path::new(&repo.workdir).is_dir() {
            return Err(LegionError::WatchConfig(format!(
                "workdir does not exist for repo '{}': {}",
                repo.name, repo.workdir
            )));
        }
    }

    Ok(config)
}

// -- PID Lock ----------------------------------------------------------------

/// Acquire a PID lock file. Returns an error if another watcher is running.
pub fn acquire_pid_lock(lock_path: &Path) -> Result<()> {
    if lock_path.exists() {
        let contents = std::fs::read_to_string(lock_path).unwrap_or_default();
        if let Ok(pid) = contents.trim().parse::<u32>() {
            // Check if the process is actually running
            if process_alive(pid) {
                return Err(LegionError::WatchAlreadyRunning(pid));
            }
            // Stale lock file -- process is dead, remove it
            eprintln!("[legion watch] removing stale lock (pid {})", pid);
        }
    }

    let pid = std::process::id();
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(lock_path, pid.to_string())?;
    Ok(())
}

/// Release the PID lock file.
pub fn release_pid_lock(lock_path: &Path) {
    let _ = std::fs::remove_file(lock_path);
}

/// RAII guard that releases the PID lock file on drop.
///
/// Holds the lock path and removes the file when dropped, ensuring the lock
/// is always released even on panic or task abort.
pub struct PidLockGuard(pub PathBuf);

impl Drop for PidLockGuard {
    fn drop(&mut self) {
        release_pid_lock(&self.0);
        eprintln!("[legion watch] released lock");
    }
}

/// Check whether a process with the given PID is alive.
fn process_alive(pid: u32) -> bool {
    // On Unix, signal 0 checks process existence without sending a signal.
    // SAFETY: this is not unsafe -- libc::kill with signal 0 is a standard POSIX check.
    #[cfg(unix)]
    {
        // kill(pid, 0) returns 0 if the process exists and we can signal it
        let result = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        matches!(result, Ok(status) if status.success())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

// -- Cooldown ----------------------------------------------------------------

/// Tracks per-repo cooldown to prevent wake storms.
pub struct CooldownTracker {
    last_wake: HashMap<String, Instant>,
    cooldown: Duration,
    work_hours_start: Option<u8>,
    work_hours_end: Option<u8>,
}

impl CooldownTracker {
    pub fn new(
        cooldown_secs: u64,
        work_hours_start: Option<u8>,
        work_hours_end: Option<u8>,
    ) -> Self {
        Self {
            last_wake: HashMap::new(),
            cooldown: Duration::from_secs(cooldown_secs),
            work_hours_start,
            work_hours_end,
        }
    }

    /// Check whether we are in work hours (no cooldown applies).
    fn is_work_hours(&self) -> bool {
        if let (Some(start), Some(end)) = (self.work_hours_start, self.work_hours_end) {
            let hour = chrono::Local::now().hour() as u8;
            if start <= end {
                hour >= start && hour < end
            } else {
                // Overnight range (e.g., 22-06)
                hour >= start || hour < end
            }
        } else {
            false
        }
    }

    /// Check whether the repo is on cooldown. Returns true if we should skip.
    /// During work hours, cooldown is disabled.
    pub fn is_cooling_down(&self, repo: &str) -> bool {
        if self.is_work_hours() {
            return false;
        }
        self.last_wake
            .get(repo)
            .is_some_and(|t| t.elapsed() < self.cooldown)
    }

    /// Record that a repo was just woken.
    pub fn record_wake(&mut self, repo: &str) {
        self.last_wake.insert(repo.to_string(), Instant::now());
    }
}

// -- Signal Detection --------------------------------------------------------

/// Find unhandled signals targeting a specific repo.
///
/// Returns signal reflection IDs and their text, filtered to only actual
/// signals (text starts with @).
pub fn find_pending_signals(
    db: &Database,
    repo_name: &str,
    since: Option<&str>,
) -> Result<Vec<(String, String, String)>> {
    let reflections = db.get_unhandled_signals_for_repo(repo_name, since)?;

    let mut signals: Vec<(String, String, String)> = Vec::new();
    for r in reflections {
        if signal::is_signal(&r.text) {
            signals.push((r.id, r.text, r.repo));
        }
    }

    Ok(signals)
}

// -- Agent Spawning ----------------------------------------------------------

/// Build the prompt context for a woken agent from pending signals.
pub fn build_wake_prompt(repo_name: &str, signals: &[(String, String, String)]) -> String {
    let mut prompt = format!(
        "You were auto-woken by legion watch. The following signal(s) are directed at you ({}):\n\n",
        repo_name
    );

    for (id, text, from_repo) in signals {
        prompt.push_str(&format!("- [from {}] {} (id: {})\n", from_repo, text, id));
    }

    prompt.push_str(
        "\nRead and respond to each signal. Use `legion signal` to reply if needed. \
         Use `legion bullpen` to check for broader context. When done, use `legion reflect` \
         to store any learnings.\n\n\
         IMPORTANT: Do NOT respond to announcements or signals that don't need a response. \
         Silence is acknowledgment. Only respond if you have NEW information, a concern, \
         a dissent, or an action item. Empty acknowledgments like 'acknowledged, no action needed' \
         waste tokens and trigger wake storms. If you have nothing substantive to add, \
         reflect and exit.",
    );

    prompt
}

/// Spawn a `claude --print` session for the given repo.
///
/// Returns the child process handle on success.
pub fn spawn_agent(workdir: &str, prompt: &str) -> Result<Child> {
    let child = std::process::Command::new("claude")
        .args(["--print", "-p", prompt])
        .current_dir(workdir)
        .env("LEGION_AUTO_WAKE", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match child {
        Ok(c) => Ok(c),
        Err(e) => {
            eprintln!("[legion watch] failed to spawn agent: {}", e);
            Err(LegionError::Io(e))
        }
    }
}

// -- Agent Tracker -----------------------------------------------------------

/// Tracks spawned child processes for active agent counting.
pub struct AgentTracker {
    children: Vec<(String, Child)>,
}

impl AgentTracker {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    /// Record a spawned child process.
    pub fn track(&mut self, repo: String, child: Child) {
        self.children.push((repo, child));
    }

    /// Reap finished child processes, removing them from tracking.
    pub fn reap_finished(&mut self) {
        self.children.retain_mut(|(repo, child)| {
            match child.try_wait() {
                Ok(Some(status)) => {
                    eprintln!(
                        "[legion watch] agent for {} exited ({})",
                        repo,
                        if status.success() { "ok" } else { "error" }
                    );
                    false // remove from list
                }
                Ok(None) => true, // still running
                Err(e) => {
                    eprintln!("[legion watch] error checking agent for {}: {}", repo, e);
                    true // keep tracking -- process may still be running
                }
            }
        });
    }

    /// Number of currently active agents.
    pub fn active_count(&self) -> i32 {
        self.children.len() as i32
    }
}

// -- Main Loop ---------------------------------------------------------------

/// Run a single poll cycle across all configured repos.
///
/// Returns the number of agents spawned in this cycle.
/// Check for blocked cards that might be unblocked by recent announce signals.
///
/// When an agent announces completion ("@all announce -- repo completed: ..."),
/// check if any cards blocked on that repo can be auto-unblocked.
pub fn check_auto_unblock(db: &Database, signals: &[(String, String, String)]) -> u32 {
    let mut unblocked = 0u32;
    for (_id, text, _created) in signals {
        // Look for announce signals about completed work
        if !text.contains("announce") || !text.contains("completed:") {
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

pub fn poll_cycle(
    db: &Database,
    config: &WatchConfig,
    cooldown: &mut CooldownTracker,
    tracker: &mut AgentTracker,
    since: Option<&str>,
) -> Result<u32> {
    let mut spawned: u32 = 0;

    // Check for auto-unblock opportunities from recent signals
    for repo in &config.repos {
        if let Ok(signals) = find_pending_signals(db, repo.recipient(), since) {
            check_auto_unblock(db, &signals);
        }
    }

    for repo in &config.repos {
        if cooldown.is_cooling_down(&repo.name) {
            continue;
        }

        let recipient = repo.recipient();
        let signals = find_pending_signals(db, recipient, since)?;
        if signals.is_empty() {
            continue;
        }

        eprintln!(
            "[legion watch] {} signal(s) for {} -- waking agent",
            signals.len(),
            repo.name
        );

        let prompt = build_wake_prompt(recipient, &signals);

        if spawned > 0 && config.stagger_secs > 0 {
            std::thread::sleep(Duration::from_secs(config.stagger_secs));
        }

        match spawn_agent(&repo.workdir, &prompt) {
            Ok(child) => {
                // Mark ALL signals as handled for THIS repo (per-repo tracking).
                // This includes @all broadcasts -- each repo marks its own copy,
                // so other repos still see the signal on their next poll.
                for (id, _, _) in &signals {
                    if db.mark_signal_handled_for_repo(id, recipient).is_err() {
                        eprintln!(
                            "[legion watch] failed to mark signal {} as handled for {}",
                            id, repo.name
                        );
                    }
                }
                tracker.track(repo.name.clone(), child);
                cooldown.record_wake(&repo.name);
                spawned += 1;
                eprintln!("[legion watch] spawned agent for {}", repo.name);
            }
            Err(e) => {
                eprintln!("[legion watch] spawn failed for {}: {}", repo.name, e);
            }
        }
    }

    Ok(spawned)
}

/// Run the watch daemon main loop.
///
/// Uses a dual-interval loop: health sampling every `health_poll_secs`
/// (default 5s) and spawn checks every `poll_interval_secs` (default 30s).
/// Spawning is gated on system health -- if pressure exceeds the threshold,
/// the spawn cycle is skipped.
pub fn run(data_dir: &Path) -> Result<()> {
    let config_path: PathBuf = data_dir.join("watch.toml");
    let lock_path: PathBuf = data_dir.join("watch.pid");
    let db_path: PathBuf = data_dir.join("legion.db");

    let config = load_config(&config_path)?;

    eprintln!(
        "[legion watch] config loaded: {} repo(s), poll every {}s, cooldown {}s, stagger {}s, \
         health threshold {}%",
        config.repos.len(),
        config.poll_interval_secs,
        config.cooldown_secs,
        config.stagger_secs,
        config.health_threshold_pct
    );

    acquire_pid_lock(&lock_path)?;
    eprintln!("[legion watch] acquired lock (pid {})", std::process::id());

    // Guard that releases the PID lock when dropped
    let _guard = PidLockGuard(lock_path);

    let db = Database::open(&db_path)?;
    let mut cooldown = CooldownTracker::new(
        config.cooldown_secs,
        config.work_hours_start,
        config.work_hours_end,
    );
    let mut tracker = AgentTracker::new();
    let mut sampler = HealthSampler::new(config.health_window_size);

    let poll_interval = Duration::from_secs(config.poll_interval_secs);
    let health_interval = Duration::from_secs(config.health_poll_secs);
    let retention_cutoff = chrono::Duration::days(config.retention_days as i64);
    // On startup, only look back 24 hours for unhandled signals.
    // Prevents historical flood when watch restarts after downtime.
    // The watch_handled table prevents re-processing, but without a
    // lookback window, the first poll returns every unhandled signal
    // ever created, overwhelming agents with stale notifications.
    let lookback = (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339();

    let mut poll_timer = Instant::now() - poll_interval; // poll immediately on start
    let mut health_timer = Instant::now() - health_interval; // sample immediately on start

    eprintln!(
        "[legion watch] watching repos: {}",
        config
            .repos
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    loop {
        // Health sample on its own interval
        if health_timer.elapsed() >= health_interval {
            sampler.sample();
            tracker.reap_finished();

            // Persist health sample (failure is non-fatal)
            match sampler.to_health_sample(tracker.active_count()) {
                Ok(sample) => {
                    if let Err(e) = db.insert_health_sample(&sample) {
                        eprintln!("[legion watch] health persist error: {}", e);
                    }
                }
                Err(e) => {
                    eprintln!("[legion watch] health sample error: {}", e);
                }
            }

            health_timer = Instant::now();
        }

        // Spawn check on the poll interval
        if poll_timer.elapsed() >= poll_interval {
            if sampler.can_spawn(config.health_threshold_pct) {
                match poll_cycle(&db, &config, &mut cooldown, &mut tracker, Some(&lookback)) {
                    Ok(n) if n > 0 => {
                        eprintln!("[legion watch] cycle complete: {} agent(s) spawned", n);
                    }
                    Ok(_) => {} // quiet cycle, no spam
                    Err(e) => {
                        eprintln!("[legion watch] poll error: {}", e);
                    }
                }
            } else {
                let pressure: f64 = sampler.pressure();
                eprintln!(
                    "[legion watch] pressure {:.1}% >= threshold {:.0}% -- skipping spawn cycle",
                    pressure, config.health_threshold_pct
                );
            }

            // Prune old health samples and stale watch_handled records (non-fatal)
            let cutoff = (chrono::Utc::now() - retention_cutoff).to_rfc3339();
            if let Err(e) = db.prune_health_samples(&cutoff) {
                eprintln!("[legion watch] health prune error: {}", e);
            }
            if let Err(e) = db.prune_watch_handled(&cutoff) {
                eprintln!("[legion watch] watch_handled prune error: {}", e);
            }

            poll_timer = Instant::now();
        }

        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_storage;

    #[test]
    fn parse_config_basic() {
        let toml_str = r#"
poll_interval_secs = 15
cooldown_secs = 120

[[repos]]
name = "rafters"
workdir = "/tmp"

[[repos]]
name = "legion"
workdir = "/tmp"
"#;
        let config: WatchConfig = toml::from_str(toml_str).expect("parse config");
        assert_eq!(config.poll_interval_secs, 15);
        assert_eq!(config.cooldown_secs, 120);
        assert_eq!(config.repos.len(), 2);
        assert_eq!(config.repos[0].name, "rafters");
        assert_eq!(config.repos[1].name, "legion");
    }

    #[test]
    fn parse_config_defaults() {
        let toml_str = r#"
[[repos]]
name = "test"
workdir = "/tmp"
"#;
        let config: WatchConfig = toml::from_str(toml_str).expect("parse config");
        assert_eq!(config.poll_interval_secs, 30);
        assert_eq!(config.cooldown_secs, 300);
    }

    #[test]
    fn cooldown_tracker_prevents_rapid_wake() {
        let mut tracker = CooldownTracker::new(300, None, None);
        assert!(!tracker.is_cooling_down("rafters"));

        tracker.record_wake("rafters");
        assert!(tracker.is_cooling_down("rafters"));
        assert!(!tracker.is_cooling_down("legion"));
    }

    #[test]
    fn find_pending_signals_detects_targeted_signals() {
        let (db, _index, _dir) = test_storage();

        // Post a signal from kelex to legion
        db.insert_reflection("kelex", "@legion review:approved", "team")
            .expect("insert signal");

        // Post a non-signal
        db.insert_reflection("rafters", "just a musing", "team")
            .expect("insert musing");

        // Post a signal to all
        db.insert_reflection("rafters", "@all announce: shipped", "team")
            .expect("insert broadcast");

        let signals = find_pending_signals(&db, "legion", None).expect("find signals");
        assert_eq!(signals.len(), 2);

        // Verify the targeted signal is found
        assert!(
            signals
                .iter()
                .any(|(_, text, _)| text == "@legion review:approved")
        );
        // Verify the broadcast is found
        assert!(
            signals
                .iter()
                .any(|(_, text, _)| text == "@all announce: shipped")
        );
    }

    #[test]
    fn find_pending_signals_detects_multi_recipient() {
        let (db, _index, _dir) = test_storage();

        // Multi-recipient signal: @shingle @huttspawn -- message
        db.insert_reflection(
            "legion",
            "@shingle @huttspawn -- build draft sites from current content",
            "team",
        )
        .expect("insert multi-recipient");

        // Both shingle and huttspawn should see it
        let shingle = find_pending_signals(&db, "shingle", None).expect("shingle");
        let huttspawn = find_pending_signals(&db, "huttspawn", None).expect("huttspawn");
        assert_eq!(
            shingle.len(),
            1,
            "shingle should see multi-recipient signal"
        );
        assert_eq!(
            huttspawn.len(),
            1,
            "huttspawn should see multi-recipient signal"
        );

        // legion (sender) should NOT see it
        let legion = find_pending_signals(&db, "legion", None).expect("legion");
        assert!(legion.is_empty(), "sender should not see own signal");

        // unrelated repo should NOT see it
        let kelex = find_pending_signals(&db, "kelex", None).expect("kelex");
        assert!(kelex.is_empty(), "unmentioned repo should not see signal");
    }

    #[test]
    fn find_pending_signals_excludes_self_signals() {
        let (db, _index, _dir) = test_storage();

        // Signal from legion to legion should not be returned
        db.insert_reflection("legion", "@legion review:approved", "team")
            .expect("insert self-signal");

        let signals = find_pending_signals(&db, "legion", None).expect("find signals");
        assert!(signals.is_empty(), "self-signals should be excluded");
    }

    #[test]
    fn mark_handled_prevents_re_detection() {
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("kelex", "@legion review:approved", "team")
            .expect("insert signal");

        let signals = find_pending_signals(&db, "legion", None).expect("first poll");
        assert_eq!(signals.len(), 1);

        // Mark as handled for legion
        let (id, _, _) = &signals[0];
        db.mark_signal_handled_for_repo(id, "legion")
            .expect("mark handled");

        // Should not appear again for legion
        let signals = find_pending_signals(&db, "legion", None).expect("second poll");
        assert!(signals.is_empty());
    }

    #[test]
    fn build_wake_prompt_formats_signals() {
        let signals = vec![
            (
                "id-1".to_string(),
                "@legion review:approved".to_string(),
                "kelex".to_string(),
            ),
            (
                "id-2".to_string(),
                "@all announce: shipped".to_string(),
                "rafters".to_string(),
            ),
        ];

        let prompt = build_wake_prompt("legion", &signals);
        assert!(prompt.contains("auto-woken by legion watch"));
        assert!(prompt.contains("@legion review:approved"));
        assert!(prompt.contains("@all announce: shipped"));
        assert!(prompt.contains("from kelex"));
        assert!(prompt.contains("from rafters"));
    }

    #[test]
    fn poll_cycle_skips_cooling_repos() {
        let (db, _index, _dir) = test_storage();

        let config = WatchConfig {
            repos: vec![WatchRepoConfig {
                name: "legion".to_string(),
                workdir: "/tmp".to_string(),
                agent: None,
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
        let spawned = poll_cycle(&db, &config, &mut cooldown, &mut tracker, None).expect("poll");
        assert_eq!(spawned, 0, "cooling repo should be skipped");
    }

    #[test]
    fn broadcast_signals_visible_to_all_repos() {
        let (db, _index, _dir) = test_storage();

        // Post an @all signal from kelex
        db.insert_reflection("kelex", "@all RFC:help -- discover proposal", "team")
            .expect("insert broadcast");

        // Both legion and rafters should see it
        let legion_signals = find_pending_signals(&db, "legion", None).expect("legion");
        let rafters_signals = find_pending_signals(&db, "rafters", None).expect("rafters");
        assert_eq!(legion_signals.len(), 1);
        assert_eq!(rafters_signals.len(), 1);

        // Mark handled for legion using per-repo tracking
        for (id, _, _) in &legion_signals {
            db.mark_signal_handled_for_repo(id, "legion")
                .expect("mark handled for legion");
        }

        // legion should NOT see it anymore
        let legion_after = find_pending_signals(&db, "legion", None).expect("legion after");
        assert!(
            legion_after.is_empty(),
            "legion should not see broadcast after handling"
        );

        // rafters should STILL see the broadcast
        let rafters_after = find_pending_signals(&db, "rafters", None).expect("rafters after");
        assert_eq!(
            rafters_after.len(),
            1,
            "broadcast should remain visible to other repos"
        );

        // Mark handled for rafters too
        for (id, _, _) in &rafters_signals {
            db.mark_signal_handled_for_repo(id, "rafters")
                .expect("mark handled for rafters");
        }

        // Now rafters should not see it either
        let rafters_final = find_pending_signals(&db, "rafters", None).expect("rafters final");
        assert!(
            rafters_final.is_empty(),
            "rafters should not see broadcast after handling"
        );
    }

    #[test]
    fn load_config_rejects_empty_repos() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        std::fs::write(&config_path, "poll_interval_secs = 10\n").expect("write");

        let err = load_config(&config_path).unwrap_err();
        assert!(err.to_string().contains("no repos configured"));
    }

    #[test]
    fn load_config_rejects_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("nonexistent.toml");

        let err = load_config(&config_path).unwrap_err();
        assert!(err.to_string().contains("config file not found"));
    }

    #[test]
    fn load_config_rejects_bad_workdir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        std::fs::write(
            &config_path,
            r#"
[[repos]]
name = "test"
workdir = "/nonexistent/path/that/does/not/exist"
"#,
        )
        .expect("write");

        let err = load_config(&config_path).unwrap_err();
        assert!(err.to_string().contains("workdir does not exist"));
    }

    #[test]
    fn pid_lock_acquire_and_release() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("test.pid");

        acquire_pid_lock(&lock_path).expect("acquire lock");
        assert!(lock_path.exists());

        release_pid_lock(&lock_path);
        assert!(!lock_path.exists());
    }

    #[test]
    fn pid_lock_detects_stale_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("test.pid");

        // Write a fake PID that is very unlikely to be running
        std::fs::write(&lock_path, "999999999").expect("write stale lock");

        // Should succeed because the process is not running
        acquire_pid_lock(&lock_path).expect("acquire lock over stale");
    }

    // -- Config management tests -----------------------------------------------

    #[test]
    fn add_repo_creates_file_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir = dir.path().to_path_buf();

        let added = add_repo_to_config(&config_path, "rafters", &workdir, None).expect("add");
        assert!(added, "should report added=true for new entry");
        assert!(config_path.exists(), "config file should be created");

        let repos = list_repos_in_config(&config_path).expect("list");
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "rafters");
        assert!(repos[0].agent.is_none());
    }

    #[test]
    fn add_repo_with_agent_stores_agent_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir = dir.path().to_path_buf();

        add_repo_to_config(&config_path, "legion", &workdir, Some("my-agent")).expect("add");

        let repos = list_repos_in_config(&config_path).expect("list");
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].agent.as_deref(), Some("my-agent"));
    }

    #[test]
    fn add_repo_is_idempotent_by_canonical_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir = dir.path().to_path_buf();

        let first = add_repo_to_config(&config_path, "rafters", &workdir, None).expect("first");
        assert!(first);

        let second = add_repo_to_config(&config_path, "rafters", &workdir, None).expect("second");
        assert!(!second, "duplicate by canonical path should return false");

        let repos = list_repos_in_config(&config_path).expect("list");
        assert_eq!(repos.len(), 1, "should still have only one entry");
    }

    #[test]
    fn add_repo_errors_on_name_collision_with_different_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir_a = dir.path().join("a");
        let workdir_b = dir.path().join("b");
        std::fs::create_dir_all(&workdir_a).expect("create a");
        std::fs::create_dir_all(&workdir_b).expect("create b");

        add_repo_to_config(&config_path, "foo", &workdir_a, None).expect("add a");
        let err = add_repo_to_config(&config_path, "foo", &workdir_b, None).unwrap_err();
        assert!(
            err.to_string().contains("already in use"),
            "expected 'already in use' error, got: {}",
            err
        );
    }

    #[test]
    fn watch_repo_config_recipient_prefers_agent_over_name() {
        let repo_with_agent = WatchRepoConfig {
            name: "ledger".to_string(),
            workdir: "/tmp".to_string(),
            agent: Some("platform".to_string()),
        };
        assert_eq!(repo_with_agent.recipient(), "platform");

        let repo_without_agent = WatchRepoConfig {
            name: "legion".to_string(),
            workdir: "/tmp".to_string(),
            agent: None,
        };
        assert_eq!(repo_without_agent.recipient(), "legion");
    }

    #[test]
    fn add_repo_rejects_nonexistent_workdir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let bad_path = std::path::Path::new("/nonexistent/path/that/does/not/exist");

        let err = add_repo_to_config(&config_path, "test", bad_path, None).unwrap_err();
        assert!(
            err.to_string().contains("not a directory"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn remove_repo_removes_existing_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");

        // Use two distinct sub-directories so the idempotency check passes.
        let workdir_a = dir.path().join("a");
        let workdir_b = dir.path().join("b");
        std::fs::create_dir_all(&workdir_a).expect("create a");
        std::fs::create_dir_all(&workdir_b).expect("create b");

        add_repo_to_config(&config_path, "rafters", &workdir_a, None).expect("add rafters");
        add_repo_to_config(&config_path, "legion", &workdir_b, None).expect("add legion");

        let removed = remove_repo_from_config(&config_path, "rafters").expect("remove");
        assert!(removed);

        let repos = list_repos_in_config(&config_path).expect("list");
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "legion");
    }

    #[test]
    fn remove_repo_returns_false_when_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir = dir.path().to_path_buf();

        add_repo_to_config(&config_path, "rafters", &workdir, None).expect("add");

        let removed = remove_repo_from_config(&config_path, "nonexistent").expect("remove");
        assert!(!removed, "unknown name should return false");
    }

    #[test]
    fn remove_repo_returns_false_when_file_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");

        let removed = remove_repo_from_config(&config_path, "anything").expect("remove");
        assert!(!removed);
    }

    #[test]
    fn list_repos_returns_empty_when_file_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");

        let repos = list_repos_in_config(&config_path).expect("list");
        assert!(repos.is_empty());
    }

    #[test]
    fn add_repo_stores_canonicalized_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir = dir.path().to_path_buf();
        let canonical = std::fs::canonicalize(&workdir).expect("canonicalize");

        add_repo_to_config(&config_path, "test", &workdir, None).expect("add");

        let repos = list_repos_in_config(&config_path).expect("list");
        // The stored path must be the canonicalized form.
        assert_eq!(
            repos[0].workdir,
            canonical.to_string_lossy().as_ref(),
            "workdir should be canonical"
        );
    }

    #[test]
    fn existing_config_without_agent_field_deserializes_cleanly() {
        // Ensures backward compatibility: old watch.toml files without agent= still parse.
        let toml_str = r#"
[[repos]]
name = "rafters"
workdir = "/tmp"
"#;
        let config: WatchConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(config.repos[0].agent, None);
    }

    #[test]
    fn config_with_agent_field_round_trips() {
        let toml_str = r#"
[[repos]]
name = "legion"
workdir = "/tmp"
agent = "my-agent"
"#;
        let config: WatchConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(config.repos[0].agent.as_deref(), Some("my-agent"));

        // Serialize and re-parse to confirm round-trip.
        let serialized = toml::to_string_pretty(&config).expect("serialize");
        let reparsed: WatchConfig = toml::from_str(&serialized).expect("reparse");
        assert_eq!(reparsed.repos[0].agent.as_deref(), Some("my-agent"));
    }
}
