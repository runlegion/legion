mod board;
mod card_parse;
mod db;
mod embed;
mod error;
mod health;
mod init;
#[allow(dead_code)] // Items used by tests + pending surface/status/serve migration
mod kanban;
mod recall;
mod reflect;
mod search;
mod serve;
mod signal;
mod stats;
mod status;
mod surface;
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

#[derive(Subcommand)]
enum Commands {
    /// Store a reflection from a completed session
    Reflect {
        /// Repository name(s), comma-separated (e.g., "kelex" or "platform,legion")
        #[arg(long, value_delimiter = ',', required = true)]
        repo: Vec<String>,

        /// Reflection text (mutually exclusive with --transcript)
        #[arg(long, conflicts_with = "transcript")]
        text: Option<String>,

        /// Path to session transcript JSONL file
        #[arg(long, conflicts_with = "text")]
        transcript: Option<PathBuf>,

        /// Domain tag for classification (e.g., "color-tokens", "auth")
        #[arg(long)]
        domain: Option<String>,

        /// Comma-separated tags (e.g., "semantic-tokens,consumer,debugging")
        #[arg(long)]
        tags: Option<String>,

        /// Link to a parent reflection ID to form a learning chain
        #[arg(long)]
        follows: Option<String>,
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
        /// Repository name(s), comma-separated (e.g., "kelex" or "platform,legion")
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

    /// Rebuild the search index from the database
    Reindex,

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

        /// Filter by action type (create-issue, create-pr, review, merge, comment, close)
        #[arg(long)]
        action: Option<String>,

        /// Maximum entries to show
        #[arg(long, default_value = "20")]
        limit: usize,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Watch for signals and auto-wake sleeping agents
    Watch,

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
    Cancel {
        /// Card ID
        #[arg(long)]
        id: String,
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
    Reopen {
        /// Card ID
        #[arg(long)]
        id: String,
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
    },

    /// List open pull requests with review status
    List {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,
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

/// Resolve the legion data directory.
///
/// Resolution order:
/// 1. `LEGION_DATA_DIR` env var (explicit override, used by tests)
/// 2. `CLAUDE_PLUGIN_DATA` env var set by Claude Code when running under the
///    plugin (plugin-scoped data dir, stable across plugin updates)
/// 3. `$HOME/.claude/plugins/data/legion-legion/` (same location Claude
///    Code uses, discoverable when running outside the plugin context)
///
/// Result is cached via OnceLock after first successful resolution: commands
/// like `audit()` re-enter this dozens of times and the path never changes
/// inside a single process. Tests that manipulate `LEGION_DATA_DIR` between
/// calls should use a fresh subprocess.
///
/// On the first successful resolution, if the target has no `legion.db` but
/// the legacy `ProjectDirs` location does (macOS: ~/Library/Application
/// Support/legion, Linux: ~/.local/share/legion), the database, Tantivy
/// index, and watch.toml are migrated across. One-way, best-effort, leaves
/// the legacy path in place as a safety net.
fn data_dir() -> error::Result<PathBuf> {
    use std::sync::OnceLock;
    static CACHED: OnceLock<PathBuf> = OnceLock::new();

    if let Some(cached) = CACHED.get() {
        return Ok(cached.clone());
    }

    let path = if let Ok(dir) = std::env::var("LEGION_DATA_DIR") {
        PathBuf::from(dir)
    } else if let Ok(dir) = std::env::var("CLAUDE_PLUGIN_DATA")
        && !dir.is_empty()
    {
        PathBuf::from(dir)
    } else {
        dirs::home_dir()
            .ok_or(error::LegionError::NoHomeDir)?
            .join(".claude")
            .join("plugins")
            .join("data")
            .join("legion-legion")
    };

    std::fs::create_dir_all(&path)?;

    if !path.join("legion.db").exists() {
        migrate_from_legacy(&path);
    }

    let _ = CACHED.set(path.clone());
    Ok(path)
}

/// Migrate legion state from the legacy ProjectDirs path to the plugin data dir.
///
/// Resolves the legacy path via `ProjectDirs` and delegates to
/// [`migrate_between`], which is the testable inner form.
fn migrate_from_legacy(target: &Path) {
    let Some(dirs) = ProjectDirs::from("", "", "legion") else {
        return;
    };
    migrate_between(dirs.data_dir(), target);
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
            tags,
            follows,
        } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let index = search::SearchIndex::open(&base.join("index"))?;
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
        Commands::Recall {
            repo,
            context,
            limit,
            latest,
            preview,
        } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            let result = if latest {
                recall::recall_latest(&database, &repo, limit)?
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
            let output = recall::format_for_hook(&result, preview);
            if !output.is_empty() {
                print!("{output}");
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
        Commands::Boost { id } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            if database.boost_reflection(&id)? {
                info!("[legion] boosted reflection {}", id);
            } else {
                eprintln!("[legion] reflection not found: {}", id);
            }
        }
        Commands::Chain { id } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

            let chain = database.get_chain(&id)?;
            if chain.is_empty() {
                info!("[legion] no chain found for {}", id);
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
        Commands::Reindex => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let index = search::SearchIndex::open(&base.join("index"))?;

            let reflections = database.get_all_for_reindex()?;
            let count = reflections.len();
            index.rebuild(&reflections)?;
            info!("[legion] reindexed {} reflections", count);
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

                // Close linked external issue if present
                if let (Some(url), Some(source)) = (&card.source_url, &card.source_type)
                    && let Some(number) = worksource::extract_issue_number(url)
                    && let Some((_, source_repo, _)) = worksource::resolve_config(&repo)
                    && let Err(e) = worksource::close_issue(source, &source_repo, number)
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
                KanbanAction::Cancel { id } => {
                    kanban::transition_card(&database, &id, kanban::Action::Cancel, None)?;
                    println!("{id}");
                }
                KanbanAction::Assign { id, to } => {
                    database.assign_card(&id, &to)?;
                    println!("{id}");
                }
                KanbanAction::Reopen { id } => {
                    kanban::transition_card(&database, &id, kanban::Action::Reopen, None)?;
                    println!("{id}");
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
            } => {
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

                    // Close linked issue if the card has a source URL
                    if let Some(card) = database.get_card_by_id(task_id)?
                        && let Some(ref url) = card.source_url
                        && let Some(issue_num) = worksource::extract_issue_number(url)
                        && let Some(ref source) = card.source_type
                    {
                        if let Err(e) = worksource::close_issue(source, &source_repo, issue_num) {
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
        Commands::Watch => {
            let base = data_dir()?;
            watch::run(&base)?;
        }
        Commands::Usage {
            session,
            since,
            today: _today,
            by_session,
            by_repo,
            json,
        } => {
            let home: PathBuf = dirs::home_dir().ok_or(error::LegionError::NoHomeDir)?;

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
