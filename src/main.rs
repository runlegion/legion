mod board;
mod card_parse;
mod channel;
mod cluster;
mod daemon;
mod db;
mod embed;
mod error;
mod health;
mod init;
#[allow(dead_code)] // Items used by tests + pending surface/status/serve migration
mod kanban;
mod mcp;
mod mesh;
mod pr_view;
mod recall;
mod reflect;
mod scip;
mod search;
mod serve;
mod signal;
mod stats;
mod status;
mod statusline;
mod surface;
mod sym;
mod sync;
mod sync_actor;
mod task;
#[cfg(test)]
mod testutil;
mod usage;
mod watch;
mod worksource;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Parser, Subcommand};
use directories::ProjectDirs;

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Print an informational message to stderr, only when --verbose is set.
macro_rules! info {
    ($($arg:tt)*) => {
        if VERBOSE.load(Ordering::Relaxed) {
            eprintln!($($arg)*);
        }
    };
}

#[derive(Parser)]
#[command(
    name = "legion",
    about = "Agent specialization through deliberate practice",
    version
)]
struct Cli {
    /// Show informational messages on stderr (quiet by default)
    #[arg(long, short, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

/// Controls near-duplicate detection behavior on `legion reflect`.
#[derive(clap::ValueEnum, Clone, Debug, Default)]
enum DedupeMode {
    /// Warn on stderr when a near-duplicate is found, but still store the reflection.
    #[default]
    Warn,
    /// Refuse to store the reflection and exit non-zero when a near-duplicate is found.
    Strict,
    /// Skip the near-duplicate check entirely.
    Off,
}

#[derive(Subcommand)]
enum Commands {
    /// Store a reflection from a completed session
    Reflect {
        /// Repository name(s), comma-separated (e.g., "myrepo" or "frontend,backend")
        #[arg(long, value_delimiter = ',', required = true)]
        repo: Vec<String>,

        /// Reflection text (mutually exclusive with --transcript)
        #[arg(long, conflicts_with = "transcript")]
        text: Option<String>,

        /// Path to session transcript JSONL file
        #[arg(long, conflicts_with = "text")]
        transcript: Option<PathBuf>,

        /// Domain tag for classification (e.g., "auth", "schema", "ui")
        #[arg(long)]
        domain: Option<String>,

        /// Shortcut for --domain identity. Stores as an identity reflection,
        /// which the SessionStart hook injects on every boot.
        #[arg(long, conflicts_with = "domain")]
        whoami: bool,

        /// Comma-separated tags (e.g., "debugging,performance,review")
        #[arg(long)]
        tags: Option<String>,

        /// Link to a parent reflection ID to form a learning chain
        #[arg(long)]
        follows: Option<String>,

        /// Skip near-duplicate detection regardless of --dedupe-mode
        #[arg(long)]
        force: bool,

        /// Near-duplicate detection mode: warn (default), strict (block), or off (skip)
        #[arg(long, value_enum, default_value_t = DedupeMode::Warn)]
        dedupe_mode: DedupeMode,
    },

    /// Permanently delete a reflection by id.
    ///
    /// Destructive. No soft-delete, no undo. Removes the row from both
    /// the SQLite reflections table and the tantivy search index. Used
    /// to retire stale workaround reflections, demonstrably-wrong
    /// reflections, or personal data that should not persist in the
    /// corpus.
    ///
    /// The optional `--repo` flag is a safety check: when provided,
    /// the delete is refused unless the reflection's actual repo
    /// matches. Prevents accidentally nuking the wrong reflection when
    /// working with a similarly-shaped id.
    Forget {
        /// Reflection id to delete.
        #[arg(long)]
        id: String,

        /// Optional safety check: refuse the delete unless the
        /// reflection's repo matches this value.
        #[arg(long)]
        repo: Option<String>,
    },

    /// Recall relevant reflections for the current context
    Recall {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Current task context to match against (ignored with --latest)
        #[arg(long, default_value = "")]
        context: String,

        /// Maximum number of reflections to return
        #[arg(long, default_value = "5")]
        limit: usize,

        /// Return most recent reflections instead of BM25 search
        #[arg(long)]
        latest: bool,

        /// Truncate each reflection text to this many characters. Used by
        /// hooks to keep injected context compact.
        #[arg(long)]
        preview: Option<usize>,

        /// Skip BM25; rank purely by cosine similarity (requires embed model)
        #[arg(long, conflicts_with = "latest")]
        cosine_only: bool,

        /// Filter out results with a final score below this threshold
        #[arg(long)]
        min_score: Option<f32>,

        /// Return latest reflections matching this domain (bypasses search)
        #[arg(long, conflicts_with_all = ["latest", "cosine_only", "context"])]
        domain: Option<String>,
    },

