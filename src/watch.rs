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
}

/// Multi-node cluster configuration for LAN broadcast sync.
/// When present, enables smugglr-core UDP broadcast discovery.
/// When absent, legion runs in single-node mode (no sync).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClusterConfig {
    /// UDP port for broadcast (default: 31337)
    #[serde(default = "default_cluster_port")]
    pub port: u16,

    /// Broadcast interval in seconds (default: 30)
    #[serde(default = "default_cluster_interval")]
    pub interval_secs: u64,

    /// Instance identity (defaults to hostname at runtime)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,

    /// 256-bit pre-shared key, hex-encoded (64 hex chars).
    /// When set, all broadcast traffic is encrypted with XChaCha20-Poly1305.
    /// Validated at parse time: must be exactly 64 valid hex characters.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_secret"
    )]
    pub secret: Option<String>,
}

fn default_cluster_port() -> u16 {
    31337
}

fn default_cluster_interval() -> u64 {
    30
}

/// Validate the secret field at deserialization time.
/// Rejects invalid hex or wrong length with a clear error message.
fn deserialize_secret<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let opt: Option<String> = Option::deserialize(deserializer)?;
    if let Some(ref s) = opt {
        if s.len() != 64 {
            return Err(serde::de::Error::custom(format!(
                "cluster.secret must be exactly 64 hex characters (256-bit key), got {} characters",
                s.len()
            )));
        }
        if !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(serde::de::Error::custom(
                "cluster.secret contains invalid hex characters (must be 0-9, a-f, A-F)",
            ));
        }
    }
    Ok(opt)
}

impl ClusterConfig {
    /// Decode the hex-encoded secret into a 256-bit (32-byte) encryption key.
    /// Returns None if secret is absent. Invalid secrets are rejected at parse
    /// time by the serde deserializer, so configs loaded from TOML are guaranteed
    /// to have valid secrets. Returns None defensively if called with invalid data
    /// (e.g., from direct struct construction in tests).
    #[allow(dead_code)] // Used when broadcast sync is wired up
    pub fn encryption_key(&self) -> Option<[u8; 32]> {
        let secret = self.secret.as_ref()?;
        let bytes = hex_decode(secret)?;
        if bytes.len() != 32 {
            return None;
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        Some(key)
    }

    /// Convert to smugglr-core's BroadcastConfig for use with the broadcast system.
    #[allow(dead_code)] // Used when broadcast sync is wired up
    pub fn to_broadcast_config(&self) -> smugglr_core::broadcast::BroadcastConfig {
        smugglr_core::broadcast::BroadcastConfig {
            port: self.port,
            interval_secs: self.interval_secs,
            instance_id: self.instance_id.clone(),
            secret: self.secret.clone(),
        }
    }
}

/// Decode hex string to bytes. Returns None on invalid hex.
#[allow(dead_code)] // Used by ClusterConfig::encryption_key
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
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

    /// Whether this node serves the web dashboard.
    /// Only one node per network should have this set to true.
    #[serde(default)]
    pub serve: bool,

