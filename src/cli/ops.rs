//! Operational surfaces: telemetry, uncertainty, cluster, audit log, mesh,
//! usage, health (carved from main.rs, #610).

use std::path::PathBuf;

use clap::Subcommand;

use crate::cli::datadir::data_dir;
use crate::cli::util::format_age;
use crate::cli::util::open_db;
use crate::{cluster, error, health, mesh, telemetry, uncertainty, usage};

#[derive(Subcommand)]
pub(crate) enum TelemetryAction {
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

    /// Summarize `sym etc` / `sym tree` usage by query shape (#713): the
    /// PRIMARY success metric for the sym-etc epic (#704). Rewording the
    /// grep/find guard changes what gets classified as a bypass
    /// mid-experiment, so raw bypass counts (`summary` above) are the
    /// SECONDARY signal only. This reads `etc-usage.jsonl` instead of
    /// `bypass.jsonl` and reports usage volume + zero-result rate per
    /// shape (find-content/tree/extract/find-file) -- whether the
    /// sanctioned surface actually answers.
    EtcSummary {
        /// Drop rows older than this duration before summarizing.
        #[arg(long)]
        since: Option<String>,
        /// Restrict to one query shape: find-content, tree, extract, find-file.
        #[arg(long)]
        command: Option<String>,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum UncertaintyAction {
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
pub(crate) enum ClusterAction {
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
pub(crate) enum MeshAction {
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

/// Dispatch `legion uncertainty ...`. Non-blocking emit posture: emit
/// failures log to stderr and return Ok so an upstream hook can never
/// abort the agent on a telemetry-shaped problem. Witness / calibration /
/// orphans surface errors normally -- those are explicit user actions.
fn run_uncertainty(action: UncertaintyAction) -> error::Result<()> {
    use std::io::Write;
    let database = open_db()?;
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

pub(crate) fn handle_telemetry(action: TelemetryAction) -> error::Result<()> {
    match action {
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
        TelemetryAction::EtcSummary {
            since,
            command,
            json,
        } => {
            let since_dur = match since {
                Some(s) => Some(telemetry::parse_duration(&s)?),
                None => None,
            };
            let rows = telemetry::list_etc_usage(since_dur, command.as_deref())?;
            let summary = telemetry::summarize_etc_usage(&rows);
            if json {
                println!("{}", serde_json::to_string(&summary)?);
            } else if summary.is_empty() {
                println!("[legion] no sym etc/tree usage recorded in scope");
            } else {
                use std::io::Write;
                let stdout = std::io::stdout();
                let mut out = stdout.lock();
                writeln!(
                    out,
                    "{:<14} {:<8} {:<12} {:<8}",
                    "command", "count", "zero-res %", "errors"
                )?;
                for row in &summary {
                    writeln!(
                        out,
                        "{:<14} {:<8} {:<12.1} {:<8}",
                        row.command,
                        row.count,
                        row.zero_result_pct * 100.0,
                        row.error_count,
                    )?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn handle_uncertainty(action: UncertaintyAction) -> error::Result<()> {
    run_uncertainty(action)?;
    Ok(())
}

pub(crate) fn handle_cluster(action: ClusterAction) -> error::Result<()> {
    let base = data_dir()?;
    cluster::handle_cluster_command(&base, action)?;
    Ok(())
}

pub(crate) fn handle_audit(
    repo: Option<String>,
    action: Option<String>,
    limit: usize,
    json: bool,
) -> error::Result<()> {
    let database = open_db()?;
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
    Ok(())
}

pub(crate) fn handle_mesh(action: MeshAction) -> error::Result<()> {
    let database = open_db()?;
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
                    serde_json::to_string_pretty(&rows).expect("ranked rows serialize infallibly")
                );
            } else if ranked.is_empty() {
                eprintln!(
                    "[legion] no samples yet -- run `legion statusline` on at least one node first"
                );
            } else {
                println!("host                 score    5h%    7d%   last_turn       age  status");
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
    Ok(())
}

pub(crate) fn handle_usage(
    session: Option<String>,
    since: Option<String>,
    by_session: bool,
    by_repo: bool,
    json: bool,
) -> error::Result<()> {
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
            return Err(error::LegionError::ExitWith(1));
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

    let sessions = usage::discover_sessions(&home, since_str.as_deref(), session.as_deref());

    if session.is_some() && sessions.is_empty() {
        eprintln!(
            "[legion] error: session not found: {}",
            session.as_deref().unwrap_or("")
        );
        return Err(error::LegionError::ExitWith(1));
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
                serde_json::to_string_pretty(&sessions).map_err(error::LegionError::Json)?
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
    Ok(())
}

pub(crate) fn handle_health(
    history: Option<String>,
    all_hosts: bool,
    json: bool,
) -> error::Result<()> {
    let database = open_db()?;

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
    Ok(())
}