    /// Find reflections similar to a given reflection by cosine similarity
    Similar {
        /// Reflection ID to find neighbors for
        #[arg(long)]
        id: String,

        /// Maximum number of neighbors to return
        #[arg(long, default_value = "5")]
        limit: usize,

        /// Include reflections from all repos (default: same repo as the source)
        #[arg(long)]
        cross_repo: bool,

        /// Filter out results with a score below this threshold
        #[arg(long)]
        min_score: Option<f32>,

        /// Truncate reflection text to this many characters in output
        #[arg(long)]
        preview: Option<usize>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Search reflections across all repos for cross-agent consultation
    Consult {
        /// Context describing the problem to search for
        #[arg(long)]
        context: String,

        /// Maximum number of reflections to return
        #[arg(long, default_value = "3")]
        limit: usize,
    },

    /// Configure Claude Code hooks for legion
    Init {
        /// Skip confirmation prompts
        #[arg(long)]
        force: bool,
    },

    /// Post a message to the shared bullpen for other agents
    Post {
        /// Repository name(s), comma-separated (e.g., "myrepo" or "frontend,backend")
        #[arg(long, value_delimiter = ',', required = true)]
        repo: Vec<String>,

        /// Post text (mutually exclusive with --transcript)
        #[arg(long, conflicts_with = "transcript")]
        text: Option<String>,

        /// Path to session transcript JSONL file
        #[arg(long, conflicts_with = "text")]
        transcript: Option<PathBuf>,

        /// Domain tag for classification
        #[arg(long)]
        domain: Option<String>,

        /// Comma-separated tags
        #[arg(long)]
        tags: Option<String>,

        /// Link to a parent reflection ID to form a learning chain
        #[arg(long)]
        follows: Option<String>,
    },

    /// Mark a reflection as useful after recalling and applying it
    Boost {
        /// Reflection ID to boost
        #[arg(long)]
        id: String,
    },

    /// Trace a learning chain from a reflection
    Chain {
        /// Any reflection ID in the chain
        #[arg(long)]
        id: String,
        /// Emit full reflection text instead of the 80-char preview. Used by
        /// hooks that inject chain content into agent context, where the
        /// truncated visualization is insufficient.
        #[arg(long)]
        full: bool,
    },

    /// Send a structured signal to another agent
    Signal {
        /// Repository name (identifies the sender)
        #[arg(long)]
        repo: Vec<String>,

        /// Recipient agent name (or "all")
        #[arg(long)]
        to: String,

        /// Signal verb (e.g., review, request, announce, question, blocker)
        #[arg(long)]
        verb: String,

        /// Signal status (e.g., approved, blocked, ready)
        #[arg(long)]
        status: Option<String>,

        /// Free-text note
        #[arg(long)]
        note: Option<String>,

        /// Comma-separated key:value detail pairs (e.g., "surface:cap-output,chain:confirmed")
        #[arg(long)]
        details: Option<String>,

        /// Link to a parent reflection ID to thread signals
        #[arg(long)]
        follows: Option<String>,

        /// Domain tag for classification
        #[arg(long)]
        domain: Option<String>,

        /// Comma-separated tags
        #[arg(long)]
        tags: Option<String>,
    },

    /// Print a wake-prompt for pending request-shaped signals.
    ///
    /// Used by SessionStart hooks to surface directed questions/requests with
    /// strong "REQUIRES A REPLY" framing that survives the additionalContext
    /// system-reminder wrapper. Prints nothing (and exits 0) if there are no
    /// pending reply-required signals targeting this repo.
    PendingReplies {
        /// Repository name (the recipient)
        #[arg(long)]
        repo: String,
    },

    /// Read the bullpen or check for unread posts
    #[command(alias = "bp", alias = "board")]
    Bullpen {
        /// Repository name (identifies who is reading)
        #[arg(long, required_unless_present_any = ["archive", "archived"])]
        repo: Option<String>,

        /// Only show unread count instead of full bullpen
        #[arg(long)]
        count: bool,

        /// Show only signals (structured coordination messages)
        #[arg(long, conflicts_with = "musings")]
        signals: bool,

        /// Show only musings (natural language posts)
        #[arg(long, conflicts_with = "signals")]
        musings: bool,

        /// Archive posts that all readers have read
        #[arg(long, conflicts_with_all = ["count", "signals", "musings", "archived"])]
        archive: bool,

        /// Show archived posts instead of active ones
        #[arg(long, conflicts_with_all = ["count", "signals", "musings", "archive"])]
        archived: bool,
    },

    /// Surface cross-repo highlights for a session start
    Surface {
        /// Repository name
        #[arg(long)]
        repo: String,
    },

    /// Print identity reflections for a repo (alias for recall --domain identity)
    Whoami {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Maximum number of identity reflections to return
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },

    /// Symbol queries against stored SCIP indexes (#282).
    ///
    /// Answers def/refs/impl/hover questions by parsing the protobuf
    /// blobs written by `legion index` -- bytes, not full file loads.
    Sym {
        #[command(subcommand)]
        action: SymAction,
    },

    /// Build or refresh the SCIP code-intelligence index for a repo (#278).
    ///
    /// Resolves the repo path from `watch.toml`, detects every supported
    /// language present, runs each indexer (e.g. `scip-rust`) as a
    /// subprocess, and stores one row per (repo, lang). Idempotent on
    /// content hash.
    ///
    /// When `--file <path>` is given, only the single language for that
    /// file is re-indexed (#280). Repo is resolved by longest-prefix
    /// match against watch.toml workdirs when omitted.
    Index {
        /// Repo name (looked up in watch.toml). Optional when --file or
        /// --status is given.
        #[arg(required_unless_present_any = ["file", "status"])]
        repo: Option<String>,
        /// Re-index only the language for this single file (absolute or
        /// repo-relative path).
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// Print background index job status from the local store
        /// instead of running a fresh index (#284).
        #[arg(long)]
        status: bool,
        /// Emit JSON output (currently only meaningful with --status).
        #[arg(long)]
        json: bool,
    },

    /// Rebuild the search index from the database
    Reindex,

    /// Remove old tombstones (soft-deleted rows) from the database
    Cleanup {
        /// Retention period in days (default: 30)
        #[arg(long, default_value = "30")]
        retention_days: i64,
    },

    /// Rename a repo across all tables and watch config
    Rename {
        /// Current repo name
        #[arg(long)]
        from: String,
        /// New repo name
        #[arg(long)]
        to: String,
    },

    /// Compute embeddings for all reflections that are missing them
    Backfill,

    /// Show reflection statistics
    Stats {
        /// Repository name (omit for all repos)
        #[arg(long)]
        repo: Option<String>,
    },

    /// Start the web dashboard
    Serve {
        /// Port to listen on
        #[arg(long, default_value = "3131")]
        port: u16,
    },

    /// Check your work state, team needs, and recent changes
    Status {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Output as JSON with counts only (for hooks/scripts)
        #[arg(long)]
        json: bool,
    },

    /// Show what the team needs help with
    Needs {
        /// Repository name
        #[arg(long)]
        repo: String,
    },

    /// Announce completed work and notify blocked agents
    Done {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Description of what was completed
        #[arg(long)]
        text: String,

        /// Card ID to mark as complete (optional)
        #[arg(long)]
        id: Option<String>,
    },

    /// Get next work item from the scheduler
    Work {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Peek only (don't auto-accept the card)
        #[arg(long)]
        peek: bool,
    },

    /// Sync issues from work source into kanban board
    Sync {
        /// Repository name
        #[arg(long)]
        repo: String,
    },

    /// Multi-node cluster sync (LAN broadcast with encryption)
    Cluster {
        #[command(subcommand)]
        action: ClusterAction,
    },

    /// Manage the kanban board
    Kanban {
        #[command(subcommand)]
        action: KanbanAction,
    },

    /// Manage delegated tasks between agents (deprecated, use kanban)
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },

    /// Manage scheduled bullpen posts
    Schedule {
        #[command(subcommand)]
        action: ScheduleAction,
    },

    /// Manage issues via work source plugins
    Issue {
        #[command(subcommand)]
        action: IssueAction,
    },

    /// Post a comment on an issue or PR via work source plugins
    Comment {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Issue or PR number
        #[arg(long)]
        number: u64,

        /// Comment body
        #[arg(long)]
        body: String,
    },

    /// Manage pull requests via work source plugins
    Pr {
        #[command(subcommand)]
        action: PrAction,
    },

    /// View the audit log of work source actions
    Audit {
        /// Filter by agent name
        #[arg(long)]
        repo: Option<String>,

        /// Filter by action type. Known verbs:
        /// create-issue, close-issue, reopen-issue, edit-issue,
        /// create-pr, close-pr, review, merge, comment,
        /// delete-card, update-card.
        /// Filter is an exact string match; additional verbs
        /// introduced by future subcommands work automatically.
        #[arg(long)]
        action: Option<String>,

        /// Maximum entries to show
        #[arg(long, default_value = "20")]
        limit: usize,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Watch for signals and auto-wake sleeping agents (daemon mode when run without a subcommand)
    Watch {
        #[command(subcommand)]
        action: Option<WatchAction>,
    },

    /// MCP stdio server for Claude Code channel integration
    Mcp,

    /// Start the legion daemon (channel + watch)
    Daemon {
        /// HTTP port for the channel server
        #[arg(long, default_value = "3131")]
        port: u16,
    },

    /// Spawn the daemon in the background (idempotent, for plugin auto-start)
    DaemonSpawn {
        /// HTTP port for the channel server
        #[arg(long, default_value = "3131")]
        port: u16,
    },

    /// Record a quality gate result for a skill run
    QualityGate {
        #[command(subcommand)]
        action: QualityGateAction,
    },

    /// Show current system health and recent trend
    Health {
        /// Show history for the last N duration (e.g., "1h", "30m", "24h")
        #[arg(long)]
        history: Option<String>,

        /// Show health for all hosts (after smuggler replication)
        #[arg(long)]
        all_hosts: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Claude Code statusLine subcommand: ingest rate-limit + usage, render chip
    ///
    /// Reads the Claude Code statusLine JSON on stdin, persists a
    /// `rate_limit_samples` row (and a `usage_samples` row when the
    /// transcript is readable), then prints a single-line chip on stdout.
    /// Wire via `statusLine.command` in settings.json.
    Statusline {
        /// Print the parsed sample state as JSON instead of the chip
        #[arg(long)]
        json: bool,
    },

    /// Mesh-aware task placement -- rank hosts by statusline-sample headroom
    Mesh {
        #[command(subcommand)]
        action: MeshAction,
    },

    /// Show Claude Code session token usage and cost analysis
    Usage {
        /// Show only this specific session UUID
        #[arg(long)]
        session: Option<String>,

        /// Show all sessions on or after this date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,

        /// Show sessions from today only (default when no flags given)
        #[arg(long)]
        today: bool,

        /// One row per session, sorted by cost (default for --since)
        #[arg(long)]
        by_session: bool,

        /// Group results by repo
        #[arg(long, conflicts_with = "by_session")]
        by_repo: bool,

        /// Output as JSON instead of a human-readable table
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TaskAction {
    /// Create a new task for another agent
    Create {
        /// Sender repository name
        #[arg(long)]
        from: String,

        /// Target repository name
        #[arg(long)]
        to: String,

        /// Task description
        #[arg(long)]
        text: String,

        /// Additional context for the task
        #[arg(long)]
        context: Option<String>,

        /// Priority: low, med, high (default: med)
        #[arg(long, default_value = "med", value_parser = ["low", "med", "high"])]
        priority: String,
    },

    /// List tasks for a repo
    List {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Show outbound tasks (tasks created by this repo) instead of inbound
        #[arg(long)]
        from: bool,
    },

    /// Accept a pending task
    Accept {
        /// Task ID
        #[arg(long)]
        id: String,
    },

    /// Mark an accepted task as done
    Done {
        /// Task ID
        #[arg(long)]
        id: String,

        /// Completion note
        #[arg(long)]
        note: Option<String>,
    },

    /// Block an accepted task
    Block {
        /// Task ID
        #[arg(long)]
        id: String,

        /// Reason for blocking
        #[arg(long)]
        reason: Option<String>,
    },

    /// Unblock a blocked task (returns to accepted)
    Unblock {
        /// Task ID
        #[arg(long)]
        id: String,
    },
}

#[derive(Subcommand)]
enum SymAction {
    /// Print definition locations for the given symbol name.
    Def {
        name: String,
        /// Restrict to a single repo. Default scans every stored index.
        #[arg(long)]
        repo: Option<String>,
        /// Restrict to a single language.
        #[arg(long)]
        lang: Option<String>,
        /// Emit JSON instead of human-readable file:line lines.
        #[arg(long)]
        json: bool,
    },
    /// Print reference locations (call sites) for the given name.
    Refs {
        name: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print structs/types that implement the named trait or interface.
    Impl {
        #[arg(value_name = "TRAIT")]
        name: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print signature + docstring for the symbol.
    Hover {
        name: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ClusterAction {
    /// Initialize cluster sync with a shared encryption key
    Init {
        /// 256-bit hex-encoded key (64 chars). Generated if omitted.
        #[arg(long)]
        key: Option<String>,

        /// UDP port for broadcast (default: 31337)
        #[arg(long, default_value = "31337")]
        port: u16,
    },

    /// Show the current cluster key (for sharing with other nodes)
    Key,

    /// Enable cluster sync (start broadcasting)
    Enable,

    /// Disable cluster sync (stop broadcasting)
    Disable,

    /// Show cluster status: peers, sync state, last sync time
    Status,
}

#[derive(Subcommand)]
enum MeshAction {
    /// Print per-host ranked table with headroom, burn, and staleness
    Headroom {
        /// Emit raw JSON instead of the human-formatted table
        #[arg(long)]
        json: bool,
    },

    /// Print the best hostname to run a task on. Exit 1 if no host is fresh.
    Pick {
        /// Comma-separated hostnames to omit from the ranking
        #[arg(long)]
        exclude: Option<String>,

        /// Emit raw JSON instead of the plain hostname
        #[arg(long)]
        json: bool,

        /// Kanban card ID this pick is for (reserved for future
        /// task-specific weighting; today a plain tag in the JSON output).
        #[arg(long)]
        for_task: Option<String>,
    },
}

#[derive(Subcommand)]
enum KanbanAction {
    /// Create a new card on the kanban board
    Create {
        /// Who is creating the card
        #[arg(long)]
        from: String,

        /// Which agent this card is assigned to
        #[arg(long)]
        to: String,

        /// Card description
        #[arg(long)]
        text: String,

        /// Additional context
        #[arg(long)]
        context: Option<String>,

        /// Priority: low, med, high, critical (default: med)
        #[arg(long, default_value = "med", value_parser = ["low", "med", "high", "critical"])]
        priority: String,

        /// Comma-separated labels
        #[arg(long)]
        labels: Option<String>,

        /// Parent card ID (for delegation chains)
        #[arg(long)]
        parent: Option<String>,

        /// Link to external issue (e.g., GitHub issue URL)
        #[arg(long)]
        source_url: Option<String>,

        /// Source type (e.g., "github", "jira")
        #[arg(long)]
        source_type: Option<String>,
    },

    /// View a single card by ID
    View {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Output as a single JSON object instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Update mutable fields on an existing card
    Update {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Repository name (used as the audit agent)
        #[arg(long)]
        repo: String,

        /// New title text
        #[arg(long)]
        text: Option<String>,

        /// New body (markdown); re-parsed into problem/solution/acceptance sections
        #[arg(long)]
        body: Option<String>,

        /// New priority: low, med, high, critical
        #[arg(long, value_parser = ["low", "med", "high", "critical"])]
        priority: Option<String>,

        /// Replace labels with this comma-separated list
        #[arg(long, conflicts_with_all = ["add_labels", "remove_labels"])]
        labels: Option<String>,

        /// Append comma-separated labels (deduplicated against existing)
        #[arg(long, conflicts_with = "labels")]
        add_labels: Option<String>,

        /// Remove comma-separated labels
        #[arg(long, conflicts_with = "labels")]
        remove_labels: Option<String>,
    },

    /// List cards for a repo
    List {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Show outbound cards (created by this repo) instead of inbound
        #[arg(long)]
        from: bool,

        /// Emit JSONL (one summary object per line) instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Accept a pending card (move to in-progress)
    Accept {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// Block a card (technical blocker)
    Block {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Reason for blocking
        #[arg(long)]
        reason: Option<String>,
    },

    /// Unblock a blocked card (returns to in-progress)
    Unblock {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// Mark a card for review
    Review {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// Mark a card as needing human input
    NeedInput {
        /// Card ID
        #[arg(long)]
        id: String,

        /// What input is needed
        #[arg(long)]
        reason: Option<String>,
    },

    /// Resume a card from needs-input or in-review
    Resume {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// Cancel a card
    ///
    /// When the card has a linked external issue (`source_url`), the
    /// corresponding GitHub issue is automatically closed as
    /// "not-planned" via the work source plugin. Pass `--no-propagate`
    /// to transition only the local card state without touching
    /// GitHub.
    Cancel {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Optional cancel reason, posted as a comment on the GitHub
        /// issue before the close
        #[arg(long)]
        reason: Option<String>,

        /// Transition the card locally without closing the linked
        /// GitHub issue. Use this when the kanban state needs to
        /// diverge from the external issue state deliberately.
        #[arg(long)]
        no_propagate: bool,
    },

    /// Assign a backlog card to an agent
    Assign {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Target agent/repo
        #[arg(long)]
        to: String,
    },

    /// Reopen a done or cancelled card
    ///
    /// When the card has a linked external issue that was previously
    /// closed by a kanban transition, the corresponding GitHub issue
    /// is automatically reopened via the work source plugin. Pass
    /// `--no-propagate` to transition only the local card state.
    Reopen {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Optional reopen reason, posted as a comment on the GitHub
        /// issue after the reopen
        #[arg(long)]
        reason: Option<String>,

        /// Transition the card locally without reopening the linked
        /// GitHub issue
        #[arg(long)]
        no_propagate: bool,
    },

    /// Permanently delete a card from the kanban board.
    ///
    /// Unlike `cancel` (which transitions to a terminal `cancelled`
    /// state where the card still appears in `legion kanban list`),
    /// `delete` removes the row entirely. Used to hard-remove a card
    /// filed in error. Does NOT touch the linked GitHub issue; use
    /// `legion issue close` or `legion issue reopen` separately if the
    /// public state needs to change.
    Delete {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// Reconcile kanban done/cancelled cards with their linked GitHub
    /// issue state.
    ///
    /// Finds every card whose local state is `done` or `cancelled` but
    /// whose linked GitHub issue is still `OPEN`, and reports the
    /// mismatches. Used to backfill the stale-open drift that
    /// accumulated before the `cancel`/`reopen` auto-propagation
    /// shipped.
    ///
    /// Default mode is read-only: the command scans and reports
    /// without changing any GitHub state. Pass `--close-stale` to
    /// actually close each mismatched issue via the work source
    /// plugin with a canned reconciliation comment.
    Reconcile {
        /// Optional repo filter -- only reconcile cards owned by this
        /// agent. Default is all cards across all repos.
        #[arg(long)]
        repo: Option<String>,

        /// Actually close the stale GitHub issues. Without this flag,
        /// the command is read-only and safe to run repeatedly.
        #[arg(long)]
        close_stale: bool,
    },
}

#[derive(Subcommand)]
enum ScheduleAction {
    /// Create a new scheduled bullpen post
    Create {
        /// Human-readable name for the schedule
        #[arg(long)]
        name: String,

        /// Cron expression: "HH:MM" for daily or "*/Nm" for every N minutes
        #[arg(long)]
        cron: String,

        /// Text to post to the bullpen when the schedule fires
        #[arg(long)]
        command: String,

        /// Repository name for the post
        #[arg(long)]
        repo: String,

        /// Active window start time (HH:MM UTC). Only fires within the window. Requires --active-end.
        #[arg(long, requires = "active_end")]
        active_start: Option<String>,

        /// Active window end time (HH:MM UTC). Only fires within the window. Requires --active-start.
        #[arg(long, requires = "active_start")]
        active_end: Option<String>,
    },

    /// List all schedules
    List,

    /// Enable a schedule
    Enable {
        /// Schedule ID
        #[arg(long)]
        id: String,
    },

    /// Disable a schedule
    Disable {
        /// Schedule ID
        #[arg(long)]
        id: String,
    },

    /// Delete a schedule
    Delete {
        /// Schedule ID
        #[arg(long)]
        id: String,
    },

    /// Update a schedule's active window or cron expression
    Update {
        /// Schedule ID
        #[arg(long)]
        id: String,

        /// New cron expression
        #[arg(long)]
        cron: Option<String>,

        /// Active window start time (HH:MM UTC)
        #[arg(long)]
        active_start: Option<String>,

        /// Active window end time (HH:MM UTC)
        #[arg(long)]
        active_end: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum IssueAction {
    /// Create an issue via the configured work source
    Create {
        /// Repository name (used to resolve work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Issue title
        #[arg(long)]
        title: String,

        /// Issue body
        #[arg(long)]
        body: Option<String>,

        /// Comma-separated labels
        #[arg(long)]
        labels: Option<String>,

        /// Assignee login
        #[arg(long)]
        assignee: Option<String>,
    },
    /// View an issue (local card data + live GitHub state)
    View {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Issue number
        #[arg(long)]
        number: u64,
    },
    /// Close an issue via the configured work source
    ///
    /// Used to reconcile a shipped kanban card with its public GitHub state
    /// when the card was closed through a path other than `pr merge --task`
    /// (which already auto-closes the linked issue). The optional `--comment`
    /// is posted on the issue before it transitions to closed.
    Close {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Issue number
        #[arg(long)]
        number: u64,

        /// Optional closing comment posted before the close
        #[arg(long)]
        comment: Option<String>,
    },
    /// Reopen a previously closed issue via the configured work source
    ///
    /// Symmetrical with `close` for reverting a kanban transition that
    /// already propagated to GitHub.
    Reopen {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Issue number
        #[arg(long)]
        number: u64,

        /// Optional reopening comment posted after the reopen
        #[arg(long)]
        comment: Option<String>,
    },
    /// Edit the title and/or body of an existing issue
    ///
    /// At least one of `--title` or `--body` must be provided. Used for
    /// scope amendments and stale-content fixes after a sync, so agents
    /// do not have to drop scope addenda into comment threads where they
    /// are buried below fold on the public GitHub view.
    Edit {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Issue number
        #[arg(long)]
        number: u64,

        /// Replace the issue title
        #[arg(long)]
        title: Option<String>,

        /// Replace the issue body
        #[arg(long)]
        body: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum PrAction {
    /// Create a pull request via the configured work source
    Create {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR title
        #[arg(long)]
        title: String,

        /// PR body
        #[arg(long)]
        body: Option<String>,

        /// Base branch (default: repo default branch)
        #[arg(long)]
        base: Option<String>,

        /// Head branch (default: current branch)
        #[arg(long)]
        head: Option<String>,

        /// Create as draft
        #[arg(long)]
        draft: bool,

        /// Comma-separated labels
        #[arg(long)]
        labels: Option<String>,

        /// Reviewer login (e.g., the review agent's GitHub account)
        #[arg(long)]
        reviewer: Option<String>,

        /// Kanban card ID to link (stores PR URL on the card)
        #[arg(long)]
        task: Option<String>,

        /// Skip the legion-simplify quality gate check.
        /// FOR BOOTSTRAP ONLY -- use when the branch IS the simplify skill itself.
        /// An audit entry is written so this cannot be done silently.
        #[arg(long)]
        skip_gates: bool,
    },

    /// List open pull requests with review status
    List {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,
    },

    /// Show CI check status for a pull request.
    /// Exits non-zero if any check is not in a known passing or in-flight
    /// state (see `ExternalPRCheck::is_failing` -- fail-closed on unknown
    /// states so a future gh release with a new failure variant cannot
    /// silently render as green to the merge gate).
    Checks {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Emit raw JSON instead of human-formatted output
        #[arg(long)]
        json: bool,

        /// Additionally stream the raw CI log for every failing check.
        /// Each log is preceded by a `===== <name> (<job-id>) =====` header.
        /// Per-job log fetch failures print a marker and do not abort the
        /// run -- the exit code still reflects the overall check status.
        #[arg(long)]
        log_failed: bool,
    },

    /// Show a pull request's body and metadata.
    View {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Emit raw JSON instead of human-formatted output
        #[arg(long)]
        json: bool,
    },

    /// List comments on a pull request (top-level + inline review comments)
    /// in chronological order.
    Comments {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Emit raw JSON instead of human-formatted output
        #[arg(long)]
        json: bool,
    },

    /// List submitted reviews on a pull request with their bodies and
    /// inline comments.
    Reviews {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Emit raw JSON instead of human-formatted output
        #[arg(long)]
        json: bool,
    },

    /// Post a review on a pull request
    Review {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Approve the PR
        #[arg(long, group = "verdict")]
        approve: bool,

        /// Request changes on the PR
        #[arg(long, group = "verdict")]
        request_changes: bool,

        /// Review body text
        #[arg(long)]
        body: Option<String>,

        /// Path to JSON file with inline comments (GitHub API format)
        #[arg(long)]
        comments: Option<String>,
    },

    /// Merge an approved pull request
    Merge {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Merge strategy: squash (default), merge, rebase
        #[arg(long, default_value = "squash", value_parser = ["squash", "merge", "rebase"])]
        strategy: String,

        /// Keep the branch after merging (default: delete)
        #[arg(long)]
        keep_branch: bool,

        /// Kanban card ID to transition to done
        #[arg(long)]
        task: Option<String>,
    },

    /// Close a pull request without merging
    Close {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Comment to post before closing
        #[arg(long)]
        reason: Option<String>,

        /// Delete the remote branch after closing
        #[arg(long)]
        delete_branch: bool,
    },
}

#[derive(Subcommand, Debug)]
enum QualityGateAction {
    /// Record a quality gate result for the current HEAD commit.
    ///
    /// Reads git HEAD and branch automatically. The skill runner calls this
    /// after inspecting the diff. `legion pr create` checks the gate before
    /// calling the work source so the result cannot be faked via a file flag.
    Record {
        /// Skill name (e.g., "legion-simplify")
        #[arg(long)]
        skill: String,

        /// Gate result: "clean" or "issues"
        #[arg(long, value_parser = ["clean", "issues"])]
        result: String,

        /// Number of findings (default 0)
        #[arg(long, default_value = "0")]
        findings_count: u64,

        /// Raw JSON details from the skill (full findings array)
        #[arg(long)]
        details_json: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum WatchAction {
    /// Add a working directory to watch.toml (idempotent by canonicalized path)
    Add {
        /// Absolute or relative path to the working directory
        path: std::path::PathBuf,

        /// Display name for this repo (used for cooldown tracking).
        /// Defaults to the basename of the resolved path.
        #[arg(long)]
        name: Option<String>,

        /// Signal recipient name. Signals addressed to this value (e.g., @my-agent)
        /// will wake this repo. Defaults to --name when absent.
        #[arg(long)]
        agent: Option<String>,
    },

    /// Remove a repo entry from watch.toml by name
    Remove {
        /// Repo name to remove
        name: String,
    },

    /// List all repos currently in watch.toml
    List,

    /// Inspect or manage persona wake leases (cluster-wide wake coordination)
    Leases {
        #[command(subcommand)]
        action: LeaseAction,
    },
}

#[derive(Subcommand, Debug)]
enum LeaseAction {
    /// List currently held persona wake leases
    List {
        /// Show only leases for this persona (e.g. `legion`, `huttspawn`)
        #[arg(long)]
        persona: Option<String>,
    },

    /// Release a single lease by (persona, signal_id). Idempotent.
    Release {
        /// Persona whose lease should be released
        #[arg(long)]
        persona: String,

        /// Signal id whose lease should be released
        #[arg(long)]
        signal: String,
    },
}

/// Resolve the legion data directory.
///
/// Resolution order:
/// 1. `LEGION_DATA_DIR` env var (explicit override, used by tests)
/// 2. `ProjectDirs` platform-native default:
///    - macOS:   `~/Library/Application Support/legion/`
///    - Linux:   `~/.local/share/legion/`
///    - Windows: `%APPDATA%\legion\`
///
/// `CLAUDE_PLUGIN_DATA` is deliberately NOT consulted. A prior version of
/// this function preferred that env var, which caused a split-brain on
/// macOS: the Claude Code plugin context wrote to
/// `~/.claude/plugins/data/legion-legion/` while bare CLI invocations and
/// long-running `legion watch` processes wrote to the ProjectDirs path.
/// Two divergent databases, invisible to each other, silently broke the
/// agent learning loop (reflections in one DB were invisible to sessions
/// that opened the other). The single-path rule is load-bearing for
/// legion's memory integrity. Do not reintroduce a second default.
///
/// Result is cached via OnceLock after first successful resolution:
/// commands like `audit()` re-enter this dozens of times and the path
/// never changes inside a single process. Tests that manipulate
/// `LEGION_DATA_DIR` between calls should use a fresh subprocess.
///
/// On the first successful resolution, if the target has no `legion.db`
/// but the legacy plugin-data-dir location does, the database, Tantivy
/// index, and `watch.toml` are migrated across. One-way, best-effort,
/// leaves the legacy path in place as a safety net. This is the reverse
/// of the previous migration direction, which moved data INTO the plugin
/// data dir and caused the split-brain in the first place.
pub(crate) fn data_dir() -> error::Result<PathBuf> {
    use std::sync::OnceLock;
    static CACHED: OnceLock<PathBuf> = OnceLock::new();

    if let Some(cached) = CACHED.get() {
        return Ok(cached.clone());
    }

    // `via_override` tracks whether the caller explicitly set LEGION_DATA_DIR.
    // When true (tests, CI, or explicit user override), the plugin-data-dir
    // migration MUST be skipped -- otherwise a test tempdir target would
    // inherit content from the real user's plugin data dir on the host
    // machine and tests that expect a clean starting state would fail
    // unpredictably depending on whose box they ran on. The override is the
    // explicit "I know what I'm doing, stay out of the filesystem" signal.
    let (path, via_override) = if let Ok(dir) = std::env::var("LEGION_DATA_DIR") {
        (PathBuf::from(dir), true)
    } else {
        let p = ProjectDirs::from("", "", "legion")
            .ok_or(error::LegionError::NoHomeDir)?
            .data_dir()
            .to_path_buf();
        (p, false)
    };

    std::fs::create_dir_all(&path)?;

    if !via_override && !path.join("legion.db").exists() {
        migrate_from_plugin_data_dir(&path);
    }

    let _ = CACHED.set(path.clone());
    Ok(path)
}

/// Dispatch `legion sym <action>` against the local SCIP index store.
fn run_sym(action: SymAction) -> error::Result<()> {
    let base = data_dir()?;
    let database = db::Database::open(&base.join("legion.db"))?;

    let watch_path = base.join("watch.toml");
    let blobs_for =
        |repo: Option<&str>, lang: Option<&str>| -> error::Result<Vec<scip::ScipIndex>> {
            let mut out: Vec<scip::ScipIndex> = Vec::new();
            if let Some(r) = repo {
                if let Some(l) = lang {
                    if let Some(idx) = database.get_scip_index(r, l)? {
                        out.push(idx);
                    }
                } else {
                    out.extend(database.list_scip_indexes(r)?);
                }
            } else {
                let repos = watch::list_repos_in_config(&watch_path)?;
                for entry in repos {
                    let mut for_repo = database.list_scip_indexes(&entry.name)?;
                    if let Some(l) = lang {
                        for_repo.retain(|i| i.lang == l);
                    }
                    out.append(&mut for_repo);
                }
            }
            Ok(out)
        };

    let ensure_some =
        |blobs: &[scip::ScipIndex], repo: Option<&str>, lang: Option<&str>| -> error::Result<()> {
            if blobs.is_empty() {
                let r = repo.unwrap_or("(any)");
                let l = lang.unwrap_or("(any)");
                return Err(error::LegionError::Search(format!(
                    "no SCIP index found for {r}/{l}; run `legion index <repo>` first"
                )));
            }
            Ok(())
        };

    match action {
        SymAction::Def {
            name,
            repo,
            lang,
            json,
        } => {
            let blobs = blobs_for(repo.as_deref(), lang.as_deref())?;
            ensure_some(&blobs, repo.as_deref(), lang.as_deref())?;
            let mut hits: Vec<sym::SymbolLocation> = Vec::new();
            for idx in &blobs {
                hits.extend(sym::query_definitions(
                    &idx.blob, &name, &idx.repo, &idx.lang,
                )?);
            }
            print_locations(&hits, json);
        }
        SymAction::Refs {
            name,
            repo,
            lang,
            json,
        } => {
            let blobs = blobs_for(repo.as_deref(), lang.as_deref())?;
            ensure_some(&blobs, repo.as_deref(), lang.as_deref())?;
            let mut hits: Vec<sym::SymbolLocation> = Vec::new();
            for idx in &blobs {
                hits.extend(sym::query_references(
                    &idx.blob, &name, &idx.repo, &idx.lang,
                )?);
            }
            print_locations(&hits, json);
        }
        SymAction::Impl {
            name,
            repo,
            lang,
            json,
        } => {
            let blobs = blobs_for(repo.as_deref(), lang.as_deref())?;
            ensure_some(&blobs, repo.as_deref(), lang.as_deref())?;
            let mut hits: Vec<sym::SymbolLocation> = Vec::new();
            for idx in &blobs {
                hits.extend(sym::query_implementors(
                    &idx.blob, &name, &idx.repo, &idx.lang,
                )?);
            }
            if hits.is_empty() {
                eprintln!(
                    "[legion sym impl] no implementors found for '{name}' (note: only languages whose SCIP indexer emits is_implementation relationships will return results)"
                );
                return Ok(());
            }
            print_locations(&hits, json);
        }
        SymAction::Hover {
            name,
            repo,
            lang,
            json,
        } => {
            let blobs = blobs_for(repo.as_deref(), lang.as_deref())?;
            ensure_some(&blobs, repo.as_deref(), lang.as_deref())?;
            let mut hover: Option<sym::HoverInfo> = None;
            for idx in &blobs {
                if let Some(h) = sym::query_hover(&idx.blob, &name, &idx.repo, &idx.lang)? {
                    hover = Some(h);
                    break;
                }
            }
            match (hover, json) {
                (Some(h), true) => println!(
                    "{}",
                    // HoverInfo has only String/Option<String> fields, so
                    // serde_json::to_string cannot fail in practice; the
                    // fallback is defensive against future shape changes.
                    serde_json::to_string(&h).unwrap_or_else(|_| "{}".to_string())
                ),
                (Some(h), false) => {
                    println!("{}", h.symbol);
                    if let Some(sig) = h.signature.as_deref() {
                        println!("{sig}");
                    }
                    if let Some(doc) = h.docstring.as_deref() {
                        println!();
                        println!("{doc}");
                    }
                }
                (None, _) => {
                    eprintln!("[legion sym hover] no info found for '{name}'");
                }
            }
        }
    }
    Ok(())
}

/// Implements `legion index --status [--repo X] [--json]` (#284).
fn run_index_status(repo: Option<&str>, json: bool) -> error::Result<()> {
    let base = data_dir()?;
    let database = db::Database::open(&base.join("legion.db"))?;
    let jobs = database.list_index_jobs()?;
    let filtered: Vec<&scip::IndexJob> = match repo {
        Some(name) => jobs.iter().filter(|j| j.repo == name).collect(),
        None => jobs.iter().collect(),
    };

    if json {
        let body = serde_json::to_string(&filtered)
            .map_err(|e| error::LegionError::Search(format!("serialize index_jobs: {e}")))?;
        println!("{body}");
        return Ok(());
    }

    if filtered.is_empty() {
        if let Some(name) = repo {
            println!("no index job found for {name}");
        } else {
            println!("no index jobs recorded");
        }
        return Ok(());
    }

    for job in filtered {
        let langs = if job.languages.is_empty() {
            "(none)".to_string()
        } else {
            job.languages.join(",")
        };
        let eta = job
            .eta_secs
            .map(|s| format!("eta={s}s"))
            .unwrap_or_default();
        let err = job
            .error
            .as_deref()
            .map(|e| format!(" error={e}"))
            .unwrap_or_default();
        println!(
            "{}: {} {}% [{}] {}{}",
            job.repo,
            job.status.as_str(),
            job.progress_pct,
            langs,
            eta,
            err
        );
    }
    Ok(())
}

fn print_locations(hits: &[sym::SymbolLocation], json: bool) {
    if json {
        println!(
            "{}",
            // SymbolLocation has only string + integer fields, so this
            // serializer call is effectively infallible.
            serde_json::to_string(hits).unwrap_or_else(|_| "[]".to_string())
        );
        return;
    }
    for h in hits {
        println!("{}:{}:{} ({}/{})", h.file, h.line, h.column, h.repo, h.lang);
    }
}

/// Migrate legion state from the plugin data dir back to the ProjectDirs target.
///
/// Reverse of the previous migration direction. The prior version moved
/// ProjectDirs -> plugin data dir, which created the split-brain this
/// module now exists to clean up. This function copies the OTHER way,
/// recovering any user whose plugin data dir has their authoritative
/// legion state but whose ProjectDirs path was empty.
///
/// Resolves the plugin data dir from `CLAUDE_PLUGIN_DATA` if set (when
/// running under Claude Code), otherwise from the known fallback path
/// `$HOME/.claude/plugins/data/legion-legion/`. Delegates the copy to
/// [`migrate_between`], which is the testable inner form.
fn migrate_from_plugin_data_dir(target: &Path) {
    let source = if let Ok(dir) = std::env::var("CLAUDE_PLUGIN_DATA")
        && !dir.is_empty()
    {
        PathBuf::from(dir)
    } else if let Some(home) = dirs::home_dir() {
        home.join(".claude")
            .join("plugins")
            .join("data")
            .join("legion-legion")
    } else {
        return;
    };
    migrate_between(&source, target);
}

/// Copy legion state from `legacy` to `target`.
///
/// Runs only when `legacy/legion.db` exists and `target/legion.db` does not.
/// Best-effort: logs to stderr on each failure and continues. Moves
/// `legion.db` (after a WAL checkpoint on the source so the copy is
/// consistent), `watch.toml`, and the Tantivy `index/` directory tree. Does
/// NOT delete the source -- leaves it in place as a safety net until the
/// user cleans up manually.
///
/// The Tantivy index is copied BEFORE `legion.db` so a mid-migration failure
/// leaves the target without a DB, which causes the next run to retry the
/// whole migration from scratch rather than strand a populated DB with an
/// empty index.
fn migrate_between(legacy: &Path, target: &Path) {
    let legacy_db = legacy.join("legion.db");
    if !legacy_db.exists() {
        return;
    }
    if target.join("legion.db").exists() {
        return;
    }

    eprintln!(
        "[legion] first-run migration: copying state from {} to {}",
        legacy.display(),
        target.display()
    );

    // 1. Tantivy index first: if this fails, the target still has no
    //    legion.db so the next run retries the full migration.
    let legacy_index = legacy.join("index");
    let target_index = target.join("index");
    if legacy_index.is_dir()
        && !target_index.exists()
        && let Err(e) = copy_dir_recursive(&legacy_index, &target_index)
    {
        eprintln!(
            "[legion] migration: failed to copy index tree: {e} -- aborting before DB copy so the next run retries"
        );
        return;
    }

    // 2. WAL checkpoint on the source so the DB copy is internally
    //    consistent. Opening the legacy DB briefly and issuing
    //    PRAGMA wal_checkpoint(TRUNCATE) flushes any pending writes into the
    //    main file and empties the WAL. If the checkpoint fails (e.g. locked
    //    by a running watch daemon), we log and proceed with the copy anyway;
    //    the target may need a manual recovery but this is no worse than the
    //    previous behavior.
    if let Err(e) = checkpoint_sqlite(&legacy_db) {
        eprintln!("[legion] migration: WAL checkpoint failed ({e}); copy may be inconsistent");
    }

    // 3. Copy the main DB last. After this point the target is "owned" and
    //    the migration guard (`target/legion.db.exists()`) will prevent re-runs.
    if let Err(e) = std::fs::copy(&legacy_db, target.join("legion.db")) {
        eprintln!("[legion] migration: failed to copy legion.db: {e}");
        return;
    }

    // 4. watch.toml: optional, best-effort.
    let legacy_watch = legacy.join("watch.toml");
    if legacy_watch.exists()
        && let Err(e) = std::fs::copy(&legacy_watch, target.join("watch.toml"))
    {
        eprintln!("[legion] migration: failed to copy watch.toml: {e}");
    }

    eprintln!(
        "[legion] migration complete. Legacy path left in place: {}",
        legacy.display()
    );
    eprintln!(
        "[legion] delete manually once verified: rm -rf {}",
        legacy.display()
    );
}

/// Open the given SQLite file briefly and issue a TRUNCATE checkpoint.
///
/// Used by [`migrate_between`] to make sure the WAL is drained into the main
/// file before the copy. Returns the rusqlite error on failure; caller decides
/// whether to proceed with the copy anyway.
fn checkpoint_sqlite(path: &Path) -> std::result::Result<(), rusqlite::Error> {
    let conn = rusqlite::Connection::open(path)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

/// Recursively copy a directory tree. Used for the Tantivy index migration.
/// Follows symlinks (the legacy index is expected to be a normal directory
/// tree with no symlinks); if that ever changes, switch to `symlink_metadata`
/// and preserve the link type.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

/// Best-effort audit log entry. Failures warn to stderr, never abort.
fn audit(input: &db::AuditInput<'_>) {
    if let Ok(base) = data_dir()
        && let Ok(database) = db::Database::open(&base.join("legion.db"))
        && let Err(e) = database.insert_audit_entry(input)
    {
        eprintln!("[legion] warning: audit log failed: {}", e);
    }
}

/// Outcome of a kanban-to-worksource propagation attempt.
///
/// Returned by `propagate_card_close_to_worksource` and
/// `propagate_card_reopen_to_worksource` so the caller can make the
/// success/failure state visible on stdout in addition to the stderr
/// breadcrumbs emitted by the helpers. Scripts piping legion output can
/// detect `Failed` and surface partial-success errors; humans reading the
/// terminal see either the silent success or the explicit warning line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PropagateOutcome {
    /// Plugin call succeeded and the GitHub issue state changed.
    Propagated,
    /// Card had no linked external issue (pure local card), or its
    /// `to_repo` has no work source configured. Nothing to propagate.
    /// Safe to no-op without caller intervention.
    Skipped,
    /// Plugin call was attempted and failed, OR a card-level precondition
    /// was not met (e.g. source_url present but no extractable issue
    /// number, or source_type missing). The local card transition has
    /// already completed; the caller should surface a visible warning so
    /// scripted consumers can detect partial success.
    Failed,
}

/// Close the GitHub issue linked to a kanban card via the work source
/// plugin, and write an audit entry describing the propagation. Called by
/// `legion kanban cancel` to keep the public GitHub state consistent with
/// the local kanban state.
///
/// NOT called by `legion done --id`, which has an equivalent inline
/// propagation at its own call site in `Commands::Done` (search for the
/// `// Close the linked external issue if present` block). The two paths
/// are functionally equivalent today; folding `Commands::Done` through
/// this helper is tracked as post-#192 cleanup.
///
/// Returns a `PropagateOutcome` so the caller can emit a visible warning
/// to stdout on failure. Stderr breadcrumbs explain the failure; stdout
/// visibility is what lets scripted pipelines detect partial success.
///
/// A card with no `source_url` is a pure local card and is skipped
/// silently. A card whose `to_repo` has no work source configured in
/// `watch.toml` is also skipped, with a warning -- the agent can still
/// transition the card locally, they just have to reconcile GitHub state
/// manually. All other failure modes log to stderr and do not abort the
/// calling handler, because the card transition has already happened and
/// returning an error here would leave the agent with an inconsistent
/// understanding of local state.
///
/// `comment` is the closing comment posted on the GitHub issue before the
/// state transition. When `None`, the plugin closes the issue without a
/// comment.
fn propagate_card_close_to_worksource(
    database: &db::Database,
    card_id: &str,
    comment: Option<&str>,
) -> PropagateOutcome {
    let card = match database.get_card_by_id(card_id) {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("[legion] propagate: card {card_id} not found; skipping");
            return PropagateOutcome::Failed;
        }
        Err(e) => {
            eprintln!("[legion] propagate: lookup of card {card_id} failed: {e}; skipping");
            return PropagateOutcome::Failed;
        }
    };

    let Some(ref source_url) = card.source_url else {
        // Pure local card with no linked issue -- nothing to propagate.
        return PropagateOutcome::Skipped;
    };

    let Some(number) = worksource::extract_issue_number(source_url) else {
        eprintln!(
            "[legion] propagate: card {card_id} has source_url {source_url} but no extractable issue number; skipping"
        );
        return PropagateOutcome::Failed;
    };

    let Some(source) = card.source_type.as_deref() else {
        eprintln!(
            "[legion] propagate: card {card_id} has source_url {source_url} but no source_type; skipping"
        );
        return PropagateOutcome::Failed;
    };

    let Some((_, source_repo, _)) = worksource::resolve_config(&card.to_repo) else {
        eprintln!(
            "[legion] propagate: to_repo '{}' for card {card_id} has no work source configured in watch.toml; skipping GitHub close",
            card.to_repo
        );
        // Missing watch.toml entry is treated as Skipped (not Failed)
        // because it is a configuration-level "not applicable here"
        // rather than an attempt-and-fail. Scripted consumers that
        // care about this case should rely on the stderr warning.
        return PropagateOutcome::Skipped;
    };

    match worksource::close_issue(source, &source_repo, number, comment) {
        Ok(()) => {
            eprintln!("[legion] closed {source} issue #{number}");
            let details = serde_json::json!({
                "card_id": card_id,
                "propagation": "kanban-transition",
                "comment": comment,
            });
            let details_str = details.to_string();
            audit(&db::AuditInput {
                agent: &card.to_repo,
                action: "close-issue",
                target_type: "issue",
                target_ref: &number.to_string(),
                task_id: Some(card_id),
                source_type: source,
                details: Some(&details_str),
                outcome: "success",
            });
            PropagateOutcome::Propagated
        }
        Err(e) => {
            eprintln!("[legion] propagate: failed to close {source} issue #{number}: {e}");
            PropagateOutcome::Failed
        }
    }
}

/// Reopen the GitHub issue linked to a kanban card via the work source
/// plugin. Symmetrical with `propagate_card_close_to_worksource`. Called by
/// `legion kanban reopen` so a card being moved back to in-progress reopens
/// its public GitHub issue.
fn propagate_card_reopen_to_worksource(
    database: &db::Database,
    card_id: &str,
    comment: Option<&str>,
) -> PropagateOutcome {
    let card = match database.get_card_by_id(card_id) {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("[legion] propagate: card {card_id} not found; skipping");
            return PropagateOutcome::Failed;
        }
        Err(e) => {
            eprintln!("[legion] propagate: lookup of card {card_id} failed: {e}; skipping");
            return PropagateOutcome::Failed;
        }
    };

    let Some(ref source_url) = card.source_url else {
        return PropagateOutcome::Skipped;
    };
    let Some(number) = worksource::extract_issue_number(source_url) else {
        eprintln!(
            "[legion] propagate: card {card_id} has source_url {source_url} but no extractable issue number; skipping"
        );
        return PropagateOutcome::Failed;
    };
    let Some(source) = card.source_type.as_deref() else {
        eprintln!(
            "[legion] propagate: card {card_id} has source_url {source_url} but no source_type; skipping"
        );
        return PropagateOutcome::Failed;
    };
    let Some((_, source_repo, _)) = worksource::resolve_config(&card.to_repo) else {
        eprintln!(
            "[legion] propagate: to_repo '{}' for card {card_id} has no work source configured in watch.toml; skipping GitHub reopen",
            card.to_repo
        );
        return PropagateOutcome::Skipped;
    };

    match worksource::reopen_issue(source, &source_repo, number, comment) {
        Ok(()) => {
            eprintln!("[legion] reopened {source} issue #{number}");
            let details = serde_json::json!({
                "card_id": card_id,
                "propagation": "kanban-transition",
                "comment": comment,
            });
            let details_str = details.to_string();
            audit(&db::AuditInput {
                agent: &card.to_repo,
                action: "reopen-issue",
                target_type: "issue",
                target_ref: &number.to_string(),
                task_id: Some(card_id),
                source_type: source,
                details: Some(&details_str),
                outcome: "success",
            });
            PropagateOutcome::Propagated
        }
        Err(e) => {
            eprintln!("[legion] propagate: failed to reopen {source} issue #{number}: {e}");
            PropagateOutcome::Failed
        }
    }
}

/// Run a compound command (text or transcript) across multiple repos with metadata.
///
/// Prints each stored ID to stdout (one per repo) so callers and scripts
/// can capture them. Returns an error if any repo fails.
#[allow(clippy::too_many_arguments)]
fn run_compound_command_with_meta(
    db: &db::Database,
    index: &search::SearchIndex,
    repos: &[String],
    text: &Option<String>,
    transcript: &Option<PathBuf>,
    meta: &db::ReflectionMeta,
    from_text: fn(
        &db::Database,
        &search::SearchIndex,
        &str,
        &str,
        &db::ReflectionMeta,
    ) -> error::Result<String>,
    from_transcript: fn(
        &db::Database,
        &search::SearchIndex,
        &str,
        &std::path::Path,
        &db::ReflectionMeta,
    ) -> error::Result<String>,
    label: &str,
) -> error::Result<()> {
    if text.is_none() && transcript.is_none() {
        return Err(error::LegionError::NoReflectionInput);
    }

    let mut had_error = false;
    for r in repos {
        let result = match (text, transcript) {
            (Some(t), None) => from_text(db, index, r, t, meta),
            (None, Some(path)) => from_transcript(db, index, r, path, meta),
            (Some(_), Some(_)) => return Err(error::LegionError::NoReflectionInput),
            (None, None) => unreachable!("guarded by early return above"),
        };
        match result {
            Ok(id) => {
                info!("[legion] {label} for {r} ({id})");
                println!("{id}");
            }
            Err(e) => {
                eprintln!("[legion] error {label} for {r}: {e}");
                had_error = true;
            }
        }
    }
    if had_error {
        return Err(error::LegionError::ReflectPartialFailure);
    }
    Ok(())
}

/// Try to load the embedding model. Returns None if not available.
///
/// Logs a warning to stderr on failure so degraded hybrid search is visible.
fn try_load_embed_model() -> Option<embed::EmbedModel> {
    match embed::EmbedModel::load() {
        Ok(model) => Some(model),
        Err(e) => {
            eprintln!("[legion] embedding model unavailable, falling back to BM25: {e}");
            None
        }
    }
}

/// Compute and store embeddings for all reflections that are missing them.
fn backfill_embeddings(db: &db::Database, model: &embed::EmbedModel) -> error::Result<usize> {
    let missing = db.get_ids_without_embeddings()?;
    let mut count: usize = 0;

    for (id, text) in &missing {
        match model.encode_one(text) {
            Ok(embedding) => {
                let bytes = embed::embedding_to_bytes(&embedding);
                if db.store_embedding(id, &bytes)? {
                    count += 1;
                }
            }
            Err(e) => {
                eprintln!("[legion] warning: failed to embed {}: {}", id, e);
            }
        }
    }

    Ok(count)
}

/// Check new reflection text for near-duplicates in the same repo.
///
/// Computes the embedding of `text`, then compares it against the 100 most
/// recent reflections with embeddings for `repo`. When cosine similarity
/// exceeds 0.95, warns to stderr. In `Strict` mode the function returns an
/// error so the caller aborts before storing. In `Warn` mode it returns Ok
/// and the reflection is stored anyway.
fn run_dedupe_check(
    db: &db::Database,
    model: &embed::EmbedModel,
    repo: &str,
    text: &str,
    mode: &DedupeMode,
) -> error::Result<()> {
    const DEDUPE_THRESHOLD: f32 = 0.95;
    const DEDUPE_LOOKBACK: usize = 100;

    let new_emb = match model.encode_one(text) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[legion] warning: dedupe check skipped (embed failed): {e}");
            return Ok(());
        }
    };

    let recent = db.get_recent_reflections_with_embeddings(repo, DEDUPE_LOOKBACK)?;

    for (id, blob, existing_text, created_at) in &recent {
        let existing_emb = embed::embedding_from_bytes(blob);
        let sim = embed::cosine_similarity(&new_emb, &existing_emb);

        if sim >= DEDUPE_THRESHOLD {
            let preview: String = existing_text.chars().take(80).collect();
            let date = db::format_date(created_at);
            eprintln!(
                "[legion] warning: this reflection is {sim:.2} cosine to reflection {id} \
                 (created {date}): \"{preview}\". \
                 stored anyway (set --dedupe-mode strict to refuse). \
                 Consider `legion reflect --follows {id}` to extend."
            );

            if matches!(mode, DedupeMode::Strict) {
                return Err(error::LegionError::Embedding(format!(
                    "near-duplicate detected (cosine {sim:.2} >= {DEDUPE_THRESHOLD}) \
                     with reflection {id} -- use --force to store anyway"
                )));
            }

            // In Warn mode we warn once for the closest match and stop checking.
            break;
        }
    }

    Ok(())
}

/// Raise the soft file-descriptor limit to the hard limit.
///
/// macOS ships a low soft limit (often 2560) which Tantivy can exhaust
/// when opening index segments. The hard limit is much higher (or unlimited).
/// This is a no-op on failure -- the worst case is the original limit.
fn raise_fd_limit() {
    match rlimit::increase_nofile_limit(u64::MAX) {
        Ok(_) => {}
        Err(e) => eprintln!("[legion] warning: could not raise fd limit: {e}"),
    }
}

/// Mesh stale-threshold. Statusline writes on every assistant turn, so
/// minutes of silence usually means "no session live on that host". Let
/// operators override via env -- useful when running a flaky network
/// where 5min is too tight, or when diagnosing a specific host.
fn resolve_stale_cutoff() -> std::time::Duration {
    let secs = std::env::var("LEGION_MESH_STALE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(mesh::DEFAULT_STALE_SECS);
    std::time::Duration::from_secs(secs)
}

fn round_f64(v: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    (v * factor).round() / factor
}

fn fmt_pct(v: Option<f64>) -> String {
    match v {
        Some(p) => format!("{:.0}", p),
        None => "-".into(),
    }
}

fn fmt_i64(v: Option<i64>) -> String {
    match v {
        Some(n) if n >= 1000 => format!("{:.0}K", n as f64 / 1000.0),
        Some(n) => n.to_string(),
        None => "-".into(),
    }
}

fn format_age(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

fn main() -> error::Result<()> {
    // Windows default stack (1MB) is too small for clap + Tantivy init.
    // Spawn with 8MB stack to match macOS/Linux defaults.
    const STACK_SIZE: usize = 8 * 1024 * 1024;
    let builder = std::thread::Builder::new().stack_size(STACK_SIZE);
    let handler = builder.spawn(run).map_err(error::LegionError::Io)?;
    handler.join().unwrap_or_else(|_| {
        Err(error::LegionError::Io(std::io::Error::other(
            "thread panicked",
        )))
    })
}

fn run() -> error::Result<()> {
    raise_fd_limit();
    let cli = Cli::parse();
    VERBOSE.store(cli.verbose, Ordering::Relaxed);

    match cli.command {
        Commands::Reflect {
            repo,
            text,
            transcript,
            domain,
            whoami,
            tags,
            follows,
            force,
            dedupe_mode,
        } => {
            let domain = if whoami {
                Some("identity".to_owned())
            } else {
                domain
            };
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let index = search::SearchIndex::open(&base.join("index"))?;

            // Near-duplicate detection before storing (Card C).
            // Skip when --force or --dedupe-mode off.
            let skip_dedupe = force || matches!(dedupe_mode, DedupeMode::Off);
            if !skip_dedupe
                && let Some(ref t) = text
                && let Some(model) = try_load_embed_model()
            {
                // Compound repos (comma-separated) each get their own check.
                for r in &repo {
                    run_dedupe_check(&database, &model, r, t, &dedupe_mode)?;
                }
                // If model is unavailable, skip dedupe silently (best-effort).
            }

            let meta = db::ReflectionMeta {
                domain,
                tags,
                parent_id: follows,
            };

            run_compound_command_with_meta(
                &database,
                &index,
                &repo,
                &text,
                &transcript,
                &meta,
                reflect::reflect_from_text_with_meta,
                reflect::reflect_from_transcript_with_meta,
                "storing reflection",
            )?;

            // Compute embeddings for new reflections (silent fail if model unavailable)
            if let Some(model) = try_load_embed_model() {
                let n = backfill_embeddings(&database, &model)?;
                if n > 0 {
                    info!("[legion] embedded {} reflections", n);
                }
            }
        }
        Commands::Forget { id, repo } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let index = search::SearchIndex::open(&base.join("index"))?;

            // Peek at the reflection first so we can run the optional
            // --repo safety check AND print a summary of what is about
            // to be deleted. The actual delete goes through
            // `delete_reflection` which re-verifies the id exists.
            let existing = database
                .get_reflection_by_id(&id)?
                .ok_or_else(|| error::LegionError::ReflectionNotFound(id.clone()))?;

            // Every audit row for this command shares action/target_type/
            // target_ref/task_id/source_type. Build them through one closure
            // so the three call sites (rejected / success / partial) only
            // name the three things that actually differ.
            let write_audit = |agent: &str, outcome: &str, details: Option<&str>| {
                audit(&db::AuditInput {
                    agent,
                    action: "delete-reflection",
                    target_type: "reflection",
                    target_ref: &id,
                    task_id: None,
                    source_type: "legion",
                    details,
                    outcome,
                });
            };

            if let Some(ref expected_repo) = repo
                && existing.repo != *expected_repo
            {
                // Audit the rejection BEFORE returning. Destructive-command
                // rejections are forensically relevant -- we want a trace
                // of every attempted forget, not just successful ones.
                let details = format!("expected={} actual={}", expected_repo, existing.repo);
                write_audit(&existing.repo, "rejected", Some(&details));
                return Err(error::LegionError::ReflectionRepoMismatch {
                    id: id.clone(),
                    actual: existing.repo.clone(),
                    expected: expected_repo.clone(),
                });
            }

            // Delete from SQLite first. The audit entry is written AFTER
            // the SQLite delete succeeds but BEFORE the index delete,
            // so even if the tantivy side fails we still have a record
            // that the db-side delete happened. The two-store write is
            // not transactional -- if index.delete fails, the next
            // `legion reindex` run will reconcile.
            let deleted = database.delete_reflection(&id)?;
            write_audit(&deleted.repo, "success", None);

            if let Err(e) = index.delete(&id) {
                // SQLite row is gone; tantivy still has the document.
                // Tell the operator exactly what state the system is in
                // and how to recover. Do not silently succeed.
                eprintln!(
                    "[legion forget] WARNING: SQLite delete succeeded but tantivy index delete failed for {id}.\n\
                     The reflection is gone from the database but may still appear in BM25 recall results\n\
                     as a ghost document until the index is rebuilt. Run `legion reindex` to reconcile.\n\
                     Underlying error: {e}"
                );
                write_audit(
                    &deleted.repo,
                    "partial",
                    Some("index-orphan: SQLite row deleted, tantivy index delete failed"),
                );
                return Err(e);
            }

            let preview: String = deleted.text.chars().take(80).collect();
            let ellipsis = if deleted.text.chars().count() > 80 {
                "..."
            } else {
                ""
            };
            println!(
                "forgot reflection {} ({}): {}{}",
                id, deleted.repo, preview, ellipsis
            );
        }
        Commands::Recall {
            repo,
            context,
            limit,
            latest,
            preview,
            cosine_only,
            min_score,
            domain,
        } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            let mut result = if let Some(ref dom) = domain {
                recall::recall_by_domain(&database, &repo, dom, limit)?
            } else if latest {
                recall::recall_latest(&database, &repo, limit)?
            } else if cosine_only {
                // --cosine-only requires the embed model; error if unavailable.
                let model = embed::EmbedModel::load().map_err(|e| {
                    error::LegionError::Embedding(format!(
                        "--cosine-only requires embedding model: {e}"
                    ))
                })?;
                recall::recall_cosine_only(&database, &model, &repo, &context, limit, min_score)?
            } else {
                let index = search::SearchIndex::open(&base.join("index"))?;
                // Try hybrid (BM25 + cosine) recall, fall back to BM25-only
                match try_load_embed_model() {
                    Some(model) => {
                        recall::recall(&database, &index, &model, &repo, &context, limit)?
                    }
                    None => recall::recall_bm25(&database, &index, &repo, &context, limit)?,
                }
            };
            // Apply min-score filter on hybrid/latest paths (cosine-only applies it inline).
            if !cosine_only && let Some(threshold) = min_score {
                recall::filter_by_min_score(&mut result, threshold);
            }
            let output = recall::format_for_hook(&result, preview);
            if !output.is_empty() {
                print!("{output}");
            }
        }
        Commands::Similar {
            id,
            limit,
            cross_repo,
            min_score,
            preview,
            json,
        } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            // Validate the embed model is available; similar needs embeddings to work.
            embed::EmbedModel::load().map_err(|e| {
                error::LegionError::Embedding(format!(
                    "legion similar requires embedding model: {e}"
                ))
            })?;
            let result = recall::find_similar_by_id(&database, &id, limit, cross_repo, min_score)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                let output = recall::format_for_hook(&result, preview);
                if !output.is_empty() {
                    print!("{output}");
                }
            }
        }
        Commands::Consult { context, limit } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let index = search::SearchIndex::open(&base.join("index"))?;

            let result = match try_load_embed_model() {
                Some(model) => recall::consult(&database, &index, &model, &context, limit)?,
                None => recall::consult_bm25(&database, &index, &context, limit)?,
            };
            let output = recall::format_for_consult(&result);
            if output.is_empty() {
                info!("[legion] no reflections matched context: \"{}\"", context);
            } else {
                print!("{output}");
            }
        }
        Commands::Post {
            repo,
            text,
            transcript,
            domain,
            tags,
            follows,
        } => {
            // Redirect @self posts to reflect -- they're private, not for the team
            let is_self_post = text.as_deref().is_some_and(|t| {
                let lower = t.trim_start().to_lowercase();
                lower.starts_with("@self ") || lower.starts_with("@self\t") || lower == "@self"
            });
            if is_self_post {
                eprintln!("[legion] @self posts are private -- redirecting to reflect");
            }

            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let index = search::SearchIndex::open(&base.join("index"))?;
            let meta = db::ReflectionMeta {
                domain,
                tags,
                parent_id: follows,
            };

            if is_self_post {
                run_compound_command_with_meta(
                    &database,
                    &index,
                    &repo,
                    &text,
                    &transcript,
                    &meta,
                    reflect::reflect_from_text_with_meta,
                    reflect::reflect_from_transcript_with_meta,
                    "reflecting",
                )?;
            } else {
                run_compound_command_with_meta(
                    &database,
                    &index,
                    &repo,
                    &text,
                    &transcript,
                    &meta,
                    board::post_from_text_with_meta,
                    board::post_from_transcript_with_meta,
                    "posting",
                )?;
            }

            // Compute embeddings for new posts
            if let Some(model) = try_load_embed_model() {
                let n = backfill_embeddings(&database, &model)?;
                if n > 0 {
                    info!("[legion] embedded {} posts", n);
                }
            }
        }
        Commands::Signal {
            repo,
            to,
            verb,
            status,
            note,
            details,
            follows,
            domain,
            tags,
        } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let index = search::SearchIndex::open(&base.join("index"))?;

            let detail_pairs: Vec<(String, String)> = details
                .as_deref()
                .map(|d| {
                    d.split(',')
                        .filter_map(|pair| {
                            let pair = pair.trim();
                            pair.find(':').map(|pos| {
                                (
                                    pair[..pos].trim().to_string(),
                                    pair[pos + 1..].trim().to_string(),
                                )
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            if let Some(ref n) = note {
                signal::validate_note(n)?;
            }

            let text = signal::format_signal(
                &to,
                &verb,
                status.as_deref(),
                note.as_deref(),
                &detail_pairs,
            );

            let meta = db::ReflectionMeta {
                domain,
                tags,
                parent_id: follows,
            };

            run_compound_command_with_meta(
                &database,
                &index,
                &repo,
                &Some(text),
                &None,
                &meta,
                board::post_from_text_with_meta,
                board::post_from_transcript_with_meta,
                "sending signal",
            )?;

            // Compute embeddings for new signals
            if let Some(model) = try_load_embed_model() {
                let n = backfill_embeddings(&database, &model)?;
                if n > 0 {
                    info!("[legion] embedded {} signals", n);
                }
            }
        }
        Commands::PendingReplies { repo } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            // Recipient is the agent name when configured via watch.toml,
            // else the repo name. Fall back to repo for un-watched callers.
            let recipient = watch::load_config(&base.join("watch.toml"))
                .ok()
                .and_then(|cfg| {
                    cfg.repos
                        .iter()
                        .find(|r| r.name == repo)
                        .map(|r| r.recipient().to_string())
                })
                .unwrap_or_else(|| repo.clone());

            let signals = watch::find_pending_signals(&database, &repo, &recipient, None)?;
            let reply_required: Vec<(String, String, String)> = signals
                .into_iter()
                .filter(|(_, text, _)| watch::signal_requires_reply(text))
                .collect();

            if !reply_required.is_empty() {
                print!("{}", watch::build_wake_prompt(&repo, &reply_required));
            }
        }
        Commands::Boost { id } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            if database.boost_reflection(&id)? {
                info!("[legion] boosted reflection {}", id);
            } else {
                eprintln!("[legion] reflection not found: {}", id);
            }
        }
        Commands::Chain { id, full } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            let chain = database.get_chain(&id)?;
            if chain.is_empty() {
                info!("[legion] no chain found for {}", id);
            } else if full {
                // Boundary marker `--- ` is parsed by the
                // identity-chain-load.sh UserPromptSubmit hook (#345) to
                // count chain links. Do not change the prefix without
                // updating the hook script.
                for r in &chain {
                    let date = db::format_date(&r.created_at);
                    let domain_tag = r
                        .domain
                        .as_deref()
                        .map(|d| format!(" [{}]", d))
                        .unwrap_or_default();
                    println!("--- {} {}{} (id: {}) ---", r.repo, date, domain_tag, r.id);
                    println!("{}", r.text);
                    println!();
                }
            } else {
                for (i, r) in chain.iter().enumerate() {
                    let prefix = if i == 0 {
                        String::new()
                    } else {
                        "  ".repeat(i) + "-> "
                    };
                    let date = db::format_date(&r.created_at);
                    let domain_tag = r
                        .domain
                        .as_deref()
                        .map(|d| format!(" [{}]", d))
                        .unwrap_or_default();
                    let truncated: String = r.text.chars().take(80).collect();
                    let ellipsis = if r.text.len() > 80 { "..." } else { "" };
                    println!(
                        "{}{} {}{}: {}{}",
                        prefix, r.repo, date, domain_tag, truncated, ellipsis
                    );
                }
            }
        }
        Commands::Bullpen {
            repo,
            count,
            signals,
            musings,
            archive,
            archived,
        } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            if archive {
                let count = board::archive_read_posts(&database)?;
                eprintln!("[legion] archived {count} posts");
            } else if archived {
                let posts = board::bullpen_archived(&database)?;
                let output = board::format_bullpen(&posts);
                if !output.is_empty() {
                    print!("{output}");
                }
            } else {
                // repo is guaranteed by clap's required_unless_present_any
                let repo = repo.expect("--repo required for this path");
                if count {
                    let post_count = board::bullpen_count(&database, &repo)?;
                    let task_count = task::count_pending_inbound(&database, &repo)?;
                    let output = board::format_bullpen_count(post_count, task_count);
                    if !output.is_empty() {
                        println!("{output}");
                    }
                } else {
                    let filter = if signals {
                        board::BullpenFilter::SignalsOnly
                    } else if musings {
                        board::BullpenFilter::MusingsOnly
                    } else {
                        board::BullpenFilter::All
                    };
                    let posts = board::bullpen_filtered(&database, &repo, filter)?;
                    let mut output = board::format_bullpen(&posts);
                    if filter == board::BullpenFilter::All {
                        let pending_tasks = task::get_pending_inbound(&database, &repo)?;
                        let task_output = task::format_pending_for_surface(&pending_tasks);
                        output.push_str(&task_output);
                    }
                    if !output.is_empty() {
                        print!("{output}");
                    }
                }
            }
        }
        Commands::Sym { action } => {
            run_sym(action)?;
        }
        Commands::Index {
            repo,
            file,
            status,
            json,
        } => {
            if status {
                run_index_status(repo.as_deref(), json)?;
                return Ok(());
            }
            let base = data_dir()?;
            let watch_path = base.join("watch.toml");
            let repos = watch::list_repos_in_config(&watch_path)?;

            // When --file is given, resolve the enclosing repo by longest
            // workdir prefix. Otherwise the repo arg is mandatory and
            // looked up by name.
            let entry = if let Some(file_arg) = &file {
                let abs = if file_arg.is_absolute() {
                    file_arg.clone()
                } else {
                    std::env::current_dir()?.join(file_arg)
                };
                let canon = std::fs::canonicalize(&abs).unwrap_or(abs);
                let mut best: Option<&watch::WatchRepoConfig> = None;
                let mut best_len = 0usize;
                for r in &repos {
                    let workdir = std::path::Path::new(&r.workdir);
                    if canon.starts_with(workdir) && r.workdir.len() > best_len {
                        best = Some(r);
                        best_len = r.workdir.len();
                    }
                }
                let chosen = best.ok_or_else(|| {
                    error::LegionError::WatchConfig(format!(
                        "no watch.toml repo encloses {}. Add the parent with `legion watch add <name> <path>`.",
                        canon.display()
                    ))
                })?;
                if let Some(name) = repo.as_deref()
                    && name != chosen.name
                {
                    return Err(error::LegionError::WatchConfig(format!(
                        "file path is under repo '{}', not '{name}'",
                        chosen.name
                    )));
                }
                chosen
            } else {
                let name = repo.as_deref().ok_or_else(|| {
                    error::LegionError::WatchConfig(
                        "repo name required when --file is not given".to_string(),
                    )
                })?;
                repos.iter().find(|r| r.name == name).ok_or_else(|| {
                    error::LegionError::WatchConfig(format!(
                        "repo '{name}' not in watch.toml. Add it with `legion watch add {name} <path>`."
                    ))
                })?
            };

            let repo_name = entry.name.clone();
            let repo_path = std::path::PathBuf::from(&entry.workdir);

            // Per-repo IndexConfig lives in the watch.toml `extra` table
            // alongside other integration config. Missing -> defaults.
            // A malformed table is surfaced on stderr but does not abort
            // -- a typo in one repo's overrides should not block indexing.
            let cfg: scip::IndexConfig = match entry.extra.clone().try_into() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "[legion index] warning: malformed indexer config for {repo_name}: {e}; using defaults"
                    );
                    scip::IndexConfig::default()
                }
            };

            let database = db::Database::open(&base.join("legion.db"))?;

            // --file branch: re-index the single language for the file.
            if let Some(file_arg) = file {
                let abs = if file_arg.is_absolute() {
                    file_arg.clone()
                } else {
                    std::env::current_dir()?.join(&file_arg)
                };
                let canon = std::fs::canonicalize(&abs).unwrap_or(abs);
                let hash =
                    scip::incremental_index_file(&database, &repo_name, &repo_path, &canon, &cfg)?;
                let hash_short: String = hash.chars().take(16).collect();
                eprintln!(
                    "[legion index] indexed {} (file: {}): hash {}",
                    repo_name,
                    canon.display(),
                    hash_short
                );
                return Ok(());
            }

            let detection = scip::detect_languages(&repo_path, &cfg);

            for missing in &detection.missing_binary {
                eprintln!(
                    "[legion index] skipping {}: {} not found on PATH",
                    missing.lang, missing.binary
                );
            }

            if detection.indexable.is_empty() && detection.missing_binary.is_empty() {
                return Err(error::LegionError::WatchConfig(format!(
                    "no supported language detected at {}",
                    repo_path.display()
                )));
            }

            let mut indexed = 0usize;
            let mut failed: Vec<(String, String)> = Vec::new();

            for lang_entry in &detection.indexable {
                match scip::run_indexer(lang_entry, &repo_path) {
                    Ok(blob) => {
                        let hash = scip::content_hash(&blob);
                        let now = chrono::Utc::now().to_rfc3339();
                        let blob_len = blob.len();
                        let hash_short = hash.chars().take(16).collect::<String>();
                        let index = scip::ScipIndex {
                            id: uuid::Uuid::now_v7().to_string(),
                            repo: repo_name.clone(),
                            lang: lang_entry.lang.clone(),
                            content_hash: hash,
                            blob,
                            updated_at: now,
                            deleted_at: None,
                        };
                        database.upsert_scip_index(&index)?;
                        eprintln!(
                            "[legion index] indexed {}: {} bytes, hash {}",
                            lang_entry.lang, blob_len, hash_short
                        );
                        indexed += 1;
                    }
                    Err(e) => {
                        eprintln!("[legion index] failed {}: {}", lang_entry.lang, e);
                        failed.push((lang_entry.lang.clone(), e.to_string()));
                    }
                }
            }

            eprintln!(
                "[legion index] summary {}: {} indexed, {} skipped, {} failed",
                repo_name,
                indexed,
                detection.missing_binary.len(),
                failed.len()
            );

            if indexed == 0 {
                // We reach here whenever languages were detected but none
                // produced a stored index -- either every attempt failed
                // outright, or every detected lang's binary was absent.
                // The "no language detected" case returned earlier as a
                // WatchConfig error, so this is always user-actionable.
                return Err(error::LegionError::IndexerFailed {
                    lang: "all".to_string(),
                    stderr: "no language indexed successfully -- see warnings above".to_string(),
                });
            }
        }
        Commands::Reindex => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let index = search::SearchIndex::open(&base.join("index"))?;

            let reflections = database.get_all_for_reindex()?;
            let count = reflections.len();
            index.rebuild(&reflections)?;
            info!("[legion] reindexed {} reflections", count);
        }
        Commands::Cleanup { retention_days } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            let result = database.cleanup_tombstones(retention_days)?;
            if result.is_empty() {
                eprintln!("[legion] no tombstones older than {} days", retention_days);
            } else {
                eprintln!(
                    "[legion] cleaned up tombstones older than {} days: {} reflections, {} tasks, {} schedules ({} total)",
                    retention_days,
                    result.reflections,
                    result.tasks,
                    result.schedules,
                    result.total()
                );
            }
        }
        Commands::Rename { from, to } => {
            if from == to {
                eprintln!("[legion] source and destination are the same, nothing to do");
                return Ok(());
            }

            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let index = search::SearchIndex::open(&base.join("index"))?;

            let counts = database.rename_repo(&from, &to)?;
            eprintln!(
                "[legion] renamed '{}' -> '{}': {} reflections, {} tasks (from), {} tasks (to), {} board reads, {} watch handled, {} schedules",
                from,
                to,
                counts.reflections,
                counts.tasks_from,
                counts.tasks_to,
                counts.board_reads,
                counts.watch_handled,
                counts.schedules
            );

            // Reindex since repo name is in the search index
            let reflections = database.get_all_for_reindex()?;
            let reindex_count = reflections.len();
            index.rebuild(&reflections)?;
            eprintln!("[legion] reindexed {} reflections", reindex_count);

            // Update watch.toml
            let watch_path = base.join("watch.toml");
            if watch::rename_in_config(&watch_path, &from, &to)? {
                eprintln!("[legion] updated watch.toml: '{}' -> '{}'", from, to);
            }

            eprintln!("[legion] total: {} rows updated", counts.total());
        }
        Commands::Backfill => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            let model = embed::EmbedModel::load()?;
            let count = backfill_embeddings(&database, &model)?;
            info!("[legion] embedded {} reflections", count);
        }
        Commands::Init { force } => {
            init::init(force)?;
        }
        Commands::Surface { repo } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            let result = surface::surface(&database, &repo)?;
            let output = surface::format_surface(&result, &repo);
            if !output.is_empty() {
                print!("{output}");
            }
        }
        Commands::Whoami { repo, limit } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let roots = database.get_identity_roots(&repo, limit)?;
            if roots.is_empty() {
                return Ok(());
            }
            let mut entries = Vec::with_capacity(roots.len());
            for r in roots {
                let in_chain = database.is_in_chain(&r.id)?;
                entries.push(recall::WhoamiEntry {
                    id: r.id,
                    text: r.text,
                    in_chain,
                });
            }
            let output = recall::format_whoami(&repo, &entries);
            print!("{output}");
        }
        Commands::Stats { repo } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            stats::stats(&database, repo.as_deref())?;
        }
        Commands::Serve { port } => {
            let base = data_dir()?;
            let watch_path = base.join("watch.toml");
            if watch_path.exists() {
                let contents = std::fs::read_to_string(&watch_path)?;
                let config: watch::WatchConfig = toml::from_str(&contents)?;
                if !config.serve {
                    return Err(error::LegionError::Server(
                        "serve is not enabled on this node. Set serve = true in watch.toml to designate this machine as the dashboard server.".to_string(),
                    ));
                }
            }
            serve::run_server(port, base)?;
        }
        Commands::Status { repo, json } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let output = status::get_status(&database, &repo)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&status::format_summary(&output))?
                );
            } else {
                let formatted = status::format_status(&output);
                if formatted.is_empty() {
                    println!(
                        "[Legion] Status for {}: all clear. Check `gh issue list` for GitHub issues.",
                        repo
                    );
                } else {
                    print!("{formatted}");
                }
            }
        }
        Commands::Needs { repo } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let items = status::get_needs(&database, &repo)?;
            print!("{}", status::format_needs(&repo, &items));
        }
        Commands::Done { repo, text, id } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let index = search::SearchIndex::open(&base.join("index"))?;

            // Validate card transition BEFORE posting announcements
            if let Some(ref card_id) = id {
                let card =
                    kanban::transition_card(&database, card_id, kanban::Action::Done, Some(&text))?;
                println!("{card_id}");

                // Close the linked external issue if present, using the
                // done-text as the closing comment so the GitHub issue thread
                // records why it was closed.
                if let (Some(url), Some(source)) = (&card.source_url, &card.source_type)
                    && let Some(number) = worksource::extract_issue_number(url)
                    && let Some((_, source_repo, _)) = worksource::resolve_config(&repo)
                    && let Err(e) =
                        worksource::close_issue(source, &source_repo, number, Some(&text))
                {
                    eprintln!("[legion] failed to close {source} issue #{number}: {e}");
                }
            }

            let announcement = format!("{repo} completed: {text}");
            let reflection = database.insert_reflection_with_meta(
                &repo,
                &announcement,
                "team",
                &db::ReflectionMeta::default(),
            )?;
            if let Err(e) = index.add(&reflection.id, &reflection.repo, &announcement) {
                eprintln!("[legion] search index add failed: {e}");
            }
            info!("[legion] done: {text}");

            let blocked_agents = status::find_blocked_agents(&database, &repo)?;
            for agent in &blocked_agents {
                let notify_text = format!(
                    "@{agent} announce from {repo} -- {repo} completed: {text}. Your blocker may be cleared."
                );
                let notify_ref = database.insert_reflection_with_meta(
                    &repo,
                    &notify_text,
                    "team",
                    &db::ReflectionMeta::default(),
                )?;
                if let Err(e) = index.add(&notify_ref.id, &notify_ref.repo, &notify_text) {
                    eprintln!("[legion] search index add failed: {e}");
                }
                info!("[legion] notified {agent} (was blocked on {repo})");
            }

            if blocked_agents.is_empty() {
                info!("[legion] no blocked agents found");
            }
        }
        Commands::Work { repo, peek } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            // Sync from external work sources before checking the queue
            if let Some((plugin, source_repo, workdir)) = worksource::resolve_config(&repo) {
                match worksource::sync_issues(&database, &plugin, &source_repo, &workdir, &repo) {
                    Ok(n) if n > 0 => info!("[legion] synced {n} new issues from {plugin}"),
                    Ok(_) => {}
                    Err(e) => eprintln!("[legion] work source sync failed: {e}"),
                }
            }

            let card = if peek {
                kanban::peek_work(&database, &repo)?
            } else {
                kanban::next_work(&database, &repo)?
            };

            match card {
                Some(c) => print!("{}", kanban::format_work_card(&c)),
                None => info!("[legion] no pending work for {repo}"),
            }
        }
        Commands::Sync { repo } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            // Sync from external work sources
            if let Some((plugin, source_repo, workdir)) = worksource::resolve_config(&repo) {
                match worksource::sync_issues(&database, &plugin, &source_repo, &workdir, &repo) {
                    Ok(n) if n > 0 => info!("[legion] synced {n} new issues from {plugin}"),
                    Ok(_) => {}
                    Err(e) => eprintln!("[legion] work source sync failed: {e}"),
                }
            } else {
                return Err(error::LegionError::WorkSource(format!(
                    "no work source configured for {repo}"
                )));
            }
        }
        Commands::Cluster { action } => {
            let base = data_dir()?;
            cluster::handle_cluster_command(&base, action)?;
        }
        Commands::Kanban { action } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            match action {
                KanbanAction::Create {
                    from,
                    to,
                    text,
                    context,
                    priority,
                    labels,
                    parent,
                    source_url,
                    source_type,
                } => {
                    let id = kanban::create_card(
                        &database,
                        &from,
                        &to,
                        &text,
                        context.as_deref(),
                        &priority,
                        labels.as_deref(),
                        parent.as_deref(),
                        source_url.as_deref(),
                        source_type.as_deref(),
                        None,
                    )?;
                    println!("{id}");
                }
                KanbanAction::View { id, json } => {
                    let card = kanban::view_card(&database, &id).map_err(|e| {
                        eprintln!("{e}");
                        e
                    })?;
                    if json {
                        println!("{}", kanban::format_card_json(&card)?);
                    } else {
                        print!("{}", kanban::format_card_view(&card));
                    }
                }
                KanbanAction::Update {
                    id,
                    repo,
                    text,
                    body,
                    priority,
                    labels,
                    add_labels,
                    remove_labels,
                } => {
                    let any_set = text.is_some()
                        || body.is_some()
                        || priority.is_some()
                        || labels.is_some()
                        || add_labels.is_some()
                        || remove_labels.is_some();
                    if !any_set {
                        eprintln!(
                            "[legion] no fields to update: pass at least one of --text, --body, --priority, --labels, --add-labels, --remove-labels"
                        );
                        std::process::exit(1);
                    }
                    let params = kanban::CardUpdateParams {
                        text,
                        body,
                        priority,
                        labels,
                        add_labels,
                        remove_labels,
                    };
                    let card_id = kanban::update_card(&database, &id, &repo, &params)?;
                    println!("{card_id}");
                    audit(&db::AuditInput {
                        agent: &repo,
                        action: "update-card",
                        target_type: "card",
                        target_ref: &id,
                        task_id: Some(&id),
                        source_type: "legion",
                        details: None,
                        outcome: "success",
                    });
                }
                KanbanAction::List { repo, from, json } => {
                    let direction = if from {
                        kanban::Direction::Outbound
                    } else {
                        kanban::Direction::Inbound
                    };
                    let cards = kanban::list_cards(&database, &repo, direction)?;
                    if json {
                        let output = kanban::format_card_list_json(&cards)?;
                        print!("{output}");
                    } else {
                        let output = kanban::format_card_list(&cards, &repo, direction);
                        if output.is_empty() {
                            info!("[legion] no cards found");
                        } else {
                            print!("{output}");
                        }
                    }
                }
                KanbanAction::Accept { id } => {
                    kanban::transition_card(&database, &id, kanban::Action::Accept, None)?;
                    println!("{id}");
                }
                KanbanAction::Block { id, reason } => {
                    kanban::transition_card(
                        &database,
                        &id,
                        kanban::Action::Block,
                        reason.as_deref(),
                    )?;
                    println!("{id}");
                }
                KanbanAction::Unblock { id } => {
                    kanban::transition_card(&database, &id, kanban::Action::Unblock, None)?;
                    println!("{id}");
                }
                KanbanAction::Review { id } => {
                    kanban::transition_card(&database, &id, kanban::Action::Review, None)?;
                    println!("{id}");
                }
                KanbanAction::NeedInput { id, reason } => {
                    kanban::transition_card(
                        &database,
                        &id,
                        kanban::Action::NeedInput,
                        reason.as_deref(),
                    )?;
                    println!("{id}");
                }
                KanbanAction::Resume { id } => {
                    kanban::transition_card(&database, &id, kanban::Action::Resume, None)?;
                    println!("{id}");
                }
                KanbanAction::Cancel {
                    id,
                    reason,
                    no_propagate,
                } => {
                    kanban::transition_card(
                        &database,
                        &id,
                        kanban::Action::Cancel,
                        reason.as_deref(),
                    )?;
                    println!("{id}");
                    if !no_propagate {
                        match propagate_card_close_to_worksource(&database, &id, reason.as_deref())
                        {
                            PropagateOutcome::Propagated | PropagateOutcome::Skipped => {}
                            PropagateOutcome::Failed => {
                                // Emit a visible warning line on stdout so
                                // scripted callers (`legion kanban cancel |
                                // ...`) can detect partial success. The
                                // card transition has already succeeded --
                                // the first println of {id} above is still
                                // the authoritative success marker -- but
                                // the linked GitHub issue was NOT closed
                                // and the caller needs to know.
                                println!(
                                    "[legion] WARNING: card {id} cancelled locally but linked github issue propagation FAILED -- run `legion kanban reconcile --close-stale` to retry"
                                );
                            }
                        }
                    }
                }
                KanbanAction::Assign { id, to } => {
                    database.assign_card(&id, &to)?;
                    println!("{id}");
                }
                KanbanAction::Reopen {
                    id,
                    reason,
                    no_propagate,
                } => {
                    kanban::transition_card(
                        &database,
                        &id,
                        kanban::Action::Reopen,
                        reason.as_deref(),
                    )?;
                    println!("{id}");
                    if !no_propagate {
                        match propagate_card_reopen_to_worksource(&database, &id, reason.as_deref())
                        {
                            PropagateOutcome::Propagated | PropagateOutcome::Skipped => {}
                            PropagateOutcome::Failed => {
                                // Same stdout-visibility pattern as
                                // KanbanAction::Cancel above. The card
                                // is reopened locally but the linked
                                // GitHub issue was not reopened, and
                                // scripted callers need to see this on
                                // stdout, not buried in stderr.
                                println!(
                                    "[legion] WARNING: card {id} reopened locally but linked github issue propagation FAILED -- the public issue may still be closed"
                                );
                            }
                        }
                    }
                }
                KanbanAction::Delete { id } => {
                    // Capture the card's to_repo before the delete so the
                    // audit entry records which agent owned it. A card
                    // that does not exist at delete time is a hard error
                    // from the DB layer (CardNotFound), matching the
                    // shape of the other id-targeted kanban subcommands;
                    // the lookup here therefore either returns a real
                    // repo or propagates the error before we ever reach
                    // the audit call, so there is no need for a fallback
                    // value.
                    let agent_repo = database
                        .get_card_by_id(&id)?
                        .ok_or_else(|| error::LegionError::CardNotFound(id.clone()))?
                        .to_repo;

                    database.delete_card(&id)?;
                    println!("{id}");

                    audit(&db::AuditInput {
                        agent: &agent_repo,
                        action: "delete-card",
                        target_type: "card",
                        target_ref: &id,
                        task_id: Some(&id),
                        source_type: "legion",
                        details: None,
                        outcome: "success",
                    });
                }
                KanbanAction::Reconcile { repo, close_stale } => {
                    // Default mode is read-only. `--close-stale` is the
                    // only flag that actually touches GitHub.
                    let cards = kanban::board_cards(&database)?;
                    let mut stale: Vec<(kanban::Card, u64, String)> = Vec::new();

                    for card in cards {
                        // Only done/cancelled cards are candidates for
                        // reconciliation. Active cards are expected to
                        // have open issues.
                        if !matches!(
                            card.status,
                            kanban::CardStatus::Done | kanban::CardStatus::Cancelled
                        ) {
                            continue;
                        }

                        if let Some(ref filter) = repo
                            && card.to_repo != *filter
                        {
                            continue;
                        }

                        let Some(ref source_url) = card.source_url else {
                            continue;
                        };
                        let Some(number) = worksource::extract_issue_number(source_url) else {
                            continue;
                        };
                        let Some(source) = card.source_type.as_deref() else {
                            continue;
                        };
                        let Some((_, source_repo, _)) = worksource::resolve_config(&card.to_repo)
                        else {
                            eprintln!(
                                "[legion] reconcile: skipping card {} -- to_repo '{}' has no work source configured",
                                card.id, card.to_repo
                            );
                            continue;
                        };

                        // Query the live issue state. view_issue returns
                        // ExternalIssue with a `state` field that is
                        // "OPEN" or "CLOSED" for GitHub. Any other
                        // value is treated as "unknown" and skipped
                        // with a warning so a plugin that returns a
                        // non-standard state does not silently get
                        // reported as stale-open.
                        let state = match worksource::view_issue(source, &source_repo, number) {
                            Ok(issue) => issue.state,
                            Err(e) => {
                                eprintln!(
                                    "[legion] reconcile: failed to view {} issue #{}: {}",
                                    source_repo, number, e
                                );
                                continue;
                            }
                        };

                        if state == "OPEN" {
                            stale.push((card.clone(), number, source_repo.clone()));
                        }
                    }

                    if stale.is_empty() {
                        println!("[legion] reconcile: no stale-open issues found");
                    } else {
                        println!(
                            "[legion] reconcile: {} stale-open issue(s) found",
                            stale.len()
                        );
                        for (card, number, source_repo) in &stale {
                            println!(
                                "  card={} status={} source={}#{} to_repo={}",
                                card.id,
                                card.status.label(),
                                source_repo,
                                number,
                                card.to_repo
                            );
                        }
                    }

                    if close_stale && !stale.is_empty() {
                        println!("[legion] reconcile: closing {} stale issue(s)", stale.len());
                        let mut closed = 0_u64;
                        let mut failed = 0_u64;
                        for (card, number, source_repo) in &stale {
                            let Some(source) = card.source_type.as_deref() else {
                                continue;
                            };
                            let reconcile_comment = format!(
                                "Closed by legion reconcile: local card {} is {} but this issue was left open. Reconciling to match the local state.",
                                card.id,
                                card.status.label()
                            );
                            match worksource::close_issue(
                                source,
                                source_repo,
                                *number,
                                Some(&reconcile_comment),
                            ) {
                                Ok(()) => {
                                    closed += 1;
                                    eprintln!(
                                        "[legion] reconcile: closed {} issue #{}",
                                        source_repo, number
                                    );
                                    let details = serde_json::json!({
                                        "card_id": card.id,
                                        "propagation": "reconcile",
                                    });
                                    let details_str = details.to_string();
                                    audit(&db::AuditInput {
                                        agent: &card.to_repo,
                                        action: "close-issue",
                                        target_type: "issue",
                                        target_ref: &number.to_string(),
                                        task_id: Some(&card.id),
                                        source_type: source,
                                        details: Some(&details_str),
                                        outcome: "success",
                                    });
                                }
                                Err(e) => {
                                    failed += 1;
                                    eprintln!(
                                        "[legion] reconcile: failed to close {} issue #{}: {}",
                                        source_repo, number, e
                                    );
                                    // Record the failure in the audit log
                                    // so a human operator running
                                    // `legion audit --action close-issue`
                                    // can see exactly which stale issues
                                    // reconcile could not close. Without
                                    // this, a reconcile run that fails
                                    // on 3 of 10 issues leaves no durable
                                    // record of which 3 -- same class of
                                    // invisibility the PR is fixing for
                                    // the direct cancel path.
                                    let err_msg = e.to_string();
                                    let details = serde_json::json!({
                                        "card_id": card.id,
                                        "propagation": "reconcile",
                                        "error": err_msg,
                                    });
                                    let details_str = details.to_string();
                                    audit(&db::AuditInput {
                                        agent: &card.to_repo,
                                        action: "close-issue",
                                        target_type: "issue",
                                        target_ref: &number.to_string(),
                                        task_id: Some(&card.id),
                                        source_type: source,
                                        details: Some(&details_str),
                                        outcome: "failure",
                                    });
                                }
                            }
                        }
                        println!("[legion] reconcile: {} closed, {} failed", closed, failed);
                    } else if close_stale {
                        println!("[legion] reconcile: nothing to close");
                    } else if !stale.is_empty() {
                        println!(
                            "[legion] reconcile: dry-run -- pass --close-stale to actually close these issues"
                        );
                    }
                }
            }
        }
        Commands::Task { action } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            match action {
                TaskAction::Create {
                    from,
                    to,
                    text,
                    context,
                    priority,
                } => {
                    let id = task::create_task(
                        &database,
                        &from,
                        &to,
                        &text,
                        context.as_deref(),
                        &priority,
                    )?;
                    println!("{id}");
                    info!("[legion] task created: {} -> {}", from, to);
                }
                TaskAction::List { repo, from } => {
                    let direction = if from {
                        task::Direction::Outbound
                    } else {
                        task::Direction::Inbound
                    };
                    let tasks = task::list_tasks(&database, &repo, direction)?;
                    let output = task::format_task_list(&tasks, &repo, direction);
                    if output.is_empty() {
                        info!("[legion] no tasks found");
                    } else {
                        print!("{output}");
                    }
                }
                TaskAction::Accept { id } => {
                    task::accept_task(&database, &id)?;
                    info!("[legion] task accepted: {}", id);
                }
                TaskAction::Done { id, note } => {
                    task::complete_task(&database, &id, note.as_deref())?;
                    info!("[legion] task completed: {}", id);
                }
                TaskAction::Block { id, reason } => {
                    task::block_task(&database, &id, reason.as_deref())?;
                    info!("[legion] task blocked: {}", id);
                }
                TaskAction::Unblock { id } => {
                    task::unblock_task(&database, &id)?;
                    info!("[legion] task unblocked: {}", id);
                }
            }
        }
        Commands::Schedule { action } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            match action {
                ScheduleAction::Create {
                    name,
                    cron,
                    command,
                    repo,
                    active_start,
                    active_end,
                } => {
                    let id = database.insert_schedule(
                        &name,
                        &cron,
                        &command,
                        &repo,
                        active_start.as_deref(),
                        active_end.as_deref(),
                    )?;
                    println!("{id}");
                    info!("[legion] schedule created: {}", name);
                }
                ScheduleAction::List => {
                    let schedules = database.list_schedules()?;
                    if schedules.is_empty() {
                        info!("[legion] no schedules");
                    } else {
                        println!("[Legion] Schedules:");
                        for s in &schedules {
                            let status = if s.enabled { "on" } else { "off" };
                            let next = if s.enabled { &s.next_run } else { "-" };
                            let truncated: String = s.command.chars().take(20).collect();
                            let ellipsis = if s.command.len() > 20 { "..." } else { "" };
                            let window = match (&s.active_start, &s.active_end) {
                                (Some(start), Some(end)) => format!("  window: {start}-{end}"),
                                _ => String::new(),
                            };
                            println!(
                                "  [{status}] {cron:<6} {name:<20} \"{text}{ellip}\"  ({repo})  next: {next}{window}",
                                status = status,
                                cron = s.cron,
                                name = s.name,
                                text = truncated,
                                ellip = ellipsis,
                                repo = s.repo,
                                next = next,
                                window = window,
                            );
                        }
                    }
                }
                ScheduleAction::Enable { id } => {
                    if database.toggle_schedule(&id, true)? {
                        info!("[legion] schedule enabled: {}", id);
                    } else {
                        eprintln!("[legion] schedule not found: {}", id);
                    }
                }
                ScheduleAction::Disable { id } => {
                    if database.toggle_schedule(&id, false)? {
                        info!("[legion] schedule disabled: {}", id);
                    } else {
                        eprintln!("[legion] schedule not found: {}", id);
                    }
                }
                ScheduleAction::Delete { id } => {
                    if database.delete_schedule(&id)? {
                        info!("[legion] schedule deleted: {}", id);
                    } else {
                        eprintln!("[legion] schedule not found: {}", id);
                    }
                }
                ScheduleAction::Update {
                    id,
                    cron,
                    active_start,
                    active_end,
                } => {
                    if let Some(ref c) = cron {
                        db::validate_hhmm(c).ok();
                    }
                    if let Some(ref s) = active_start {
                        db::validate_hhmm(s)?;
                    }
                    if let Some(ref e) = active_end {
                        db::validate_hhmm(e)?;
                    }
                    if database.update_schedule(
                        &id,
                        cron.as_deref(),
                        active_start.as_deref(),
                        active_end.as_deref(),
                    )? {
                        eprintln!("[legion] schedule updated: {}", id);
                    } else {
                        eprintln!("[legion] schedule not found or nothing to update: {}", id);
                    }
                }
            }
        }
        Commands::Issue { action } => match action {
            IssueAction::Create {
                repo,
                title,
                body,
                labels,
                assignee,
            } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                let created = worksource::create_issue(
                    &plugin_name,
                    &source_repo,
                    &title,
                    body.as_deref(),
                    labels.as_deref(),
                    assignee.as_deref(),
                )?;

                let details = serde_json::json!({
                    "title": title, "labels": labels, "assignee": assignee,
                });
                let details_str = details.to_string();
                audit(&db::AuditInput {
                    agent: &repo,
                    action: "create-issue",
                    target_type: "issue",
                    target_ref: &created.number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                });

                println!("{}", created.url);
                eprintln!(
                    "[legion] created issue #{} on {}",
                    created.number, source_repo
                );
            }
            IssueAction::View { repo, number } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                let issue = worksource::view_issue(&plugin_name, &source_repo, number)?;
                let parsed = card_parse::parse_issue_body(issue.body.as_deref().unwrap_or(""));

                // Structured output
                println!("# {} #{}\n", issue.title, issue.number);

                if let Some(ref problem) = parsed.problem {
                    println!("Problem: {}\n", problem);
                }
                if let Some(ref solution) = parsed.solution {
                    println!("Solution: {}\n", solution);
                }
                if !parsed.acceptance.is_empty() {
                    println!("Acceptance criteria:");
                    for item in &parsed.acceptance {
                        println!("  - {}", item);
                    }
                    println!();
                }
                for (heading, content) in &parsed.sections {
                    println!("{}:\n{}\n", heading, content);
                }
                if let Some(ref body) = parsed.body {
                    println!("{}\n", body);
                }

                println!("State: {}", issue.state);
                println!("URL: {}", issue.url);
            }
            IssueAction::Close {
                repo,
                number,
                comment,
            } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                worksource::close_issue(&plugin_name, &source_repo, number, comment.as_deref())?;

                let details = serde_json::json!({ "comment": comment });
                let details_str = details.to_string();
                audit(&db::AuditInput {
                    agent: &repo,
                    action: "close-issue",
                    target_type: "issue",
                    target_ref: &number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                });

                println!("closed issue #{} on {}", number, source_repo);
            }
            IssueAction::Reopen {
                repo,
                number,
                comment,
            } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                worksource::reopen_issue(&plugin_name, &source_repo, number, comment.as_deref())?;

                let details = serde_json::json!({ "comment": comment });
                let details_str = details.to_string();
                audit(&db::AuditInput {
                    agent: &repo,
                    action: "reopen-issue",
                    target_type: "issue",
                    target_ref: &number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                });

                println!("reopened issue #{} on {}", number, source_repo);
            }
            IssueAction::Edit {
                repo,
                number,
                title,
                body,
            } => {
                if title.is_none() && body.is_none() {
                    return Err(error::LegionError::WorkSource(
                        "legion issue edit requires at least one of --title or --body".to_string(),
                    ));
                }

                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                worksource::edit_issue(
                    &plugin_name,
                    &source_repo,
                    number,
                    title.as_deref(),
                    body.as_deref(),
                )?;

                let details = serde_json::json!({
                    "title_set": title.is_some(),
                    "body_set": body.is_some(),
                });
                let details_str = details.to_string();
                audit(&db::AuditInput {
                    agent: &repo,
                    action: "edit-issue",
                    target_type: "issue",
                    target_ref: &number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                });

                println!("edited issue #{} on {}", number, source_repo);
            }
        },
        Commands::Pr { action } => match action {
            PrAction::Create {
                repo,
                title,
                body,
                base,
                head,
                draft,
                labels,
                reviewer,
                task,
                skip_gates,
            } => {
                // Quality gate: verify legion-simplify ran clean on HEAD before
                // calling the work source. This runs before worksource::resolve_config
                // so the gate error is always surfaced, even if the worksource is not
                // configured. --skip-gates is a bootstrap escape hatch for the branch
                // that ships the skill itself; it writes an audit entry so it cannot
                // be done silently.
                if skip_gates {
                    audit(&db::AuditInput {
                        agent: &repo,
                        action: "skip-gate-bootstrap",
                        target_type: "pr",
                        target_ref: "pending",
                        task_id: task.as_deref(),
                        source_type: "unknown",
                        details: Some(r#"{"skill":"legion-simplify","reason":"bootstrap"}"#),
                        outcome: "skipped",
                    });
                    eprintln!("[legion] warning: quality gate skipped (--skip-gates bootstrap)");
                } else {
                    let head_hash = std::process::Command::new("git")
                        .args(["rev-parse", "HEAD"])
                        .output()
                        .map_err(|e| {
                            error::LegionError::WorkSource(format!("failed to read git HEAD: {e}"))
                        })?;
                    if !head_hash.status.success() {
                        return Err(error::LegionError::WorkSource(
                            "git rev-parse HEAD failed -- is this a git repo?".to_owned(),
                        ));
                    }
                    let commit_hash: String =
                        String::from_utf8_lossy(&head_hash.stdout).trim().to_owned();
                    let short_hash: &str = &commit_hash[..commit_hash.len().min(8)];

                    let db_base = data_dir()?;
                    let gate_db = db::Database::open(&db_base.join("legion.db"))?;
                    match gate_db.get_quality_gate(&commit_hash, "legion-simplify")? {
                        None => {
                            eprintln!(
                                "[legion] error: no clean legion-simplify gate on HEAD ({short_hash}). \
                                 Run /legion-simplify before creating the PR."
                            );
                            std::process::exit(1);
                        }
                        Some(gate) if gate.result != "clean" => {
                            eprintln!(
                                "[legion] error: legion-simplify recorded issues on HEAD ({short_hash}), \
                                 {} findings. Fix them and re-run the skill before creating the PR.",
                                gate.findings_count
                            );
                            std::process::exit(1);
                        }
                        Some(_) => {
                            // Gate is clean -- proceed.
                        }
                    }
                }

                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                let created = worksource::create_pr(
                    &plugin_name,
                    &source_repo,
                    &title,
                    body.as_deref(),
                    base.as_deref(),
                    head.as_deref(),
                    draft,
                    labels.as_deref(),
                    reviewer.as_deref(),
                )?;

                // Store PR URL on the kanban card if linked
                if let Some(ref task_id) = task {
                    let db_base = data_dir()?;
                    let database = db::Database::open(&db_base.join("legion.db"))?;
                    // Update the card's context with the PR URL
                    if let Some(_card) = database.get_card_by_id(task_id)? {
                        eprintln!("[legion] linked PR #{} to card {}", created.number, task_id);
                    }
                }

                let details = serde_json::json!({
                    "title": title, "base": base, "head": head, "draft": draft,
                });
                let details_str = details.to_string();
                audit(&db::AuditInput {
                    agent: &repo,
                    action: "create-pr",
                    target_type: "pr",
                    target_ref: &created.number.to_string(),
                    task_id: task.as_deref(),
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                });

                // Auto-signal review agent if configured
                if let Some(review_cfg) = worksource::resolve_review_config()
                    && review_cfg.auto_signal
                {
                    let signal_text = format!(
                        "@{} review:ready -- repo:{},pr:{},source:{}",
                        review_cfg.agent, repo, created.number, source_repo
                    );
                    if let Ok(db_base) = data_dir()
                        && let Ok(database) = db::Database::open(&db_base.join("legion.db"))
                        && let Ok(index) = search::SearchIndex::open(&db_base.join("index"))
                    {
                        let meta = db::ReflectionMeta::default();
                        match board::post_from_text_with_meta(
                            &database,
                            &index,
                            &repo,
                            &signal_text,
                            &meta,
                        ) {
                            Ok(_) => {
                                eprintln!("[legion] signaled @{} for review", review_cfg.agent)
                            }
                            Err(e) => {
                                eprintln!("[legion] warning: review signal failed: {}", e)
                            }
                        }
                    }
                }

                println!("{}", created.url);
                eprintln!("[legion] created PR #{} on {}", created.number, source_repo);
            }
            PrAction::List { repo } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                let prs = worksource::list_prs(&plugin_name, &source_repo)?;
                if prs.is_empty() {
                    eprintln!("[legion] no open PRs on {}", source_repo);
                } else {
                    for pr in &prs {
                        let review = pr.review_decision.as_deref().unwrap_or("PENDING");
                        let draft = if pr.is_draft { " [draft]" } else { "" };
                        println!(
                            "#{} {} ({}) {}{}",
                            pr.number, pr.title, pr.head_ref_name, review, draft
                        );
                    }
                }
            }
            PrAction::Checks {
                repo,
                number,
                json,
                log_failed,
            } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                let checks = worksource::pr_checks(&plugin_name, &source_repo, number)?;

                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&checks)
                            .expect("ExternalPRCheck serializes infallibly")
                    );
                } else if checks.is_empty() {
                    eprintln!(
                        "[legion] no checks reported for PR #{} on {}",
                        number, source_repo
                    );
                } else {
                    let mut name_w = 0usize;
                    let mut wf_w = 0usize;
                    for c in &checks {
                        name_w = name_w.max(c.name.len());
                        wf_w = wf_w.max(c.workflow.len());
                    }
                    for c in &checks {
                        println!(
                            "{:<8} {:<name_w$}  {:<wf_w$}  {}",
                            c.state,
                            c.name,
                            c.workflow,
                            c.link,
                            name_w = name_w,
                            wf_w = wf_w,
                        );
                    }
                }

                if log_failed && !json {
                    for c in checks.iter().filter(|c| c.is_failing()) {
                        match worksource::job_id_from_link(&c.link) {
                            Some(job_id) => {
                                println!("\n===== {} ({}) =====", c.name, job_id);
                                match worksource::fetch_check_log(
                                    &plugin_name,
                                    &source_repo,
                                    job_id,
                                ) {
                                    Ok(log) => print!("{log}"),
                                    Err(e) => {
                                        println!("(log unavailable: {e})");
                                    }
                                }
                            }
                            None => {
                                println!(
                                    "\n===== {} =====\n(log unavailable: non-Actions check link: {})",
                                    c.name, c.link
                                );
                            }
                        }
                    }
                }

                let failed: Vec<&str> = checks
                    .iter()
                    .filter(|c| c.is_failing())
                    .map(|c| c.name.as_str())
                    .collect();
                if !failed.is_empty() {
                    return Err(error::LegionError::WorkSource(format!(
                        "{} check(s) failed on PR #{}: {}",
                        failed.len(),
                        number,
                        failed.join(", ")
                    )));
                }
            }
            PrAction::View { repo, number, json } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                let pr = worksource::view_pr(&plugin_name, &source_repo, number)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&pr)
                            .expect("ExternalPRDetails serializes infallibly")
                    );
                } else {
                    pr_view::render_pr(&pr);
                }
            }
            PrAction::Comments { repo, number, json } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                let comments = worksource::list_pr_comments(&plugin_name, &source_repo, number)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&comments)
                            .expect("ExternalPRComment serializes infallibly")
                    );
                } else if comments.is_empty() {
                    eprintln!("[legion] no comments on PR #{} on {}", number, source_repo);
                } else {
                    pr_view::render_comments(&comments);
                }
            }
            PrAction::Reviews { repo, number, json } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                let reviews = worksource::list_pr_reviews(&plugin_name, &source_repo, number)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&reviews)
                            .expect("ExternalPRReview serializes infallibly")
                    );
                } else if reviews.is_empty() {
                    eprintln!("[legion] no reviews on PR #{} on {}", number, source_repo);
                } else {
                    pr_view::render_reviews(&reviews);
                }
            }
            PrAction::Review {
                repo,
                number,
                approve,
                request_changes,
                body,
                comments,
            } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                let event = if approve {
                    "APPROVE"
                } else if request_changes {
                    "REQUEST_CHANGES"
                } else {
                    // COMMENT requires a body or inline comments
                    if body.is_none() && comments.is_none() {
                        return Err(error::LegionError::WorkSource(
                            "comment review requires --body or --comments".to_string(),
                        ));
                    }
                    "COMMENT"
                };

                worksource::review_pr(
                    &plugin_name,
                    &source_repo,
                    number,
                    event,
                    body.as_deref(),
                    comments.as_deref(),
                )?;

                let details = serde_json::json!({ "event": event });
                let details_str = details.to_string();
                audit(&db::AuditInput {
                    agent: &repo,
                    action: "review",
                    target_type: "pr",
                    target_ref: &number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                });

                eprintln!(
                    "[legion] posted {} review on PR #{} on {}",
                    event, number, source_repo
                );
            }
            PrAction::Merge {
                repo,
                number,
                strategy,
                keep_branch,
                task,
            } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                worksource::merge_pr(&plugin_name, &source_repo, number, &strategy, !keep_branch)?;

                // Transition kanban card to done if linked
                if let Some(ref task_id) = task {
                    let db_base = data_dir()?;
                    let database = db::Database::open(&db_base.join("legion.db"))?;
                    match kanban::transition_card(
                        &database,
                        task_id,
                        kanban::Action::Done,
                        Some(&format!("PR #{} merged", number)),
                    ) {
                        Ok(_) => eprintln!("[legion] card {} marked done", task_id),
                        Err(e) => eprintln!(
                            "[legion] warning: could not complete card {}: {}",
                            task_id, e
                        ),
                    }

                    // Close the linked issue if the card has a source URL,
                    // using a generated merge comment so the GitHub issue
                    // records which PR merge closed it.
                    if let Some(card) = database.get_card_by_id(task_id)?
                        && let Some(ref url) = card.source_url
                        && let Some(issue_num) = worksource::extract_issue_number(url)
                        && let Some(ref source) = card.source_type
                    {
                        let merge_comment =
                            format!("Closed by PR #{number} merge via legion kanban propagation.");
                        if let Err(e) = worksource::close_issue(
                            source,
                            &source_repo,
                            issue_num,
                            Some(&merge_comment),
                        ) {
                            eprintln!(
                                "[legion] warning: could not close issue #{}: {}",
                                issue_num, e
                            );
                        } else {
                            eprintln!("[legion] closed issue #{}", issue_num);
                        }
                    }
                }

                let details = serde_json::json!({
                    "strategy": strategy, "delete_branch": !keep_branch,
                });
                let details_str = details.to_string();
                audit(&db::AuditInput {
                    agent: &repo,
                    action: "merge",
                    target_type: "pr",
                    target_ref: &number.to_string(),
                    task_id: task.as_deref(),
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                });

                println!("PR #{} merged on {}", number, source_repo);
            }
            PrAction::Close {
                repo,
                number,
                reason,
                delete_branch,
            } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                worksource::close_pr(
                    &plugin_name,
                    &source_repo,
                    number,
                    reason.as_deref(),
                    delete_branch,
                )?;

                let details = serde_json::json!({
                    "reason": reason, "delete_branch": delete_branch,
                });
                let details_str = details.to_string();
                audit(&db::AuditInput {
                    agent: &repo,
                    action: "close-pr",
                    target_type: "pr",
                    target_ref: &number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                });

                println!("closed PR #{} on {}", number, source_repo);
            }
        },
        Commands::Comment { repo, number, body } => {
            let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                .ok_or_else(|| {
                    error::LegionError::WorkSource(format!(
                        "no work source configured for repo '{}' in watch.toml",
                        repo
                    ))
                })?;

            worksource::comment(&plugin_name, &source_repo, number, &body)?;

            audit(&db::AuditInput {
                agent: &repo,
                action: "comment",
                target_type: "comment",
                target_ref: &number.to_string(),
                task_id: None,
                source_type: &plugin_name,
                details: None,
                outcome: "success",
            });

            eprintln!("[legion] commented on #{} on {}", number, source_repo);
        }
        Commands::Audit {
            repo,
            action,
            limit,
            json,
        } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let entries = database.query_audit_log(repo.as_deref(), action.as_deref(), limit)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else if entries.is_empty() {
                eprintln!("[legion] no audit entries found");
            } else {
                for entry in &entries {
                    let task = entry.task_id.as_deref().unwrap_or("-");
                    let ts = entry.timestamp.get(..19).unwrap_or(&entry.timestamp);
                    println!(
                        "{} {} {} {} #{} [{}] task:{}",
                        ts,
                        entry.agent,
                        entry.action,
                        entry.target_type,
                        entry.target_ref,
                        entry.outcome,
                        task
                    );
                }
            }
        }
        Commands::Watch { action } => {
            let base = data_dir()?;
            match action {
                None => {
                    eprintln!(
                        "[legion] `legion watch` is deprecated -- use `legion daemon` to run channel + watch in one process"
                    );
                    // #284: drain pending index jobs once at watch startup.
                    // Long-running poll integration is a follow-on; this
                    // covers the "queue on add, drain on next watch start"
                    // case which is the most common operator workflow.
                    let database = db::Database::open(&base.join("legion.db"))?;
                    let watch_path = base.join("watch.toml");
                    if let Err(e) = scip::run_pending_index_jobs(&database, &watch_path) {
                        eprintln!("[legion watch] index drain failed: {e}");
                    }
                    drop(database);
                    watch::run(&base)?;
                }
                Some(WatchAction::Add { path, name, agent }) => {
                    let config_path = base.join("watch.toml");
                    let effective_name = match name {
                        Some(n) => n,
                        None => {
                            let canonical = path.canonicalize().map_err(|e| {
                                error::LegionError::WatchConfig(format!(
                                    "path {} cannot be resolved: {}",
                                    path.display(),
                                    e
                                ))
                            })?;
                            canonical
                                .file_name()
                                .and_then(std::ffi::OsStr::to_str)
                                .map(str::to_string)
                                .ok_or_else(|| {
                                    error::LegionError::WatchConfig(format!(
                                        "cannot derive a repo name from path {} -- pass --name <name> explicitly",
                                        path.display()
                                    ))
                                })?
                        }
                    };
                    let added = watch::add_repo_to_config(
                        &config_path,
                        &effective_name,
                        &path,
                        agent.as_deref(),
                    )?;
                    if added {
                        let agent_note = agent
                            .as_deref()
                            .map(|a| format!(" (agent: {})", a))
                            .unwrap_or_default();
                        println!(
                            "added: {} -> {}{}",
                            effective_name,
                            path.display(),
                            agent_note
                        );

                        // #284: queue a background index job for the new repo
                        // unless one is already recorded. Detection is best
                        // effort -- empty languages means the daemon will
                        // re-detect when it picks up the queued row.
                        let database = db::Database::open(&base.join("legion.db"))?;
                        if database.get_index_job(&effective_name)?.is_none() {
                            let now = chrono::Utc::now().to_rfc3339();
                            let job = scip::IndexJob {
                                id: uuid::Uuid::now_v7().to_string(),
                                repo: effective_name.clone(),
                                status: scip::IndexJobStatus::Queued,
                                languages: Vec::new(),
                                progress_pct: 0,
                                eta_secs: None,
                                error: None,
                                created_at: now.clone(),
                                updated_at: now,
                            };
                            if let Err(e) = database.upsert_index_job(&job) {
                                eprintln!(
                                    "[legion watch] failed to queue index job for {effective_name}: {e}"
                                );
                            } else {
                                println!("queued background index job for {effective_name}");
                            }
                        }
                    } else {
                        println!("already present: {}", effective_name);
                    }
                }
                Some(WatchAction::Remove { name }) => {
                    let config_path = base.join("watch.toml");
                    let removed = watch::remove_repo_from_config(&config_path, &name)?;
                    if removed {
                        println!("removed: {}", name);
                    } else {
                        println!("not found: {}", name);
                    }
                }
                Some(WatchAction::List) => {
                    let config_path = base.join("watch.toml");
                    let repos = watch::list_repos_in_config(&config_path)?;
                    if repos.is_empty() {
                        println!("no repos in watch.toml");
                    } else {
                        for repo in &repos {
                            let agent_note = repo
                                .agent
                                .as_deref()
                                .map(|a| format!(" (agent: {})", a))
                                .unwrap_or_default();
                            println!("{}\t{}{}", repo.name, repo.workdir, agent_note);
                        }
                    }
                }
                Some(WatchAction::Leases { action }) => {
                    let base = data_dir()?;
                    let db_path = base.join("legion.db");
                    let db = db::Database::open(&db_path)?;
                    match action {
                        LeaseAction::List { persona } => {
                            let leases = db.list_persona_leases(persona.as_deref())?;
                            if leases.is_empty() {
                                println!("no active persona wake leases");
                            } else {
                                println!("PERSONA\tSIGNAL\tHOST\tACQUIRED\tEXPIRES");
                                for l in &leases {
                                    println!(
                                        "{}\t{}\t{}\t{}\t{}",
                                        l.persona_id,
                                        l.signal_id,
                                        l.acquired_by_host,
                                        l.acquired_at,
                                        l.expires_at,
                                    );
                                }
                            }
                        }
                        LeaseAction::Release { persona, signal } => {
                            let released = db.release_persona_lease(&persona, &signal)?;
                            if released {
                                println!("released lease {}/{}", persona, signal);
                            } else {
                                println!(
                                    "no live lease found for {}/{} (may already be released)",
                                    persona, signal
                                );
                            }
                        }
                    }
                }
            }
        }
        Commands::Mcp => {
            let base = data_dir()?;
            let version = env!("CARGO_PKG_VERSION").to_string();
            let (tx, _rx) = tokio::sync::broadcast::channel(16);
            mcp::run_stdio_loop(base, version, tx)?;
        }
        Commands::Daemon { port } => {
            let base = data_dir()?;
            daemon::run_daemon(daemon::DaemonConfig {
                data_dir: base,
                port,
                enable_mcp: false,
            })?;
        }
        Commands::DaemonSpawn { port } => {
            let base = data_dir()?;
            daemon::spawn_detached(&base, port)?;
        }
        Commands::QualityGate { action } => match action {
            QualityGateAction::Record {
                skill,
                result,
                findings_count,
                details_json,
            } => {
                let head_output = std::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .output()
                    .map_err(|e| {
                        error::LegionError::WorkSource(format!("failed to read git HEAD: {e}"))
                    })?;
                if !head_output.status.success() {
                    return Err(error::LegionError::WorkSource(
                        "git rev-parse HEAD failed -- is this a git repo?".to_owned(),
                    ));
                }
                let commit_hash: String = String::from_utf8_lossy(&head_output.stdout)
                    .trim()
                    .to_owned();

                let branch_output = std::process::Command::new("git")
                    .args(["rev-parse", "--abbrev-ref", "HEAD"])
                    .output()
                    .map_err(|e| {
                        error::LegionError::WorkSource(format!("failed to read git branch: {e}"))
                    })?;
                let branch: String = if branch_output.status.success() {
                    String::from_utf8_lossy(&branch_output.stdout)
                        .trim()
                        .to_owned()
                } else {
                    "unknown".to_owned()
                };

                let base = data_dir()?;
                let database = db::Database::open(&base.join("legion.db"))?;
                let row = database.record_quality_gate(
                    &branch,
                    &commit_hash,
                    &skill,
                    &result,
                    findings_count,
                    details_json.as_deref(),
                )?;
                println!("{}", row.id);
            }
        },
        Commands::Statusline { json } => {
            // Never surface an error to the Claude Code UI: statusline::run
            // already swallows internal failures and logs them, and its
            // signature reflects that contract (cannot return an error).
            statusline::run(json);
        }

        Commands::Mesh { action } => {
            let db_path = data_dir()?.join("legion.db");
            let database = db::Database::open(&db_path)?;
            let stale_cutoff = resolve_stale_cutoff();
            let now = chrono::Utc::now();
            let ranked = mesh::ranked_hosts_from_db(&database, now, stale_cutoff)?;

            match action {
                MeshAction::Headroom { json } => {
                    if json {
                        let rows: Vec<_> = ranked
                            .iter()
                            .map(|h| {
                                serde_json::json!({
                                    "hostname": h.hostname,
                                    "score": round_f64(h.score, 1),
                                    "fiveHourPct": h.five_hour_pct,
                                    "sevenDayPct": h.seven_day_pct,
                                    "lastEffectiveTokens": h.last_effective_tokens,
                                    "sampledAt": h.sampled_at,
                                    "ageSecs": h.age.map(|a| a.as_secs()),
                                    "stale": h.stale,
                                })
                            })
                            .collect();
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&rows)
                                .expect("ranked rows serialize infallibly")
                        );
                    } else if ranked.is_empty() {
                        eprintln!(
                            "[legion] no samples yet -- run `legion statusline` on at least one node first"
                        );
                    } else {
                        println!(
                            "host                 score    5h%    7d%   last_turn       age  status"
                        );
                        for h in &ranked {
                            let age = h.age.map(format_age).unwrap_or_else(|| "-".into());
                            let status = if h.stale { "stale" } else { "fresh" };
                            println!(
                                "{:<20} {:>6.1}  {:>5}  {:>5}  {:>10}  {:>8}  {}",
                                h.hostname,
                                h.score,
                                fmt_pct(h.five_hour_pct),
                                fmt_pct(h.seven_day_pct),
                                fmt_i64(h.last_effective_tokens),
                                age,
                                status,
                            );
                        }
                    }
                }
                MeshAction::Pick {
                    exclude,
                    json,
                    for_task,
                } => {
                    let excluded: std::collections::HashSet<String> = exclude
                        .as_deref()
                        .map(|s| s.split(',').map(|v| v.trim().to_string()).collect())
                        .unwrap_or_default();
                    let winner = ranked
                        .iter()
                        .find(|h| !h.stale && !excluded.contains(&h.hostname));
                    match winner {
                        Some(h) => {
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&serde_json::json!({
                                        "hostname": h.hostname,
                                        "score": round_f64(h.score, 1),
                                        "forTask": for_task,
                                    }))
                                    .expect("pick payload serializes infallibly")
                                );
                            } else {
                                println!("{}", h.hostname);
                            }
                        }
                        None => {
                            return Err(error::LegionError::Mesh(
                                "no fresh host available to pick".to_string(),
                            ));
                        }
                    }
                }
            }
        }

        Commands::Usage {
            session,
            since,
            today: _today,
            by_session,
            by_repo,
            json,
        } => {
            // LEGION_HOME overrides dirs::home_dir() for test isolation.
            // dirs 5.x on Windows uses SHGetKnownFolderPath and ignores HOME/USERPROFILE
            // env vars, so tests need an explicit override to point at a temp dir.
            let home: PathBuf = std::env::var_os("LEGION_HOME")
                .map(PathBuf::from)
                .or_else(dirs::home_dir)
                .ok_or(error::LegionError::NoHomeDir)?;

            // Determine the since filter.
            // --since takes an explicit date. --today and no-args both mean today.
            let since_str: Option<String> = if let Some(ref d) = since {
                // Validate the date looks like YYYY-MM-DD and convert to an
                // RFC3339-comparable prefix (timestamps sort lexicographically).
                if d.len() != 10 || !d.chars().all(|c| c.is_ascii_digit() || c == '-') {
                    eprintln!("[legion] error: --since expects YYYY-MM-DD, got '{d}'");
                    std::process::exit(1);
                }
                Some(format!("{d}T00:00:00"))
            } else if session.is_none() {
                // Default: today only.
                let today = chrono::Utc::now().format("%Y-%m-%dT00:00:00").to_string();
                Some(today)
            } else {
                // --session bypasses date filtering.
                None
            };

            let sessions =
                usage::discover_sessions(&home, since_str.as_deref(), session.as_deref());

            if session.is_some() && sessions.is_empty() {
                eprintln!(
                    "[legion] error: session not found: {}",
                    session.as_deref().unwrap_or("")
                );
                std::process::exit(1);
            }

            if json {
                if by_repo {
                    let groups = usage::group_by_repo(&sessions);
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&groups).map_err(error::LegionError::Json)?
                    );
                } else {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&sessions)
                            .map_err(error::LegionError::Json)?
                    );
                }
            } else if by_repo {
                let groups = usage::group_by_repo(&sessions);
                usage::print_repo_table(&groups);
            } else if by_session || since.is_some() || session.is_some() {
                usage::print_session_table(&sessions);
            } else {
                // Default (today): show by-session table.
                usage::print_session_table(&sessions);
            }
        }

        Commands::Health {
            history,
            all_hosts,
            json,
        } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            if let Some(duration_str) = history {
                // History mode: read from DB only
                let minutes: i64 = parse_duration_minutes(&duration_str)?;
                let since = (chrono::Utc::now() - chrono::Duration::minutes(minutes)).to_rfc3339();
                let hostname = sysinfo::System::host_name().unwrap_or_else(|| {
                    eprintln!("[legion] warning: could not determine hostname, using 'unknown'");
                    "unknown".to_string()
                });

                let samples = if all_hosts {
                    database.get_health_all_hosts(&since)?
                } else {
                    database.get_health_history(&hostname, &since)?
                };

                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&samples).map_err(error::LegionError::Json)?
                    );
                } else if samples.is_empty() {
                    eprintln!("[legion] no health samples found (is watch running?)");
                } else {
                    print_health_history(&samples);
                }
            } else if all_hosts {
                // All-hosts summary from DB
                let since = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
                let samples = database.get_health_all_hosts(&since)?;

                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&samples).map_err(error::LegionError::Json)?
                    );
                } else if samples.is_empty() {
                    eprintln!("[legion] no health samples found (is watch running?)");
                } else {
                    print_health_all_hosts(&samples);
                }
            } else {
                // Default: live sample + trend from DB
                let mut sampler = health::HealthSampler::new(6);
                std::thread::sleep(std::time::Duration::from_millis(250));
                sampler.sample();
                let sample = sampler.to_health_sample(0)?;

                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&sample).map_err(error::LegionError::Json)?
                    );
                } else {
                    print_health_live(&sample);

                    // Try to show trend from DB
                    let since = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
                    let history = database.get_health_history(sampler.hostname(), &since)?;
                    if !history.is_empty() {
                        print_health_trend(&history);
                    } else {
                        info!("\n  (no trend data -- start `legion watch` for history)");
                    }
                }
            }
        }
    }

    Ok(())
}

