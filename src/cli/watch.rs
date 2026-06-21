//! `legion watch` handlers and watch-status rendering (carved from main.rs, #610).

use clap::Subcommand;

use crate::cli::datadir::data_dir;
use crate::cli::index_cmd::spawn_background_indexer;
use crate::cli::util::open_db;
use crate::{db, error, watch};

#[derive(Subcommand, Debug)]
pub(crate) enum WatchAction {
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

    /// Record an interactive (human-started) session lock for a repo (#583).
    ///
    /// Called from the SessionStart hook with the Claude session PID so watch
    /// does not spawn a duplicate agent while an interactive session is open.
    /// Uses a separate `.session` lock file; the gate is PID-liveness alone
    /// (no TTL) because an idle interactive session must still hold the gate.
    /// Non-fatal and fast -- hook failure never blocks Claude Code startup.
    SessionStart {
        /// Repo name to lock (basename of the working directory).
        #[arg(long)]
        repo: String,

        /// PID of the interactive session process to track. When omitted,
        /// falls back to the current process id -- prefer supplying $PPID
        /// from the hook so the tracked pid is the long-lived session process.
        #[arg(long)]
        pid: Option<u32>,
    },

    /// Optimistic stop-hook handoff for the watch reaper (#493). Writes
    /// `exit_observed_at` on the wake_attempts row so the reaper can
    /// skip a poll cycle. PTY EOF + PID-poll remain authoritative; this
    /// is a speed-up only. Idempotent; exits 0 on a missing row so a
    /// hook failure cannot block Claude Code's Stop.
    ///
    /// For interactive (human-started) sessions, `$LEGION_WAKE_ATTEMPT_ID`
    /// is empty; pass `--repo <name>` so the `.session` file is still
    /// cleaned up even when no wake_attempts row exists (#673 fix 2).
    SessionEnd {
        /// UUIDv7 attempt id, sourced from $LEGION_WAKE_ATTEMPT_ID.
        #[arg(long)]
        attempt_id: String,

        /// Repo name for interactive sessions that have no wake_attempts row.
        /// When provided and the attempt-id lookup finds no row, this repo's
        /// `.session` file is removed. Idempotent; ignored when the row exists
        /// (the repo is resolved from the row instead).
        #[arg(long)]
        repo: Option<String>,
    },

