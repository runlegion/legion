//! CLI surface for the legion binary: the clap `Cli`/`Commands` tree.
//! Domain handlers live in the sibling modules; `main.rs` owns dispatch.

pub(crate) mod autonomy;
pub(crate) mod datadir;
pub(crate) mod document;
pub(crate) mod index_cmd;
pub(crate) mod issue;
pub(crate) mod kanban;
pub(crate) mod memory;
pub(crate) mod misc;
pub(crate) mod ops;
pub(crate) mod pr;
pub(crate) mod push;
pub(crate) mod schedule;
pub(crate) mod signal;
pub(crate) mod spec_gen;
pub(crate) mod util;
pub(crate) mod verify;
pub(crate) mod watch;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use self::autonomy::AutonomyAction;
use self::document::DocumentAction;
use self::index_cmd::SymAction;
use self::issue::{IssueAction, SubIssueAction};
use self::kanban::KanbanAction;
use self::memory::DedupeMode;
use self::misc::TaskAction;
use self::ops::{ClusterAction, MeshAction, TelemetryAction, UncertaintyAction};
use self::pr::PrAction;
use self::schedule::ScheduleAction;
use self::verify::QualityGateAction;
use self::watch::WatchAction;

/// `legion --version` output, including the build id (#698) so the value
/// matches what the daemon reports at /health. The semver version stays the
/// FIRST token after the program name ("legion <version> (build <id>)") so
/// the supervisor's `legion --version | awk '{print $2}'` version parse is
/// unaffected; the build id is parsed separately from the parenthetical.
const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (build ",
    env!("LEGION_BUILD_ID"),
    ")"
);

#[derive(Parser)]
#[command(
    name = "legion",
    about = "Agent specialization through deliberate practice",
    version = LONG_VERSION
)]
pub(crate) struct Cli {
    /// Show informational messages on stderr (quiet by default)
    #[arg(long, short, global = true)]
    pub(crate) verbose: bool,

    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
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

    /// Permanently delete a reflection by id, or (with --persist) archive
    /// it instead.
    ///
    /// Default (no --persist): destructive. No soft-delete, no undo.
    /// Removes the row from both the SQLite reflections table and the
    /// tantivy search index. Used to retire stale workaround reflections,
    /// demonstrably-wrong reflections, or personal data that should not
    /// persist in the corpus.
    ///
    /// The optional `--repo` flag is a safety check: when provided,
    /// the delete (or archive) is refused unless the reflection's actual
    /// repo matches. Prevents accidentally nuking the wrong reflection
    /// when working with a similarly-shaped id.
    Forget {
        /// Reflection id to forget.
        #[arg(long)]
        id: String,

        /// Optional safety check: refuse the operation unless the
        /// reflection's repo matches this value.
        #[arg(long)]
        repo: Option<String>,

        /// Archive instead of delete (#782): moves the reflection to
        /// #457's cold tier. The row and search index entry survive --
        /// it drops out of hot `recall` / `whoami` / `whatami` but stays
        /// reachable via `recall --archives` / `--include-archives`
        /// (including the `--domain` and `--latest` paths). One-way
        /// today: legion ships no un-persist verb yet, matching
        /// `document`'s archive (also writer-only).
        #[arg(long)]
        persist: bool,
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
        /// --include-archives. Applies to the BM25/hybrid, --domain, and
        /// --latest paths (#782); --cosine-only stays hot-mode regardless
        /// (it ranks by embedding similarity with no archive-mode join).
        /// See #457.
        #[arg(long)]
        archives: bool,

        /// Search BOTH hot and archived reflections. Mutually exclusive
        /// with --archives. Same --cosine-only scope caveat as --archives.
        #[arg(long, conflicts_with = "archives")]
        include_archives: bool,

        /// Only include reflections created on or after this date
        /// (YYYY-MM-DD, <N>d, <N>w, today, yesterday). Applies as a hard
        /// `created_at` predicate before ranking, in every recall mode
        /// (#786).
        #[arg(long)]
        since: Option<String>,

        /// Only include reflections created on or before this date (same
        /// grammar as --since).
        #[arg(long, conflicts_with = "on")]
        until: Option<String>,

        /// Exactly this date; sugar for --since X --until X.
        #[arg(long, conflicts_with_all = ["since", "until"])]
        on: Option<String>,
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

        /// Only include reflections created on or after this date
        /// (YYYY-MM-DD, <N>d, <N>w, today, yesterday). Reflection mode
        /// only (#786); has no effect on --symbol.
        #[arg(long)]
        since: Option<String>,

        /// Only include reflections created on or before this date (same
        /// grammar as --since).
        #[arg(long, conflicts_with = "on")]
        until: Option<String>,

        /// Exactly this date; sugar for --since X --until X.
        #[arg(long, conflicts_with_all = ["since", "until"])]
        on: Option<String>,
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
    /// Watch wakes the recipient when `--verb` is in the wake-worthy set.
    /// Wake-worthy verbs: `question`, `request`, `handoff`, `correction`,
    /// `proposal`, `decision`, `routing` -- these spawn an asleep recipient.
    /// `rfc` is also wake-worthy but additionally requires a `budget:` detail
    /// in `--details` (e.g. `--details "budget:2h"`).
    ///
    /// Informational verbs: `announce`, `ack`, `info`, `answer` -- these
    /// deliver to live sessions via the channel push but do not wake an
    /// asleep recipient. Silence is acknowledgment; do not send empty acks.
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

        /// Signal verb.
        /// Wake-worthy (spawn an asleep recipient): question, request,
        /// handoff, correction, proposal, decision, rfc, routing.
        /// rfc additionally requires --details budget:<amount>.
        /// Informational (deliver to live sessions only, no wake):
        /// announce, ack, info, answer.
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

        /// Only include posts created on or after this date (YYYY-MM-DD,
        /// <N>d, <N>w, today, yesterday). Applies to the `--repo` bullpen
        /// listing path (#786); has no effect on `--count`, `--archive`,
        /// or `--archived`.
        #[arg(long)]
        since: Option<String>,

        /// Only include posts created on or before this date (same
        /// grammar as --since).
        #[arg(long, conflicts_with = "on")]
        until: Option<String>,

        /// Exactly this date; sugar for --since X --until X.
        #[arg(long, conflicts_with_all = ["since", "until"])]
        on: Option<String>,
    },