/// Parse a duration string like "1h", "30m", "24h" into minutes.
fn parse_duration_minutes(s: &str) -> error::Result<i64> {
    let s = s.trim();
    if let Some(hours) = s.strip_suffix('h') {
        let h: i64 = hours
            .parse()
            .map_err(|_| error::LegionError::Health(format!("invalid duration: {s}")))?;
        Ok(h * 60)
    } else if let Some(minutes) = s.strip_suffix('m') {
        let m: i64 = minutes
            .parse()
            .map_err(|_| error::LegionError::Health(format!("invalid duration: {s}")))?;
        Ok(m)
    } else {
        Err(error::LegionError::Health(format!(
            "invalid duration '{s}': use '1h' or '30m'"
        )))
    }
}

fn print_health_live(sample: &health::HealthSample) {
    println!(
        "[legion] health @ {} ({})\n",
        sample.hostname, sample.sampled_at
    );
    println!(
        "  CPU:     {:5.1}%  {}  ({} cores)",
        sample.cpu_usage_pct,
        health::render_gauge(sample.cpu_usage_pct, 20),
        sample.cpu_core_count
    );
    println!(
        "  Memory:  {:5.1}%  {}  ({} / {})",
        sample.mem_usage_pct,
        health::render_gauge(sample.mem_usage_pct, 20),
        health::format_bytes(sample.mem_used_bytes),
        health::format_bytes(sample.mem_total_bytes)
    );

    let swap_pct: f64 = sample.swap_pct();
    let swap_total_str: String = sample
        .swap_total_bytes
        .map_or_else(|| "N/A".to_string(), health::format_bytes);
    let swap_used_str: String = sample
        .swap_used_bytes
        .map_or_else(|| "0".to_string(), health::format_bytes);
    println!(
        "  Swap:    {:5.1}%  {}  ({} / {})",
        swap_pct,
        health::render_gauge(swap_pct, 20),
        swap_used_str,
        swap_total_str
    );

    if let Some(temp) = sample.cpu_temp_celsius {
        println!(
            "  Temp:    {:5.1}C  {}",
            temp,
            health::render_gauge(temp, 20)
        );
    }

    if let (Some(l1), Some(l5), Some(l15)) =
        (sample.load_avg_1, sample.load_avg_5, sample.load_avg_15)
    {
        println!("  Load:    {:.2} / {:.2} / {:.2}", l1, l5, l15);
    }

    let status: &str = if sample.pressure < 60.0 {
        "OK"
    } else if sample.pressure < 80.0 {
        "ELEVATED"
    } else {
        "HIGH"
    };
    println!(
        "\n  Pressure: {:.1}%  -- {} (threshold: 80%)",
        sample.pressure, status
    );
    println!("  Agents:   {} active", sample.agents_active);
}

