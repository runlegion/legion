//! watch.toml configuration: the repo table, validation, and the CRUD
//! (add/remove/list/rename) used by the `legion watch` subcommands.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{LegionError, Result};

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

#[cfg(test)]
mod tests {
    use super::*;

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
