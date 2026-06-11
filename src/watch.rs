#![allow(clippy::manual_is_multiple_of)] // Use modulo for MSRV compatibility

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

    /// Group tags this repo subscribes to. A signal addressed to `@<tag>` wakes
    /// every repo that lists `<tag>` in `broadcast_tags`. Tags complement the
    /// reserved `@all` / `@everyone` broadcasts: they let an operator fan out
    /// to a named subset of repos without waking the entire mesh. An empty vec
    /// (the default) means this repo subscribes to no tags.
    ///
    /// No tag may equal any repo's `recipient()` value -- `load_config` rejects
    /// that collision so the match layer never confuses a tag with a direct address.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broadcast_tags: Vec<String>,

    /// Any additional per-repo fields the struct does not model explicitly
    /// (e.g., `github`, `worksource`, `review`). These survive a read-modify-write
    /// cycle so CRUD on one entry never strips integration config on another.
    #[serde(flatten, default, skip_serializing_if = "toml::Table::is_empty")]
    pub extra: toml::Table,
}

impl WatchRepoConfig {
    /// Recipient name used for signal matching. Prefers `agent` when set to a
    /// non-empty value, falls back to `name`. Empty or whitespace-only `agent`
    /// values are treated as absent so a hand-edited `agent = ""` cannot silently
    /// route to nothing. Centralizes the fallback rule so callers never recompute it.
    pub fn recipient(&self) -> &str {
        self.agent
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.name)
    }

    /// The full addressable name set for this repo: `recipient()` plus every
    /// `broadcast_tags` entry. This is exactly the `names` contract of
    /// `find_pending_signals` -- the wake path (poll_cycle) and the read path
    /// (`legion pending-replies`) must agree on the address set by
    /// construction, so all callers build it here instead of hand-rolling
    /// the vec (#611; the #585/#586 routing conflict surface).
    pub fn wake_addresses(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::with_capacity(1 + self.broadcast_tags.len());
        names.push(self.recipient().to_string());
        names.extend(self.broadcast_tags.iter().cloned());
        names
    }
}

/// A signal recipient parsed from a raw address string.
///
/// The reserved names `all` and `everyone` (case-insensitive, with or without
/// a leading `@`) map to `Broadcast(All)`. Everything else is treated as a
/// direct `Agent` address. Tag resolution is NOT done here -- the caller must
/// compare the address against each repo's `broadcast_tags` at match time.
///
/// This type is part of the public API for #585. Production call sites that
/// consume `Recipient` are in #586 (wake-verb canon) and the future
/// signal-routing layer; it is defined here as a stable contract.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // public API foundation for #585/#586; callers ship in follow-on issues
pub enum Recipient {
    /// Directly addressed to a named agent or repo.
    Agent(String),
    /// Addressed to a broadcast group.
    Broadcast(BroadcastTarget),
}

/// The target of a broadcast signal.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // public API foundation for #585; Tag variant consumed by routing layer in #586
pub enum BroadcastTarget {
    /// The reserved `@all` / `@everyone` group -- wakes every watcher.
    All,
    /// A named tag group -- wakes only repos whose `broadcast_tags` include
    /// this name.
    Tag(String),
}