fn print_health_trend(samples: &[health::HealthSample]) {
    if samples.is_empty() {
        return;
    }
    println!("\n  Trend ({} samples):", samples.len());

    let pressures: Vec<String> = samples
        .iter()
        .rev()
        .map(|s| format!("{:.0}", s.pressure))
        .collect();
    let avg_p: f64 = samples.iter().map(|s| s.pressure).sum::<f64>() / samples.len() as f64;
    println!("    pressure  {}  avg: {:.1}", pressures.join(" "), avg_p);

    let cpus: Vec<String> = samples
        .iter()
        .rev()
        .map(|s| format!("{:.0}", s.cpu_usage_pct))
        .collect();
    let avg_c: f64 = samples.iter().map(|s| s.cpu_usage_pct).sum::<f64>() / samples.len() as f64;
    println!("    cpu       {}  avg: {:.1}", cpus.join(" "), avg_c);

    let mems: Vec<String> = samples
        .iter()
        .rev()
        .map(|s| format!("{:.0}", s.mem_usage_pct))
        .collect();
    let avg_m: f64 = samples.iter().map(|s| s.mem_usage_pct).sum::<f64>() / samples.len() as f64;
    println!("    memory    {}  avg: {:.1}", mems.join(" "), avg_m);
}

fn print_health_history(samples: &[health::HealthSample]) {
    println!("[legion] health history ({} samples)\n", samples.len());
    println!(
        "  {:<20} {:>6} {:>6} {:>6} {:>7} {:>9} {:>7}",
        "Time", "CPU", "Mem", "Swap", "Temp", "Pressure", "Agents"
    );
    for s in samples {
        let time: &str = s
            .sampled_at
            .split_once('T')
            .map_or(s.sampled_at.as_str(), |(_, t)| {
                t.split_once('.').map_or(t, |(hms, _)| hms)
            });
        let swap_pct: f64 = s.swap_pct();
        let temp_str: String = s
            .cpu_temp_celsius
            .map_or_else(|| "--".to_string(), |t| format!("{:.1}C", t));
        println!(
            "  {:<20} {:5.1}% {:5.1}% {:5.1}% {:>7} {:8.1}% {:>7}",
            time, s.cpu_usage_pct, s.mem_usage_pct, swap_pct, temp_str, s.pressure, s.agents_active
        );
    }
}