    /// Surface cross-repo highlights for a session start
    Surface {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Only include highlights created on or after this date
        /// (YYYY-MM-DD, <N>d, <N>w, today, yesterday). Applies across all
        /// four surfaced queries (recent posts, high-value cross-repo,
        /// chain extensions, pending tasks) except the recent-posts board
        /// query's 24h default, which an explicit range overrides rather
        /// than composes with (#786).
        #[arg(long)]
        since: Option<String>,

        /// Only include highlights created on or before this date (same
        /// grammar as --since).
        #[arg(long, conflicts_with = "on")]
        until: Option<String>,

        /// Exactly this date; sugar for --since X --until X.
        #[arg(long, conflicts_with_all = ["since", "until"])]
        on: Option<String>,
    },

    /// Print identity reflections for a repo (alias for recall --domain identity)
    Whoami {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Maximum number of identity reflections to return (plain listing
        /// mode only; ignored when --generate is set)
        #[arg(long, default_value_t = 50)]
        limit: usize,

        /// Enter generate mode: gather claimed-half (self-authored) and
        /// given-half (cross-agent) source material for an identity rebuild
        /// (default), or write an authored replacement (with --apply).
        #[arg(long)]
        generate: bool,

        /// Repo (must be present in watch.toml) holding the agent's bylined
        /// writing. Required with --generate unless --apply is also set.
        #[arg(long, requires = "generate")]
        vault_repo: Option<String>,

        /// Comma-separated byline(s)/author name(s) to match against each
        /// candidate file's frontmatter `author` field. Required with
        /// --generate unless --apply is also set.
        #[arg(long, value_delimiter = ',', requires = "generate")]
        byline: Vec<String>,

        /// Switch --generate into apply mode: perform the guarded swap from
        /// an authored manifest instead of gathering source material.
        #[arg(long, requires = "generate")]
        apply: bool,

        /// Path to the authored manifest (JSON: IdentityManifest shape).
        /// Required with --apply.
        #[arg(long, requires = "apply")]
        from_file: Option<PathBuf>,

        /// Compute and report the apply plan without writing or deleting
        /// anything. Apply mode only.
        #[arg(long, requires = "apply")]
        dry_run: bool,
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
        /// loud on stale or missing. A missing language names WHY: "not
        /// indexed yet" (running this command fixes it) versus "indexer
        /// unavailable" (the binary that language needs is not on PATH,
        /// so this command cannot fix it by itself) -- the coverage
        /// guarantee from #713, so a repo showing only some languages
        /// indexed reads as a known, explained gap rather than "sym does
        /// not cover language X" in general. Requires `<repo>` and
        /// `--status`. Used by `plugin/hooks/session-start.sh` and the
        /// `legion-explore` agent's preflight so agents see whether
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
    /// signal that sym/recall is missing an answer agents expected. Also
    /// reads `sym etc`/`sym tree` usage from the sibling `etc-usage.jsonl`
    /// (`etc-summary`, #713) -- the PRIMARY sym-etc epic (#704) metric,
    /// since rewording the guard changes what counts as a bypass mid-
    /// experiment.
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

        /// Card ID to mark as complete (optional). When the card has
        /// acceptance criteria, a clean `legion verify` verdict must
        /// exist for it before Done is accepted; exits non-zero otherwise.
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

    /// Push a branch to origin -- the sanctioned in-band push path (#791).
    ///
    /// Resolves the checkout that has `--branch` checked out via `git
    /// worktree list --porcelain` and runs the push FROM that checkout, so
    /// the push-from-own-checkout doctrine is enforced by the tool instead
    /// of agent discipline: the pre-push hook reviews the CWD's checked-out
    /// branch, not the ref being pushed, so pushing a ref from the wrong
    /// checkout silently reviews (or blocks on) the wrong diff. Hard errors
    /// if no checkout has the branch, naming the worktrees it searched.
    ///
    /// Refuses to push `main`/`master` (Sean merges; agents never push main)
    /// and refuses any `--branch` value shaped like a git flag or a
    /// force/retarget refspec (leading `-`/`+`, embedded `:`, whitespace) --
    /// there is no `--force` flag on this command, and a crafted branch
    /// value cannot recover force semantics either. Sets upstream (`-u
    /// origin <branch>`) on every push, which is a no-op after the first.
    /// Every attempt (success or failure) is audit-logged with the branch,
    /// resolved checkout path, and head SHA.
    Push {
        /// Repository name (identifies the calling agent for the audit log)
        #[arg(long)]
        repo: String,

        /// Branch to push. Defaults to the CWD's checked-out branch.
        #[arg(long)]
        branch: Option<String>,
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

    /// Verify a card's acceptance criteria before it can reach Done (#520).
    /// Reads per-criterion verdicts (pass/fail/uncertain + evidence) as JSON
    /// from --verdicts-file or stdin, loads the card's acceptance criteria,
    /// and decides the card's fate: every criterion Pass with evidence allows
    /// ->Done (records a clean legion-verify:<card> gate); any Fail hard-blocks;
    /// any Uncertain (or a Pass with no evidence) routes the card to NeedsInput
    /// for a human. A card with no acceptance criteria is blocked outright.
    ///
    /// Acceptance-criteria precedence: when the card has a `document_id`,
    /// the bound document's top-level `verification.acceptance` array is
    /// consulted first; a missing or empty block (or an unparseable payload)
    /// falls back to `tasks.acceptance`. A dangling `document_id` -- the
    /// document does not exist -- is a hard error.
    Verify {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Kanban card ID whose acceptance criteria are being verified.
        #[arg(long)]
        card: String,

        /// Path to a JSON file with the per-criterion verdicts. Reads stdin
        /// when omitted. Shape: `[{"criterion": "...", "verdict":
        /// "pass|fail|uncertain", "evidence": "..."}, ...]`.
        #[arg(long)]
        verdicts_file: Option<String>,

        /// Spec-revision deviation gate (#554): assert that the work
        /// diverges from the card's frozen acceptance criteria, naming why.
        /// Checked against the card's `ReplanRecord` before verdicts are
        /// read: a ratified record lets verify proceed against the revised
        /// AC as normal; without one, verify hard-blocks as an unratified
        /// deviation (docs/decisions/2026-05-31-spec-revision-protocol.md).
        #[arg(long)]
        deviation: Option<String>,
    },

    /// Weekly autonomy budget (#524): the governor on self-directed work.
    /// `status` shows the window; `gate` asks whether a unit of autonomous
    /// work (self-acceptance or free-time) fits, recording the spend when it
    /// does. Operator-requested work bypasses with --operator.
    Autonomy {
        #[command(subcommand)]
        action: AutonomyAction,
    },

    /// Board-derived goal (#525): print the active Accepted card's acceptance
    /// criteria framed as the agent's completion condition, to carry across
    /// turns. Native `/goal` cannot be set programmatically, so SessionStart
    /// emits this each session; it is empty when nothing is in progress.
    Goal {
        /// Repository name (the agent whose active card to read).
        #[arg(long)]
        repo: String,
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

    /// Generate requirement documents from service-design artifacts (#527).
    ///
    /// Reads all non-archived service-design documents (types: persona, journey,
    /// blueprint, painmatrix, ecosystem) on the given surface, derives one
    /// requirement candidate per moment_of_truth, validates each candidate
    /// against the requirement schema, and inserts new requirement documents
    /// plus born-Backlog kanban cards.  Re-running on unchanged input is safe
    /// (idempotent): existing (traces_to, surface) pairs are skipped.
    SpecGen {
        /// Functional surface to generate requirements for. This is the
        /// `surface` field stored on the source service-design documents
        /// (e.g. "payments", "onboarding"), NOT a git repository name.
        /// All non-archived service-design documents whose `surface` field
        /// matches this value are used as input.
        #[arg(long)]
        repo: String,
    },
}
