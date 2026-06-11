//! `legion schedule` handlers (carved from main.rs, #610).

use clap::Subcommand;

use crate::cli::util::open_db;
use crate::{db, error};

#[derive(Subcommand)]
pub(crate) enum ScheduleAction {
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

pub(crate) fn handle(action: ScheduleAction) -> error::Result<()> {
    let database = open_db()?;

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
    Ok(())
}
