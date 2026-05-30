mod board;
mod card_parse;
mod channel;
mod cluster;
mod daemon;
mod db;
mod documents;
mod embed;
mod error;
mod health;
mod init;
#[allow(dead_code)] // Items used by tests + pending surface/status/serve migration
mod kanban;
mod mcp;
mod mesh;
mod now;
mod pr_view;
mod pr_write;
mod pty;
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
mod telemetry;
#[cfg(test)]
mod testutil;
mod uncertainty;
mod usage;
mod wake_attempts;
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

        /// Search ONLY archived reflections (the deep-dive). Default mode
        /// is hot-only; this flag inverts. Mutually exclusive with
        /// --include-archives. v1 only affects the BM25/hybrid path;
        /// --latest, --domain, --cosine-only stay hot-mode regardless.
        /// See #457.
        #[arg(long)]
        archives: bool,

        /// Search BOTH hot and archived reflections. Mutually exclusive
        /// with --archives. Same v1 scope caveat as --archives.
        #[arg(long, conflicts_with = "archives")]
        include_archives: bool,
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

    /// Search across legion's cross-agent surfaces.
    ///
    /// Two query modes (mutually exclusive):
    /// - `--context "<query>"` searches reflections across every repo.
    /// - `--symbol <name>` searches SCIP indexes (#285) across every
    ///   repo and reports `(repo, lang, def_location, refs_count)` for
    ///   each match. Used by recall-first.sh's cheap chain to answer
    ///   code-intelligence questions without spawning Explore.
    Consult {
        /// Context describing the problem to search for (reflection mode).
        /// Mutually exclusive with --symbol.
        #[arg(long, conflicts_with = "symbol")]
        context: Option<String>,

        /// Symbol name to look up across every indexed repo (SCIP mode).
        /// Mutually exclusive with --context.
        #[arg(long, conflicts_with = "context")]
        symbol: Option<String>,

        /// Maximum number of reflections to return (reflection mode only).
        #[arg(long, default_value = "3")]
        limit: usize,

        /// Emit JSON for the symbol mode output (reflection mode is
        /// always human-formatted today).
        #[arg(long)]
        json: bool,
    },

    /// Configure Claude Code hooks for legion
    Init {
        /// Skip confirmation prompts
        #[arg(long)]
        force: bool,
    },

    /// Post a broadcast to the shared bullpen.
    ///
    /// Posts have no recipient and never wake an asleep agent. Use
    /// `legion signal --to <agent> --verb <wake-worthy-verb>` for directed
    /// asks that need a reply (see `legion signal --help` for the verb set).
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

    /// Mark a bullpen post or signal as resolved (#362).
    ///
    /// Stops a converged thread from re-surfacing in the bullpen, channel
    /// notifications, and the wake-loop signal feed. Optional `--reflection`
    /// links the converged decision so future recall surfaces it together
    /// with the resolved post.
    Resolve {
        /// Bullpen post or signal ID to mark resolved
        #[arg(long)]
        id: String,

        /// Reflection ID holding the team's converged decision
        #[arg(long)]
        reflection: Option<String>,
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

    /// Send a directed message to another agent.
    ///
    /// Watch wakes the recipient when `--verb` is in the wake-worthy set
    /// (`question`, `request`, `help`, `blocker`). Other verbs (`announce`,
    /// `ack`, `info`, `answer`, bare `review`) deliver to live sessions via
    /// the channel push but do not wake an asleep recipient.
    ///
    /// The minimum form is `--to <agent> --verb <verb>`; everything else is
    /// optional structure for RFC-shaped asks. For a one-line ask, just
    /// `--verb question --note "..."` is enough.
    Signal {
        /// Repository name (identifies the sender)
        #[arg(long)]
        repo: Vec<String>,

        /// Recipient agent name (or "all")
        #[arg(long)]
        to: String,

        /// Signal verb. Wake-worthy: question, request, help, blocker
        /// (these spawn an asleep recipient). Informational: announce, ack,
        /// info, answer, review (deliver to live sessions only).
        #[arg(long)]
        verb: String,

        /// Optional status decoration (e.g., approved, blocked, ready). Does
        /// NOT affect wake routing -- only `--verb` does. Useful for RFC-
        /// shaped asks that downstream tools may parse.
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

    /// Print a wake-prompt for pending wake-worthy signals.
    ///
    /// Used by SessionStart hooks to surface directed signals carrying a
    /// wake-worthy verb (`question`, `request`, `help`, `blocker`) with
    /// strong "REQUIRES A REPLY" framing that survives the additionalContext
    /// system-reminder wrapper. Prints nothing (and exits 0) if there are no
    /// pending wake-worthy signals targeting this repo.
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

        /// Include past-TTL non-evergreen posts (operator review only -- agents
        /// must not pass this flag, see #376).
        #[arg(long)]
        include_stale: bool,

        /// Include resolved threads (operator review only -- agents must not
        /// pass this flag, see #362).
        #[arg(long)]
        include_resolved: bool,
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

    /// Print the operating contract for a repo -- how I operate, distinct from
    /// whoami (who I am). Alias for recall --domain workflow.
    Whatami {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Maximum number of operating-contract reflections to return
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },

    /// Build or refresh the SCIP code-intelligence index for a repo (#278).
    ///
    /// Resolves the repo path from `watch.toml`, detects every recognized
    /// language, runs the corresponding SCIP indexer per language as a
    /// subprocess, and stores each protobuf blob keyed on (repo, lang).
    /// Idempotent on content hash.
    ///
    /// With `--file <path>`, resolves the owning repo by walking parents
    /// against `watch.toml` and re-indexes that repo. Used by the
    /// PostToolUse re-index hook (#281) so edits keep the index fresh.
    /// `--repo` is optional in `--file` mode.
    Index {
        /// Repo name (must be present in watch.toml). Optional when --file,
        /// --status, or --logs is supplied.
        repo: Option<String>,
        /// Re-index the repo containing this file path. Resolves the repo
        /// by ancestor match against watch.toml. The whole repo is still
        /// re-indexed; per-file granularity is a future refinement.
        #[arg(long, conflicts_with_all = ["status", "logs"])]
        file: Option<PathBuf>,
        /// Show the current SCIP index inventory: per-row `(repo, lang,
        /// blob_size, updated_at)` for everything in `scip_indexes`.
        /// Used by operators to confirm a background indexer (#284)
        /// finished after `legion watch add`.
        #[arg(long, conflicts_with_all = ["file", "logs"])]
        status: bool,
        /// Print recent background-indexer log lines per repo. Reads the
        /// XDG_STATE_HOME-rooted log directory (default
        /// `~/.local/state/legion/index-logs/`). Use `--repo` to filter to
        /// one repo, `--lines` to override the default 50-line tail, and
        /// `--follow` to tail-follow new output.
        #[arg(long, conflicts_with_all = ["file", "status"])]
        logs: bool,
        /// Tail-follow the log files when used with `--logs`. Polls every
        /// 250ms; SIGINT (Ctrl-C) terminates.
        #[arg(long, requires = "logs")]
        follow: bool,
        /// Number of trailing lines to print per log file when used with
        /// `--logs` (default 50).
        #[arg(long, requires = "logs", default_value_t = 50)]
        lines: usize,
        /// Render a SessionStart-friendly status banner for one repo.
        /// Silent on healthy (every detected language has a fresh index),
        /// loud on stale or missing. Requires `<repo>` and `--status`.
        /// Used by `plugin/hooks/session-start.sh` so agents see whether
        /// `legion sym` will succeed before they try.
        #[arg(long, requires = "status")]
        banner: bool,
        /// Emit `--status` output as a JSON array of {repo, lang, size_bytes,
        /// updated_at} objects. Hooks (#437/#438/#439) consume this to probe
        /// whether a repo is indexed before applying enforcement.
        #[arg(long, requires = "status", conflicts_with = "banner")]
        json: bool,
    },

    /// Query SCIP symbol indexes (def, refs, impl, hover).
    ///
    /// Reads stored protobuf blobs from `scip_indexes` and answers symbol
    /// lookups in-process. Run `legion index <repo>` first to populate.
    Sym {
        #[command(subcommand)]
        action: SymAction,
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

    /// Coordination substrate documents (#455/#456): specs, NFRs,
    /// blueprints, personas, journeys, etc. Type-agnostic at the storage
    /// layer; payload is a validated JSON blob.
    Document {
        #[command(subcommand)]
        action: DocumentAction,
    },

    /// Sub-issue verbs over the work source plugin (#462). Operator-
    /// facing surface for creating a child issue linked to a parent
    /// via GitHub's native sub-issue relationship, and listing the
    /// children of a parent.
    SubIssue {
        #[command(subcommand)]
        action: SubIssueAction,
    },

    /// Probe a fresh MCP subprocess for notifier health (#391). Spawns
    /// `legion mcp` over stdio, sends `initialize` + `legion/notifier_health`
    /// JSON-RPC requests, prints the health JSON. This spins up a NEW MCP
    /// -- it does NOT probe the MCP attached to a running Claude Code
    /// session (that one is owned by the parent CC process and is not
    /// externally addressable). Use as a contract smoke test.
    McpHealth {
        /// Wait this many seconds for the notifier to tick at least once
        /// before sampling its health. Default 3 (>= one full poll interval
        /// at the default 1500ms tick rate).
        #[arg(long, default_value_t = 3)]
        wait: u64,
    },

    /// Print the current local time, weekday, and sunphase (#410).
    ///
    /// Used by SessionStart to inject a one-line framing block so agents
    /// stop pattern-matching "tonight" / "wind down" when the operator
    /// has the rest of the workday ahead. Day-of-week is included
    /// because date alone does not tell an agent it's Sunday vs Monday.
    Now {
        /// Emit the SessionStart-friendly one-line banner. Default
        /// without --json or --banner is the human-readable banner;
        /// --json emits a structured snapshot for other consumers.
        #[arg(long)]
        banner: bool,
        /// Emit a JSON snapshot {date, weekday, time, timezone, sunphase}.
        #[arg(long, conflicts_with = "banner")]
        json: bool,
    },

    /// Bypass telemetry: log when an agent escapes a grep/Read enforcement
    /// hook (#438/#439), and read the log back. Append-only JSONL at
    /// `${XDG_STATE_HOME:-~/.local/state}/legion/bypass.jsonl`. Feeds the
    /// uncertainty engine (#354) -- bypass volume on a (repo, pattern) is a
    /// signal that sym/recall is missing an answer agents expected.
    Telemetry {
        #[command(subcommand)]
        action: TelemetryAction,
    },

    /// Pillar 2: emit / witness predictions, read calibration + orphan metrics
    Uncertainty {
        #[command(subcommand)]
        action: UncertaintyAction,
    },

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

    /// Show or tail the most recent MCP process log file (#395). Each MCP
    /// subprocess redirects its stderr to a per-PID file under
    /// `~/Library/Logs/legion/mcp/<pid>.log` (macOS) or the XDG state dir
    /// (Linux). Use this to see notifier seed errors, per-poll cursor state,
    /// per-post deliver decisions, and write failures that would otherwise
    /// be swallowed by Claude Code's MCP transport.
    McpLogs {
        /// Specific PID to tail (default: most recently modified .log file).
        #[arg(long)]
        pid: Option<u32>,
        /// Follow the file (like `tail -f`). Default prints existing
        /// contents and exits.
        #[arg(long)]
        tail: bool,
    },

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

    /// Stop the running background daemon (SIGTERM, then SIGKILL if it does not exit)
    DaemonStop,

    /// Restart the background daemon: stop the running one, then spawn a fresh one
    DaemonRestart {
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
enum DocumentAction {
    /// Insert a new document. Payload is read from --from (file path)
    /// or stdin when --from is omitted. Meta fields are passed
    /// explicitly to keep the storage layer type-agnostic; the schema
    /// registry (sibling child of #455) will validate payloads against
    /// the meta.type's schema once it ships.
    Create {
        /// Document type (e.g. "requirement", "nfr", "persona").
        #[arg(long, value_name = "TYPE")]
        doc_type: String,
        /// Owner agent id.
        #[arg(long)]
        owner: String,
        /// Optional explicit id. Defaults to a UUIDv7 when omitted; for
        /// canonical typed ids like FR-EMAIL-003, pass them here.
        #[arg(long)]
        id: Option<String>,
        /// Functional surface (omit for cross-cutting / type=blueprint).
        #[arg(long)]
        surface: Option<String>,
        /// Initial lifecycle status (default: "draft").
        #[arg(long)]
        status: Option<String>,
        /// RFC 2119 conformance level (requirement only): SHALL|SHOULD|MAY.
        #[arg(long)]
        priority: Option<String>,
        /// Read payload from this file. When omitted, reads from stdin.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// View a document by id.
    View {
        id: String,
        /// Emit full row as JSON (default: human-friendly summary).
        #[arg(long)]
        json: bool,
    },
    /// List documents, optionally filtered.
    List {
        #[arg(long, value_name = "TYPE")]
        doc_type: Option<String>,
        #[arg(long)]
        surface: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        owner: Option<String>,
        /// Include archived documents (default: hot only).
        #[arg(long)]
        archived: bool,
        /// Emit full result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Mark a document archived.
    Archive { id: String },
}

#[derive(Subcommand)]
enum SubIssueAction {
    /// Create a child issue linked to a parent via GitHub's native
    /// sub-issue relationship (#462). The plugin looks up the parent
    /// node id first, errors if the parent does not exist, then
    /// creates the child and links via the addSubIssue mutation.
    Create {
        /// Repo containing the parent issue (used to resolve the work
        /// source plugin).
        #[arg(long)]
        repo: String,
        /// Parent issue number.
        #[arg(long)]
        parent: u64,
        /// Title of the new child issue.
        #[arg(long)]
        title: String,
        /// Optional body for the child issue. Reads from stdin when
        /// --body is omitted AND stdin is not a TTY.
        #[arg(long)]
        body: Option<String>,
    },
    /// List sub-issues of a parent (#462).
    List {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        parent: u64,
        /// State filter: open (default) | closed | all.
        #[arg(long, default_value = "open")]
        state: String,
        /// Emit as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SymAction {
    /// Print definitions of a symbol
    Def {
        /// Symbol name to look up (substring-matched against SCIP symbol strings)
        name: String,
        /// Restrict to a single repo (default: every repo with a stored index)
        #[arg(long)]
        repo: Option<String>,
        /// Restrict to a single language (default: every language with a stored index)
        #[arg(long)]
        lang: Option<String>,
        /// Emit results as a JSON array of SymbolLocation objects
        #[arg(long)]
        json: bool,
    },

    /// Print references / call sites of a symbol
    Refs {
        name: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Print types implementing a trait or interface
    Impl {
        /// Trait or interface name
        trait_name: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Print signature + docstring for a symbol
    Hover {
        name: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Impact radius: for every symbol whose definition the given diff
    /// touches, report the SCIP reference count across the repo's index.
    /// Used by smugglr to flag wide-blast-radius PRs at review time.
    Impact {
        /// Repo whose SCIP index to query
        #[arg(long)]
        repo: String,
        /// Path to a unified diff file, or "-" to read from stdin
        #[arg(long)]
        diff: String,
        /// Emit results as JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TelemetryAction {
    /// Append one bypass row to `bypass.jsonl`. Called by the grep/Read
    /// enforcement hooks (#438/#439) when an agent escapes via env var or
    /// `# legion-bypass:` sentinel in a Bash command. Errors are surfaced;
    /// the hook layer decides whether to swallow them (it does -- telemetry
    /// must never break the agent).
    RecordBypass {
        /// Repo the bypass occurred in
        #[arg(long)]
        repo: String,
        /// Claude Code session id (passed through from the hook input)
        #[arg(long)]
        session_id: String,
        /// Tool name that was bypassed (`Bash`, `Grep`, `Glob`, `Read`)
        #[arg(long)]
        tool: String,
        /// Pattern or path the bypass applied to
        #[arg(long)]
        pattern: String,
        /// Free-form reason captured from the bypass mechanism (env value,
        /// `# legion-bypass: <reason>` substring, etc).
        #[arg(long)]
        bypass_reason: String,
        /// Whether `legion sym def/refs` had hits the agent ignored
        #[arg(long)]
        had_sym_hits: bool,
        /// Whether `legion recall` had hits the agent ignored
        #[arg(long)]
        had_recall_hits: bool,
        /// Optional agent identifier (default: empty). Hooks resolve this
        /// once per session via `legion whoami` and pass it through.
        #[arg(long, default_value = "")]
        agent: String,
    },

    /// Read the bypass log. Filters by `--since` (duration like `24h`,
    /// `7d`) and `--repo`. Used by `legion telemetry summary` (#440) and by
    /// downstream consumers (uncertainty engine #354).
    ListBypasses {
        /// Drop rows older than this duration. Format: `30s`, `15m`, `24h`, `7d`.
        #[arg(long)]
        since: Option<String>,
        /// Restrict to a single repo
        #[arg(long)]
        repo: Option<String>,
    },

    /// Summarize bypass volume by `(tool, repo, pattern)`. Top under-served
    /// query shapes surface first. Feeds the dashboard surface in #440 and
    /// the uncertainty engine in #354.
    Summary {
        /// Drop rows older than this duration before summarizing.
        #[arg(long)]
        since: Option<String>,
        /// Restrict to a single repo
        #[arg(long)]
        repo: Option<String>,
        /// Top N rows by count (default 20, 0 means all).
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum UncertaintyAction {
    /// Record a fresh prediction. Mirrors platform's
    /// POST /api/uncertainty/predictions. Returns JSON `{id, orphan_after}`
    /// on stdout. Non-blocking: failures log to stderr and exit 0 so a
    /// downstream emit hook can never break the agent.
    Emit {
        /// Logical surface for the prediction (e.g. `legion.task`,
        /// `legion.review`, `legion.scip-query`).
        #[arg(long)]
        surface: String,
        /// Feature key from SCIP / task classifier
        /// (e.g. `scip.high-connectivity-refactor`, `schema.new-table`).
        #[arg(long)]
        feature_key: String,
        /// Stable hash of `(features + task description)`. Lets the
        /// witness path look up the matching prediction without an id.
        #[arg(long)]
        input_fingerprint: String,
        /// Model that produced the prediction (e.g. `claude-opus-4-7`).
        #[arg(long)]
        model: String,
        /// Model version string. Free-form; calibration cohorts include it
        /// to detect regressions across releases.
        #[arg(long)]
        model_version: String,
        /// Claimed probability of shipping without iteration. [0.0, 1.0].
        #[arg(long)]
        claimed_confidence: f64,
        /// Prediction payload (JSON). Free-form per-surface schema, e.g.
        /// `{"predicted_tokens": 5000000, "predicted_wallclock_seconds": 7200}`.
        #[arg(long)]
        payload: String,
        /// Days until the prediction transitions to orphan if not witnessed.
        /// Default 30. Setting 0 disables the orphan sweep for this row.
        #[arg(long, default_value_t = 30)]
        orphan_ttl_days: u32,
    },

    /// Record an outcome for a prediction. Mirrors platform's
    /// PUT /api/uncertainty/predictions/:id/witness. Idempotent failure:
    /// re-witnessing an already-witnessed prediction is an error.
    Witness {
        /// Prediction id to witness against.
        prediction_id: String,
        /// Outcome category: `shipped`, `scoped-down`, `escalated`, `abandoned`.
        #[arg(long)]
        outcome_label: String,
        /// Normalized correctness in [0.0, 1.0]. 1.0 = predicted exactly,
        /// 0.0 = totally wrong. Calibration math uses this.
        #[arg(long)]
        outcome_correctness: f64,
        /// Optional outcome payload (JSON) -- typically
        /// `{"actual_tokens": ..., "actual_wallclock_seconds": ...}`.
        #[arg(long)]
        payload: Option<String>,
    },

    /// Read calibration snapshots for a cohort. Output is one row per
    /// reliability bucket with claimed vs actual, counts, Brier score.
    Calibration {
        /// Filter to one surface.
        #[arg(long)]
        surface: Option<String>,
        /// Filter to one model.
        #[arg(long)]
        model: Option<String>,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },

    /// Count predictions currently in the orphan state. Surface-grouped.
    /// For the dashboard + nightly Slack digest in #360.
    Orphans {
        /// Filter to one surface.
        #[arg(long)]
        surface: Option<String>,
        /// Emit JSON instead of human-readable text.
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

        /// Show all cards, including Backlog and terminal (Done/Cancelled)
        #[arg(long, conflicts_with = "backlog")]
        all: bool,

        /// Show only the raw Backlog (the unconsented inbox)
        #[arg(long)]
        backlog: bool,
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

    /// Reconcile kanban cards with their linked GitHub issue state.
    ///
    /// Detects two drift directions:
    ///
    ///   1. **stale-open**: local state is `done` or `cancelled` but the
    ///      linked GitHub issue is still `OPEN`. Pass `--close-stale` to
    ///      close those issues via the work source plugin.
    ///   2. **shipped-pending**: local state is `pending` (or any other
    ///      active state) but the linked GitHub issue is `CLOSED` or
    ///      `MERGED`. Pass `--cancel-shipped` to cancel those cards
    ///      locally without touching GitHub (the issue is already closed).
    ///
    /// `--apply` is shorthand for both action flags.
    ///
    /// Default mode is read-only: the command scans and reports without
    /// changing any state. Per-card failures are logged and counted but
    /// do not abort the run.
    Reconcile {
        /// Optional repo filter -- only reconcile cards owned by this
        /// agent. Default is all cards across all repos.
        #[arg(long)]
        repo: Option<String>,

        /// Actually close the stale GitHub issues (direction 1).
        /// Without any action flag, the command is read-only and safe
        /// to run repeatedly.
        #[arg(long)]
        close_stale: bool,

        /// Actually cancel the shipped-pending cards locally with
        /// `--no-propagate` semantics (direction 2). The linked GitHub
        /// issue is already closed, so propagating again is unnecessary.
        #[arg(long)]
        cancel_shipped: bool,

        /// Convenience: apply both `--close-stale` and `--cancel-shipped`.
        #[arg(long, conflicts_with_all = ["close_stale", "cancel_shipped"])]
        apply: bool,
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

    /// Validate a drafted PR body against an issue's acceptance criteria
    /// (#519 PR-write forcing function). Reads the body from --body-file (or
    /// stdin), loads the issue's acceptance criteria, and refuses an empty or
    /// boilerplate criterion-to-work mapping. On success it records a clean
    /// `legion-pr-write` quality gate for HEAD so `legion pr create` can gate
    /// on it; on failure it records issues and exits non-zero with the gaps.
    WriteCheck {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Issue number whose acceptance criteria the body must map.
        #[arg(long)]
        issue: u64,

        /// Path to the drafted PR body (markdown). Reads stdin when omitted.
        #[arg(long)]
        body_file: Option<String>,
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

    /// Optimistic stop-hook handoff for the watch reaper (#493). Writes
    /// `exit_observed_at` on the wake_attempts row so the reaper can
    /// skip a poll cycle. PTY EOF + PID-poll remain authoritative; this
    /// is a speed-up only. Idempotent; exits 0 on a missing row so a
    /// hook failure cannot block Claude Code's Stop.
    SessionEnd {
        /// UUIDv7 attempt id, sourced from $LEGION_WAKE_ATTEMPT_ID.
        #[arg(long)]
        attempt_id: String,
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
/// Logs a warning via `info!` on failure so degraded hybrid search is visible
/// in `--verbose` mode without spamming default-quiet runs (which otherwise
/// fail integration tests asserting `stderr.is_empty()` whenever the model
/// fetch hits a transient network error like HuggingFace 429).
fn try_load_embed_model() -> Option<embed::EmbedModel> {
    match embed::EmbedModel::load() {
        Ok(model) => Some(model),
        Err(e) => {
            info!("[legion] embedding model unavailable, falling back to BM25: {e}");
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

/// Read the current HEAD commit hash and branch name. Errors if `git
/// rev-parse HEAD` fails (not a git repo / no commits); the branch falls back
/// to "unknown" when its lookup fails, since a detached HEAD still has a valid
/// commit to key a quality gate on. Shared by the quality-gate recorder and
/// the `pr create` / `pr write-check` gate checks.
fn git_head_commit_and_branch() -> Result<(String, String), error::LegionError> {
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to read git HEAD: {e}")))?;
    if !head.status.success() {
        return Err(error::LegionError::WorkSource(
            "git rev-parse HEAD failed -- is this a git repo?".to_owned(),
        ));
    }
    let commit_hash = String::from_utf8_lossy(&head.stdout).trim().to_owned();

    let branch_out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to read git branch: {e}")))?;
    let branch = if branch_out.status.success() {
        String::from_utf8_lossy(&branch_out.stdout)
            .trim()
            .to_owned()
    } else {
        "unknown".to_owned()
    };
    Ok((commit_hash, branch))
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

/// Dispatch `legion sym <action>` against the local index store.
///
/// Loads every matching `scip_indexes` row (filtered by optional repo and
/// language), runs the per-blob query, and prints results to stdout.
/// Exits with code 1 and a stderr message when no index rows match the
/// filter, distinguishing "no data" from "empty result."
fn run_sym_action(database: &db::Database, action: SymAction) -> error::Result<()> {
    match action {
        SymAction::Def {
            name,
            repo,
            lang,
            json,
        } => run_location_query(database, repo, lang, json, false, |idx| {
            sym::query_definitions(&idx.blob, &name, &idx.repo, &idx.lang)
        }),
        SymAction::Refs {
            name,
            repo,
            lang,
            json,
        } => run_location_query(database, repo, lang, json, false, |idx| {
            sym::query_references(&idx.blob, &name, &idx.repo, &idx.lang)
        }),
        SymAction::Impl {
            trait_name,
            repo,
            lang,
            json,
        } => run_location_query(database, repo, lang, json, true, |idx| {
            sym::query_implementors(&idx.blob, &trait_name, &idx.repo, &idx.lang)
        }),
        SymAction::Hover {
            name,
            repo,
            lang,
            json,
        } => run_hover_query(database, &name, repo, lang, json),
        SymAction::Impact { repo, diff, json } => run_sym_impact(database, &repo, &diff, json),
    }
}

/// Read the diff source -- file path or "-" for stdin -- and run impact
/// radius analysis against the repo's SCIP index. Prints sorted output
/// (highest refs_count first) as either text or JSON.
fn run_sym_impact(
    database: &db::Database,
    repo: &str,
    diff_arg: &str,
    json: bool,
) -> error::Result<()> {
    use std::io::{Read, Write};

    let diff_text = if diff_arg == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(error::LegionError::Io)?;
        buf
    } else {
        std::fs::read_to_string(diff_arg).map_err(error::LegionError::Io)?
    };

    let indexes = database.list_scip_indexes_filtered(Some(repo), None)?;
    if indexes.is_empty() {
        eprintln!("[legion] no SCIP index for repo '{repo}' -- run `legion index {repo}` first");
        return Ok(());
    }

    // Cross-index dedup: a polyglot repo can have the same logical symbol
    // appear in two language indexes. `diff_impact_radius` dedupes within
    // one blob; this loop dedupes across blobs so the CLI never prints
    // the same symbol twice.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut all_hits: Vec<sym::ImpactRadius> = Vec::new();
    for idx in &indexes {
        let hits = sym::diff_impact_radius(&idx.blob, &diff_text)?;
        for hit in hits {
            if seen.insert(hit.symbol.clone()) {
                all_hits.push(hit);
            }
        }
    }
    all_hits.sort_by_key(|h| std::cmp::Reverse(h.refs_count));

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        let body = serde_json::to_string(&all_hits)
            .map_err(|e| error::LegionError::Search(format!("impact json: {e}")))?;
        writeln!(out, "{body}")?;
        return Ok(());
    }
    if all_hits.is_empty() {
        writeln!(out, "[legion] no symbol definitions touched by this diff")?;
        return Ok(());
    }
    let high_threshold: u32 = std::env::var("LEGION_IMPACT_HIGH_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    for hit in &all_hits {
        let high = if hit.refs_count >= high_threshold {
            "  HIGH"
        } else {
            ""
        };
        let loc = match &hit.def_location {
            Some(loc) => format!("{}:{}", loc.file, loc.line),
            None => "(no def location)".to_string(),
        };
        writeln!(out, "{loc}\t{}\trefs:{}{high}", hit.symbol, hit.refs_count)?;
    }
    Ok(())
}

/// One row in the cross-repo symbol consult output.
#[derive(serde::Serialize)]
struct ConsultSymbolHit {
    repo: String,
    lang: String,
    file: String,
    line: u32,
    column: u32,
    refs_count: usize,
}

/// Implementation of `legion consult --symbol <name>` (#285).
///
/// Walks every (repo, lang) pair in `scip_indexes`, finds matching
/// definitions, counts references per match. Output is sorted by repo
/// then lang then file. Empty result exits 0 silently in human mode and
/// emits `[]` in JSON mode -- a thin response is data, not failure.
fn run_consult_symbol(database: &db::Database, name: &str, json: bool) -> error::Result<()> {
    use std::io::Write;
    let indexes = database.list_scip_indexes_filtered(None, None)?;

    let mut hits: Vec<ConsultSymbolHit> = Vec::new();
    for idx in &indexes {
        let defs = match sym::query_definitions(&idx.blob, name, &idx.repo, &idx.lang) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[legion] skipped {}/{}: {e}", idx.repo, idx.lang);
                continue;
            }
        };
        if defs.is_empty() {
            continue;
        }
        let refs_count = sym::query_references(&idx.blob, name, &idx.repo, &idx.lang)
            .map(|r| r.len())
            .unwrap_or(0);
        for d in defs {
            hits.push(ConsultSymbolHit {
                repo: d.repo,
                lang: d.lang,
                file: d.file,
                line: d.line,
                column: d.column,
                refs_count,
            });
        }
    }

    hits.sort_by(|a, b| {
        a.repo
            .cmp(&b.repo)
            .then(a.lang.cmp(&b.lang))
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        serde_json::to_writer(&mut out, &hits)?;
        writeln!(out)?;
    } else if hits.is_empty() {
        info!("[legion] no SCIP index has a definition for `{}`", name);
    } else {
        for h in &hits {
            writeln!(
                out,
                "{}/{}\t{}:{}:{}\t({} ref(s))",
                h.repo, h.lang, h.file, h.line, h.column, h.refs_count
            )?;
        }
    }
    Ok(())
}

/// Spawn `legion index <repo>` as a detached background process (#284).
///
/// Called from `WatchAction::Add` so a freshly-watched repo gets its
/// SCIP indexes populated without blocking the operator. Both stdout
/// and stderr go to a per-repo log file under the temp dir; on any
/// failure the watch add still succeeds and a warning is printed --
/// background indexing is best-effort, the operator can always run
/// `legion index <repo>` manually later.
fn spawn_background_indexer(repo_name: &str) {
    let self_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[legion] background indexer not started: cannot resolve own binary: {e}");
            return;
        }
    };
    // One-shot migration: on every spawn, sweep any leftover
    // /tmp/legion-index-*.log files from the prior /tmp-rooted location into
    // the new XDG_STATE_HOME directory. Idempotent so the next spawn is a
    // no-op once the migration ran.
    scip::migrate_legacy_index_logs();
    let log_path = scip::index_log_path(repo_name);
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path);
    let (stdout, stderr, log_note) = match log_file {
        Ok(f) => match f.try_clone() {
            Ok(f2) => (
                std::process::Stdio::from(f),
                std::process::Stdio::from(f2),
                format!("indexing in background; log: {}", log_path.display()),
            ),
            Err(_) => (
                std::process::Stdio::from(f),
                std::process::Stdio::null(),
                format!(
                    "indexing in background; stdout log: {} (stderr discarded)",
                    log_path.display()
                ),
            ),
        },
        Err(e) => {
            eprintln!(
                "[legion] background indexer log {} not writable: {e} -- spawning silently",
                log_path.display()
            );
            (
                std::process::Stdio::null(),
                std::process::Stdio::null(),
                "indexing in background (no log)".to_string(),
            )
        }
    };
    match std::process::Command::new(self_path)
        .arg("index")
        .arg(repo_name)
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
    {
        Ok(_) => println!("{log_note}"),
        Err(e) => eprintln!("[legion] background indexer spawn failed: {e}"),
    }
}

/// Print the tail of every per-repo background-indexer log, optionally
/// filtered to one repo. With `follow = true`, tails new output as it
/// arrives until SIGINT.
fn run_index_logs(repo: Option<&str>, tail_lines: usize, follow: bool) -> error::Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let initial = scip::read_index_logs(repo, tail_lines)?;
    if initial.is_empty() {
        let dir = scip::index_log_dir();
        writeln!(
            out,
            "[legion] no index logs in {} -- run `legion watch add <repo>` or `legion index <repo>` to generate one",
            dir.display()
        )?;
        return Ok(());
    }
    for (label, content) in &initial {
        writeln!(out, "=== {label} ===")?;
        writeln!(out, "{content}")?;
        writeln!(out)?;
    }
    out.flush()?;
    if !follow {
        return Ok(());
    }

    // Track per-repo byte offsets so we only print new content on each
    // poll. Initialized at current end-of-file so the first follow cycle
    // emits only fresh writes (we already printed the initial tail above).
    use std::collections::HashMap;
    let mut offsets: HashMap<String, u64> = HashMap::new();
    let dir = scip::index_log_dir();
    if dir.is_dir()
        && let Ok(entries) = std::fs::read_dir(&dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some(filter) = repo
                && stem != filter
            {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&path) {
                offsets.insert(stem.to_string(), meta.len());
            }
        }
    }

    // SIGINT (Ctrl-C) terminates the process via the OS default handler;
    // there is no per-iteration cleanup state to preserve, so the polling
    // loop has no explicit signal handling.
    loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<std::path::PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("log"))
            .collect();
        paths.sort();
        for path in paths {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some(filter) = repo
                && stem != filter
            {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let last = offsets.get(stem).copied().unwrap_or(0);
            let now = meta.len();
            if now <= last {
                continue;
            }
            // Read only the new bytes by seeking past `last`.
            use std::io::{Read, Seek, SeekFrom};
            let Ok(mut f) = std::fs::File::open(&path) else {
                continue;
            };
            if f.seek(SeekFrom::Start(last)).is_err() {
                continue;
            }
            let mut buf = String::new();
            if f.read_to_string(&mut buf).is_err() {
                continue;
            }
            if buf.is_empty() {
                continue;
            }
            writeln!(out, "=== {stem} ===")?;
            write!(out, "{buf}")?;
            out.flush()?;
            offsets.insert(stem.to_string(), now);
        }
    }
}

/// Render a one-shot SCIP index health banner for a single repo, intended
/// to be appended to the SessionStart hook output. Silent on healthy
/// (every detected language has a fresh index); loud on stale, missing,
/// or unindexable repos. Never errors -- a banner-mode failure prints
/// "unavailable" and returns Ok so SessionStart never blocks on this.
fn run_index_status_banner(base: &std::path::Path, repo: Option<&str>) -> error::Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let Some(repo_name) = repo else {
        // --banner without a repo is a misconfigured caller. Print a quiet
        // hint so the failure is observable but exit cleanly.
        writeln!(
            out,
            "[Legion] Index status: --banner requires <repo> -- pass the repo name positionally"
        )?;
        return Ok(());
    };

    let watch_path = base.join("watch.toml");
    let repos = match watch::list_repos_in_config(&watch_path) {
        Ok(r) => r,
        Err(_) => {
            writeln!(out, "[Legion] Index status: unavailable")?;
            return Ok(());
        }
    };
    let entry = match repos.iter().find(|r| r.name == repo_name) {
        Some(e) => e,
        None => {
            // Repo is not watched. Quiet for SessionStart -- this is the
            // common case for one-off shells in unrelated dirs.
            return Ok(());
        }
    };
    let repo_path = std::path::PathBuf::from(&entry.workdir);
    let detected = scip::detect_languages(&repo_path);

    let database = match db::Database::open(&base.join("legion.db")) {
        Ok(d) => d,
        Err(_) => {
            writeln!(out, "[Legion] Index status: unavailable")?;
            return Ok(());
        }
    };
    let indexes = match database.list_scip_indexes_filtered(Some(repo_name), None) {
        Ok(i) => i,
        Err(_) => {
            writeln!(out, "[Legion] Index status: unavailable")?;
            return Ok(());
        }
    };

    let now = chrono::Utc::now();
    let banner = render_index_status_banner(repo_name, &detected, &indexes, now);
    if !banner.is_empty() {
        writeln!(out, "{banner}")?;
    }
    Ok(())
}

/// Pure renderer for the SessionStart index status banner. Empty string
/// means "silent" (every detected language has a fresh index). Returns a
/// multi-line block when anything is stale or missing.
///
/// Stale threshold: 7 days. Override-friendly through a build constant if
/// future tuning needs it; not exposed as a CLI flag in v1.
fn render_index_status_banner(
    repo_name: &str,
    detected_langs: &[&str],
    indexes: &[scip::ScipIndex],
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    const STALE_THRESHOLD_DAYS: i64 = 7;

    if detected_langs.is_empty() {
        // No supported language detected -- silent. Avoids cluttering
        // SessionStart for repos that legion does not index.
        return String::new();
    }

    enum LangHealth {
        Fresh { lang: String, age: String },
        Stale { lang: String, age: String },
        Missing { lang: String },
    }

    fn humanize_age(seconds: i64) -> String {
        if seconds < 60 {
            return "just now".to_string();
        }
        if seconds < 3600 {
            return format!("{}m ago", seconds / 60);
        }
        if seconds < 86_400 {
            return format!("{}h ago", seconds / 3600);
        }
        format!("{}d ago", seconds / 86_400)
    }

    let mut report = Vec::with_capacity(detected_langs.len());
    for lang in detected_langs {
        let row = indexes.iter().find(|i| i.lang == *lang);
        match row {
            Some(idx) => match chrono::DateTime::parse_from_rfc3339(&idx.updated_at) {
                Ok(parsed) => {
                    let age_seconds = (now - parsed.with_timezone(&chrono::Utc)).num_seconds();
                    let age = humanize_age(age_seconds);
                    if age_seconds > STALE_THRESHOLD_DAYS * 86_400 {
                        report.push(LangHealth::Stale {
                            lang: (*lang).to_string(),
                            age,
                        });
                    } else {
                        report.push(LangHealth::Fresh {
                            lang: (*lang).to_string(),
                            age,
                        });
                    }
                }
                Err(_) => {
                    // Unparseable timestamp -- treat as stale so the operator
                    // sees a signal to rebuild rather than silently trusting
                    // an index whose freshness we cannot verify.
                    report.push(LangHealth::Stale {
                        lang: (*lang).to_string(),
                        age: "unknown age".to_string(),
                    });
                }
            },
            None => report.push(LangHealth::Missing {
                lang: (*lang).to_string(),
            }),
        }
    }

    let all_fresh = report.iter().all(|h| matches!(h, LangHealth::Fresh { .. }));
    if all_fresh {
        let summary: Vec<String> = report
            .iter()
            .map(|h| match h {
                LangHealth::Fresh { lang, age } => format!("{lang}: fresh ({age})"),
                _ => unreachable!(),
            })
            .collect();
        return format!(
            "[Legion] Index status for {repo_name}: {}",
            summary.join(", ")
        );
    }

    let mut lines = vec![format!("[Legion] Index status for {repo_name}:")];
    for h in &report {
        match h {
            LangHealth::Fresh { lang, age } => {
                lines.push(format!("  {lang}: fresh ({age})"));
            }
            LangHealth::Stale { lang, age } => {
                lines.push(format!(
                    "  {lang}: STALE -- last built {age}, run `legion index {repo_name}` to refresh"
                ));
            }
            LangHealth::Missing { lang } => {
                lines.push(format!(
                    "  {lang}: not indexed -- run `legion index {repo_name}` or `legion watch add {repo_name}` to build"
                ));
            }
        }
    }
    lines.join("\n")
}

fn run_location_query<F>(
    database: &db::Database,
    repo: Option<String>,
    lang: Option<String>,
    json: bool,
    rust_only: bool,
    mut query: F,
) -> error::Result<()>
where
    F: FnMut(&scip::ScipIndex) -> error::Result<Vec<sym::SymbolLocation>>,
{
    use std::io::Write;
    let indexes = database.list_scip_indexes_filtered(repo.as_deref(), lang.as_deref())?;
    if indexes.is_empty() {
        no_index_found(repo.as_deref(), lang.as_deref());
        std::process::exit(1);
    }

    // Skip non-rust indexes for `impl` queries (SCIP only models the
    // relationship for languages with traits/interfaces). Stay quiet
    // when `--lang` already restricts the scope -- the user clearly
    // knows the constraint.
    let lang_filter_active = lang.is_some();
    let mut all = Vec::new();
    for idx in &indexes {
        if rust_only && idx.lang != "rust" {
            if !lang_filter_active {
                eprintln!(
                    "[legion] note: 'impl' relationships not modeled for {} -- skipping {}/{}",
                    idx.lang, idx.repo, idx.lang
                );
            }
            continue;
        }
        let mut hits = query(idx)?;
        all.append(&mut hits);
    }
    all.sort_by(|a, b| {
        a.repo
            .cmp(&b.repo)
            .then(a.lang.cmp(&b.lang))
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        serde_json::to_writer(&mut out, &all)?;
        writeln!(out)?;
    } else {
        for loc in &all {
            writeln!(
                out,
                "{}:{}:{}\t[{}/{}]",
                loc.file, loc.line, loc.column, loc.repo, loc.lang
            )?;
        }
    }
    Ok(())
}

/// First-match hover lookup. Iterates indexes in the order returned by
/// `list_scip_indexes_filtered` (sorted by repo, lang) and returns the
/// first symbol that matches. When the query spans multiple indexes,
/// callers typically scope with `--repo` / `--lang` to disambiguate.
fn run_hover_query(
    database: &db::Database,
    name: &str,
    repo: Option<String>,
    lang: Option<String>,
    json: bool,
) -> error::Result<()> {
    use std::io::Write;
    let indexes = database.list_scip_indexes_filtered(repo.as_deref(), lang.as_deref())?;
    if indexes.is_empty() {
        no_index_found(repo.as_deref(), lang.as_deref());
        std::process::exit(1);
    }
    let mut hover: Option<sym::HoverInfo> = None;
    for idx in &indexes {
        if let Some(h) = sym::query_hover(&idx.blob, name, &idx.repo, &idx.lang)? {
            hover = Some(h);
            break;
        }
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        serde_json::to_writer(&mut out, &hover)?;
        writeln!(out)?;
    } else if let Some(h) = hover {
        writeln!(out, "{}", h.symbol)?;
        if let Some(sig) = h.signature {
            writeln!(out, "{sig}")?;
        }
        if let Some(doc) = h.docstring {
            writeln!(out)?;
            writeln!(out, "{doc}")?;
        }
    }
    Ok(())
}

fn no_index_found(repo: Option<&str>, lang: Option<&str>) {
    let scope = match (repo, lang) {
        (Some(r), Some(l)) => format!("{r}/{l}"),
        (Some(r), None) => r.to_string(),
        (None, Some(l)) => format!("(any repo)/{l}"),
        (None, None) => "(any repo)".to_string(),
    };
    eprintln!("[legion] no index found for {scope}; run `legion index <repo>` first");
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

/// Dispatch `legion uncertainty ...`. Non-blocking emit posture: emit
/// failures log to stderr and return Ok so an upstream hook can never
/// abort the agent on a telemetry-shaped problem. Witness / calibration /
/// orphans surface errors normally -- those are explicit user actions.
fn run_uncertainty(action: UncertaintyAction) -> error::Result<()> {
    use std::io::Write;
    let base = data_dir()?;
    let database = db::Database::open(&base.join("legion.db"))?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    match action {
        UncertaintyAction::Emit {
            surface,
            feature_key,
            input_fingerprint,
            model,
            model_version,
            claimed_confidence,
            payload,
            orphan_ttl_days,
        } => {
            let confidence = match uncertainty::types::Confidence::from_f64(claimed_confidence) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[legion uncertainty emit] {e}");
                    return Ok(());
                }
            };
            let payload_value: serde_json::Value = match serde_json::from_str(&payload) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[legion uncertainty emit] payload not valid JSON: {e}");
                    return Ok(());
                }
            };
            let input = uncertainty::types::PredictionInput {
                surface,
                feature_key,
                input_fingerprint,
                model,
                model_version,
                claimed_confidence: confidence,
                prediction_payload: payload_value,
                orphan_after: uncertainty::storage::orphan_after_from_ttl(orphan_ttl_days),
            };
            let prediction = uncertainty::types::Prediction::new(input);
            if let Err(e) = database.insert_prediction(&prediction) {
                eprintln!("[legion uncertainty emit] insert failed: {e}");
                return Ok(());
            }
            let out_json = serde_json::json!({
                "id": prediction.id,
                "orphan_after": prediction.orphan_after,
            });
            // Emit is non-blocking: a closed stdout pipe or serialize error
            // must not propagate as a process error, otherwise an upstream
            // hook could break the agent on a stdout-side problem unrelated
            // to the prediction having landed in the DB.
            match serde_json::to_string(&out_json) {
                Ok(s) => {
                    if let Err(e) = writeln!(out, "{s}") {
                        eprintln!("[legion uncertainty emit] stdout write failed: {e}");
                    }
                }
                Err(e) => eprintln!("[legion uncertainty emit] serialize failed: {e}"),
            }
        }

        UncertaintyAction::Witness {
            prediction_id,
            outcome_label,
            outcome_correctness,
            payload,
        } => {
            let label = uncertainty::types::OutcomeLabel::from_str(&outcome_label)
                .map_err(|e| error::LegionError::WorkSource(format!("{e}")))?;
            let correctness = uncertainty::types::Correctness::from_f64(outcome_correctness)
                .map_err(|e| error::LegionError::WorkSource(format!("{e}")))?;
            let payload_value: serde_json::Value = match payload {
                Some(s) => serde_json::from_str(&s)
                    .map_err(|e| error::LegionError::WorkSource(format!("payload JSON: {e}")))?,
                None => serde_json::json!({}),
            };
            let mut prediction = database
                .get_prediction(&prediction_id)
                .map_err(|e| error::LegionError::WorkSource(format!("{e}")))?
                .ok_or_else(|| {
                    error::LegionError::WorkSource(format!("prediction not found: {prediction_id}"))
                })?;
            let now = chrono::Utc::now().to_rfc3339();
            prediction
                .witness(label, payload_value, correctness, &now)
                .map_err(|e| error::LegionError::WorkSource(format!("{e}")))?;
            database
                .update_prediction(&prediction)
                .map_err(|e| error::LegionError::WorkSource(format!("{e}")))?;
            let out_json = serde_json::json!({
                "id": prediction.id,
                "state": prediction.state.as_str(),
                "witnessed_at": prediction.witnessed_at,
            });
            writeln!(out, "{}", serde_json::to_string(&out_json)?)?;
        }

        UncertaintyAction::Calibration {
            surface,
            model,
            json,
        } => {
            let snaps = database
                .list_calibration_snapshots(surface.as_deref(), model.as_deref())
                .map_err(|e| error::LegionError::WorkSource(format!("{e}")))?;
            if json {
                writeln!(out, "{}", serde_json::to_string(&snaps)?)?;
            } else if snaps.is_empty() {
                writeln!(
                    out,
                    "[legion uncertainty] no calibration snapshots in scope"
                )?;
            } else {
                writeln!(
                    out,
                    "{:<48} {:<8} {:<8} {:<8} {:<8} {:<8} {:<8}",
                    "cohort", "lower", "upper", "claimed", "actual", "n", "brier"
                )?;
                for s in &snaps {
                    writeln!(
                        out,
                        "{:<48} {:<8.2} {:<8.2} {:<8.2} {:<8.2} {:<8} {:<8.4}",
                        s.cohort_key,
                        s.bucket_lower,
                        s.bucket_upper,
                        s.claimed_confidence,
                        s.actual_correctness,
                        s.prediction_count,
                        s.brier_score,
                    )?;
                }
            }
        }

        UncertaintyAction::Orphans { surface, json } => {
            let rows = database
                .count_orphans_by_surface(surface.as_deref())
                .map_err(|e| error::LegionError::WorkSource(format!("{e}")))?;
            if json {
                writeln!(out, "{}", serde_json::to_string(&rows)?)?;
            } else if rows.is_empty() {
                writeln!(out, "[legion uncertainty] no orphans in scope")?;
            } else {
                writeln!(out, "{:<32} {:<8}", "surface", "count")?;
                for r in &rows {
                    writeln!(out, "{:<32} {:<8}", r.surface, r.count)?;
                }
            }
        }
    }
    Ok(())
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
            archives,
            include_archives,
        } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            // Resolve archive mode (#457). Mutually-exclusive flags
            // enforced at the clap layer via conflicts_with.
            let mode = if archives {
                recall::ArchiveMode::Cold
            } else if include_archives {
                recall::ArchiveMode::Both
            } else {
                recall::ArchiveMode::Hot
            };

            // v1 scope: --archives / --include-archives only affect
            // the BM25/hybrid path. Warn loudly when combined with the
            // other variants so an operator does not silently get
            // hot-only results from --latest --archives.
            if (archives || include_archives) && (latest || cosine_only || domain.is_some()) {
                eprintln!(
                    "[legion] warning: --archives / --include-archives currently apply only to the BM25/hybrid recall path. Combined with --latest / --domain / --cosine-only, this run uses hot-only results. Extending coverage is tracked as a #457 follow-up."
                );
            }

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
                    Some(model) => recall::recall_in_mode(
                        &database, &index, &model, &repo, &context, limit, mode,
                    )?,
                    None => recall::recall_bm25_in_mode(
                        &database, &index, &repo, &context, limit, mode,
                    )?,
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
        Commands::Consult {
            context,
            symbol,
            limit,
            json,
        } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            match (context, symbol) {
                (Some(ctx), None) => {
                    let index = search::SearchIndex::open(&base.join("index"))?;
                    let result = match try_load_embed_model() {
                        Some(model) => recall::consult(&database, &index, &model, &ctx, limit)?,
                        None => recall::consult_bm25(&database, &index, &ctx, limit)?,
                    };
                    let output = recall::format_for_consult(&result);
                    if output.is_empty() {
                        info!("[legion] no reflections matched context: \"{}\"", ctx);
                    } else {
                        print!("{output}");
                    }
                }
                (None, Some(name)) => {
                    run_consult_symbol(&database, &name, json)?;
                }
                (None, None) => {
                    return Err(error::LegionError::WatchConfig(
                        "either --context <query> or --symbol <name> is required".to_string(),
                    ));
                }
                (Some(_), Some(_)) => {
                    // clap's conflicts_with should reject this before we get here.
                    return Err(error::LegionError::WatchConfig(
                        "--context and --symbol are mutually exclusive".to_string(),
                    ));
                }
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
        Commands::Resolve { id, reflection } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            if database.resolve_post(&id, reflection.as_deref())? {
                info!("[legion] resolved {}", id);
            } else {
                eprintln!("[legion] post not found: {}", id);
                std::process::exit(1);
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
            include_stale,
            include_resolved,
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
                    let posts = board::bullpen_filtered_with_decay(
                        &database,
                        &repo,
                        filter,
                        include_stale,
                        include_resolved,
                    )?;
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
        Commands::Index {
            repo,
            file,
            status,
            logs,
            follow,
            lines,
            banner,
            json,
        } => {
            let base = data_dir()?;

            // --logs: print recent background-indexer log content and return.
            if logs {
                run_index_logs(repo.as_deref(), lines, follow)?;
                return Ok(());
            }

            // --status --banner: SessionStart-friendly per-repo health line.
            if status && banner {
                run_index_status_banner(&base, repo.as_deref())?;
                return Ok(());
            }

            // --status: dump scip_indexes inventory and return.
            if status {
                let database = db::Database::open(&base.join("legion.db"))?;
                let indexes = database.list_scip_indexes_filtered(repo.as_deref(), None)?;
                if json {
                    let rows: Vec<serde_json::Value> = indexes
                        .iter()
                        .map(|idx| {
                            serde_json::json!({
                                "repo": idx.repo,
                                "lang": idx.lang,
                                "size_bytes": idx.blob.len(),
                                "updated_at": idx.updated_at,
                            })
                        })
                        .collect();
                    println!("{}", serde_json::to_string(&rows)?);
                } else if indexes.is_empty() {
                    info!("[legion] no SCIP indexes recorded yet");
                } else {
                    use std::io::Write;
                    let stdout = std::io::stdout();
                    let mut out = stdout.lock();
                    for idx in &indexes {
                        let bytes = idx.blob.len();
                        writeln!(
                            out,
                            "{}/{}\t{} bytes\t{}",
                            idx.repo, idx.lang, bytes, idx.updated_at
                        )?;
                    }
                }
                return Ok(());
            }

            let watch_path = base.join("watch.toml");
            let repos = watch::list_repos_in_config(&watch_path)?;

            // --file overrides --repo: resolve the owning repo by walking
            // ancestors of the file path against watch.toml workdirs.
            // Falls through to the named-repo path with a synthesized repo
            // name so the indexer + upsert loop is the single code path.
            let (repo, entry): (String, &watch::WatchRepoConfig) = match (repo, file.as_ref()) {
                (_, Some(file_path)) => {
                    let canon = std::fs::canonicalize(file_path).map_err(|e| {
                        error::LegionError::WatchConfig(format!(
                            "cannot canonicalize {}: {e}",
                            file_path.display()
                        ))
                    })?;
                    let owner = repos
                        .iter()
                        .find(|r| {
                            std::fs::canonicalize(&r.workdir)
                                .map(|w| canon.starts_with(&w))
                                .unwrap_or(false)
                        })
                        .ok_or_else(|| {
                            error::LegionError::WatchConfig(format!(
                                "no watch.toml entry owns {} -- file is outside every watched workdir",
                                canon.display()
                            ))
                        })?;
                    (owner.name.clone(), owner)
                }
                (Some(name), None) => {
                    let owner = repos.iter().find(|r| r.name == name).ok_or_else(|| {
                        error::LegionError::WatchConfig(format!(
                            "repo '{name}' not in watch.toml. Add it with `legion watch add {name} <path>`."
                        ))
                    })?;
                    (name, owner)
                }
                (None, None) => {
                    return Err(error::LegionError::WatchConfig(
                        "either <repo> or --file <path> is required".to_string(),
                    ));
                }
            };
            let repo_path = std::path::PathBuf::from(&entry.workdir);

            let langs = scip::detect_languages(&repo_path);
            if langs.is_empty() {
                return Err(error::LegionError::WatchConfig(format!(
                    "no supported language detected at {}. Markers checked: Cargo.toml, package.json, pyproject.toml, requirements.txt, go.mod.",
                    repo_path.display()
                )));
            }

            let database = db::Database::open(&base.join("legion.db"))?;
            let mut indexed: u32 = 0;
            for lang in &langs {
                let blob = match scip::run_indexer(lang, &repo_path) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("[legion] skipped {repo} ({lang}): {e}");
                        continue;
                    }
                };
                let hash = scip::content_hash(&blob);
                let now = chrono::Utc::now().to_rfc3339();
                let index = scip::ScipIndex {
                    id: uuid::Uuid::now_v7().to_string(),
                    repo: repo.clone(),
                    lang: (*lang).to_string(),
                    content_hash: hash,
                    blob,
                    updated_at: now,
                    deleted_at: None,
                };
                let bytes_len = index.blob.len();
                let hash_prefix = &index.content_hash[..16];
                database.upsert_scip_index(&index)?;
                eprintln!(
                    "[legion] indexed {repo} ({lang}): {bytes_len} bytes, hash {hash_prefix}"
                );
                indexed += 1;
            }
            if indexed == 0 {
                return Err(error::LegionError::WatchConfig(format!(
                    "no language indexed for {repo} -- every detected language ({}) failed; see warnings above",
                    langs.join(", ")
                )));
            }
        }
        Commands::Sym { action } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            run_sym_action(&database, action)?;
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
        Commands::Document { action } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            match action {
                DocumentAction::Create {
                    doc_type,
                    owner,
                    id,
                    surface,
                    status,
                    priority,
                    from,
                } => {
                    // Payload from --from <path> or stdin.
                    let payload = match from {
                        Some(p) => std::fs::read_to_string(&p)?,
                        None => {
                            use std::io::Read;
                            let mut buf = String::new();
                            std::io::stdin().read_to_string(&mut buf)?;
                            buf
                        }
                    };
                    // Sanity-validate JSON shape so a typo doesn't land
                    // in the table as a string blob. Schema-level
                    // validation per type lives in a sibling child issue.
                    serde_json::from_str::<serde_json::Value>(&payload).map_err(|e| {
                        error::LegionError::WorkSource(format!("payload is not valid JSON: {e}"))
                    })?;
                    let meta = documents::DocumentMeta {
                        id: id.as_deref(),
                        doc_type: &doc_type,
                        surface: surface.as_deref(),
                        status: status.as_deref(),
                        priority: priority.as_deref(),
                        owner: &owner,
                    };
                    let doc = database.insert_document(&meta, &payload)?;
                    println!("{}", doc.id);
                }
                DocumentAction::View { id, json } => {
                    let doc = database.get_document(&id)?.ok_or_else(|| {
                        error::LegionError::WorkSource(format!("document '{id}' not found"))
                    })?;
                    if json {
                        println!("{}", serde_json::to_string(&doc)?);
                    } else {
                        println!("id:       {}", doc.id);
                        println!("type:     {}", doc.doc_type);
                        if let Some(s) = &doc.surface {
                            println!("surface:  {s}");
                        }
                        println!("status:   {}", doc.status);
                        if let Some(p) = &doc.priority {
                            println!("priority: {p}");
                        }
                        println!("owner:    {}", doc.owner);
                        if let Some(a) = &doc.archived_at {
                            println!("archived: {a}");
                        }
                        println!("created:  {}", doc.created_at);
                        println!("updated:  {}", doc.updated_at);
                        println!("--- payload ---");
                        println!("{}", doc.payload);
                    }
                }
                DocumentAction::List {
                    doc_type,
                    surface,
                    status,
                    owner,
                    archived,
                    json,
                } => {
                    let filter = documents::DocumentFilter {
                        doc_type: doc_type.as_deref(),
                        surface: surface.as_deref(),
                        status: status.as_deref(),
                        owner: owner.as_deref(),
                        archived: if archived { Some(true) } else { None },
                    };
                    let docs = database.list_documents(&filter)?;
                    if json {
                        println!("{}", serde_json::to_string(&docs)?);
                    } else if docs.is_empty() {
                        println!("[legion] no documents match filter");
                    } else {
                        use std::io::Write;
                        let stdout = std::io::stdout();
                        let mut out = stdout.lock();
                        writeln!(
                            out,
                            "{:<24} {:<14} {:<14} {:<14} {:<10}",
                            "id", "type", "surface", "owner", "status"
                        )?;
                        for d in &docs {
                            writeln!(
                                out,
                                "{:<24} {:<14} {:<14} {:<14} {:<10}",
                                d.id,
                                d.doc_type,
                                d.surface.as_deref().unwrap_or("-"),
                                d.owner,
                                d.status,
                            )?;
                        }
                    }
                }
                DocumentAction::Archive { id } => {
                    let doc = database.archive_document(&id)?;
                    println!(
                        "archived {} at {}",
                        doc.id,
                        doc.archived_at.as_deref().unwrap_or("?")
                    );
                }
            }
        }
        Commands::SubIssue { action } => match action {
            SubIssueAction::Create {
                repo,
                parent,
                title,
                body,
            } => {
                let (plugin, github_repo, _) =
                    worksource::resolve_config(&repo).ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{repo}'"
                        ))
                    })?;
                let created = worksource::create_sub_issue(
                    &plugin,
                    &github_repo,
                    parent,
                    &title,
                    body.as_deref(),
                )?;
                println!("{}", created.url);
            }
            SubIssueAction::List {
                repo,
                parent,
                state,
                json,
            } => {
                let (plugin, github_repo, _) =
                    worksource::resolve_config(&repo).ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{repo}'"
                        ))
                    })?;
                let issues =
                    worksource::list_sub_issues(&plugin, &github_repo, parent, Some(&state))?;
                if json {
                    println!("{}", serde_json::to_string(&issues)?);
                } else if issues.is_empty() {
                    println!("[legion] no sub-issues of #{parent} in {github_repo}");
                } else {
                    for i in &issues {
                        println!("#{}\t{}\t{}", i.number, i.state, i.title);
                    }
                }
            }
        },
        Commands::McpHealth { wait } => {
            // Spawn `legion mcp` as a subprocess. Send `initialize`, wait
            // for the notifier to tick at least once, then send
            // `legion/notifier_health`. Print the result JSON. Exit
            // non-zero only on hard transport failure (binary missing,
            // stdio closed); a `stale` or `unknown` health verdict is
            // still a successful PROBE and prints to stdout for the
            // operator to interpret.
            let exe = std::env::current_exe()
                .map_err(|e| error::LegionError::Server(format!("current_exe: {e}")))?;
            let mut child = std::process::Command::new(exe)
                .arg("mcp")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| error::LegionError::Server(format!("spawn legion mcp: {e}")))?;
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| error::LegionError::Server("mcp stdin missing".into()))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| error::LegionError::Server("mcp stdout missing".into()))?;
            let mut reader = std::io::BufReader::new(stdout);

            use std::io::{BufRead, Write};
            let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"legion-mcp-health","version":"0"}}}"#;
            writeln!(stdin, "{init}").ok();
            // Drain the init response so subsequent reads land on the
            // health response.
            let mut buf = String::new();
            reader.read_line(&mut buf).ok();

            // Give the notifier time to tick at least once.
            std::thread::sleep(std::time::Duration::from_secs(wait));

            let health_req = r#"{"jsonrpc":"2.0","id":2,"method":"legion/notifier_health"}"#;
            writeln!(stdin, "{health_req}").ok();
            buf.clear();
            reader
                .read_line(&mut buf)
                .map_err(|e| error::LegionError::Server(format!("read mcp response: {e}")))?;

            let _ = child.kill();
            let _ = child.wait();

            // Parse and pretty-print the result field; fall back to raw
            // line on parse failure so the operator sees the actual bytes.
            match serde_json::from_str::<serde_json::Value>(buf.trim()) {
                Ok(v) => {
                    let result = v.get("result").cloned().unwrap_or(v);
                    println!("{}", serde_json::to_string_pretty(&result)?);
                }
                Err(_) => print!("{buf}"),
            }
        }
        Commands::Now { banner: _, json } => {
            // The default and `--banner` output are the same one-line
            // string; `banner` is accepted for explicitness in scripts
            // (matches `legion index --status --banner`'s convention).
            let snap = now::NowSnapshot::from_local(chrono::Local::now());
            if json {
                println!("{}", serde_json::to_string(&snap)?);
            } else {
                println!("{}", snap.banner());
            }
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
        Commands::Whatami { repo, limit } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let roots = database.get_domain_roots(&repo, "workflow", limit)?;
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
            let output = recall::format_whatami(&repo, &entries);
            print!("{output}");
        }
        Commands::Stats { repo } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            stats::stats(&database, repo.as_deref())?;
        }
        Commands::Telemetry { action } => match action {
            TelemetryAction::RecordBypass {
                repo,
                session_id,
                tool,
                pattern,
                bypass_reason,
                had_sym_hits,
                had_recall_hits,
                agent,
            } => {
                let record = telemetry::BypassRecord {
                    ts: chrono::Utc::now(),
                    repo,
                    session_id,
                    agent,
                    tool,
                    pattern,
                    bypass_reason,
                    had_sym_hits,
                    had_recall_hits,
                };
                telemetry::append_bypass(&record)?;
            }
            TelemetryAction::ListBypasses { since, repo } => {
                let since_dur = match since {
                    Some(s) => Some(telemetry::parse_duration(&s)?),
                    None => None,
                };
                let rows = telemetry::list_bypasses(since_dur, repo.as_deref())?;
                println!("{}", serde_json::to_string(&rows)?);
            }
            TelemetryAction::Summary {
                since,
                repo,
                top,
                json,
            } => {
                let since_dur = match since {
                    Some(s) => Some(telemetry::parse_duration(&s)?),
                    None => None,
                };
                let rows = telemetry::list_bypasses(since_dur, repo.as_deref())?;
                let summary = telemetry::summarize(&rows, top);
                if json {
                    println!("{}", serde_json::to_string(&summary)?);
                } else if summary.is_empty() {
                    println!("[legion] no bypasses recorded in scope");
                } else {
                    use std::io::Write;
                    let stdout = std::io::stdout();
                    let mut out = stdout.lock();
                    writeln!(
                        out,
                        "{:<6} {:<14} {:<32} {:<6} {:<8} {:<8}",
                        "tool", "repo", "pattern", "count", "sym%", "recall%"
                    )?;
                    for row in &summary {
                        // chars().take is char-boundary-safe; row.pattern
                        // may contain multi-byte unicode (file paths,
                        // quoted strings) where byte slicing would panic.
                        let pattern = if row.pattern.chars().count() > 32 {
                            let head: String = row.pattern.chars().take(29).collect();
                            format!("{head}...")
                        } else {
                            row.pattern.clone()
                        };
                        writeln!(
                            out,
                            "{:<6} {:<14} {:<32} {:<6} {:<8.1} {:<8.1}",
                            row.tool,
                            row.repo,
                            pattern,
                            row.count,
                            row.had_sym_hits_pct * 100.0,
                            row.had_recall_hits_pct * 100.0,
                        )?;
                    }
                }
            }
        },
        Commands::Uncertainty { action } => {
            run_uncertainty(action)?;
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
                KanbanAction::List {
                    repo,
                    from,
                    json,
                    all,
                    backlog,
                } => {
                    let direction = if from {
                        kanban::Direction::Outbound
                    } else {
                        kanban::Direction::Inbound
                    };
                    // Default to the working set; --all and --backlog widen/redirect.
                    let scope = if all {
                        kanban::CardScope::All
                    } else if backlog {
                        kanban::CardScope::Backlog
                    } else {
                        kanban::CardScope::WorkingSet
                    };
                    let cards = kanban::list_cards(&database, &repo, direction, scope)?;
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
                KanbanAction::Reconcile {
                    repo,
                    close_stale,
                    cancel_shipped,
                    apply,
                } => {
                    // `--apply` is shorthand for both action flags.
                    let close_stale = close_stale || apply;
                    let cancel_shipped = cancel_shipped || apply;

                    // One pass over the board, partitioning by drift
                    // direction. Each card with a `source_url` triggers
                    // exactly one `view_issue` probe; the resulting state
                    // determines which (if any) bucket it lands in.
                    let cards = kanban::board_cards(&database)?;
                    let mut stale: Vec<(kanban::Card, u64, String)> = Vec::new();
                    let mut shipped_pending: Vec<(kanban::Card, u64, String)> = Vec::new();

                    for card in cards {
                        // Active vs. terminal: terminal cards (done /
                        // cancelled) are direction-1 candidates;
                        // anything else (pending, in-progress, blocked,
                        // ...) is direction-2.
                        let is_terminal = matches!(
                            card.status,
                            kanban::CardStatus::Done | kanban::CardStatus::Cancelled
                        );

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

                        // Direction 1: terminal-local + open-on-GH.
                        // Direction 2: active-local + closed/merged-on-GH.
                        // GitHub PR-merge events report state="CLOSED" with
                        // a separate stateReason or pr metadata; treat
                        // anything in the closed-family as "shipped" for
                        // direction 2 purposes.
                        let upper = state.to_uppercase();
                        match (is_terminal, upper.as_str()) {
                            (true, "OPEN") => {
                                stale.push((card.clone(), number, source_repo.clone()));
                            }
                            (false, "CLOSED" | "MERGED") => {
                                shipped_pending.push((card.clone(), number, source_repo.clone()));
                            }
                            _ => {}
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

                    if shipped_pending.is_empty() {
                        println!("[legion] reconcile: no shipped-pending cards found");
                    } else {
                        println!(
                            "[legion] reconcile: {} shipped-pending card(s) found",
                            shipped_pending.len()
                        );
                        for (card, number, source_repo) in &shipped_pending {
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

                    // Direction 2 action: cancel shipped-pending cards
                    // locally with --no-propagate semantics. The linked
                    // GitHub issue is already CLOSED/MERGED, so calling
                    // worksource::close_issue would either no-op or
                    // double-touch the audit log.
                    if cancel_shipped && !shipped_pending.is_empty() {
                        println!(
                            "[legion] reconcile: cancelling {} shipped-pending card(s) locally",
                            shipped_pending.len()
                        );
                        let mut cancelled = 0_u64;
                        let mut failed = 0_u64;
                        for (card, number, source_repo) in &shipped_pending {
                            // The partition above only enqueued cards
                            // whose source_type was Some; defensively
                            // re-check to avoid a misleading audit row
                            // if a future refactor moves the guard.
                            let Some(source_type) = card.source_type.as_deref() else {
                                continue;
                            };
                            let note = format!(
                                "reconcile: linked {}#{} already closed on GitHub",
                                source_repo, number
                            );
                            match kanban::transition_card(
                                &database,
                                &card.id,
                                kanban::Action::Cancel,
                                Some(&note),
                            ) {
                                Ok(_) => {
                                    cancelled += 1;
                                    eprintln!(
                                        "[legion] reconcile: cancelled card {} ({}#{})",
                                        card.id, source_repo, number
                                    );
                                    let details = serde_json::json!({
                                        "card_id": card.id,
                                        "propagation": "reconcile-shipped",
                                        "external_number": number,
                                    });
                                    let details_str = details.to_string();
                                    audit(&db::AuditInput {
                                        agent: &card.to_repo,
                                        action: "cancel-card",
                                        target_type: "card",
                                        target_ref: &card.id,
                                        task_id: Some(&card.id),
                                        source_type,
                                        details: Some(&details_str),
                                        outcome: "success",
                                    });
                                }
                                Err(e) => {
                                    failed += 1;
                                    eprintln!(
                                        "[legion] reconcile: failed to cancel card {}: {}",
                                        card.id, e
                                    );
                                    let err_msg = e.to_string();
                                    let details = serde_json::json!({
                                        "card_id": card.id,
                                        "propagation": "reconcile-shipped",
                                        "error": err_msg,
                                    });
                                    let details_str = details.to_string();
                                    audit(&db::AuditInput {
                                        agent: &card.to_repo,
                                        action: "cancel-card",
                                        target_type: "card",
                                        target_ref: &card.id,
                                        task_id: Some(&card.id),
                                        source_type,
                                        details: Some(&details_str),
                                        outcome: "failure",
                                    });
                                }
                            }
                        }
                        println!(
                            "[legion] reconcile: {} cancelled, {} failed",
                            cancelled, failed
                        );
                    } else if cancel_shipped {
                        println!("[legion] reconcile: nothing to cancel");
                    } else if !shipped_pending.is_empty() {
                        println!(
                            "[legion] reconcile: dry-run -- pass --cancel-shipped to actually cancel these cards"
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
                // Quality gates: verify legion-simplify AND legion-pr-write ran
                // clean on HEAD before calling the work source. This runs before
                // worksource::resolve_config so the gate error is always surfaced,
                // even if the worksource is not configured. --skip-gates is a
                // bootstrap escape hatch for the branch that ships the skills
                // themselves; it writes an audit entry so it cannot be done silently.
                if skip_gates {
                    audit(&db::AuditInput {
                        agent: &repo,
                        action: "skip-gate-bootstrap",
                        target_type: "pr",
                        target_ref: "pending",
                        task_id: task.as_deref(),
                        source_type: "unknown",
                        details: Some(
                            r#"{"skills":["legion-simplify","legion-pr-write"],"reason":"bootstrap"}"#,
                        ),
                        outcome: "skipped",
                    });
                    eprintln!("[legion] warning: quality gate skipped (--skip-gates bootstrap)");
                } else {
                    let (commit_hash, _branch) = git_head_commit_and_branch()?;
                    let short_hash: &str = &commit_hash[..commit_hash.len().min(8)];

                    let db_base = data_dir()?;
                    let gate_db = db::Database::open(&db_base.join("legion.db"))?;
                    // Both forcing-function gates must be clean on HEAD before the
                    // PR opens: simplify (structural quality) and pr-write (the
                    // articulated criterion-to-work mapping the verify gate later
                    // audits). Each is recorded by its own skill on the current
                    // commit, so a post-gate commit invalidates them (re-run).
                    for (skill, run_hint) in [
                        ("legion-simplify", "/legion-simplify"),
                        ("legion-pr-write", "/legion-pr-write"),
                    ] {
                        match gate_db.get_quality_gate(&commit_hash, skill)? {
                            None => {
                                eprintln!(
                                    "[legion] error: no clean {skill} gate on HEAD ({short_hash}). \
                                     Run {run_hint} before creating the PR."
                                );
                                std::process::exit(1);
                            }
                            Some(gate) if gate.result != "clean" => {
                                eprintln!(
                                    "[legion] error: {skill} recorded issues on HEAD ({short_hash}), \
                                     {} findings. Fix them and re-run the skill before creating the PR.",
                                    gate.findings_count
                                );
                                std::process::exit(1);
                            }
                            Some(_) => {
                                // Gate is clean -- continue to the next.
                            }
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
            PrAction::WriteCheck {
                repo,
                issue,
                body_file,
            } => {
                let (plugin_name, source_repo, _workdir) = worksource::resolve_config(&repo)
                    .ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "no work source configured for repo '{}' in watch.toml",
                            repo
                        ))
                    })?;

                // Load the issue's acceptance criteria -- the claim set the body
                // must map. Parsed from the same template card_parse reads for
                // `issue view`, so the criteria match what the agent saw.
                let ext = worksource::view_issue(&plugin_name, &source_repo, issue)?;
                let parsed = card_parse::parse_issue_body(ext.body.as_deref().unwrap_or(""));

                // Read the drafted body from --body-file, or stdin when omitted.
                let body = match body_file {
                    Some(path) => std::fs::read_to_string(&path).map_err(|e| {
                        error::LegionError::WorkSource(format!(
                            "failed to read --body-file {path}: {e}"
                        ))
                    })?,
                    None => {
                        use std::io::Read as _;
                        let mut buf = String::new();
                        std::io::stdin().read_to_string(&mut buf).map_err(|e| {
                            error::LegionError::WorkSource(format!("failed to read stdin: {e}"))
                        })?;
                        buf
                    }
                };

                let report = pr_write::validate_pr_body(&parsed.acceptance, &body);

                // Record the gate on HEAD so `legion pr create` can gate on it,
                // exactly as it does for legion-simplify.
                let (commit_hash, branch) = git_head_commit_and_branch()?;
                let result = if report.ok { "clean" } else { "issues" };
                let details = serde_json::json!({
                    "skill": "legion-pr-write",
                    "issue": issue,
                    "mapping_entries": report.mapping_entries,
                    "findings": report.findings,
                })
                .to_string();

                let base = data_dir()?;
                let database = db::Database::open(&base.join("legion.db"))?;
                database.record_quality_gate(
                    &branch,
                    &commit_hash,
                    "legion-pr-write",
                    result,
                    report.findings.len() as u64,
                    Some(&details),
                )?;

                if report.ok {
                    println!(
                        "[legion] pr-write gate clean for issue #{issue} ({} mapping entries). \
                         Open the PR with this body.",
                        report.mapping_entries
                    );
                } else {
                    eprintln!(
                        "[legion] pr-write gate FAILED for issue #{issue} -- {} gap(s):",
                        report.findings.len()
                    );
                    for f in &report.findings {
                        eprintln!("  - {f}");
                    }
                    eprintln!(
                        "\nThe mapping must be composed prose -- one entry per acceptance \
                         criterion, each citing evidence (a test, a file:line, an observable \
                         behavior) -- plus a 'Not done' section. Fix the body and re-run."
                    );
                    std::process::exit(1);
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

                        // #284: background-index the new repo so `legion sym`
                        // and `legion consult --symbol` can answer immediately.
                        // Spawn `legion index <name>` as a detached subprocess
                        // with stdout/stderr redirected to a per-repo log so
                        // the operator can confirm completion without a hung
                        // foreground command. Failures along the way print a
                        // warning -- the watch add itself still succeeded.
                        spawn_background_indexer(&effective_name);
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
                        // Surface pre-existing recipient collisions (#459).
                        // The `add` path rejects new duplicates, but a
                        // manually edited watch.toml can still contain
                        // collisions. Warn loud so the operator sees them.
                        let mut recipient_counts: std::collections::HashMap<&str, Vec<&str>> =
                            std::collections::HashMap::new();
                        for r in &repos {
                            recipient_counts
                                .entry(r.recipient())
                                .or_default()
                                .push(&r.name);
                        }
                        for (recipient, names) in &recipient_counts {
                            if names.len() > 1 {
                                eprintln!(
                                    "[WARNING] recipient '{}' shared by {} repos: {}. \
                                     Directed @{} signals wake all of them silently. \
                                     Fix by editing watch.toml to give each repo a unique agent.",
                                    recipient,
                                    names.len(),
                                    names.join(", "),
                                    recipient
                                );
                            }
                        }
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
                Some(WatchAction::SessionEnd { attempt_id }) => {
                    let db_path = base.join("legion.db");
                    let db = db::Database::open(&db_path)?;
                    watch::record_session_end(&db, &attempt_id)?;
                }
            }
        }
        Commands::Mcp => {
            let base = data_dir()?;
            let version = env!("CARGO_PKG_VERSION").to_string();
            let (tx, _rx) = tokio::sync::broadcast::channel(16);
            mcp::run_stdio_loop(base, version, tx)?;
        }
        Commands::McpLogs { pid, tail } => {
            let path = match pid {
                Some(p) => mcp::mcp_log_path(p)?,
                None => match mcp::most_recent_mcp_log()? {
                    Some(p) => p,
                    None => {
                        eprintln!(
                            "[legion] no MCP log files found at {}",
                            mcp::mcp_log_dir()?.display()
                        );
                        return Ok(());
                    }
                },
            };
            if !path.exists() {
                eprintln!("[legion] log file does not exist: {}", path.display());
                return Ok(());
            }
            if tail {
                let status = std::process::Command::new("tail")
                    .args(["-F", "-n", "+1"])
                    .arg(&path)
                    .status()
                    .map_err(error::LegionError::Io)?;
                if !status.success() {
                    return Err(error::LegionError::WorkSource(format!(
                        "tail exited with status {status}"
                    )));
                }
            } else {
                let contents = std::fs::read_to_string(&path)?;
                print!("{contents}");
            }
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
        Commands::DaemonStop => {
            let base = data_dir()?;
            if daemon::stop_detached(&base)? {
                eprintln!("[legion] daemon stopped");
            } else {
                eprintln!("[legion] no running daemon to stop");
            }
        }
        Commands::DaemonRestart { port } => {
            let base = data_dir()?;
            daemon::restart_detached(&base, port)?;
        }
        Commands::QualityGate { action } => match action {
            QualityGateAction::Record {
                skill,
                result,
                findings_count,
                details_json,
            } => {
                let (commit_hash, branch) = git_head_commit_and_branch()?;

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

#[cfg(test)]
mod index_banner_tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn idx(repo: &str, lang: &str, updated_at: &str) -> scip::ScipIndex {
        scip::ScipIndex {
            id: "id".to_string(),
            repo: repo.to_string(),
            lang: lang.to_string(),
            content_hash: "h".to_string(),
            blob: vec![1, 2, 3],
            updated_at: updated_at.to_string(),
            deleted_at: None,
        }
    }

    #[test]
    fn empty_when_no_languages_detected() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let banner = render_index_status_banner("legion", &[], &[], now);
        assert!(banner.is_empty());
    }

    #[test]
    fn one_line_summary_when_all_fresh() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let recent = "2026-05-07T10:00:00Z";
        let indexes = vec![idx("legion", "rust", recent)];
        let banner = render_index_status_banner("legion", &["rust"], &indexes, now);
        assert!(banner.starts_with("[Legion] Index status for legion: "));
        assert!(banner.contains("rust: fresh"));
        assert_eq!(banner.lines().count(), 1, "fresh state must be one line");
    }

    #[test]
    fn loud_block_when_missing_language() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let banner = render_index_status_banner("legion", &["rust", "typescript"], &[], now);
        assert!(banner.contains("[Legion] Index status for legion:"));
        assert!(banner.contains("rust: not indexed"));
        assert!(banner.contains("typescript: not indexed"));
        assert!(banner.contains("legion index legion"));
    }

    #[test]
    fn loud_block_when_stale() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        // 14 days ago -- past the 7-day threshold.
        let stale = "2026-04-23T12:00:00Z";
        let indexes = vec![idx("legion", "rust", stale)];
        let banner = render_index_status_banner("legion", &["rust"], &indexes, now);
        assert!(banner.contains("rust: STALE"));
        assert!(banner.contains("14d ago"));
        assert!(banner.contains("legion index legion"));
    }

    #[test]
    fn mixed_state_lists_all_languages_in_order() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let recent = "2026-05-07T10:00:00Z"; // 2h ago
        let stale = "2026-04-23T12:00:00Z"; // 14d ago
        let indexes = vec![
            idx("legion", "rust", recent),
            idx("legion", "typescript", stale),
        ];
        let banner =
            render_index_status_banner("legion", &["rust", "typescript", "python"], &indexes, now);
        assert!(banner.contains("rust: fresh"));
        assert!(banner.contains("typescript: STALE"));
        assert!(banner.contains("python: not indexed"));
        // Multi-line block, not single line.
        assert!(banner.lines().count() > 2);
    }

    #[test]
    fn unparseable_timestamp_is_treated_as_stale() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        let indexes = vec![idx("legion", "rust", "not-a-timestamp")];
        let banner = render_index_status_banner("legion", &["rust"], &indexes, now);
        assert!(banner.contains("rust: STALE"));
        assert!(banner.contains("unknown age"));
    }

    #[test]
    fn age_humanized_into_appropriate_unit() {
        let now = Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap();
        // 90 minutes ago -> "1h ago"
        let one_hr_ago = "2026-05-07T10:30:00Z";
        let banner = render_index_status_banner(
            "legion",
            &["rust"],
            &[idx("legion", "rust", one_hr_ago)],
            now,
        );
        assert!(banner.contains("1h ago"));
    }
}