impl Recipient {
    /// Parse a raw signal address into a typed `Recipient`.
    ///
    /// Strips a leading `@` if present before comparing. The reserved set
    /// {`all`, `everyone`} (case-insensitive) maps to `Broadcast(All)`;
    /// all other values become `Agent(name)`. Tag-vs-agent disambiguation
    /// is NOT done here -- the caller resolves tags against each repo's
    /// `broadcast_tags` at match time.
    #[allow(dead_code)] // public API foundation for #585/#586; callers ship in follow-on issues
    pub fn parse(raw: &str) -> Self {
        let stripped = raw.strip_prefix('@').unwrap_or(raw);
        match stripped.to_ascii_lowercase().as_str() {
            "all" | "everyone" => Self::Broadcast(BroadcastTarget::All),
            _ => Self::Agent(stripped.to_string()),
        }
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

    /// TTL for per-repo session lockfiles, in seconds. A lock is considered
    /// abandoned when its mtime is older than this. Default 60 minutes.
    /// Set to `0` to disable the session lock gate entirely (not recommended).
    #[serde(default = "default_session_lock_ttl_secs")]
    pub session_lock_ttl_secs: u64,

    /// TTL for persona wake leases, in seconds. A lease expires this many
    /// seconds after its last heartbeat. Default 10 minutes. The daemon
    /// heartbeats every poll cycle, so healthy sessions keep their lease
    /// rolling forward; a crashed session's lease ages out within one TTL.
    #[serde(default = "default_persona_lease_ttl_secs")]
    pub persona_lease_ttl_secs: u64,

    /// Subscription-quota threshold (0-100). When this host's most recent
    /// rate-limit sample shows either the 5-hour or 7-day window at or above
    /// this percentage, watch enters panic mode: poll cycles are skipped and
    /// a single bullpen post announces the halt. A second post announces
    /// recovery once the sample drops back below the threshold. Default
    /// 99.0 -- aggressive but leaves a single-percent buffer for the sample
    /// race so we do not burn the last sliver of quota on auto-wake spawns.
    #[serde(default = "default_quota_panic_threshold_pct")]
    pub quota_panic_threshold_pct: f64,

    /// Maximum number of auto-wake agents allowed in flight at once on this
    /// host. Once `poll_cycle` reaches this many running wakes it stops
    /// spawning for the rest of the cycle and defers the remaining personas to
    /// later polls, draining as running agents finish. Without this cap a
    /// single `@all` broadcast fans out to every watched repo in one cycle
    /// (#598). Default 4. Set to `0` to disable the cap (not recommended).
    #[serde(default = "default_max_concurrent_wakes")]
    pub max_concurrent_wakes: u32,

    /// Whether this node serves the web dashboard.
    /// Only one node per network should have this set to true.
    #[serde(default)]
    pub serve: bool,

    /// Tombstone for the retired watch.toml `[cluster]` section. Cluster
    /// sync is configured in `cluster.toml` (see `crate::cluster`), never
    /// here -- the old `watch::ClusterConfig` was parsed but consumed by
    /// nothing, a silent misconfiguration trap (#611). The field is kept as
    /// a raw table for two reasons: `load_config` rejects a populated
    /// section LOUDLY instead of serde silently ignoring an unknown key,
    /// and the CRUD read-modify-write path (`add`/`remove`) round-trips the
    /// section instead of stripping operator-written config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster: Option<toml::Table>,

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

/// Default TTL for the watch-spawn `.lock` files.
///
/// Exposed so callers that construct `SessionLockTracker` outside of the
/// watch daemon (e.g. `legion watch session-start`) can use the same
/// default without reading watch.toml.
pub fn default_session_lock_ttl_secs() -> u64 {
    3600
}

fn default_persona_lease_ttl_secs() -> u64 {
    600
}

fn default_quota_panic_threshold_pct() -> f64 {
    99.0
}

fn default_max_concurrent_wakes() -> u32 {
    4
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
            session_lock_ttl_secs: default_session_lock_ttl_secs(),
            persona_lease_ttl_secs: default_persona_lease_ttl_secs(),
            quota_panic_threshold_pct: default_quota_panic_threshold_pct(),
            max_concurrent_wakes: default_max_concurrent_wakes(),
            serve: false,
            cluster: None,
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
    let contents = std::fs::read_to_string(path)
        .map_err(|e| LegionError::WatchConfig(format!("cannot read {}: {}", path.display(), e)))?;
    let config: WatchConfig = toml::from_str(&contents)
        .map_err(|e| LegionError::WatchConfig(format!("malformed {}: {}", path.display(), e)))?;
    Ok(config)
}

/// Write a `WatchConfig` to disk as TOML.
///
/// Creates parent directories if they do not exist.
fn write_config(path: &Path, config: &WatchConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            LegionError::WatchConfig(format!(
                "cannot create parent dir for {}: {}",
                path.display(),
                e
            ))
        })?;
    }
    let toml_str = toml::to_string_pretty(config).map_err(|e| {
        LegionError::WatchConfig(format!(
            "cannot serialize watch config for {}: {}",
            path.display(),
            e
        ))
    })?;
    std::fs::write(path, toml_str)
        .map_err(|e| LegionError::WatchConfig(format!("cannot write {}: {}", path.display(), e)))?;
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

    // Trim empty/whitespace-only agent values to None so they never round-trip
    // to disk or bypass the recipient() fallback.
    let normalized_agent = agent
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let canonical = std::fs::canonicalize(workdir).map_err(|e| {
        LegionError::WatchConfig(format!(
            "cannot resolve workdir {}: {}",
            workdir.display(),
            e
        ))
    })?;
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

    // A shared `agent` across repos is intentional (#595): the field marks the
    // maintaining persona, so several repos legitimately share one agent (e.g.
    // `platform` owning `ledger`). The persona wake-lease dedups a directed
    // `@agent` signal to a single wake, so there is nothing to refuse here --
    // only a duplicate name or path (handled above) is a real conflict.

    config.repos.push(WatchRepoConfig {
        name: name.to_string(),
        workdir: canonical_str,
        agent: normalized_agent,
        broadcast_tags: Vec::new(),
        extra: toml::Table::new(),
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

    // The [cluster] section was retired from watch.toml: it was parsed but
    // never consumed, so an operator who configured it got validated-then-
    // ignored settings. Reject it loudly and point at the real home (#611).
    if config.cluster.is_some() {
        return Err(LegionError::WatchConfig(format!(
            "watch.toml contains a [cluster] section, but cluster sync is \
             configured in cluster.toml (same directory), not watch.toml. \
             Remove [cluster] from {} and put the settings in cluster.toml \
             (see `legion cluster --help`).",
            path.display()
        )));
    }

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

    // A shared `agent` across repos is intentional, not a collision (#595).
    // The `agent` field marks which persona MAINTAINS a repo, so a tiny lib
    // (e.g. `ledger`, owned by `platform`) deliberately shares the `platform`
    // agent instead of conjuring its own voice. The persona wake-lease (#314)
    // is keyed on the recipient, so a `@platform` signal wakes exactly one
    // platform session -- the other sharing repos lose the lease race and skip.
    // There is no multi-wake to guard against here.
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
///
/// Shells out to `kill -0` on Unix (signal 0 probes existence without
/// delivering a signal; the no-unsafe invariant rules out libc::kill).
/// Always returns `false` on non-Unix platforms where we cannot probe
/// process state. This is the single liveness probe for the whole binary --
/// watch locks and the daemon pidfile machinery share it so they can never
/// disagree about whether a pidfile holder is alive (#611).
pub(crate) fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
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

// -- Session Lock ------------------------------------------------------------

/// Per-repo session lock. Prevents `watch` from spawning a second agent session
/// for a repo while a prior session is still running.
///
/// The lockfile lives at `<data-dir>/sessions/<repo>.lock` and contains the
/// child process PID. Two signals count a lock as held:
/// - The PID in the file is alive (signal-0 check via `process_alive`).
/// - The file's mtime is within `ttl` of now.
///
/// Either condition failing (dead PID, or mtime older than TTL) causes the
/// lock to be treated as abandoned; the next spawn overwrites it. Explicit
/// release is available via `release` and is called both on clean exit
/// (reaper path) and on the stop-hook fast path (`record_session_end`).
pub struct SessionLockTracker {
    lock_dir: PathBuf,
    ttl: Duration,
}

impl SessionLockTracker {
    pub fn new(data_dir: &Path, ttl_secs: u64) -> Self {
        Self {
            lock_dir: data_dir.join("sessions"),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    fn lock_path(&self, repo: &str) -> PathBuf {
        self.lock_dir.join(format!("{}.lock", repo))
    }

    /// Path for the interactive-session lock file.
    ///
    /// Distinct from the TTL-gated `.lock` path used by watch-spawned children.
    /// Interactive sessions write their pid here; the gate is PID-liveness alone
    /// (no TTL) because an idle-but-open interactive session must still hold the
    /// gate -- a live process IS the authoritative signal.
    fn session_path(&self, repo: &str) -> PathBuf {
        self.lock_dir.join(format!("{}.session", repo))
    }

    /// Returns the holding PID when any live session (watch-spawned or interactive)
    /// holds the gate. Returns `None` when no live holder exists in either lock
    /// file; callers can safely spawn in all `None` cases.
    ///
    /// Two independent sources are checked:
    ///
    /// 1. `<repo>.lock` -- watch-spawned children. Held when BOTH mtime is within
    ///    `ttl` AND the PID is alive. The TTL guards against PID-reuse on
    ///    un-released locks from crashed spawns.
    ///
    /// 2. `<repo>.session` -- interactive (human-started) sessions. Held when
    ///    the PID is alive, regardless of mtime. No TTL is applied because a
    ///    live interactive session may be idle for hours and must still gate.
    ///    When the PID is dead the stale file is deleted opportunistically.
    ///
    /// The `.lock` holder is preferred when both sources report a live pid.
    /// The existing `.lock` mtime-TTL semantics are unchanged.
    pub fn active_pid(&self, repo: &str) -> Option<u32> {
        // -- .lock path (watch-spawned; TTL-gated) ----------------------------
        let lock_pid: Option<u32> = (|| {
            let path = self.lock_path(repo);
            let meta = std::fs::metadata(&path).ok()?;
            let mtime = meta.modified().ok()?;
            let age = mtime.elapsed().unwrap_or(Duration::ZERO);
            if age > self.ttl {
                return None;
            }
            let contents = std::fs::read_to_string(&path).ok()?;
            let pid = contents.trim().parse::<u32>().ok()?;
            if process_alive(pid) { Some(pid) } else { None }
        })();

        // -- .session path (interactive; PID-liveness only) -------------------
        let session_pid: Option<u32> = (|| {
            let path = self.session_path(repo);
            let contents = std::fs::read_to_string(&path).ok()?;
            let pid = contents.trim().parse::<u32>().ok()?;
            if process_alive(pid) {
                Some(pid)
            } else {
                // Opportunistically clean up the stale file so it does not
                // accumulate after the interactive session exits.
                let _ = std::fs::remove_file(&path);
                None
            }
        })();

        // Prefer .lock if both are held; fall back to .session.
        lock_pid.or(session_pid)
    }

    /// Write or overwrite the lockfile for `repo` with `pid`. Creates the
    /// parent directory if needed. Caller is responsible for having checked
    /// `active_pid` first if they want to respect an existing lock.
    pub fn record_spawn(&self, repo: &str, pid: u32) -> Result<()> {
        std::fs::create_dir_all(&self.lock_dir)?;
        std::fs::write(self.lock_path(repo), pid.to_string())?;
        Ok(())
    }

    /// Write or overwrite the interactive-session lock for `repo` with `pid`.
    ///
    /// Called from the `legion watch session-start` subcommand, which the
    /// SessionStart hook invokes with `$PPID` (the Claude session process).
    /// Unlike `record_spawn`, there is no TTL -- `active_pid` checks the
    /// written PID for liveness directly, so the lock is released only when
    /// the process exits (or when `release_interactive` is called explicitly).
    pub fn record_interactive(&self, repo: &str, pid: u32) -> Result<()> {
        std::fs::create_dir_all(&self.lock_dir)?;
        std::fs::write(self.session_path(repo), pid.to_string())?;
        Ok(())
    }

    /// Delete the interactive-session lock for `repo`.
    ///
    /// Idempotent: a missing file is not an error. Removes only the
    /// `.session` file; the `.lock` file (watch-spawned) is untouched.
    ///
    /// Available for explicit cleanup; the passive path is `active_pid`
    /// deleting stale files opportunistically when it reads a dead PID.
    #[allow(dead_code)] // public API completion -- no production call site yet
    pub fn release_interactive(&self, repo: &str) -> Result<()> {
        let path = self.session_path(repo);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(LegionError::Io(e)),
        }
    }

    /// Delete the lockfile for `repo`, freeing the per-repo wake gate so
    /// the next poll cycle can spawn a new agent session immediately rather
    /// than waiting for the TTL to expire.
    ///
    /// Idempotent: a missing lockfile is not an error. Called from two sites:
    /// - `reap_finished` when the tracked child has exited or its
    ///   `exit_observed_at` is set (stop-hook fired) -- authoritative path.
    /// - `record_session_end` immediately on the stop-hook fast path so the
    ///   gate clears before the next poll cycle.
    pub fn release(&self, repo: &str) -> Result<()> {
        let path = self.lock_path(repo);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(LegionError::Io(e)),
        }
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
/// `names` is the full addressable name set for this repo: the repo's
/// `recipient()` value plus any `broadcast_tags`. Signals addressed to any
/// name in this set are returned, along with reserved `@all` / `@everyone`
/// broadcasts. The DB handles per-repo dedup via `watch_handled` so each
/// broadcast wakes a repo exactly once regardless of which names it carries.
///
/// Returns signal reflection IDs and their text, filtered to only actual
/// signals (text starts with @).
pub fn find_pending_signals(
    db: &Database,
    repo_name: &str,
    names: &[String],
    since: Option<&str>,
) -> Result<Vec<(String, String, String)>> {
    let reflections = db.get_unhandled_signals_for_repo(repo_name, names, since)?;

    let mut signals: Vec<(String, String, String)> = Vec::new();
    for r in reflections {
        if signal::is_signal(&r.text) {
            signals.push((r.id, r.text, r.repo));
        }
    }

    Ok(signals)
}

// -- Agent Spawning ----------------------------------------------------------

/// Verbs whose presence in a directed signal triggers a watch wake (#404, #586).
///
/// `--verb` was designed to express intent; this set is the subsection of
/// intents that warrant spawning an asleep recipient. Other verbs
/// (`announce`, `ack`, `info`, `answer`, bare `review`) deliver to live
/// sessions via the channel push but do not page an asleep agent.
///
/// The canon (#586) is the set of verbs that actually mean "I need you to act":
/// `question`, `request`, `handoff`, `correction`, `proposal`, `decision`,
/// `rfc`, `routing`. The original #404 set carried `help` and `blocker`, but
/// those are *statuses* (`request:help`, `review:blocker`), not bare verbs --
/// they decorated the status slot, never the verb slot, so they never matched a
/// real signal's verb and only crowded the wake set. The verbs added here are
/// the ones bullpen mining shows the team actually uses to page each other.
///
/// Posts (text without a leading `@recipient`) never wake -- posts are
/// broadcasts; the `legion signal --to <agent> --verb <wake-worthy>`
/// primitive is the agent equivalent of a tweet at someone.
pub const WAKE_WORTHY_VERBS: &[&str] = &[
    "question",
    "request",
    "handoff",
    "correction",
    "proposal",
    "decision",
    "rfc",
    "routing",
];

/// Whether a directed `legion signal --to <to> --verb <verb>` will fail to wake
/// its recipient, so the sender can be warned at send time (#586).
///
/// True when the target is a specific agent (not the `@all`/`@everyone`
/// broadcast, which is never a directed page) AND the verb is not wake-worthy
/// in the active manifest. Such a signal still delivers to a live session via
/// the channel push, but it does not page an asleep agent -- surfacing that
/// avoids the silent "I signaled but nobody woke" trap. Reserved broadcast
/// names are matched case-insensitively.
pub fn directed_verb_will_not_wake(to: &str, verb: &str) -> bool {
    let to_normalized = to.trim().to_ascii_lowercase();
    let is_broadcast = matches!(to_normalized.as_str(), "all" | "everyone");
    !is_broadcast && !crate::verbs::active_manifest().is_wake_worthy(verb)
}

/// Whether a signal text triggers a wake under the verb-driven gate.
///
/// Returns `false` for posts, malformed signals, and signals whose verb is not
/// wake-worthy in the active manifest. The decision is verb-only -- status is
/// decoration that downstream tools may use, but the wake gate ignores it.
///
/// The active manifest defaults to `builtin_default()`, which reproduces the
/// #586 canon exactly. Operators can overlay additional verbs via TOML files
/// in the verbs directory without a legion release.
pub fn is_wake_worthy(text: &str) -> bool {
    match signal::parse_signal(text) {
        Some(sig) => crate::verbs::active_manifest().is_wake_worthy(&sig.verb),
        None => false,
    }
}

/// Signals that require the recipient to reply, even if the reply is a refusal.
///
/// Same gate as [`is_wake_worthy`] -- the wake decision and the
/// REQUIRES A REPLY routing in [`build_wake_prompt`] use one verb cut so a
/// signal that woke the agent always lands in the section the agent must
/// answer.
pub fn signal_requires_reply(text: &str) -> bool {
    is_wake_worthy(text)
}

/// Maximum reply-required signals rendered inline in the wake prompt; the
/// rest collapse into a tail line pointing at `legion bullpen --signals`.
/// Caps prevent the SessionStart additionalContext block from being drowned
/// by deep backlogs (rafters' pending block was 100KB pre-cap).
const PENDING_REPLY_CAP: usize = 10;
const PENDING_INFORMATIONAL_CAP: usize = 5;

/// Build the prompt context for a woken agent from pending signals.
///
/// Signals are split into two buckets:
/// - **Requires reply**: directed questions and requests. The agent MUST reply,
///   even if the reply is "no", "can't help", or "handing to X". Ghosting is not
///   an option -- it breaks team trust.
/// - **Informational**: announcements, updates, approvals. Silence is
///   acknowledgment; only reply if there is new information, concern, or dissent.
pub fn build_wake_prompt(repo_name: &str, signals: &[(String, String, String)]) -> String {
    let (must_reply, informational): (Vec<_>, Vec<_>) = signals
        .iter()
        .partition(|(_, text, _)| signal_requires_reply(text));

    let mut prompt = format!(
        "You were auto-woken by legion watch. The following signal(s) are directed at you ({}).\n",
        repo_name
    );

    let mut append_section = |header: &str, bucket: &[&(String, String, String)], cap: usize| {
        if bucket.is_empty() {
            return;
        }
        prompt.push('\n');
        prompt.push_str(header);
        prompt.push_str("\n\n");
        for (id, text, from_repo) in bucket.iter().take(cap) {
            prompt.push_str(&format!("- [from {}] {} (id: {})\n", from_repo, text, id));
        }
        if bucket.len() > cap {
            let extra = bucket.len() - cap;
            prompt.push_str(&format!(
                "- ... and {} more. Run `legion bullpen --repo {} --signals` to see them all.\n",
                extra, repo_name
            ));
        }
    };

    append_section(
        "REQUIRES A REPLY -- these are directed questions or requests. \
         You MUST reply to each one, even if the reply is \"no\", \"can't help\", \
         or \"handing to X\". Silence on a directed question is ghosting, not \
         acknowledgment. A short refusal is a valid reply; no reply is not.",
        &must_reply,
        PENDING_REPLY_CAP,
    );

    append_section(
        "INFORMATIONAL -- announcements, updates, approvals. \
         Silence is acknowledgment. Only reply if you have NEW information, \
         a concern, dissent, or an action item. Empty acknowledgments like \
         \"acknowledged, no action needed\" waste tokens and trigger wake storms.",
        &informational,
        PENDING_INFORMATIONAL_CAP,
    );

    prompt.push_str(
        "\nUse `legion signal` to reply, `legion bullpen` for broader context, and \
         `legion reflect` to store any learnings before you exit.",
    );

    prompt
}

/// Selects which spawn implementation `spawn_agent` dispatches to.
///
/// `Print` is the current `claude --print -p <prompt>` path. `Pty` is the
/// in-progress migration to a PTY-spawned interactive REPL (see #495);
/// the branch is stubbed at this stage so subsequent issues can fill it
/// in without risking the production path. Resolved once at watch
/// startup from `WATCH_SPAWN_MODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnMode {
    Print,
    Pty,
}

impl SpawnMode {
    /// Resolve from `WATCH_SPAWN_MODE`. Accepts `"print"` or `"pty"`
    /// (case-insensitive). Empty / unset / any other value falls back
    /// to `Pty` (the v0.16.0 default after #494 -- subscription
    /// billing for `claude --print -p` ended 2026-06-15). Operators
    /// who explicitly want the legacy path set `WATCH_SPAWN_MODE=print`.
    /// Unknown values log a warning so a typo is visible.
    pub fn from_env() -> Self {
        match std::env::var("WATCH_SPAWN_MODE") {
            Ok(raw) => Self::parse(&raw),
            Err(_) => Self::Pty,
        }
    }

    fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Self::Pty;
        }
        match trimmed.to_ascii_lowercase().as_str() {
            "print" => Self::Print,
            "pty" => Self::Pty,
            other => {
                eprintln!(
                    "[legion watch] unknown WATCH_SPAWN_MODE={:?} -- falling back to pty",
                    other
                );
                Self::Pty
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Print => "print",
            Self::Pty => "pty",
        }
    }
}

/// Spawn a `claude` session for the given repo under the resolved mode.
///
/// `Print` runs `claude --print -p <prompt>` exactly as before. `Pty`
/// is reserved for the PTY migration (#489) and currently returns
/// `LegionError::NotImplemented` -- callers must surface the error and
/// release any holds (the existing `poll_cycle` spawn-failure path does
/// this already).
///
/// Optimistic stop-hook handoff (#493). Writes `exit_observed_at` on
/// the wake_attempts row so the reaper can short-circuit a poll cycle,
/// AND releases the session lock so the wake gate clears immediately
/// rather than waiting for the TTL. PTY EOF + PID-poll remain the
/// authoritative completion signal -- this is a speed-up only.
/// Idempotent: missing rows return `Ok(())` because the hook may fire
/// for operator-attended sessions that watch never spawned (no wake row
/// -> still Ok; no lock -> release is a no-op).
pub fn record_session_end(db: &Database, attempt_id: &str, data_dir: &Path) -> Result<()> {
    match db.mark_wake_attempt_exit_observed(attempt_id) {
        Ok(()) => {}
        Err(LegionError::WakeAttemptNotFound(_)) => {
            // No wake row for this attempt_id -- operator-attended session
            // or a hook firing before the daemon wrote the row. Nothing to
            // release; return Ok so the stop hook never blocks Claude Code.
            return Ok(());
        }
        Err(e) => return Err(e),
    }

    // Resolve the repo from the wake_attempts row so we know which lockfile
    // to delete. The TTL is irrelevant here -- `release` deletes the lockfile
    // by path and never consults `ttl` -- so use the default rather than
    // reading watch.toml. A failure to release is non-fatal: the reaper's
    // authoritative path releases the lock on the next poll cycle.
    let session_locks = SessionLockTracker::new(data_dir, default_session_lock_ttl_secs());

    match db.get_wake_attempt(attempt_id) {
        Ok(Some(attempt)) => {
            if let Err(e) = session_locks.release(&attempt.repo_name) {
                eprintln!(
                    "[legion watch] session-end: failed to release lock for {}: {}",
                    attempt.repo_name, e
                );
            }
        }
        Ok(None) => {
            // Row was just written by mark_wake_attempt_exit_observed so
            // this branch should be unreachable in practice. Log and continue.
            eprintln!(
                "[legion watch] session-end: wake attempt {} not found after update (race?)",
                attempt_id
            );
        }
        Err(e) => {
            eprintln!(
                "[legion watch] session-end: could not look up attempt {} for lock release: {}",
                attempt_id, e
            );
        }
    }

    Ok(())
}

/// Polymorphic child handle returned by `spawn_agent`. The Print
/// branch wraps `std::process::Child` (unchanged from the `claude -p`
/// era); the Pty branch wraps `pty::PtySession` so the interactive
/// REPL retains subscription billing under the post-2026-06-15 cutoff.
///
/// Implements the minimal subset of operations the watch reaper +
/// lease release path actually need: `pid` for liveness + log lines,
/// `try_wait` for non-blocking exit detection, `kill` for shutdown.
/// Direct stdin/stdout streams are NOT exposed -- the PTY ring buffer
/// is for diagnostics, not control flow.
pub enum SpawnedChild {
    Print(Child),
    Pty(crate::pty::PtySession),
}

impl SpawnedChild {
    pub fn id(&self) -> u32 {
        match self {
            SpawnedChild::Print(c) => c.id(),
            SpawnedChild::Pty(s) => s.pid(),
        }
    }

    /// Non-blocking exit check. `Ok(Some(_))` once the child has
    /// exited (the inner value is the OS-reported success bit;
    /// callers that need exit code distinguishing use Print's Child
    /// directly or the PtySession's ExitStatus). `Ok(None)` while
    /// still running.
    pub fn try_wait(&mut self) -> Result<Option<bool>> {
        match self {
            SpawnedChild::Print(c) => match c.try_wait().map_err(LegionError::Io)? {
                Some(status) => Ok(Some(status.success())),
                None => Ok(None),
            },
            SpawnedChild::Pty(s) => match s.try_wait()? {
                Some(status) => Ok(Some(status.success)),
                None => Ok(None),
            },
        }
    }

    /// Terminate the child. Used by `reap_finished` to tear down an idle PTY
    /// REPL whose turn is complete, and by the spawn-failure cleanup path.
    pub fn kill(&mut self) -> Result<()> {
        match self {
            SpawnedChild::Print(c) => c.kill().map_err(LegionError::Io),
            SpawnedChild::Pty(s) => s.kill(),
        }
    }
}

pub fn spawn_agent(
    workdir: &str,
    prompt: &str,
    mode: SpawnMode,
    attempt_id: Option<&str>,
) -> Result<SpawnedChild> {
    match mode {
        SpawnMode::Print => {
            let mut cmd = std::process::Command::new("claude");
            cmd.args(["--print", "-p", prompt])
                .current_dir(workdir)
                .env("LEGION_AUTO_WAKE", "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            if let Some(id) = attempt_id {
                cmd.env("LEGION_WAKE_ATTEMPT_ID", id);
            }
            match cmd.spawn() {
                Ok(c) => Ok(SpawnedChild::Print(c)),
                Err(e) => {
                    eprintln!("[legion watch] failed to spawn agent: {}", e);
                    Err(LegionError::Io(e))
                }
            }
        }
        SpawnMode::Pty => {
            // PTY-spawned interactive `claude` REPL (#489). Subscription
            // billing applies because the REPL sees a TTY. Prompt is
            // injected via keystrokes through the master fd; the legion
            // plugin remains loaded inside the spawned session so the
            // stop hook can fire `legion watch session-end` (#493) as
            // an optimistic completion signal.
            let env: Vec<(&str, &str)> = {
                let mut e = vec![
                    ("LEGION_AUTO_WAKE", "1"),
                    ("LEGION_SPAWN_SOURCE", "watch-pty"),
                ];
                if let Some(id) = attempt_id {
                    e.push(("LEGION_WAKE_ATTEMPT_ID", id));
                }
                e
            };
            let cwd = std::path::Path::new(workdir);
            let mut opts = crate::pty::PtySpawnOptions::new("claude", &[], cwd);
            opts.env = &env;
            let mut session = crate::pty::PtySession::spawn(opts)?;
            // Inject the prompt as keystrokes. The trailing carriage
            // return submits the prompt; \n alone is not interpreted as
            // submit by every Claude Code interactive surface, but \r
            // works on every shipped version.
            let mut keystrokes = Vec::with_capacity(prompt.len() + 1);
            keystrokes.extend_from_slice(prompt.as_bytes());
            keystrokes.push(b'\r');
            if let Err(e) = session.write(&keystrokes) {
                eprintln!("[legion watch] PTY prompt write failed: {}", e);
                // Best-effort kill so we do not leak a half-started
                // child; the spawn-failure path in poll_cycle releases
                // any leases we acquired.
                let _ = session.kill();
                return Err(e);
            }
            Ok(SpawnedChild::Pty(session))
        }
    }
}

// -- Agent Tracker -----------------------------------------------------------

/// Tracks spawned child processes for active agent counting and persona
/// wake lease cleanup. Each tracked entry carries the (persona, signal_id)
/// pairs it acquired leases for so that `reap_finished` can release them
/// when the child exits.
pub struct TrackedChild {
    pub repo: String,
    pub child: SpawnedChild,
    /// `(persona_id, signal_id)` pairs this child acquired at spawn.
    pub held_leases: Vec<(String, String)>,
    /// Host identity the leases were acquired under. Required for the
    /// host-scoped release so a sync-resolved late-loser cannot drop the
    /// peer winner's row.
    pub host: String,
    /// UUIDv7 session id for the agent_session_log row (#389).
    pub session_id: String,
    /// RFC3339 timestamp of spawn -- defines the lower bound of the
    /// classification window for productive vs unproductive outcome.
    pub spawn_at: String,
    /// All signal ids bundled into this wake. Used to look up
    /// reflection-parent_id matches at reap time.
    pub signal_ids: Vec<String>,
    /// UUIDv7 of the wake_attempts row (#490, #491). `None` when the
    /// spawn predates the wake_attempts substrate or the enqueue
    /// failed; reap then skips the outcome recording.
    pub attempt_id: Option<String>,
}

pub struct AgentTracker {
    children: Vec<TrackedChild>,
}

impl AgentTracker {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    /// Record a spawned child process together with the leases it holds and
    /// the host identity under which they were acquired.
    #[allow(clippy::too_many_arguments)]
    pub fn track(
        &mut self,
        repo: String,
        child: SpawnedChild,
        held_leases: Vec<(String, String)>,
        host: String,
        session_id: String,
        spawn_at: String,
        signal_ids: Vec<String>,
        attempt_id: Option<String>,
    ) {
        self.children.push(TrackedChild {
            repo,
            child,
            held_leases,
            host,
            session_id,
            spawn_at,
            signal_ids,
            attempt_id,
        });
    }

    /// Reap finished child processes, removing them from tracking and
    /// releasing their held persona wake leases. Uses host-scoped release so
    /// a late-loser whose lease was overwritten by sync conflict resolution
    /// cannot accidentally drop the peer winner's row. Release errors are
    /// logged but not propagated -- a missed release ages out via TTL.
    ///
    /// In addition to reaping children that have actually exited (`try_wait`
    /// returns `Some`), this method also handles idle PTY REPLs: a PTY-spawned
    /// `claude` session sits alive after its turn (a REPL does not EOF), so
    /// `try_wait` returns `None` forever. When the child's `exit_observed_at`
    /// is set in the DB (its stop hook fired `legion watch session-end`), the
    /// turn is considered complete: the child is killed best-effort and the full
    /// reap path runs. The session lock is released in both cases so the per-repo
    /// wake gate opens immediately rather than waiting for the TTL.
    pub fn reap_finished(
        &mut self,
        db: Option<&Database>,
        session_locks: Option<&SessionLockTracker>,
    ) {
        self.children.retain_mut(|tracked| {
            // Determine whether this child should be reaped.
            // Two paths:
            //   A. Child already exited: try_wait returns Ok(Some(success)).
            //   B. Child is still alive (try_wait returns Ok(None)) but its
            //      stop hook has set exit_observed_at -- the turn is done and
            //      the REPL is sitting idle. Kill it and run the reap path.
            let reap_result: Option<bool> = match tracked.child.try_wait() {
                Ok(Some(success)) => {
                    // Path A: child exited naturally.
                    eprintln!(
                        "[legion watch] agent for {} exited ({})",
                        tracked.repo,
                        if success { "ok" } else { "error" }
                    );
                    Some(success)
                }
                Ok(None) => {
                    // Path B: child is still alive. Check exit_observed_at to
                    // detect a lingering idle PTY REPL whose turn is complete.
                    // Only consult the DB when we have an attempt_id and a DB
                    // handle; without those we cannot make the determination.
                    let turn_done = match (&tracked.attempt_id, db) {
                        (Some(aid), Some(db)) => {
                            match db.get_wake_attempt(aid) {
                                Ok(Some(ref attempt)) => attempt.exit_observed_at.is_some(),
                                Ok(None) => {
                                    // Row vanished (e.g. retention-pruned) while the
                                    // child is still alive. Log -- otherwise this is a
                                    // no-trace path: the child can no longer be reaped
                                    // via the stop-hook signal and falls back to the
                                    // TTL/exit backstop with no record of why.
                                    eprintln!(
                                        "[legion watch] wake row {} missing while child for {} still alive -- cannot confirm turn completion",
                                        aid, tracked.repo
                                    );
                                    false
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[legion watch] could not check exit_observed_at for {}: {}",
                                        aid, e
                                    );
                                    false
                                }
                            }
                        }
                        _ => false,
                    };

                    if turn_done {
                        // Stop-hook fired: turn complete, child is an idle REPL.
                        eprintln!(
                            "[legion watch] agent for {} turn complete (stop-hook) -- terminating idle REPL",
                            tracked.repo
                        );
                        match tracked.child.kill() {
                            Ok(()) => {
                                // Killed cleanly. Classify as ok: the turn completed
                                // normally (the stop hook firing is the agent's
                                // success signal); the kill is cleanup, not an error.
                                Some(true)
                            }
                            Err(e) => {
                                // Kill failed, so the REPL is STILL ALIVE. Keep it
                                // tracked and retry next poll rather than dropping
                                // it: a dropped-but-alive child leaks the process
                                // AND releases the lock, opening the gate for a
                                // colliding spawn -- the inverse of this fix. The
                                // TTL remains the ultimate backstop.
                                eprintln!(
                                    "[legion watch] kill of idle REPL for {} failed -- keeping tracked for retry: {}",
                                    tracked.repo, e
                                );
                                return true; // keep tracking; do not leak or open the gate
                            }
                        }
                    } else {
                        // Still genuinely running and no completion signal yet.
                        return true; // keep tracking
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[legion watch] error checking agent for {}: {}",
                        tracked.repo, e
                    );
                    return true; // keep tracking -- process may still be running
                }
            };

            let success: bool = reap_result.unwrap_or(false);

            // Release the session lock so the per-repo wake gate clears
            // immediately. A missing lock is Ok (already released by the
            // stop-hook fast path). Errors are logged but never panic the
            // watch thread.
            if let Some(locks) = session_locks
                && let Err(e) = locks.release(&tracked.repo)
            {
                eprintln!(
                    "[legion watch] failed to release session lock for {}: {}",
                    tracked.repo, e
                );
            }

            if let Some(db) = db {
                for (persona, signal_id) in &tracked.held_leases {
                    if let Err(e) =
                        db.release_persona_lease_if_owner(persona, signal_id, &tracked.host)
                    {
                        eprintln!(
                            "[legion watch] failed to release lease {}/{}: {}",
                            persona, signal_id, e
                        );
                    }
                }

                // #389: classify and persist the session outcome.
                // Defensive: errors here log to stderr but never break
                // the reap loop -- a missed log row is recoverable, a
                // panic in the watch thread is not.
                let exit_at = chrono::Utc::now().to_rfc3339();
                let exit_status = if success { "ok" } else { "error" };
                let outcome = if !success {
                    Database::OUTCOME_ERRORED
                } else {
                    match db.classify_session(
                        &tracked.repo,
                        &tracked.signal_ids,
                        &tracked.spawn_at,
                        &exit_at,
                    ) {
                        Ok(true) => Database::OUTCOME_PRODUCTIVE,
                        Ok(false) => Database::OUTCOME_UNPRODUCTIVE,
                        Err(e) => {
                            eprintln!(
                                "[legion watch] classify failed for {} ({}): {}",
                                tracked.repo, tracked.session_id, e
                            );
                            // Conservative: do not record an unknown
                            // outcome as productive. Skip the row.
                            return false;
                        }
                    }
                };
                if let Err(e) = db.record_session_outcome(
                    &tracked.session_id,
                    &tracked.repo,
                    &tracked.signal_ids,
                    &tracked.spawn_at,
                    &exit_at,
                    exit_status,
                    outcome,
                ) {
                    eprintln!(
                        "[legion watch] failed to record session outcome for {} ({}): {}",
                        tracked.repo, tracked.session_id, e
                    );
                }

                // #490: stamp the wake_attempt's terminal state.
                // Idempotent at the DB layer; errors logged but
                // not propagated (the lease release + outcome
                // record above are the load-bearing writes).
                if let Some(ref attempt_id) = tracked.attempt_id
                    && let Err(e) =
                        db.record_wake_attempt_outcome(attempt_id, exit_status, outcome)
                {
                    eprintln!(
                        "[legion watch] failed to record wake attempt outcome {}: {}",
                        attempt_id, e
                    );
                }
            }
            false // remove from tracking list
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

/// Resolve this host's identity for persona wake lease ownership. Falls back
/// to `"unknown"` when the system hostname is not available.
pub fn resolve_host_id() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "unknown".to_owned())
}

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
/// The things that stay loop-specific are: the sleep mechanism, the timer
/// types (`std::time::Instant` vs `tokio::time::Instant`), startup logging,
/// and the cluster sync-actor spawn (standalone only).
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
        assert_eq!(config.max_concurrent_wakes, 4);
    }

    #[test]
    fn parse_config_max_concurrent_wakes_explicit() {
        let toml_str = r#"
max_concurrent_wakes = 1

[[repos]]
name = "test"
workdir = "/tmp"
"#;
        let config: WatchConfig = toml::from_str(toml_str).expect("parse config");
        assert_eq!(config.max_concurrent_wakes, 1);
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

        let signals = find_pending_signals(&db, "legion", &["legion".to_string()], None)
            .expect("find signals");
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
        let shingle =
            find_pending_signals(&db, "shingle", &["shingle".to_string()], None).expect("shingle");
        let huttspawn = find_pending_signals(&db, "huttspawn", &["huttspawn".to_string()], None)
            .expect("huttspawn");
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
        let legion =
            find_pending_signals(&db, "legion", &["legion".to_string()], None).expect("legion");
        assert!(legion.is_empty(), "sender should not see own signal");

        // unrelated repo should NOT see it
        let kelex =
            find_pending_signals(&db, "kelex", &["kelex".to_string()], None).expect("kelex");
        assert!(kelex.is_empty(), "unmentioned repo should not see signal");
    }

    #[test]
    fn find_pending_signals_excludes_self_signals() {
        let (db, _index, _dir) = test_storage();

        // Signal from legion to legion should not be returned
        db.insert_reflection("legion", "@legion review:approved", "team")
            .expect("insert self-signal");

        let signals = find_pending_signals(&db, "legion", &["legion".to_string()], None)
            .expect("find signals");
        assert!(signals.is_empty(), "self-signals should be excluded");
    }

    #[test]
    fn find_pending_signals_with_agent_excludes_self_signals_via_repo_name() {
        // Regression guard: when agent != name (e.g., platform agent watches ledger repo),
        // a signal posted FROM the ledger repo targeting @platform must still be excluded
        // by the repo_name key, not by the recipient. Previously only recipient was passed,
        // so `r.repo != 'platform'` vs. `r.repo = 'ledger'` produced a self-wake.
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("ledger", "@platform review:approved", "team")
            .expect("insert self-signal");

        let signals = find_pending_signals(&db, "ledger", &["platform".to_string()], None)
            .expect("find signals");
        assert!(
            signals.is_empty(),
            "self-signals must be excluded by repo_name, not by recipient"
        );
    }

    #[test]
    fn find_pending_signals_with_agent_accepts_signals_from_other_repos() {
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("kelex", "@platform review:approved", "team")
            .expect("insert external signal");

        let signals = find_pending_signals(&db, "ledger", &["platform".to_string()], None)
            .expect("find signals");
        assert_eq!(signals.len(), 1, "external signal to agent should be found");
    }

    #[test]
    fn mark_handled_prevents_re_detection() {
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("kelex", "@legion review:approved", "team")
            .expect("insert signal");

        let signals =
            find_pending_signals(&db, "legion", &["legion".to_string()], None).expect("first poll");
        assert_eq!(signals.len(), 1);

        // Mark as handled for legion
        let (id, _, _) = &signals[0];
        db.mark_signal_handled_for_repo(id, "legion")
            .expect("mark handled");

        // Should not appear again for legion
        let signals = find_pending_signals(&db, "legion", &["legion".to_string()], None)
            .expect("second poll");
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
    fn build_wake_prompt_splits_questions_from_announcements() {
        let signals = vec![
            (
                "id-q".to_string(),
                "@kessel question:help -- can you extract geometry?".to_string(),
                "huttspawn".to_string(),
            ),
            (
                "id-a".to_string(),
                "@all announce:ready -- dev tracker RSS unlocked".to_string(),
                "huttspawn".to_string(),
            ),
            (
                "id-r".to_string(),
                "@platform request:help -- need embeddings review".to_string(),
                "kelex".to_string(),
            ),
        ];

        let prompt = build_wake_prompt("kessel", &signals);

        // Directed questions/requests MUST be flagged as reply-required.
        assert!(
            prompt.contains("REQUIRES A REPLY"),
            "directed question should trigger reply-required section"
        );
        assert!(
            prompt.contains("id-q"),
            "question signal missing from prompt"
        );
        assert!(
            prompt.contains("id-r"),
            "request signal missing from prompt"
        );

        // Announcements belong in the informational section.
        assert!(
            prompt.contains("INFORMATIONAL"),
            "announcement should trigger informational section"
        );
        assert!(
            prompt.contains("id-a"),
            "announce signal missing from prompt"
        );

        // The reply-required section must appear BEFORE the informational one so
        // the agent reads its obligations first.
        let reply_idx = prompt.find("REQUIRES A REPLY").expect("reply section");
        let info_idx = prompt.find("INFORMATIONAL").expect("info section");
        assert!(reply_idx < info_idx, "reply-required must come first");
    }

    #[test]
    fn build_wake_prompt_routes_by_verb_only() {
        // #404: wake/reply decision is verb-only. Status is decoration.
        // The pre-#404 fallback that treated `status=request` or `status=help`
        // on any verb as reply-required was a workaround for the broken
        // text-prefix wake gate. Senders who want a reply now use a
        // wake-worthy verb directly: `--verb request`, `--verb handoff`,
        // `--verb question`, `--verb decision` (#586 canon -- `help` and
        // `blocker` are statuses, not wake verbs).
        let signals = vec![
            (
                "id-rev-req".to_string(),
                "@platform review:request {doc: rfc.md} -- review please".to_string(),
                "smugglr".to_string(),
            ),
            (
                "id-handoff-verb".to_string(),
                "@platform handoff -- taking the rfc over to you".to_string(),
                "kelex".to_string(),
            ),
            (
                "id-request-verb".to_string(),
                "@platform request -- could you review this".to_string(),
                "kelex".to_string(),
            ),
            (
                "id-approved".to_string(),
                "@platform review:approved -- LGTM".to_string(),
                "smugglr".to_string(),
            ),
        ];

        let prompt = build_wake_prompt("platform", &signals);

        assert!(prompt.contains("REQUIRES A REPLY"));
        assert!(prompt.contains("INFORMATIONAL"));
        let reply_section = &prompt
            [prompt.find("REQUIRES A REPLY").unwrap()..prompt.find("INFORMATIONAL").unwrap()];

        assert!(
            reply_section.contains("id-handoff-verb"),
            "verb=handoff must require reply"
        );
        assert!(
            reply_section.contains("id-request-verb"),
            "verb=request must require reply"
        );
        assert!(
            !reply_section.contains("id-rev-req"),
            "verb=review with status=request is no longer wake-worthy (#404 verb-only gate)"
        );
        assert!(
            !reply_section.contains("id-approved"),
            "verb=review with status=approved must not require reply"
        );
    }

    #[test]
    fn is_wake_worthy_recognizes_designated_verbs() {
        for verb in WAKE_WORTHY_VERBS {
            let text = format!("@kessel {verb} -- something");
            assert!(
                is_wake_worthy(&text),
                "verb `{verb}` should be wake-worthy: {text}"
            );
        }
    }

    #[test]
    fn is_wake_worthy_rejects_informational_verbs() {
        // `help` and `blocker` are included here deliberately (#586): they are
        // statuses (`request:help`, `review:blocker`), not bare wake verbs, and
        // must not wake when used in the verb slot.
        for verb in [
            "announce", "ack", "info", "answer", "review", "help", "blocker",
        ] {
            let text = format!("@kessel {verb} -- fyi");
            assert!(
                !is_wake_worthy(&text),
                "verb `{verb}` must not be wake-worthy: {text}"
            );
        }
    }

    /// `signal_requires_reply` is a deliberate alias of `is_wake_worthy`:
    /// one verb cut so a signal that woke the agent always lands in the
    /// section the agent must answer. Pin the equivalence so the two names
    /// cannot drift apart silently -- an intentional divergence must come
    /// here and change this test.
    #[test]
    fn signal_requires_reply_matches_is_wake_worthy() {
        let samples = [
            "@kessel question -- can you help?",
            "@kessel request:help -- pls",
            "@kessel handoff -- yours now",
            "@all announce -- shipped",
            "@kessel ack -- got it",
            "@kessel review:approved -- LGTM",
            "not a signal at all",
        ];
        for text in samples {
            assert_eq!(
                signal_requires_reply(text),
                is_wake_worthy(text),
                "wake decision and reply-required routing diverged for: {text}"
            );
        }
    }

    #[test]
    fn is_wake_worthy_rejects_posts() {
        // Posts don't start with @recipient -- never wake.
        assert!(!is_wake_worthy("just a musing about something"));
        assert!(!is_wake_worthy("checking in @kessel did you see #401"));
    }

    #[test]
    fn is_wake_worthy_ignores_status_decoration() {
        // Status no longer gates wake; only the verb does.
        assert!(
            !is_wake_worthy("@platform review:request {doc: rfc.md}"),
            "verb=review with status=request must not wake under verb-only gate"
        );
        assert!(
            is_wake_worthy("@platform request:help -- pls"),
            "verb=request stays wake-worthy regardless of status"
        );
    }

    #[test]
    fn directed_non_wake_verb_warns() {
        // A directed signal with an informational verb should warn.
        assert!(directed_verb_will_not_wake("kessel", "announce"));
        assert!(directed_verb_will_not_wake("kessel", "ack"));
        // `help`/`blocker` are statuses, not wake verbs (#586) -- they warn too.
        assert!(directed_verb_will_not_wake("kessel", "help"));
        assert!(directed_verb_will_not_wake("kessel", "blocker"));
    }

    #[test]
    fn directed_wake_verb_does_not_warn() {
        for verb in WAKE_WORTHY_VERBS {
            assert!(
                !directed_verb_will_not_wake("kessel", verb),
                "wake-worthy verb `{verb}` must not warn"
            );
        }
    }

    #[test]
    fn broadcast_target_never_warns() {
        // @all / @everyone are not directed pages, so no warning regardless of verb.
        assert!(!directed_verb_will_not_wake("all", "announce"));
        assert!(!directed_verb_will_not_wake("everyone", "announce"));
        assert!(!directed_verb_will_not_wake("All", "ack")); // case-insensitive
    }

    #[test]
    fn build_wake_prompt_caps_long_buckets() {
        // 15 reply-required questions: cap is 10, expect 10 rendered + tail.
        let questions: Vec<(String, String, String)> = (0..15)
            .map(|i| {
                (
                    format!("id-q-{i}"),
                    format!("@legion question: thing {i}?"),
                    "rafters".to_string(),
                )
            })
            .collect();
        let prompt = build_wake_prompt("legion", &questions);
        let rendered = prompt.matches("(id: id-q-").count();
        assert_eq!(
            rendered, PENDING_REPLY_CAP,
            "expected exactly {PENDING_REPLY_CAP} rendered ids, got {rendered}: {prompt}"
        );
        assert!(
            prompt.contains("... and 5 more"),
            "expected tail line for 5 overflow signals: {prompt}"
        );
        assert!(
            prompt.contains("legion bullpen --repo legion --signals"),
            "tail should point at bullpen --signals: {prompt}"
        );

        // 8 informational announcements: cap is 5.
        let announcements: Vec<(String, String, String)> = (0..8)
            .map(|i| {
                (
                    format!("id-a-{i}"),
                    format!("@all announce: thing {i}"),
                    "rafters".to_string(),
                )
            })
            .collect();
        let prompt = build_wake_prompt("legion", &announcements);
        let rendered = prompt.matches("(id: id-a-").count();
        assert_eq!(
            rendered, PENDING_INFORMATIONAL_CAP,
            "expected exactly {PENDING_INFORMATIONAL_CAP} rendered ids, got {rendered}: {prompt}"
        );
        assert!(
            prompt.contains("... and 3 more"),
            "expected tail line for 3 overflow announcements: {prompt}"
        );
    }

    #[test]
    fn build_wake_prompt_omits_sections_when_empty() {
        let announce_only = vec![(
            "id-a".to_string(),
            "@all announce: shipped v0.9.3".to_string(),
            "rafters".to_string(),
        )];
        let prompt = build_wake_prompt("legion", &announce_only);
        assert!(!prompt.contains("REQUIRES A REPLY"));
        assert!(prompt.contains("INFORMATIONAL"));

        let question_only = vec![(
            "id-q".to_string(),
            "@eavesdrop question: when does the feed index?".to_string(),
            "huttspawn".to_string(),
        )];
        let prompt = build_wake_prompt("eavesdrop", &question_only);
        assert!(prompt.contains("REQUIRES A REPLY"));
        assert!(!prompt.contains("INFORMATIONAL"));
    }

    /// Run a short-lived child to completion and return its (now-dead) PID.
    /// Used to test the dead-PID branch of `SessionLockTracker::active_pid`
    /// without racing against PID reuse on the test host.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let pid = child.id();
        let _ = child.wait();
        pid
    }

    // These tests depend on `process_alive` returning true for our own PID,
    // which is currently Unix-only (see `process_alive` at the top of this
    // file). Gating them keeps Windows CI green while the session lock gate
    // degrades to "always allow spawn" on Windows -- a known pre-existing
    // limitation of the PID-lock code, not a regression from this change.
    // Windows support is tracked separately.
    #[cfg(unix)]
    #[test]
    fn session_lock_active_for_fresh_live_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks
            .record_spawn("legion", std::process::id())
            .expect("record");
        assert!(
            locks.active_pid("legion").is_some(),
            "own PID + fresh mtime should read as active"
        );
    }

    #[test]
    fn session_lock_inactive_for_dead_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks.record_spawn("legion", dead_pid()).expect("record");
        assert!(
            locks.active_pid("legion").is_none(),
            "dead PID should be treated as abandoned"
        );
    }

    #[test]
    fn session_lock_inactive_when_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        // TTL=1s + sleep > TTL proves the stale-mtime branch fires for a
        // non-zero TTL -- distinguishing it from the TTL=0 disable sentinel.
        let locks = SessionLockTracker::new(dir.path(), 1);
        locks
            .record_spawn("legion", std::process::id())
            .expect("record");
        std::thread::sleep(Duration::from_millis(1_100));
        assert!(
            locks.active_pid("legion").is_none(),
            "stale mtime should be treated as abandoned even with live PID"
        );
    }

    // See the cfg(unix) note on `session_lock_active_for_fresh_live_pid`:
    // the overwrite assertion relies on our own PID reading as alive.
    #[cfg(unix)]
    #[test]
    fn session_lock_record_spawn_overwrites_abandoned_lock() {
        // Acceptance criterion 5 from issue #274: a dead-PID / stale-mtime
        // lock must be overwritten on the next spawn and the fresh lock then
        // read as active. Proves the abandon-and-replace path end-to-end.
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);

        // Seed with an abandoned (dead-PID) lock.
        locks.record_spawn("legion", dead_pid()).expect("seed");
        assert!(
            locks.active_pid("legion").is_none(),
            "precondition: seeded lock must read as abandoned"
        );

        // Re-record with our own (live) PID, simulating a successful respawn.
        locks
            .record_spawn("legion", std::process::id())
            .expect("overwrite");
        assert_eq!(
            locks.active_pid("legion"),
            Some(std::process::id()),
            "overwrite must leave a live lock holding the new PID"
        );
    }

    #[test]
    fn session_lock_inactive_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        assert!(
            locks.active_pid("legion").is_none(),
            "missing lockfile should read as inactive"
        );
    }

    // -- SessionLockTracker::release tests ------------------------------------

    #[test]
    fn session_lock_release_clears_active_pid() {
        // After record_spawn, active_pid returns Some. After release it returns
        // None -- the gate is open again for the next spawn.
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks
            .record_spawn("legion", std::process::id())
            .expect("record spawn");
        // Pre-condition: lock is active (our own live PID).
        #[cfg(unix)]
        assert!(
            locks.active_pid("legion").is_some(),
            "precondition: lock must be active after record_spawn"
        );
        locks.release("legion").expect("release");
        assert!(
            locks.active_pid("legion").is_none(),
            "active_pid must return None after release"
        );
    }

    #[test]
    fn session_lock_release_of_missing_lock_is_ok() {
        // release on a repo that was never locked (or already released) is idempotent.
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        // No record_spawn called -- lockfile does not exist.
        let result = locks.release("nonexistent-repo");
        assert!(
            result.is_ok(),
            "release of a missing lockfile must return Ok, got {:?}",
            result
        );
    }

    // -- record_interactive / release_interactive / active_pid (.session) ----

    /// A .session file written with the test process's (live) PID makes
    /// active_pid return Some, even with no .lock file present.
    #[cfg(unix)]
    #[test]
    fn interactive_lock_active_for_live_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks
            .record_interactive("myrepo", std::process::id())
            .expect("record_interactive");
        let held = locks.active_pid("myrepo");
        assert!(
            held.is_some(),
            "a .session with a live pid must make active_pid return Some"
        );
        assert_eq!(
            held,
            Some(std::process::id()),
            "active_pid must return the pid from the .session file"
        );
    }

    /// A .session file with a dead PID returns None.
    #[test]
    fn interactive_lock_inactive_for_dead_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks
            .record_interactive("myrepo", dead_pid())
            .expect("record_interactive");
        assert!(
            locks.active_pid("myrepo").is_none(),
            "a .session with a dead pid must return None"
        );
    }

    /// A .session with a live PID gates even when mtime is not fresh (no TTL
    /// applied). This is the key property: an idle interactive session must
    /// still hold the gate regardless of how old the file is.
    /// We cannot easily age the mtime in a test, so we assert that a freshly
    /// written .session with a live pid -- and no .lock file present -- gates.
    /// The absence of a TTL check in active_pid for the .session path is the
    /// structural guarantee; this test confirms the live-pid gate works.
    #[cfg(unix)]
    #[test]
    fn interactive_lock_gates_without_lock_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        // TTL=1s so any .lock would expire quickly; .session must still hold.
        let locks = SessionLockTracker::new(dir.path(), 1);
        locks
            .record_interactive("myrepo", std::process::id())
            .expect("record_interactive");
        // No .lock file written; gate must still be held via .session alone.
        assert!(
            locks.active_pid("myrepo").is_some(),
            "live .session with no .lock must still gate (no TTL on .session)"
        );
    }

    /// release_interactive removes only the .session file; the .lock is untouched.
    #[cfg(unix)]
    #[test]
    fn interactive_lock_release_removes_only_session_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        // Write both a .lock and a .session for the same repo.
        locks
            .record_spawn("myrepo", std::process::id())
            .expect("record_spawn");
        locks
            .record_interactive("myrepo", std::process::id())
            .expect("record_interactive");
        assert!(
            locks.active_pid("myrepo").is_some(),
            "precondition: both locks active"
        );

        // release_interactive only.
        locks
            .release_interactive("myrepo")
            .expect("release_interactive");

        // The .lock must still hold the gate.
        assert!(
            locks.active_pid("myrepo").is_some(),
            ".lock must still gate after release_interactive"
        );

        // The .session file must be gone.
        assert!(
            !locks.session_path("myrepo").exists(),
            ".session file must be deleted by release_interactive"
        );
    }

    /// release_interactive on a missing .session file is idempotent.
    #[test]
    fn interactive_lock_release_of_missing_session_is_ok() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        let result = locks.release_interactive("nonexistent-repo");
        assert!(
            result.is_ok(),
            "release_interactive on missing .session must return Ok, got {:?}",
            result
        );
    }

    /// Coexistence: a live .lock and no .session still gates (existing behavior
    /// preserved by the refactored active_pid).
    #[cfg(unix)]
    #[test]
    fn lock_file_alone_still_gates_after_active_pid_refactor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks
            .record_spawn("myrepo", std::process::id())
            .expect("record_spawn");
        assert!(
            locks.active_pid("myrepo").is_some(),
            "a live .lock with no .session must still gate (existing semantics preserved)"
        );
    }

    // -- reap_finished: exit_observed_at path ---------------------------------

    // Spawn a long-lived child (sleep) and record its state in the tracker so
    // the exit_observed_at branch of reap_finished can be exercised without
    // waiting for the process to exit naturally.
    #[cfg(unix)]
    #[test]
    fn reap_finished_kills_idle_pty_repl_when_exit_observed() {
        // A live child process (sleep) that will not exit on its own stands in
        // for the idle PTY REPL. Once exit_observed_at is set on the DB row,
        // reap_finished must kill the child and remove it from tracking.
        let (db, _index, data_dir) = test_storage();
        let locks = SessionLockTracker::new(data_dir.path(), 3600);

        // Spawn a long-lived child -- sleep 9999 simulates an idle REPL.
        let child_proc = std::process::Command::new("sleep")
            .arg("9999")
            .spawn()
            .expect("spawn sleep");
        let child_pid = child_proc.id();

        // Set up the wake_attempts row with a known attempt_id.
        let attempt_id = uuid::Uuid::now_v7().to_string();
        let signal_id = db
            .insert_reflection("kelex", "@legion question:test-reap", "team")
            .expect("insert signal")
            .id;
        db.enqueue_wake_attempt(&attempt_id, "legion", "legion", &[signal_id])
            .expect("enqueue");

        // Record the session lock so we can verify it is released.
        locks
            .record_spawn("legion", child_pid)
            .expect("record spawn");
        assert!(
            locks.active_pid("legion").is_some(),
            "precondition: lock must be active after record_spawn"
        );

        // Mark exit_observed_at to simulate the stop hook firing.
        db.mark_wake_attempt_exit_observed(&attempt_id)
            .expect("mark exit observed");

        // Build the tracked child entry (Print branch wraps std::process::Child).
        let spawned = SpawnedChild::Print(child_proc);
        let session_id = uuid::Uuid::now_v7().to_string();
        let spawn_at = chrono::Utc::now().to_rfc3339();
        let mut tracker = AgentTracker::new();
        tracker.track(
            "legion".to_string(),
            spawned,
            Vec::new(),
            "test-host".to_string(),
            session_id,
            spawn_at,
            Vec::new(),
            Some(attempt_id.clone()),
        );

        assert_eq!(tracker.active_count(), 1, "precondition: child is tracked");

        // Run the reaper with session_locks provided.
        tracker.reap_finished(Some(&db), Some(&locks));

        // The child must have been reaped (removed from tracking).
        assert_eq!(
            tracker.active_count(),
            0,
            "child must be reaped when exit_observed_at is set"
        );

        // The session lock must have been released.
        assert!(
            locks.active_pid("legion").is_none(),
            "session lock must be released after reap"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reap_finished_keeps_running_child_without_exit_observed() {
        // A live child with no exit_observed_at must NOT be reaped -- it is
        // genuinely mid-turn and we must not interrupt it.
        let (db, _index, data_dir) = test_storage();
        let locks = SessionLockTracker::new(data_dir.path(), 3600);

        let child_proc = std::process::Command::new("sleep")
            .arg("9999")
            .spawn()
            .expect("spawn sleep");

        // Enqueue a wake attempt but do NOT mark exit_observed_at.
        let attempt_id = uuid::Uuid::now_v7().to_string();
        let sig = db
            .insert_reflection("kelex", "@legion question:active-turn", "team")
            .expect("signal")
            .id;
        db.enqueue_wake_attempt(&attempt_id, "legion", "legion", &[sig])
            .expect("enqueue");

        locks
            .record_spawn("legion", child_proc.id())
            .expect("record spawn");
        assert!(
            locks.active_pid("legion").is_some(),
            "precondition: lock is active"
        );

        let spawned = SpawnedChild::Print(child_proc);
        let session_id = uuid::Uuid::now_v7().to_string();
        let spawn_at = chrono::Utc::now().to_rfc3339();
        let mut tracker = AgentTracker::new();
        tracker.track(
            "legion".to_string(),
            spawned,
            Vec::new(),
            "test-host".to_string(),
            session_id,
            spawn_at,
            Vec::new(),
            Some(attempt_id),
        );

        assert_eq!(tracker.active_count(), 1, "precondition: child tracked");

        // Reaper: child is live and exit_observed_at is NOT set.
        tracker.reap_finished(Some(&db), Some(&locks));

        // Child must still be tracked -- the turn is genuinely in progress.
        assert_eq!(
            tracker.active_count(),
            1,
            "active child with no exit_observed_at must stay tracked"
        );

        // Session lock must still be held.
        assert!(
            locks.active_pid("legion").is_some(),
            "session lock must remain active for an in-progress turn"
        );

        // Clean up the long-running child so we don't leak it in the test suite.
        tracker.children.iter_mut().for_each(|t| {
            let _ = t.child.kill();
        });
    }

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

    #[test]
    fn broadcast_signals_visible_to_all_repos() {
        let (db, _index, _dir) = test_storage();

        // Post an @all signal from kelex
        db.insert_reflection("kelex", "@all RFC:help -- discover proposal", "team")
            .expect("insert broadcast");

        // Both legion and rafters should see it
        let legion_signals =
            find_pending_signals(&db, "legion", &["legion".to_string()], None).expect("legion");
        let rafters_signals =
            find_pending_signals(&db, "rafters", &["rafters".to_string()], None).expect("rafters");
        assert_eq!(legion_signals.len(), 1);
        assert_eq!(rafters_signals.len(), 1);

        // Mark handled for legion using per-repo tracking
        for (id, _, _) in &legion_signals {
            db.mark_signal_handled_for_repo(id, "legion")
                .expect("mark handled for legion");
        }

        // legion should NOT see it anymore
        let legion_after = find_pending_signals(&db, "legion", &["legion".to_string()], None)
            .expect("legion after");
        assert!(
            legion_after.is_empty(),
            "legion should not see broadcast after handling"
        );

        // rafters should STILL see the broadcast
        let rafters_after = find_pending_signals(&db, "rafters", &["rafters".to_string()], None)
            .expect("rafters after");
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
        let rafters_final = find_pending_signals(&db, "rafters", &["rafters".to_string()], None)
            .expect("rafters final");
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

    /// The single liveness probe shared by watch locks and the daemon
    /// pidfile machinery: our own PID reads as alive, a reaped child's PID
    /// reads as dead.
    #[cfg(unix)]
    #[test]
    fn process_alive_distinguishes_live_and_dead_pids() {
        assert!(
            process_alive(std::process::id()),
            "our own PID must read as alive"
        );
        assert!(
            !process_alive(dead_pid()),
            "a reaped child's PID must read as dead"
        );
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
    fn add_repo_allows_shared_agent_across_repos() {
        // #595: a shared `agent` is intentional (the maintaining persona), so
        // adding a second repo with an existing agent must SUCCEED. The wake
        // lease dedups a directed signal to one session; the only real add
        // conflicts are a duplicate name or path.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir_a = dir.path().join("a");
        let workdir_b = dir.path().join("b");
        std::fs::create_dir_all(&workdir_a).expect("create a");
        std::fs::create_dir_all(&workdir_b).expect("create b");

        add_repo_to_config(&config_path, "platform", &workdir_a, Some("platform"))
            .expect("add platform");
        // ledger maintained by the same platform agent -- must be allowed.
        add_repo_to_config(&config_path, "ledger", &workdir_b, Some("platform"))
            .expect("ledger sharing the platform agent must be allowed");

        let repos = list_repos_in_config(&config_path).expect("list");
        assert_eq!(repos.len(), 2);
        assert!(repos.iter().all(|r| r.recipient() == "platform"));
    }

    #[test]
    fn add_repo_allows_distinct_agents() {
        // Two repos with different agents add cleanly.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir_a = dir.path().join("a");
        let workdir_b = dir.path().join("b");
        std::fs::create_dir_all(&workdir_a).expect("create a");
        std::fs::create_dir_all(&workdir_b).expect("create b");

        add_repo_to_config(&config_path, "alpha", &workdir_a, Some("agent-a")).expect("a");
        add_repo_to_config(&config_path, "beta", &workdir_b, Some("agent-b")).expect("b");
        let repos = list_repos_in_config(&config_path).expect("list");
        assert_eq!(repos.len(), 2);
    }

    #[test]
    fn watch_repo_config_recipient_prefers_agent_over_name() {
        let repo_with_agent = WatchRepoConfig {
            name: "ledger".to_string(),
            workdir: "/tmp".to_string(),
            agent: Some("platform".to_string()),
            broadcast_tags: Vec::new(),
            extra: toml::Table::new(),
        };
        assert_eq!(repo_with_agent.recipient(), "platform");

        let repo_without_agent = WatchRepoConfig {
            name: "legion".to_string(),
            workdir: "/tmp".to_string(),
            agent: None,
            broadcast_tags: Vec::new(),
            extra: toml::Table::new(),
        };
        assert_eq!(repo_without_agent.recipient(), "legion");
    }

    #[test]
    fn wake_addresses_is_recipient_plus_broadcast_tags() {
        let repo = WatchRepoConfig {
            name: "ledger".to_string(),
            workdir: "/tmp".to_string(),
            agent: Some("platform".to_string()),
            broadcast_tags: vec!["eng-team".to_string(), "backend".to_string()],
            extra: toml::Table::new(),
        };
        assert_eq!(
            repo.wake_addresses(),
            vec!["platform", "eng-team", "backend"],
            "wake_addresses must be recipient() followed by every broadcast tag"
        );

        let untagged = WatchRepoConfig {
            name: "legion".to_string(),
            workdir: "/tmp".to_string(),
            agent: None,
            broadcast_tags: Vec::new(),
            extra: toml::Table::new(),
        };
        assert_eq!(
            untagged.wake_addresses(),
            vec!["legion"],
            "no agent + no tags must collapse to just the repo name"
        );
    }

    #[test]
    fn watch_repo_config_recipient_treats_empty_agent_as_absent() {
        // Hand-edited watch.toml may set agent = "" or all-whitespace.
        // recipient() must not return empty -- that would route no signals.
        let empty = WatchRepoConfig {
            name: "fallback".to_string(),
            workdir: "/tmp".to_string(),
            agent: Some("".to_string()),
            broadcast_tags: Vec::new(),
            extra: toml::Table::new(),
        };
        assert_eq!(empty.recipient(), "fallback");

        let whitespace = WatchRepoConfig {
            name: "fallback".to_string(),
            workdir: "/tmp".to_string(),
            agent: Some("   ".to_string()),
            broadcast_tags: Vec::new(),
            extra: toml::Table::new(),
        };
        assert_eq!(whitespace.recipient(), "fallback");
    }

    #[test]
    fn add_repo_normalizes_empty_agent_to_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir = dir.path().to_path_buf();

        add_repo_to_config(&config_path, "legion", &workdir, Some("")).expect("add");
        let repos = list_repos_in_config(&config_path).expect("list");
        assert_eq!(repos.len(), 1);
        assert!(
            repos[0].agent.is_none(),
            "empty --agent should be normalized to None, not stored as Some(\"\")"
        );
    }

    #[test]
    fn add_repo_preserves_unknown_fields_on_existing_entries() {
        // Regression guard: add/remove must NOT strip fields the struct does not
        // model explicitly (github, worksource, review, etc). A silent round-trip
        // that drops these breaks `legion pr create` for every repo.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");

        let seed = r#"
[[repos]]
name = "legion"
workdir = "/tmp"
github = "runlegion/legion"
worksource = "github"

[[repos]]
name = "rafters"
workdir = "/tmp"
github = "rafters-studio/rafters"
"#;
        std::fs::write(&config_path, seed).expect("seed config");

        // Add a new repo -- triggers full read-modify-write.
        let new_workdir = dir.path().join("newrepo");
        std::fs::create_dir_all(&new_workdir).expect("mkdir");
        add_repo_to_config(&config_path, "newrepo", &new_workdir, None).expect("add");

        // Both original repos must still carry their github field.
        let contents = std::fs::read_to_string(&config_path).expect("reread");
        assert!(
            contents.contains(r#"github = "runlegion/legion""#),
            "legion's github field was stripped: {contents}"
        );
        assert!(
            contents.contains(r#"github = "rafters-studio/rafters""#),
            "rafters's github field was stripped: {contents}"
        );
        assert!(
            contents.contains(r#"worksource = "github""#),
            "legion's worksource field was stripped: {contents}"
        );
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

    // -- Retired watch.toml [cluster] section (#611) ---------------------------

    #[test]
    fn load_config_rejects_cluster_section_loudly() {
        // The old watch::ClusterConfig was parsed-but-never-consumed: an
        // operator who configured [cluster] in watch.toml got validated-
        // then-ignored settings. load_config must now refuse the section
        // with an error naming cluster.toml as the real home.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let wd = dir.path().display().to_string();
        std::fs::write(
            &config_path,
            format!(
                "[cluster]\nport = 31337\nsecret = \"abc\"\n\n\
                 [[repos]]\nname = \"legion\"\nworkdir = '{wd}'\n"
            ),
        )
        .expect("write");

        let err = load_config(&config_path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cluster.toml"),
            "rejection must point at cluster.toml as the real home: {msg}"
        );
        assert!(
            msg.contains("[cluster]"),
            "rejection must name the offending section: {msg}"
        );
    }

    #[test]
    fn load_config_without_cluster_section_is_unaffected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let wd = dir.path().display().to_string();
        std::fs::write(
            &config_path,
            format!("[[repos]]\nname = \"legion\"\nworkdir = '{wd}'\n"),
        )
        .expect("write");

        let config = load_config(&config_path).expect("config without [cluster] must load");
        assert!(config.cluster.is_none());
    }

    #[test]
    fn add_repo_preserves_cluster_section_for_the_loud_rejection() {
        // CRUD must not silently strip an operator's [cluster] section --
        // the round-trip keeps it in place so load_config can keep
        // rejecting it loudly until the operator moves it to cluster.toml.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        std::fs::write(
            &config_path,
            "[cluster]\nport = 31337\n\n[[repos]]\nname = \"legion\"\nworkdir = \"/tmp\"\n",
        )
        .expect("seed");

        let workdir = dir.path().join("newrepo");
        std::fs::create_dir_all(&workdir).expect("mkdir");
        add_repo_to_config(&config_path, "newrepo", &workdir, None).expect("add");

        let contents = std::fs::read_to_string(&config_path).expect("reread");
        assert!(
            contents.contains("[cluster]"),
            "[cluster] section must survive the CRUD round-trip: {contents}"
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

    // -- SpawnMode (#485) ---------------------------------------------------

    #[test]
    fn spawn_mode_parse_print() {
        assert_eq!(SpawnMode::parse("print"), SpawnMode::Print);
        assert_eq!(SpawnMode::parse("PRINT"), SpawnMode::Print);
        assert_eq!(SpawnMode::parse("  print  "), SpawnMode::Print);
    }

    #[test]
    fn spawn_mode_parse_pty() {
        assert_eq!(SpawnMode::parse("pty"), SpawnMode::Pty);
        assert_eq!(SpawnMode::parse("PTY"), SpawnMode::Pty);
    }

    #[test]
    fn spawn_mode_parse_unknown_falls_back_to_pty() {
        // Default flipped to Pty in #494 (post-2026-06-15 billing
        // shift). Empty, whitespace, and unrecognized strings now
        // engage the PTY path; operators who want the legacy
        // print path set WATCH_SPAWN_MODE=print explicitly.
        assert_eq!(SpawnMode::parse(""), SpawnMode::Pty);
        assert_eq!(SpawnMode::parse("   "), SpawnMode::Pty);
        assert_eq!(SpawnMode::parse("nope"), SpawnMode::Pty);
    }

    // The stub-verifying `spawn_agent_pty_returns_not_implemented`
    // test from #485 is intentionally removed in #489 -- the Pty
    // branch now spawns a PTY-backed REPL instead of returning
    // NotImplemented. Behavior is exercised by the integration tests
    // in pty::tests; a full end-to-end spawn here would require a
    // real `claude` binary on PATH, which CI does not have.

    #[test]
    fn spawn_mode_as_str_matches_env_values() {
        assert_eq!(SpawnMode::Print.as_str(), "print");
        assert_eq!(SpawnMode::Pty.as_str(), "pty");
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

    // -- Broadcast tags + Recipient enum (#585) ----------------------------------

    #[test]
    fn recipient_parse_all_variants_map_to_broadcast_all() {
        // Case-insensitive reserved words "all" and "everyone" must parse as
        // Broadcast(All) whether or not they carry a leading @.
        assert_eq!(
            Recipient::parse("all"),
            Recipient::Broadcast(BroadcastTarget::All)
        );
        assert_eq!(
            Recipient::parse("ALL"),
            Recipient::Broadcast(BroadcastTarget::All)
        );
        assert_eq!(
            Recipient::parse("@all"),
            Recipient::Broadcast(BroadcastTarget::All)
        );
        assert_eq!(
            Recipient::parse("everyone"),
            Recipient::Broadcast(BroadcastTarget::All)
        );
        assert_eq!(
            Recipient::parse("Everyone"),
            Recipient::Broadcast(BroadcastTarget::All)
        );
        assert_eq!(
            Recipient::parse("@everyone"),
            Recipient::Broadcast(BroadcastTarget::All)
        );
    }

    #[test]
    fn recipient_parse_named_becomes_agent() {
        // Anything that is not a reserved broadcast maps to Agent, with the
        // leading @ stripped.
        assert_eq!(
            Recipient::parse("platform"),
            Recipient::Agent("platform".to_string())
        );
        assert_eq!(
            Recipient::parse("@platform"),
            Recipient::Agent("platform".to_string())
        );
        assert_eq!(
            Recipient::parse("legion"),
            Recipient::Agent("legion".to_string())
        );
    }

    #[test]
    fn at_everyone_wakes_repo_same_as_at_all() {
        // @everyone must behave identically to @all: both repos should see the
        // broadcast, each exactly once. This is the regression guard for the
        // new reserved word.
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("kelex", "@everyone RFC:help -- discover proposal", "team")
            .expect("insert @everyone broadcast");

        let legion_signals =
            find_pending_signals(&db, "legion", &["legion".to_string()], None).expect("legion");
        let rafters_signals =
            find_pending_signals(&db, "rafters", &["rafters".to_string()], None).expect("rafters");

        assert_eq!(
            legion_signals.len(),
            1,
            "@everyone must wake legion just like @all"
        );
        assert_eq!(
            rafters_signals.len(),
            1,
            "@everyone must wake rafters just like @all"
        );

        // Mark handled for legion; rafters must still see it.
        for (id, _, _) in &legion_signals {
            db.mark_signal_handled_for_repo(id, "legion")
                .expect("mark legion handled");
        }

        let rafters_after = find_pending_signals(&db, "rafters", &["rafters".to_string()], None)
            .expect("rafters after legion handled");
        assert_eq!(
            rafters_after.len(),
            1,
            "@everyone dedup must be per-repo: rafters must still see the signal"
        );
    }

    #[test]
    fn broadcast_tag_signal_wakes_only_tagged_repos() {
        // A signal addressed @eng-team should wake repos whose broadcast_tags
        // includes "eng-team", but NOT repos that do not carry that tag.
        let (db, _index, _dir) = test_storage();

        // tagged: subscribes to "eng-team"
        let tagged_names: Vec<String> = vec!["tagged".to_string(), "eng-team".to_string()];
        // untagged: no broadcast_tags
        let untagged_names: Vec<String> = vec!["untagged".to_string()];

        db.insert_reflection("other", "@eng-team question:help -- need input", "team")
            .expect("insert tag signal");

        let tagged_sigs = find_pending_signals(&db, "tagged", &tagged_names, None).expect("tagged");
        let untagged_sigs =
            find_pending_signals(&db, "untagged", &untagged_names, None).expect("untagged");

        assert_eq!(
            tagged_sigs.len(),
            1,
            "repo with matching broadcast_tag must see the @eng-team signal"
        );
        assert!(
            untagged_sigs.is_empty(),
            "repo without the tag must NOT see the @eng-team signal"
        );
    }

    #[test]
    fn directed_signal_wakes_only_target_repo() {
        // A @<name> signal wakes only the repo whose recipient() equals name,
        // not repos that happen to share a tag.
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("other", "@direct question:help -- for you only", "team")
            .expect("insert directed signal");

        let direct_sigs =
            find_pending_signals(&db, "direct", &["direct".to_string()], None).expect("direct");
        let bystander_sigs =
            find_pending_signals(&db, "bystander", &["bystander".to_string()], None)
                .expect("bystander");

        assert_eq!(direct_sigs.len(), 1, "target repo must see directed signal");
        assert!(
            bystander_sigs.is_empty(),
            "unaddressed repo must not see directed signal"
        );
    }

    #[test]
    fn broadcast_tags_deserialize_from_toml() {
        // Backward-compat: a config without broadcast_tags parses cleanly;
        // a config with broadcast_tags round-trips.
        let without = r#"
[[repos]]
name = "legion"
workdir = "/tmp"
"#;
        let cfg: WatchConfig = toml::from_str(without).expect("parse without tags");
        assert!(
            cfg.repos[0].broadcast_tags.is_empty(),
            "missing broadcast_tags field must default to empty vec"
        );

        let with_tags = r#"
[[repos]]
name = "legion"
workdir = "/tmp"
broadcast_tags = ["eng-team", "backend"]
"#;
        let cfg2: WatchConfig = toml::from_str(with_tags).expect("parse with tags");
        assert_eq!(
            cfg2.repos[0].broadcast_tags,
            vec!["eng-team", "backend"],
            "broadcast_tags must round-trip through TOML"
        );
    }

    #[test]
    fn load_config_accepts_shared_agent_across_repos() {
        // A shared `agent` across repos is intentional (#595): one persona
        // maintains several repos (e.g. platform owns ledger). load_config must
        // accept it -- the persona wake-lease dedups a directed signal to one
        // wake, so there is no collision to refuse.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let wd = dir.path().display().to_string();
        std::fs::write(
            &config_path,
            format!(
                "[[repos]]\nname = \"platform\"\nworkdir = '{wd}'\nagent = \"platform\"\n\n\
                 [[repos]]\nname = \"ledger\"\nworkdir = '{wd}'\nagent = \"platform\"\n\n\
                 [[repos]]\nname = \"astro-data\"\nworkdir = '{wd}'\nagent = \"platform\"\n"
            ),
        )
        .expect("write");

        let cfg = load_config(&config_path).expect("shared-agent config must load cleanly");
        assert_eq!(cfg.repos.len(), 3);
        // All three resolve to the same recipient -- that is the point.
        assert!(cfg.repos.iter().all(|r| r.recipient() == "platform"));
    }

    #[test]
    fn load_config_accepts_valid_config_with_unique_recipients_and_tags() {
        // A valid config with unique recipients and non-colliding tags must load cleanly.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let wd = dir.path().display().to_string();
        std::fs::write(
            &config_path,
            format!(
                "[[repos]]\nname = \"alpha\"\nworkdir = '{wd}'\nagent = \"agent-a\"\n\
                 broadcast_tags = [\"eng-team\"]\n\n\
                 [[repos]]\nname = \"beta\"\nworkdir = '{wd}'\nagent = \"agent-b\"\n\
                 broadcast_tags = [\"eng-team\", \"backend\"]\n"
            ),
        )
        .expect("write");

        let cfg = load_config(&config_path).expect("valid config must load cleanly");
        assert_eq!(cfg.repos.len(), 2);
    }
}
