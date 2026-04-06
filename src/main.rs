mod board;
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
mod watch;
mod worksource;

use std::path::PathBuf;
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
    about = "Agent specialization through deliberate practice"
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
        #[arg(long)]
        repo: String,

        /// Only show unread count instead of full bullpen
        #[arg(long)]
        count: bool,

        /// Show only signals (structured coordination messages)
        #[arg(long, conflicts_with = "musings")]
        signals: bool,

        /// Show only musings (natural language posts)
        #[arg(long, conflicts_with = "signals")]
        musings: bool,
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

    /// List cards for a repo
    List {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Show outbound cards (created by this repo) instead of inbound
        #[arg(long)]
        from: bool,
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
}

fn data_dir() -> error::Result<PathBuf> {
    let path = match std::env::var("LEGION_DATA_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            let dirs = ProjectDirs::from("", "", "legion").ok_or(error::LegionError::NoDataDir)?;
            dirs.data_dir().to_path_buf()
        }
    };
    std::fs::create_dir_all(&path)?;
    Ok(path)
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
            let output = recall::format_for_hook(&result);
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
                board::post_from_text_with_meta,
                board::post_from_transcript_with_meta,
                "posting",
            )?;

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
        } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;

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
        Commands::Status { repo } => {
            let base = data_dir()?;
            let database = db::Database::open(&base.join("legion.db"))?;
            let output = status::get_status(&database, &repo)?;
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
                    )?;
                    println!("{id}");
                }
                KanbanAction::List { repo, from } => {
                    let direction = if from {
                        kanban::Direction::Outbound
                    } else {
                        kanban::Direction::Inbound
                    };
                    let cards = kanban::list_cards(&database, &repo, direction)?;
                    let output = kanban::format_card_list(&cards, &repo, direction);
                    if output.is_empty() {
                        info!("[legion] no cards found");
                    } else {
                        print!("{output}");
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

                println!("{}", created.url);
                eprintln!(
                    "[legion] created issue #{} on {}",
                    created.number, source_repo
                );
            }
        },
        Commands::Watch => {
            let base = data_dir()?;
            watch::run(&base)?;
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