fn print_health_all_hosts(samples: &[health::HealthSample]) {
    use std::collections::HashMap;

    println!("[legion] health (all hosts)\n");

    // Group by hostname, keep latest per host
    let mut latest: HashMap<&str, &health::HealthSample> = HashMap::new();
    for s in samples {
        latest
            .entry(s.hostname.as_str())
            .and_modify(|existing| {
                if s.sampled_at > existing.sampled_at {
                    *existing = s;
                }
            })
            .or_insert(s);
    }

    let mut hosts: Vec<&&health::HealthSample> = latest.values().collect();
    hosts.sort_by(|a, b| a.hostname.cmp(&b.hostname));

    for s in hosts {
        let age: String = match chrono::DateTime::parse_from_rfc3339(&s.sampled_at) {
            Ok(dt) => {
                let secs: i64 = (chrono::Utc::now() - dt.with_timezone(&chrono::Utc)).num_seconds();
                if secs < 60 {
                    format!("{}s ago", secs)
                } else {
                    format!("{}m ago", secs / 60)
                }
            }
            Err(_) => "?".to_string(),
        };
        println!(
            "  {:<20} CPU: {:5.1}%  Mem: {:5.1}%  Pressure: {:5.1}%  Agents: {}  ({})",
            s.hostname, s.cpu_usage_pct, s.mem_usage_pct, s.pressure, s.agents_active, age
        );
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    fn seed_legacy_db(legacy: &Path) {
        std::fs::create_dir_all(legacy).expect("create legacy dir");
        let conn = rusqlite::Connection::open(legacy.join("legion.db")).expect("open legacy db");
        conn.execute_batch(
            "CREATE TABLE sentinel (value TEXT NOT NULL); \
             INSERT INTO sentinel VALUES ('migrated');",
        )
        .expect("seed legacy db");
    }

    #[test]
    fn migrate_between_skips_when_legacy_has_no_db() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("target");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        migrate_between(&legacy, &target);
        assert!(
            !target.join("legion.db").exists(),
            "target should stay empty when legacy has no db"
        );
    }

    #[test]
    fn migrate_between_skips_when_target_already_has_db() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("target");
        seed_legacy_db(&legacy);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("legion.db"), b"existing").unwrap();

        migrate_between(&legacy, &target);
        // Target DB should remain the placeholder bytes, not the legacy content.
        let content = std::fs::read(target.join("legion.db")).unwrap();
        assert_eq!(content, b"existing", "migration should not overwrite");
    }

    #[test]
    fn migrate_between_copies_db_and_index() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("target");
        seed_legacy_db(&legacy);

        // Seed a fake Tantivy-style index tree with nested files.
        std::fs::create_dir_all(legacy.join("index/segment_0")).unwrap();
        std::fs::write(legacy.join("index/meta.json"), b"{\"version\": 1}").unwrap();
        std::fs::write(
            legacy.join("index/segment_0/posting.bin"),
            b"\x00\x01\x02\x03",
        )
        .unwrap();

        // Seed watch.toml
        std::fs::write(legacy.join("watch.toml"), b"[[repos]]\nname = \"legion\"\n").unwrap();

        std::fs::create_dir_all(&target).unwrap();
        migrate_between(&legacy, &target);

        assert!(target.join("legion.db").exists(), "db not migrated");
        assert!(
            target.join("index/meta.json").exists(),
            "index meta not migrated"
        );
        assert!(
            target.join("index/segment_0/posting.bin").exists(),
            "nested index file not migrated"
        );
        assert!(
            target.join("watch.toml").exists(),
            "watch.toml not migrated"
        );

        // Legacy must remain in place as a safety net.
        assert!(
            legacy.join("legion.db").exists(),
            "legacy db removed (should be kept)"
        );

        // Migrated DB should be readable and contain the sentinel row.
        let conn = rusqlite::Connection::open(target.join("legion.db")).unwrap();
        let value: String = conn
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "migrated");
    }

    #[test]
    fn migrate_between_is_idempotent_on_rerun() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("target");
        seed_legacy_db(&legacy);
        std::fs::create_dir_all(&target).unwrap();

        migrate_between(&legacy, &target);
        // Second call should be a no-op because target now has legion.db.
        let first_content = std::fs::read(target.join("legion.db")).unwrap();
        migrate_between(&legacy, &target);
        let second_content = std::fs::read(target.join("legion.db")).unwrap();
        assert_eq!(first_content, second_content);
    }

    #[test]
    fn copy_dir_recursive_handles_nested_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(src.join("a/b/c")).unwrap();
        std::fs::write(src.join("top.txt"), b"top").unwrap();
        std::fs::write(src.join("a/mid.txt"), b"mid").unwrap();
        std::fs::write(src.join("a/b/leaf.bin"), b"\xde\xad\xbe\xef").unwrap();

        copy_dir_recursive(&src, &dst).expect("copy");

        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), b"top");
        assert_eq!(std::fs::read(dst.join("a/mid.txt")).unwrap(), b"mid");
        assert_eq!(
            std::fs::read(dst.join("a/b/leaf.bin")).unwrap(),
            b"\xde\xad\xbe\xef"
        );
        assert!(dst.join("a/b/c").is_dir(), "empty nested dir not created");
    }

    #[test]
    fn checkpoint_sqlite_drains_wal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("legion.db");
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode = WAL; \
                 CREATE TABLE t (x INTEGER); \
                 INSERT INTO t VALUES (1), (2), (3);",
            )
            .unwrap();
        }
        // WAL may be non-empty at this point.
        checkpoint_sqlite(&db_path).expect("checkpoint should succeed");
        // Data must still be readable after the checkpoint.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }
}