    /// Show whether the watch daemon is alive, stale, or absent (#581).
    ///
    /// Reads the heartbeat row the daemon writes on each health tick and
    /// prints one of:
    ///   alive  -- beat is newer than two poll intervals
    ///   stale  -- beat exists but is older than that threshold
    ///   absent -- no heartbeat has ever been written
    ///
    /// Also prints the daemon version, watched repo count, and the most
    /// recent wake attempts so the operator can verify the daemon is not
    /// just alive but also doing work.
    Status {
        /// Number of recent wake attempts to show (default 5).
        #[arg(long, default_value = "5")]
        recent: u32,

        /// Staleness threshold in seconds. A beat older than this is
        /// reported as stale. Defaults to twice the poll interval (120s).
        #[arg(long, default_value = "120")]
        stale_after_secs: u64,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum LeaseAction {
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

/// Liveness of a heartbeat row, factored out of [`run_watch_status`] so the
/// alive/stale boundary is unit-testable without going through stdout or the
/// wall clock. `Absent` (no row at all) is handled by the caller; this only
/// classifies an existing beat's `updated_at`.
#[derive(Debug, PartialEq, Eq)]
enum BeatLiveness {
    /// Beat is newer than the stale threshold.
    Alive,
    /// Beat is older than the threshold by `age_secs` seconds.
    Stale { age_secs: i64 },
    /// Beat timestamp is in the future relative to `now` (clock skew or an
    /// unparseable timestamp coerced to `i64::MAX`). Treated as not-alive so
    /// the failure is visible rather than silently passing as healthy.
    ClockSkew { age_secs: i64 },
}

/// Classify a heartbeat's `updated_at` against `now` and the stale window.
///
/// An unparseable timestamp coerces to `i64::MAX` age, which lands in `Stale`
/// -- a corrupt beat reads as dead, never alive.
fn classify_beat(
    updated_at: &str,
    now: chrono::DateTime<chrono::Utc>,
    stale_after_secs: u64,
) -> BeatLiveness {
    let age_secs: i64 = chrono::DateTime::parse_from_rfc3339(updated_at)
        .ok()
        .map(|ts| (now - ts.with_timezone(&chrono::Utc)).num_seconds())
        .unwrap_or(i64::MAX);

    if age_secs < 0 {
        BeatLiveness::ClockSkew { age_secs }
    } else if (age_secs as u64) < stale_after_secs {
        BeatLiveness::Alive
    } else {
        BeatLiveness::Stale { age_secs }
    }
}

/// Human-friendly age for a non-negative second count: `42s ago`, `5m ago`,
/// `3h ago`.
fn humanize_age(age_secs: i64) -> String {
    if age_secs < 60 {
        format!("{age_secs}s ago")
    } else if age_secs < 3600 {
        format!("{}m ago", age_secs / 60)
    } else {
        format!("{}h ago", age_secs / 3600)
    }
}

/// Render the `legion watch status` output.
///
/// Classifies the watch daemon as alive, stale, or absent by comparing
/// the heartbeat's `updated_at` against `now - stale_after_secs`. Also
/// prints the daemon version, repo count, and recent wake attempts so
/// the operator can confirm that the daemon is not just alive but working.
fn run_watch_status(db: &db::Database, recent: u32, stale_after_secs: u64) -> error::Result<()> {
    use std::io::Write;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let beat = db.get_watch_heartbeat(None)?;

    match beat {
        None => {
            writeln!(out, "status:  absent")?;
            writeln!(
                out,
                "         no heartbeat row found -- daemon has never run or DB is fresh"
            )?;
        }
        Some(ref hb) => {
            let now = chrono::Utc::now();
            match classify_beat(&hb.updated_at, now, stale_after_secs) {
                BeatLiveness::Alive => {
                    writeln!(out, "status:  alive")?;
                }
                BeatLiveness::ClockSkew { .. } => {
                    writeln!(
                        out,
                        "status:  stale  (last beat: clock skew (future timestamp))"
                    )?;
                }
                BeatLiveness::Stale { age_secs } => {
                    writeln!(
                        out,
                        "status:  stale  (last beat: {})",
                        humanize_age(age_secs)
                    )?;
                }
            }

            writeln!(out, "host:    {}", hb.host)?;
            writeln!(out, "pid:     {}", hb.pid)?;
            writeln!(out, "version: {}", hb.version)?;
            writeln!(out, "repos:   {}", hb.repo_count)?;
            writeln!(out, "beat:    {}", hb.updated_at)?;

            if let Some(ref summary) = hb.last_spawn_summary {
                writeln!(out, "last:    {summary}")?;
            }
        }
    }

    // Recent wake attempts.
    let attempts = db.recent_wake_attempts(recent)?;
    if attempts.is_empty() {
        writeln!(out, "\nwake attempts: none recorded")?;
    } else {
        writeln!(out, "\nrecent wake attempts ({}):", attempts.len())?;
        writeln!(
            out,
            "  {:<38} {:<12} {:<12} updated_at",
            "attempt_id", "persona", "state"
        )?;
        for a in &attempts {
            // Truncate attempt_id to first 8 chars of the UUID for brevity.
            let id_short: String = a.attempt_id.chars().take(8).collect();
            let persona_short: String = a.persona_id.chars().take(12).collect();
            writeln!(
                out,
                "  {:<38} {:<12} {:<12} {}",
                id_short,
                persona_short,
                a.state.as_str(),
                db::format_date(&a.updated_at),
            )?;
        }
    }

    out.flush()?;
    Ok(())
}

pub(crate) fn handle(action: Option<WatchAction>) -> error::Result<()> {
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
            let added =
                watch::add_repo_to_config(&config_path, &effective_name, &path, agent.as_deref())?;
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
                // A shared `agent` across repos is intentional (#595):
                // the field marks the maintaining persona, and the wake
                // lease dedups a directed signal to one session, so a
                // shared agent is not a collision worth warning about.
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
            let db = open_db()?;
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
        Some(WatchAction::SessionStart { repo, pid }) => {
            let effective_pid: u32 = pid.unwrap_or_else(std::process::id);
            // TTL only affects the `.lock` gate; the `.session` file is
            // PID-liveness only. Using the default avoids reading watch.toml
            // on every SessionStart hook call (matches record_session_end).
            let locks =
                watch::SessionLockTracker::new(&base, watch::default_session_lock_ttl_secs());
            if let Err(e) = locks.record_interactive(&repo, effective_pid) {
                eprintln!(
                    "[legion watch] session-start: failed to write interactive lock for {}: {}",
                    repo, e
                );
            }
        }
        Some(WatchAction::SessionEnd { attempt_id, repo }) => {
            let db = open_db()?;
            watch::record_session_end(&db, &attempt_id, repo.as_deref(), &base)?;
        }
        Some(WatchAction::Status {
            recent,
            stale_after_secs,
        }) => {
            let db = open_db()?;
            run_watch_status(&db, recent, stale_after_secs)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod watch_status_tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn now() -> chrono::DateTime<chrono::Utc> {
        Utc.with_ymd_and_hms(2026, 5, 7, 12, 0, 0).unwrap()
    }

    #[test]
    fn classify_beat_alive_within_threshold() {
        // 20s old, 90s window -> alive.
        let beat = "2026-05-07T11:59:40Z";
        assert_eq!(classify_beat(beat, now(), 90), BeatLiveness::Alive);
    }

    #[test]
    fn classify_beat_stale_past_threshold() {
        // 5 minutes old, 90s window -> stale, age 300s.
        let beat = "2026-05-07T11:55:00Z";
        assert_eq!(
            classify_beat(beat, now(), 90),
            BeatLiveness::Stale { age_secs: 300 }
        );
    }

    #[test]
    fn classify_beat_future_timestamp_is_clock_skew() {
        // 10s in the future -> clock skew, never alive.
        let beat = "2026-05-07T12:00:10Z";
        assert!(matches!(
            classify_beat(beat, now(), 90),
            BeatLiveness::ClockSkew { .. }
        ));
    }

    #[test]
    fn classify_beat_unparseable_reads_as_stale() {
        // A corrupt beat must read as dead, never alive.
        assert!(matches!(
            classify_beat("not-a-timestamp", now(), 90),
            BeatLiveness::Stale { .. }
        ));
    }

    #[test]
    fn humanize_age_picks_unit() {
        assert_eq!(humanize_age(42), "42s ago");
        assert_eq!(humanize_age(300), "5m ago");
        assert_eq!(humanize_age(7200), "2h ago");
    }
}