    /// Multi-node cluster configuration. When present, enables LAN broadcast
    /// discovery via smugglr-core. When absent, runs in single-node mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster: Option<ClusterConfig>,

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

fn default_session_lock_ttl_secs() -> u64 {
    3600
}

fn default_persona_lease_ttl_secs() -> u64 {
    600
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

    // Recipient (signal-routing identity) collision check (#459). Two repos
    // sharing one recipient creates silent multi-wake on directed signals --
    // an @platform signal fires both /Volumes/.../platform and any other
    // repo with agent=platform. The receiving agents have to flag mis-route
    // every time, burning wake budget. Refuse the add up front; operator
    // either picks a different agent or removes the conflicting entry.
    //
    // `recipient()` returns `agent` if set, else `name`. The proposed new
    // recipient is `normalized_agent.as_deref().unwrap_or(name)`. Comparing
    // both via recipient() handles all four cases:
    //   1. new agent='X', existing name='X' (no agent override) -> conflict
    //   2. new agent='X', existing agent='X' -> conflict
    //   3. new name='X' (no agent), existing agent='X' -> conflict
    //   4. new name='X' (no agent), existing name='X' -> earlier name-conflict path
    let proposed_recipient = normalized_agent.as_deref().unwrap_or(name);
    if let Some(existing) = config
        .repos
        .iter()
        .find(|r| r.recipient() == proposed_recipient)
    {
        return Err(LegionError::WatchConfig(format!(
            "recipient '{}' already in use by repo '{}' at {}. \
             Each watched repo needs a unique signal-routing identity \
             (its `agent` field, or its name when `agent` is unset). \
             Pick a different agent for this repo or remove the existing entry.",
            proposed_recipient, existing.name, existing.workdir
        )));
    }

    config.repos.push(WatchRepoConfig {
        name: name.to_string(),
        workdir: canonical_str,
        agent: normalized_agent,
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
/// lock to be treated as abandoned; the next spawn overwrites it. No explicit
/// release happens on clean exit -- the dead-PID branch of the check handles
/// that on the next poll. An explicit release hook is out of scope for this
/// issue (tracked separately).
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

    /// Returns the holding PID when a prior spawn's lockfile is still considered
    /// live (fresh mtime AND a running PID). Returns `None` on any parse error,
    /// missing file, or staleness -- callers can safely proceed to spawn in all
    /// `None` cases. The PID is surfaced so skip-log messages can identify the
    /// session holding the gate.
    pub fn active_pid(&self, repo: &str) -> Option<u32> {
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
    }

    /// Write or overwrite the lockfile for `repo` with `pid`. Creates the
    /// parent directory if needed. Caller is responsible for having checked
    /// `active_pid` first if they want to respect an existing lock.
    pub fn record_spawn(&self, repo: &str, pid: u32) -> Result<()> {
        std::fs::create_dir_all(&self.lock_dir)?;
        std::fs::write(self.lock_path(repo), pid.to_string())?;
        Ok(())
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
    recipient: &str,
    since: Option<&str>,
) -> Result<Vec<(String, String, String)>> {
    let reflections = db.get_unhandled_signals_for_repo(repo_name, recipient, since)?;

    let mut signals: Vec<(String, String, String)> = Vec::new();
    for r in reflections {
        if signal::is_signal(&r.text) {
            signals.push((r.id, r.text, r.repo));
        }
    }

    Ok(signals)
}

// -- Agent Spawning ----------------------------------------------------------

/// Verbs whose presence in a directed signal triggers a watch wake (#404).
///
/// `--verb` was designed to express intent; this set is the subsection of
/// intents that warrant spawning an asleep recipient. Other verbs
/// (`announce`, `ack`, `info`, `answer`, bare `review`) deliver to live
/// sessions via the channel push but do not page an asleep agent.
///
/// Posts (text without a leading `@recipient`) never wake -- posts are
/// broadcasts; the `legion signal --to <agent> --verb <wake-worthy>`
/// primitive is the agent equivalent of a tweet at someone.
pub const WAKE_WORTHY_VERBS: &[&str] = &["question", "request", "help", "blocker"];

/// Whether a signal text triggers a wake under the verb-driven gate.
///
/// Returns `false` for posts, malformed signals, and signals whose verb is
/// outside [`WAKE_WORTHY_VERBS`]. The decision is verb-only -- status is
/// decoration that downstream tools may use, but the wake gate ignores it.
pub fn is_wake_worthy(text: &str) -> bool {
    match signal::parse_signal(text) {
        Some(sig) => WAKE_WORTHY_VERBS.contains(&sig.verb.as_str()),
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

/// Tracks spawned child processes for active agent counting and persona
/// wake lease cleanup. Each tracked entry carries the (persona, signal_id)
/// pairs it acquired leases for so that `reap_finished` can release them
/// when the child exits.
pub struct TrackedChild {
    pub repo: String,
    pub child: Child,
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
        child: Child,
        held_leases: Vec<(String, String)>,
        host: String,
        session_id: String,
        spawn_at: String,
        signal_ids: Vec<String>,
    ) {
        self.children.push(TrackedChild {
            repo,
            child,
            held_leases,
            host,
            session_id,
            spawn_at,
            signal_ids,
        });
    }

    /// Reap finished child processes, removing them from tracking and
    /// releasing their held persona wake leases. Uses host-scoped release so
    /// a late-loser whose lease was overwritten by sync conflict resolution
    /// cannot accidentally drop the peer winner's row. Release errors are
    /// logged but not propagated -- a missed release ages out via TTL.
    pub fn reap_finished(&mut self, db: Option<&Database>) {
        self.children
            .retain_mut(|tracked| match tracked.child.try_wait() {
                Ok(Some(status)) => {
                    eprintln!(
                        "[legion watch] agent for {} exited ({})",
                        tracked.repo,
                        if status.success() { "ok" } else { "error" }
                    );
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
                        let exit_status = if status.success() { "ok" } else { "error" };
                        let outcome = if !status.success() {
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
                    }
                    false // remove from list
                }
                Ok(None) => true, // still running
                Err(e) => {
                    eprintln!(
                        "[legion watch] error checking agent for {}: {}",
                        tracked.repo, e
                    );
                    true // keep tracking -- process may still be running
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

pub fn poll_cycle(
    db: &Database,
    config: &WatchConfig,
    cooldown: &mut CooldownTracker,
    tracker: &mut AgentTracker,
    session_locks: Option<&SessionLockTracker>,
    lease_gate: Option<&PersonaLeaseGate<'_>>,
    since: Option<&str>,
) -> Result<u32> {
    let mut spawned: u32 = 0;

    // Check for auto-unblock opportunities from recent signals
    for repo in &config.repos {
        match find_pending_signals(db, &repo.name, repo.recipient(), since) {
            Ok(signals) => {
                check_auto_unblock(db, &signals);
            }
            Err(e) => eprintln!(
                "[legion watch] auto-unblock signal lookup failed for {}: {}",
                repo.name, e
            ),
        }
    }

    for repo in &config.repos {
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

        let recipient = repo.recipient();
        let signals = find_pending_signals(db, &repo.name, recipient, since)?;
        if signals.is_empty() {
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

        match spawn_agent(&repo.workdir, &prompt) {
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
                let track_host = lease_gate.map(|g| g.host.to_string()).unwrap_or_default();
                let session_id = uuid::Uuid::now_v7().to_string();
                let spawn_at = chrono::Utc::now().to_rfc3339();
                let signal_ids: Vec<String> = signals.iter().map(|(id, _, _)| id.clone()).collect();
                tracker.track(
                    repo.name.clone(),
                    child,
                    held_leases,
                    track_host,
                    session_id,
                    spawn_at,
                    signal_ids,
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
    let session_locks = SessionLockTracker::new(data_dir, config.session_lock_ttl_secs);
    let mut sampler = HealthSampler::new(config.health_window_size);

    let poll_interval = Duration::from_secs(config.poll_interval_secs);
    let health_interval = Duration::from_secs(config.health_poll_secs);
    let retention_cutoff = chrono::Duration::days(config.retention_days as i64);
    let host = resolve_host_id();
    let lease_ttl = Duration::from_secs(config.persona_lease_ttl_secs);
    let lease_gate = PersonaLeaseGate {
        db: &db,
        host: &host,
        ttl: lease_ttl,
    };
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

    // Check cluster sync config and spawn sync actor if enabled
    let _sync_handle = match crate::cluster::ClusterConfig::load(data_dir) {
        Ok(cc) if cc.enabled && cc.secret.is_some() => {
            eprintln!(
                "[legion watch] cluster sync enabled on port {} (instance: {})",
                cc.port,
                cc.resolve_instance_id()
            );
            match crate::sync_actor::SyncHandle::spawn(data_dir, cc) {
                Ok(handle) => Some(handle),
                Err(e) => {
                    eprintln!("[legion watch] failed to start sync actor: {}", e);
                    None
                }
            }
        }
        _ => None,
    };

    loop {
        // Health sample on its own interval
        if health_timer.elapsed() >= health_interval {
            sampler.sample();
            tracker.reap_finished(Some(&db));
            if let Err(e) = db.heartbeat_persona_leases(&host, lease_ttl) {
                eprintln!("[legion watch] lease heartbeat error: {}", e);
            }

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
                match poll_cycle(
                    &db,
                    &config,
                    &mut cooldown,
                    &mut tracker,
                    Some(&session_locks),
                    Some(&lease_gate),
                    Some(&lookback),
                ) {
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

        let signals = find_pending_signals(&db, "legion", "legion", None).expect("find signals");
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
        let shingle = find_pending_signals(&db, "shingle", "shingle", None).expect("shingle");
        let huttspawn =
            find_pending_signals(&db, "huttspawn", "huttspawn", None).expect("huttspawn");
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
        let legion = find_pending_signals(&db, "legion", "legion", None).expect("legion");
        assert!(legion.is_empty(), "sender should not see own signal");

        // unrelated repo should NOT see it
        let kelex = find_pending_signals(&db, "kelex", "kelex", None).expect("kelex");
        assert!(kelex.is_empty(), "unmentioned repo should not see signal");
    }

    #[test]
    fn find_pending_signals_excludes_self_signals() {
        let (db, _index, _dir) = test_storage();

        // Signal from legion to legion should not be returned
        db.insert_reflection("legion", "@legion review:approved", "team")
            .expect("insert self-signal");

        let signals = find_pending_signals(&db, "legion", "legion", None).expect("find signals");
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

        let signals = find_pending_signals(&db, "ledger", "platform", None).expect("find signals");
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

        let signals = find_pending_signals(&db, "ledger", "platform", None).expect("find signals");
        assert_eq!(signals.len(), 1, "external signal to agent should be found");
    }

    #[test]
    fn mark_handled_prevents_re_detection() {
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("kelex", "@legion review:approved", "team")
            .expect("insert signal");

        let signals = find_pending_signals(&db, "legion", "legion", None).expect("first poll");
        assert_eq!(signals.len(), 1);

        // Mark as handled for legion
        let (id, _, _) = &signals[0];
        db.mark_signal_handled_for_repo(id, "legion")
            .expect("mark handled");

        // Should not appear again for legion
        let signals = find_pending_signals(&db, "legion", "legion", None).expect("second poll");
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
        // wake-worthy verb directly: `--verb request`, `--verb help`,
        // `--verb question`, `--verb blocker`.
        let signals = vec![
            (
                "id-rev-req".to_string(),
                "@platform review:request {doc: rfc.md} -- review please".to_string(),
                "smugglr".to_string(),
            ),
            (
                "id-help-verb".to_string(),
                "@platform help -- need a hand on the rfc".to_string(),
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
            reply_section.contains("id-help-verb"),
            "verb=help must require reply"
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
        for verb in ["announce", "ack", "info", "answer", "review"] {
            let text = format!("@kessel {verb} -- fyi");
            assert!(
                !is_wake_worthy(&text),
                "verb `{verb}` must not be wake-worthy: {text}"
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
        let spawned =
            poll_cycle(&db, &config, &mut cooldown, &mut tracker, None, None, None).expect("poll");
        assert_eq!(spawned, 0, "cooling repo should be skipped");
    }

    #[test]
    fn broadcast_signals_visible_to_all_repos() {
        let (db, _index, _dir) = test_storage();

        // Post an @all signal from kelex
        db.insert_reflection("kelex", "@all RFC:help -- discover proposal", "team")
            .expect("insert broadcast");

        // Both legion and rafters should see it
        let legion_signals = find_pending_signals(&db, "legion", "legion", None).expect("legion");
        let rafters_signals =
            find_pending_signals(&db, "rafters", "rafters", None).expect("rafters");
        assert_eq!(legion_signals.len(), 1);
        assert_eq!(rafters_signals.len(), 1);

        // Mark handled for legion using per-repo tracking
        for (id, _, _) in &legion_signals {
            db.mark_signal_handled_for_repo(id, "legion")
                .expect("mark handled for legion");
        }

        // legion should NOT see it anymore
        let legion_after =
            find_pending_signals(&db, "legion", "legion", None).expect("legion after");
        assert!(
            legion_after.is_empty(),
            "legion should not see broadcast after handling"
        );

        // rafters should STILL see the broadcast
        let rafters_after =
            find_pending_signals(&db, "rafters", "rafters", None).expect("rafters after");
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
        let rafters_final =
            find_pending_signals(&db, "rafters", "rafters", None).expect("rafters final");
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
    fn add_repo_errors_on_agent_collision_with_existing_agent() {
        // #459: two repos with the same `agent` value create silent
        // multi-wake on directed signals. The add path refuses up front.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir_a = dir.path().join("a");
        let workdir_b = dir.path().join("b");
        std::fs::create_dir_all(&workdir_a).expect("create a");
        std::fs::create_dir_all(&workdir_b).expect("create b");

        add_repo_to_config(&config_path, "alpha", &workdir_a, Some("platform"))
            .expect("add a with agent");
        let err =
            add_repo_to_config(&config_path, "beta", &workdir_b, Some("platform")).unwrap_err();
        assert!(
            err.to_string()
                .contains("recipient 'platform' already in use"),
            "expected recipient-conflict error, got: {err}"
        );
    }

    #[test]
    fn add_repo_errors_when_new_agent_collides_with_existing_name() {
        // Case 1 from the recipient-conflict matrix: new agent='X' collides
        // with existing name='X' (which has no agent override). Existing
        // repo's recipient() returns its name, so the conflict is real.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir_a = dir.path().join("a");
        let workdir_b = dir.path().join("b");
        std::fs::create_dir_all(&workdir_a).expect("create a");
        std::fs::create_dir_all(&workdir_b).expect("create b");

        add_repo_to_config(&config_path, "platform", &workdir_a, None).expect("add platform");
        let err =
            add_repo_to_config(&config_path, "ledger", &workdir_b, Some("platform")).unwrap_err();
        assert!(
            err.to_string()
                .contains("recipient 'platform' already in use"),
            "expected recipient-conflict error, got: {err}"
        );
    }

    #[test]
    fn add_repo_errors_when_new_name_collides_with_existing_agent_override() {
        // Case 3: new name='X' (no agent) collides with existing agent='X'.
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("watch.toml");
        let workdir_a = dir.path().join("a");
        let workdir_b = dir.path().join("b");
        std::fs::create_dir_all(&workdir_a).expect("create a");
        std::fs::create_dir_all(&workdir_b).expect("create b");

        add_repo_to_config(&config_path, "ledger", &workdir_a, Some("platform"))
            .expect("add ledger with platform agent");
        let err = add_repo_to_config(&config_path, "platform", &workdir_b, None).unwrap_err();
        assert!(
            err.to_string()
                .contains("recipient 'platform' already in use"),
            "expected recipient-conflict error, got: {err}"
        );
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
            extra: toml::Table::new(),
        };
        assert_eq!(repo_with_agent.recipient(), "platform");

        let repo_without_agent = WatchRepoConfig {
            name: "legion".to_string(),
            workdir: "/tmp".to_string(),
            agent: None,
            extra: toml::Table::new(),
        };
        assert_eq!(repo_without_agent.recipient(), "legion");
    }

    #[test]
    fn watch_repo_config_recipient_treats_empty_agent_as_absent() {
        // Hand-edited watch.toml may set agent = "" or all-whitespace.
        // recipient() must not return empty -- that would route no signals.
        let empty = WatchRepoConfig {
            name: "fallback".to_string(),
            workdir: "/tmp".to_string(),
            agent: Some("".to_string()),
            extra: toml::Table::new(),
        };
        assert_eq!(empty.recipient(), "fallback");

        let whitespace = WatchRepoConfig {
            name: "fallback".to_string(),
            workdir: "/tmp".to_string(),
            agent: Some("   ".to_string()),
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

    #[test]
    fn cluster_config_parses_from_toml() {
        let toml_str = r#"
poll_interval_secs = 30

[cluster]
port = 31337
interval_secs = 15
instance_id = "macbook-pro"
secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

[[repos]]
name = "legion"
workdir = "/tmp"
"#;
        let config: WatchConfig = toml::from_str(toml_str).expect("parse with cluster");
        let cluster = config.cluster.expect("cluster should be present");
        assert_eq!(cluster.port, 31337);
        assert_eq!(cluster.interval_secs, 15);
        assert_eq!(cluster.instance_id.as_deref(), Some("macbook-pro"));
        assert!(cluster.secret.is_some());
    }

    #[test]
    fn cluster_config_absent_means_single_node() {
        let toml_str = r#"
[[repos]]
name = "legion"
workdir = "/tmp"
"#;
        let config: WatchConfig = toml::from_str(toml_str).expect("parse without cluster");
        assert!(
            config.cluster.is_none(),
            "missing [cluster] = single-node mode"
        );
    }

    #[test]
    fn cluster_encryption_key_decodes_hex() {
        let cluster = ClusterConfig {
            port: 31337,
            interval_secs: 30,
            instance_id: None,
            secret: Some(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            ),
        };
        let key = cluster.encryption_key().expect("valid hex secret");
        assert_eq!(key.len(), 32);
        assert_eq!(key[0], 0x01);
        assert_eq!(key[1], 0x23);
        assert_eq!(key[31], 0xef);
    }

    #[test]
    fn cluster_encryption_key_none_when_no_secret() {
        let cluster = ClusterConfig {
            port: 31337,
            interval_secs: 30,
            instance_id: None,
            secret: None,
        };
        assert!(cluster.encryption_key().is_none(), "no secret = no key");
    }

    #[test]
    fn cluster_encryption_key_none_for_invalid_hex() {
        let cluster = ClusterConfig {
            port: 31337,
            interval_secs: 30,
            instance_id: None,
            secret: Some("not-valid-hex".to_string()),
        };
        assert!(cluster.encryption_key().is_none(), "invalid hex = no key");
    }

    #[test]
    fn cluster_encryption_key_none_for_wrong_length() {
        let cluster = ClusterConfig {
            port: 31337,
            interval_secs: 30,
            instance_id: None,
            secret: Some("0123456789abcdef".to_string()), // only 16 chars = 8 bytes
        };
        assert!(cluster.encryption_key().is_none(), "wrong length = no key");
    }

    #[test]
    fn cluster_secret_validation_rejects_invalid_hex() {
        // 64 characters but 'g' is not valid hex
        let toml_str = r#"
[cluster]
secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg"
"#;
        let err = toml::from_str::<WatchConfig>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("invalid hex"),
            "error should mention invalid hex: {}",
            msg
        );
    }

    #[test]
    fn cluster_secret_validation_rejects_wrong_length() {
        let toml_str = r#"
[cluster]
secret = "0123456789abcdef"
"#;
        let err = toml::from_str::<WatchConfig>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("64 hex characters"),
            "error should mention required length: {}",
            msg
        );
    }

    #[test]
    fn cluster_secret_validation_accepts_valid_hex() {
        let toml_str = r#"
[cluster]
secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
"#;
        let config: WatchConfig = toml::from_str(toml_str).expect("valid secret should parse");
        assert!(config.cluster.unwrap().secret.is_some());
    }

    #[test]
    fn cluster_secret_validation_accepts_uppercase_hex() {
        let toml_str = r#"
[cluster]
secret = "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF"
"#;
        let config: WatchConfig = toml::from_str(toml_str).expect("uppercase hex should parse");
        let key = config
            .cluster
            .unwrap()
            .encryption_key()
            .expect("uppercase should decode");
        assert_eq!(key[15], 0xEF);
    }

    #[test]
    fn cluster_config_uses_serde_defaults() {
        let toml_str = r#"
[cluster]
"#;
        let config: WatchConfig = toml::from_str(toml_str).expect("parse with defaults");
        let cluster = config.cluster.expect("cluster should be present");
        assert_eq!(cluster.port, 31337, "default port");
        assert_eq!(cluster.interval_secs, 30, "default interval");
        assert!(cluster.instance_id.is_none());
        assert!(cluster.secret.is_none());
    }
}
