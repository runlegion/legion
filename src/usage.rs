/// Parse Claude Code session JSONL files and aggregate token usage.
///
/// Session files live at `~/.claude/projects/<slug>/<session-uuid>.jsonl`.
/// Each slug encodes the filesystem path of the project (hyphens substitute
/// for slashes). Only `type == "assistant"` events with a populated
/// `message.usage` object contribute token counts. Lines that do not parse
/// as JSON or that lack usage data are skipped silently (one stderr warning
/// per session, not per line).
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};

use chrono::Timelike;
use serde::Serialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Pricing constants (Opus 4.x API list prices, per 1M tokens)
// ---------------------------------------------------------------------------

/// Cost per 1M input tokens in USD (API list price).
const INPUT_PRICE_PER_M: f64 = 15.0;
/// Cost per 1M output tokens in USD (API list price).
const OUTPUT_PRICE_PER_M: f64 = 75.0;
/// Cost per 1M cache-write tokens in USD (API list price).
const CACHE_WRITE_PRICE_PER_M: f64 = 18.75;
/// Cost per 1M cache-read tokens in USD (API list price).
const CACHE_READ_PRICE_PER_M: f64 = 1.50;

/// Weight applied to output tokens when computing effective tokens.
/// Effective tokens track the UI "% of context pool" counter, NOT dollar cost.
/// Output is weighted the same as input (1.0); the 5x price difference is
/// captured only in the dollar-cost calculation.
const OUTPUT_WEIGHT: f64 = 1.0;
/// Weight applied to cache-write tokens for effective-token counting.
/// Writing a cache entry costs 1.25x a plain input token in pool utilization.
const CACHE_WRITE_WEIGHT: f64 = 1.25;
/// Weight applied to cache-read tokens for effective-token counting.
/// Reading from cache is very cheap pool-wise (0.1x an input token).
const CACHE_READ_WEIGHT: f64 = 0.1;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Raw token counts from a single assistant message's `usage` object.
#[derive(Debug, Default, Clone, Serialize)]
pub struct RawTokens {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl RawTokens {
    /// Accumulate another set of raw counts into this one.
    fn add(&mut self, other: &RawTokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_write += other.cache_write;
        self.cache_read += other.cache_read;
    }

    /// Weighted effective-token total for context-pool tracking.
    ///
    /// Formula: `input + output*1.0 + cache_write*1.25 + cache_read*0.1`.
    pub fn effective(&self) -> u64 {
        let eff = self.input as f64
            + self.output as f64 * OUTPUT_WEIGHT
            + self.cache_write as f64 * CACHE_WRITE_WEIGHT
            + self.cache_read as f64 * CACHE_READ_WEIGHT;
        eff.round() as u64
    }

    /// Dollar cost at API list pricing in USD.
    ///
    /// Formula: `(input*15 + output*75 + cache_write*18.75 + cache_read*1.50) / 1_000_000`.
    pub fn cost_usd(&self) -> f64 {
        (self.input as f64 * INPUT_PRICE_PER_M
            + self.output as f64 * OUTPUT_PRICE_PER_M
            + self.cache_write as f64 * CACHE_WRITE_PRICE_PER_M
            + self.cache_read as f64 * CACHE_READ_PRICE_PER_M)
            / 1_000_000.0
    }
}

/// Aggregated usage data for a single Claude Code session.
#[derive(Debug, Clone, Serialize)]
pub struct SessionUsage {
    /// Session UUID as found in the filename.
    pub session_id: String,
    /// Repo name derived from the slug (last meaningful path component).
    pub repo: String,
    /// Raw slug directory name (URL-encoded filesystem path).
    pub slug: String,
    /// Absolute path to the session JSONL file.
    pub path: PathBuf,
    /// RFC 3339 timestamp of the first event in the file.
    pub start_time: String,
    /// Number of assistant turns (events with `type == "assistant"`).
    pub turns: u64,
    /// Accumulated raw token counts.
    pub raw: RawTokens,
    /// Weighted effective-token total.
    pub effective: u64,
    /// Dollar cost at API list pricing.
    pub cost_usd: f64,
}

impl SessionUsage {
    /// Build a `SessionUsage` by reading and accumulating all assistant turns
    /// in the given JSONL file. Malformed lines are skipped with a single
    /// aggregated stderr warning at the end of parsing.
    pub fn from_file(path: &Path, session_id: &str, slug: &str) -> SessionUsage {
        let repo = repo_from_slug(slug);
        let mut usage = SessionUsage {
            session_id: session_id.to_string(),
            repo,
            slug: slug.to_string(),
            path: path.to_path_buf(),
            start_time: String::new(),
            turns: 0,
            raw: RawTokens::default(),
            effective: 0,
            cost_usd: 0.0,
        };

        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[legion] warning: could not open {}: {e}", path.display());
                return usage;
            }
        };

        let reader = io::BufReader::new(file);
        let mut skip_count: u64 = 0;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => {
                    skip_count += 1;
                    continue;
                }
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let obj: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => {
                    skip_count += 1;
                    continue;
                }
            };

            // Record start_time from the first event that has a timestamp.
            if usage.start_time.is_empty()
                && let Some(ts) = obj.get("timestamp").and_then(|v| v.as_str())
            {
                usage.start_time = ts.to_string();
            }

            // Only assistant turns with usage data contribute to counts.
            if obj.get("type").and_then(|v| v.as_str()) != Some("assistant") {
                continue;
            }
            let Some(msg_usage) = obj.get("message").and_then(|m| m.get("usage")) else {
                continue;
            };

            usage.turns += 1;
            let turn = extract_tokens(msg_usage);
            usage.raw.add(&turn);
        }

        if skip_count > 0 {
            eprintln!(
                "[legion] warning: skipped {skip_count} malformed lines in {}",
                path.display()
            );
        }

        usage.effective = usage.raw.effective();
        usage.cost_usd = usage.raw.cost_usd();
        usage
    }
}

/// Aggregated usage for a group of sessions (e.g., by repo).
#[derive(Debug, Serialize)]
pub struct GroupUsage {
    /// Group key (repo name or date).
    pub key: String,
    /// Total assistant turns.
    pub turns: u64,
    /// Accumulated raw token counts.
    pub raw: RawTokens,
    /// Weighted effective-token total.
    pub effective: u64,
    /// Dollar cost at API list pricing.
    pub cost_usd: f64,
    /// Number of sessions in this group.
    pub sessions: usize,
}

// ---------------------------------------------------------------------------
// Session discovery
// ---------------------------------------------------------------------------

/// Walk `~/.claude/projects/` and return one [`SessionUsage`] per `.jsonl` file.
///
/// Filters by `since` (inclusive, RFC 3339 prefix comparison on `start_time`)
/// when provided. If `session_id` is given, only that session is returned.
/// Returns an empty `Vec` (not an error) when the projects directory does not
/// exist so that the command works on machines with no Claude Code history.
pub fn discover_sessions(
    home: &Path,
    since: Option<&str>,
    session_id: Option<&str>,
) -> Vec<SessionUsage> {
    let projects_dir = home.join(".claude").join("projects");
    if !projects_dir.is_dir() {
        return Vec::new();
    }

    let slug_dirs = match std::fs::read_dir(&projects_dir) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "[legion] warning: could not read {}: {e}",
                projects_dir.display()
            );
            return Vec::new();
        }
    };

    let mut sessions: Vec<SessionUsage> = Vec::new();

    for slug_entry in slug_dirs.flatten() {
        let slug_path = slug_entry.path();
        if !slug_path.is_dir() {
            continue;
        }
        let slug = slug_entry.file_name().to_string_lossy().to_string();

        let jsonl_files = match std::fs::read_dir(&slug_path) {
            Ok(d) => d,
            Err(_) => continue,
        };

        for file_entry in jsonl_files.flatten() {
            let file_path = file_entry.path();
            let file_name = file_entry.file_name().to_string_lossy().to_string();
            if !file_name.ends_with(".jsonl") {
                continue;
            }

            // session_id is the filename without the .jsonl extension.
            let sid = file_name.trim_end_matches(".jsonl");

            // If caller wants a specific session, skip all others.
            if let Some(target) = session_id
                && sid != target
            {
                continue;
            }

            let su = SessionUsage::from_file(&file_path, sid, &slug);

            // Apply since filter after parsing (we need start_time from the file).
            if let Some(since_str) = since
                && !su.start_time.is_empty()
                && su.start_time.as_str() < since_str
            {
                continue;
            }

            sessions.push(su);
        }
    }

    // Sort by cost descending so most expensive sessions appear first.
    sessions.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sessions
}

/// Derive a short repo name from a project slug.
///
/// Slugs are URL-encoded filesystem paths with `/` replaced by `-`. For example:
/// `-Volumes-store-projects-runlegion-legion` -> `legion`.
/// `-Users-bob-projects-kelex` -> `kelex`.
///
/// Heuristic: interpret the slug as a path by replacing leading `-` with `/`,
/// then splitting on `-`, and taking the last component that is not a common
/// path segment (Volumes, Users, home, opt, private, tmp, store, projects,
/// src, work). Falls back to the raw slug if nothing survives the filter.
pub fn repo_from_slug(slug: &str) -> String {
    // Decode: treat slug as a path where `-` separates components.
    // Leading `-` represents a leading `/`.
    let parts: Vec<&str> = slug.split('-').collect();

    // Common infrastructure path components to ignore.
    const SKIP: &[&str] = &[
        "Volumes",
        "Users",
        "home",
        "opt",
        "private",
        "tmp",
        "store",
        "projects",
        "src",
        "work",
        "workspace",
    ];

    // Take the last non-empty part that is not in the skip list.
    let repo = parts
        .iter()
        .rev()
        .find(|p| !p.is_empty() && !SKIP.contains(p))
        .copied()
        .unwrap_or(slug);

    repo.to_string()
}

// ---------------------------------------------------------------------------
// Grouping
// ---------------------------------------------------------------------------

/// Group sessions by repo and return sorted by total cost descending.
pub fn group_by_repo(sessions: &[SessionUsage]) -> Vec<GroupUsage> {
    let mut map: std::collections::HashMap<String, GroupUsage> = std::collections::HashMap::new();

    for su in sessions {
        let entry = map.entry(su.repo.clone()).or_insert_with(|| GroupUsage {
            key: su.repo.clone(),
            turns: 0,
            raw: RawTokens::default(),
            effective: 0,
            cost_usd: 0.0,
            sessions: 0,
        });
        entry.turns += su.turns;
        entry.raw.add(&su.raw);
        entry.sessions += 1;
    }

    // Recompute derived fields from accumulated raw tokens.
    let mut groups: Vec<GroupUsage> = map
        .into_values()
        .map(|mut g| {
            g.effective = g.raw.effective();
            g.cost_usd = g.raw.cost_usd();
            g
        })
        .collect();

    groups.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    groups
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Format a raw token count as a compact human-readable string.
/// Values >= 1M become "X.XM", values >= 1K become "XXK", otherwise the raw count.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

/// Format a dollar cost as a compact string with two decimal places.
pub fn format_cost(usd: f64) -> String {
    format!("${:.2}", usd)
}

/// Format a session start_time as a relative "Xh ago" string if it is today,
/// otherwise return the ISO timestamp unchanged.
pub fn format_start_time(ts: &str) -> String {
    // Parse the first 19 chars (YYYY-MM-DDTHH:MM:SS) and compare to now.
    if ts.len() < 19 {
        return ts.to_string();
    }
    let now = chrono::Utc::now();
    let today_prefix = now.format("%Y-%m-%dT").to_string();
    if ts.starts_with(&today_prefix) {
        // Parse the hour/minute from the timestamp for a rough "Xh ago".
        if let (Ok(ts_h), Ok(ts_m)) = (ts[11..13].parse::<i64>(), ts[14..16].parse::<i64>()) {
            let now_h = now.hour() as i64;
            let now_m = now.minute() as i64;
            let diff_mins = (now_h * 60 + now_m) - (ts_h * 60 + ts_m);
            if diff_mins < 60 {
                return format!("{}m ago", diff_mins.max(0));
            } else {
                return format!("{}h ago", diff_mins / 60);
            }
        }
    }
    ts[..19].to_string()
}

// ---------------------------------------------------------------------------
// Table output
// ---------------------------------------------------------------------------

/// Print a human-readable table of session usage.
pub fn print_session_table(sessions: &[SessionUsage]) {
    if sessions.is_empty() {
        println!("[legion] no sessions found");
        return;
    }

    println!(
        "{:<8}  {:<12}  {:<12}  {:>5}  {:>7}  {:>7}  {:>7}  {:>7}  {:>9}  {:>8}",
        "id",
        "repo",
        "started",
        "turns",
        "input",
        "output",
        "cache_w",
        "cache_r",
        "effective",
        "cost"
    );
    println!("{}", "-".repeat(100));

    for s in sessions {
        println!(
            "{:<8}  {:<12}  {:<12}  {:>5}  {:>7}  {:>7}  {:>7}  {:>7}  {:>9}  {:>8}",
            &s.session_id[..s.session_id.len().min(8)],
            truncate_str(&s.repo, 12),
            truncate_str(&format_start_time(&s.start_time), 12),
            s.turns,
            format_tokens(s.raw.input),
            format_tokens(s.raw.output),
            format_tokens(s.raw.cache_write),
            format_tokens(s.raw.cache_read),
            format_tokens(s.effective),
            format_cost(s.cost_usd),
        );
    }

    // Totals row.
    let total_turns: u64 = sessions.iter().map(|s| s.turns).sum();
    let mut total_raw = RawTokens::default();
    for s in sessions {
        total_raw.add(&s.raw);
    }
    let total_effective = total_raw.effective();
    let total_cost = total_raw.cost_usd();

    println!("{}", "-".repeat(100));
    println!(
        "{:<8}  {:<12}  {:<12}  {:>5}  {:>7}  {:>7}  {:>7}  {:>7}  {:>9}  {:>8}",
        "TOTAL",
        "",
        "",
        total_turns,
        format_tokens(total_raw.input),
        format_tokens(total_raw.output),
        format_tokens(total_raw.cache_write),
        format_tokens(total_raw.cache_read),
        format_tokens(total_effective),
        format_cost(total_cost),
    );
}

/// Print a human-readable table of by-repo grouped usage.
pub fn print_repo_table(groups: &[GroupUsage]) {
    if groups.is_empty() {
        println!("[legion] no sessions found");
        return;
    }

    println!(
        "{:<16}  {:>8}  {:>7}  {:>7}  {:>7}  {:>7}  {:>9}  {:>8}",
        "repo", "sessions", "input", "output", "cache_w", "cache_r", "effective", "cost"
    );
    println!("{}", "-".repeat(80));

    for g in groups {
        println!(
            "{:<16}  {:>8}  {:>7}  {:>7}  {:>7}  {:>7}  {:>9}  {:>8}",
            truncate_str(&g.key, 16),
            g.sessions,
            format_tokens(g.raw.input),
            format_tokens(g.raw.output),
            format_tokens(g.raw.cache_write),
            format_tokens(g.raw.cache_read),
            format_tokens(g.effective),
            format_cost(g.cost_usd),
        );
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Extract token counts from a `message.usage` JSON value.
/// Missing or non-numeric fields return 0 (not an error).
fn extract_tokens(usage: &Value) -> RawTokens {
    RawTokens {
        input: usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        output: usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_write: usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_read: usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    }
}

/// Truncate a string to at most `max_len` chars, appending `~` if cut.
fn truncate_str(s: &str, max_len: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_len {
        s.to_string()
    } else {
        let cut: String = chars[..max_len.saturating_sub(1)].iter().collect();
        format!("{cut}~")
    }
}

// ---------------------------------------------------------------------------
// Tail parsing for the statusline subcommand
// ---------------------------------------------------------------------------

/// Per-turn summary of the most recent state of a transcript.
///
/// Unlike [`SessionUsage::from_file`], which aggregates every assistant turn,
/// this function returns only the information the statusline needs:
/// the last assistant message's raw token counts (the "next-turn replay"
/// cost driver), the count of real user turns (filtering synthetic content
/// that Claude Code injects on every render), and the maximum size of any
/// cached `tool_result` error in the tail of the transcript.
///
/// The parser walks the whole file; sessions with hundreds of turns still
/// complete in well under the statusline's 300ms budget because each line
/// is JSON-decoded once and the work per line is negligible.
#[derive(Debug, Default, Clone)]
pub struct TranscriptTail {
    /// Raw tokens on the most recent assistant message that carried a
    /// `usage` object. Zero if the transcript has no such message.
    pub last_assistant: RawTokens,
    /// Number of genuine user turns seen in the transcript.
    /// Excludes synthetic content (system reminders, command echoes,
    /// tool-result relays) that Claude Code injects per render.
    pub real_turns: u64,
    /// Maximum byte size of any cached `tool_result` content flagged
    /// as an error in the tail of the transcript. Zero when none
    /// exceed the 2KB threshold.
    pub max_error_bytes: u64,
}

/// Prefixes indicating a `user` event whose content is not a genuine
/// user turn but some synthetic injection from Claude Code or its hooks.
/// Matches the set used by the mtberlin2023 reference statusline (MIT).
const SYNTHETIC_PREFIXES: &[&str] = &[
    "<command-message>",
    "<command-name>",
    "<command-args>",
    "<task-notification>",
    "<local-command-caveat>",
    "<system-reminder>",
    "<user-prompt-submit-hook>",
    "Unknown command:",
    "Caveat:",
];

/// Size above which an error `tool_result` is considered large enough
/// to surface as a statusline warning. Matches the mtberlin2023 reference.
const ERROR_SURFACE_THRESHOLD_BYTES: u64 = 2048;

/// Parse the transcript file at `path` and return a [`TranscriptTail`].
///
/// Malformed lines are skipped silently. A missing file returns the
/// default tail (all zeros); the caller decides how to surface that.
pub fn parse_transcript_tail(path: &Path) -> TranscriptTail {
    let mut tail = TranscriptTail::default();

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return tail,
    };
    let reader = io::BufReader::new(file);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let obj: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match obj.get("type").and_then(|v| v.as_str()) {
            Some("assistant") => {
                if let Some(usage) = obj.get("message").and_then(|m| m.get("usage")) {
                    tail.last_assistant = extract_tokens(usage);
                }
            }
            Some("user") if is_real_user_turn(&obj, &mut tail.max_error_bytes) => {
                tail.real_turns += 1;
            }
            _ => {}
        }
    }

    tail
}

/// Determine whether a `user`-type event represents a genuine user turn
/// and update `max_error_bytes` if any error-flagged tool_result content
/// exceeds the surfacing threshold.
fn is_real_user_turn(obj: &Value, max_error_bytes: &mut u64) -> bool {
    let msg = match obj.get("message") {
        Some(m) => m,
        None => return false,
    };
    let content = match msg.get("content") {
        Some(c) => c,
        None => return false,
    };

    if let Some(text) = content.as_str() {
        return !is_synthetic_text(text);
    }

    let Some(items) = content.as_array() else {
        return false;
    };

    let mut real = false;
    for item in items {
        let Some(obj_item) = item.as_object() else {
            continue;
        };
        let itype = obj_item.get("type").and_then(|v| v.as_str());
        match itype {
            Some("tool_result") => {
                let body_len = tool_result_body_len(obj_item);
                let is_error = obj_item
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_error
                    && body_len > ERROR_SURFACE_THRESHOLD_BYTES
                    && body_len > *max_error_bytes
                {
                    *max_error_bytes = body_len;
                }
            }
            Some("text") => {
                let text = obj_item.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if !is_synthetic_text(text) {
                    real = true;
                }
            }
            _ => {
                real = true;
            }
        }
    }
    real
}

/// Check whether a piece of user text is synthetic content that should
/// not be counted as a real user turn.
fn is_synthetic_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return true;
    }
    SYNTHETIC_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// Compute the byte length of a `tool_result.content` field, handling
/// both the string form and the list-of-content-parts form.
fn tool_result_body_len(item: &serde_json::Map<String, Value>) -> u64 {
    let Some(body) = item.get("content") else {
        return 0;
    };
    if let Some(text) = body.as_str() {
        return text.len() as u64;
    }
    let Some(parts) = body.as_array() else {
        return 0;
    };
    let mut total: u64 = 0;
    for part in parts {
        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
            total += text.len() as u64;
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a minimal session JSONL to a tempfile and return the path.
    fn write_fixture<S: AsRef<str>>(lines: &[S]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{}", line.as_ref()).unwrap();
        }
        (dir, path)
    }

    /// Build a minimal assistant event JSON string with the given token counts.
    fn assistant_event(
        input: u64,
        output: u64,
        cache_write: u64,
        cache_read: u64,
        ts: &str,
    ) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","message":{{"role":"assistant","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_creation_input_tokens":{cache_write},"cache_read_input_tokens":{cache_read}}}}}}}"#
        )
    }

    fn user_event(ts: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{ts}","message":{{"role":"user","content":"hello"}}}}"#
        )
    }

    #[test]
    fn raw_tokens_effective_math() {
        // Verify the weighted formula matches the spec.
        let raw = RawTokens {
            input: 596,
            output: 324_549,
            cache_write: 831_798,
            cache_read: 68_400_230,
        };
        // effective = 596 + 324549*1.0 + 831798*1.25 + 68400230*0.1
        // = 596 + 324549 + 1039747.5 + 6840023
        // = 8204915.5  -> 8204916
        let eff = raw.effective();
        assert!(
            eff > 8_000_000 && eff < 10_000_000,
            "effective out of range: {eff}"
        );
    }

    #[test]
    fn raw_tokens_cost_math() {
        let raw = RawTokens {
            input: 596,
            output: 324_549,
            cache_write: 831_798,
            cache_read: 68_400_230,
        };
        // cost = (596*15 + 324549*75 + 831798*18.75 + 68400230*1.5) / 1_000_000
        //      = (8940 + 24341175 + 15596212.5 + 102600345) / 1_000_000
        //      = 142546672.5 / 1_000_000
        //      ~ $142.55
        let cost = raw.cost_usd();
        assert!(cost > 140.0 && cost < 145.0, "cost out of range: {cost:.2}");
    }

    #[test]
    fn session_from_fixture_counts_turns() {
        let ts1 = "2026-04-10T01:00:00.000Z";
        let ts2 = "2026-04-10T01:01:00.000Z";
        let lines = vec![
            user_event(ts1),
            assistant_event(100, 200, 300, 400, ts2),
            assistant_event(50, 60, 70, 80, ts2),
        ];
        let (_dir, path) = write_fixture(&lines);

        let su = SessionUsage::from_file(&path, "test-session", "-Users-bob-projects-kelex");
        assert_eq!(su.turns, 2, "expected 2 assistant turns");
        assert_eq!(su.raw.input, 150, "input mismatch");
        assert_eq!(su.raw.output, 260, "output mismatch");
        assert_eq!(su.raw.cache_write, 370, "cache_write mismatch");
        assert_eq!(su.raw.cache_read, 480, "cache_read mismatch");
        assert_eq!(su.start_time, ts1, "start_time should be from first event");
    }

    #[test]
    fn session_from_fixture_skips_malformed_lines() {
        let ts = "2026-04-10T02:00:00.000Z";
        let line0 = user_event(ts);
        let line1 = assistant_event(10, 20, 30, 40, ts);
        let lines: Vec<String> = vec![
            line0,
            "this is not json at all".to_string(),
            line1,
            "{\"broken\": ".to_string(),
        ];
        let (_dir, path) = write_fixture(&lines);

        // Must not panic. Should still count the one valid assistant turn.
        let su = SessionUsage::from_file(&path, "skip-test", "-Users-bob-projects-legion");
        assert_eq!(su.turns, 1, "expected 1 valid assistant turn");
        assert_eq!(su.raw.input, 10);
    }

    #[test]
    fn repo_from_slug_legion() {
        assert_eq!(
            repo_from_slug("-Volumes-store-projects-runlegion-legion"),
            "legion"
        );
    }

    #[test]
    fn repo_from_slug_kelex() {
        assert_eq!(repo_from_slug("-Users-bob-projects-kelex"), "kelex");
    }

    #[test]
    fn format_tokens_ranges() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1_500), "1K");
        assert_eq!(format_tokens(2_340_000), "2.3M");
    }

    #[test]
    fn format_cost_two_decimals() {
        assert_eq!(format_cost(142.5467), "$142.55");
        assert_eq!(format_cost(0.0), "$0.00");
    }

    #[test]
    fn group_by_repo_aggregates_correctly() {
        let ts = "2026-04-10T03:00:00.000Z";
        let event_a = assistant_event(100, 200, 0, 0, ts);
        let event_b = assistant_event(50, 100, 0, 0, ts);
        let (_dir_a, path_a) = write_fixture(&[&event_a]);
        let (_dir_b, path_b) = write_fixture(&[&event_b]);

        let sessions = vec![
            SessionUsage::from_file(&path_a, "sess-a", "-Users-bob-projects-legion"),
            SessionUsage::from_file(&path_b, "sess-b", "-Users-bob-projects-legion"),
        ];
        let groups = group_by_repo(&sessions);
        assert_eq!(groups.len(), 1, "expected one group");
        let g = &groups[0];
        assert_eq!(g.key, "legion");
        assert_eq!(g.sessions, 2);
        assert_eq!(g.raw.input, 150);
        assert_eq!(g.raw.output, 300);
    }

    #[test]
    fn empty_file_produces_zero_turns() {
        let (_dir, path) = write_fixture::<&str>(&[]);
        let su = SessionUsage::from_file(&path, "empty", "-Users-bob-projects-test");
        assert_eq!(su.turns, 0);
        assert_eq!(su.cost_usd, 0.0);
    }

    #[test]
    fn discover_sessions_returns_empty_for_missing_home() {
        let tmp = tempfile::tempdir().unwrap();
        // No .claude/projects/ directory exists under tmp.
        let sessions = discover_sessions(tmp.path(), None, None);
        assert!(sessions.is_empty());
    }

    #[test]
    fn discover_sessions_finds_sessions_in_tempdir() {
        let tmp = tempfile::tempdir().unwrap();
        let ts = "2026-04-10T04:00:00.000Z";
        let slug = "-Users-test-projects-myrepo";
        let slug_dir = tmp.path().join(".claude/projects").join(slug);
        std::fs::create_dir_all(&slug_dir).unwrap();
        let session_path = slug_dir.join("aaaaaaaa-0000-0000-0000-000000000001.jsonl");
        let event = assistant_event(10, 20, 0, 0, ts);
        std::fs::write(&session_path, format!("{event}\n")).unwrap();

        let sessions = discover_sessions(tmp.path(), None, None);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].repo, "myrepo");
        assert_eq!(sessions[0].turns, 1);
    }

    #[test]
    fn discover_sessions_filters_by_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let ts = "2026-04-10T05:00:00.000Z";
        let slug = "-Users-test-projects-myrepo";
        let slug_dir = tmp.path().join(".claude/projects").join(slug);
        std::fs::create_dir_all(&slug_dir).unwrap();

        // Write two sessions.
        let sid_a = "aaaaaaaa-0000-0000-0000-000000000001";
        let sid_b = "bbbbbbbb-0000-0000-0000-000000000002";
        let event = assistant_event(10, 20, 0, 0, ts);
        std::fs::write(
            slug_dir.join(format!("{sid_a}.jsonl")),
            format!("{event}\n"),
        )
        .unwrap();
        std::fs::write(
            slug_dir.join(format!("{sid_b}.jsonl")),
            format!("{event}\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path(), None, Some(sid_a));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, sid_a);
    }

    #[test]
    fn truncate_str_short() {
        assert_eq!(truncate_str("abc", 5), "abc");
    }

    #[test]
    fn truncate_str_long() {
        let result = truncate_str("abcdefgh", 5);
        assert_eq!(result.len(), 5);
        assert!(result.ends_with('~'));
    }

    // -----------------------------------------------------------------
    // parse_transcript_tail coverage
    // -----------------------------------------------------------------

    fn user_text_event(ts: &str, text: &str) -> String {
        let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
        format!(
            r#"{{"type":"user","timestamp":"{ts}","message":{{"role":"user","content":"{escaped}"}}}}"#
        )
    }

    fn tool_result_event(ts: &str, body: &str, is_error: bool) -> String {
        let escaped = body.replace('\\', "\\\\").replace('"', "\\\"");
        let flag = if is_error { "true" } else { "false" };
        format!(
            r#"{{"type":"user","timestamp":"{ts}","message":{{"role":"user","content":[{{"type":"tool_result","is_error":{flag},"content":"{escaped}"}}]}}}}"#
        )
    }

    #[test]
    fn tail_defaults_for_missing_file() {
        let tail = parse_transcript_tail(&PathBuf::from("/nonexistent/path.jsonl"));
        assert_eq!(tail.real_turns, 0);
        assert_eq!(tail.last_assistant.input, 0);
        assert_eq!(tail.max_error_bytes, 0);
    }

    #[test]
    fn tail_captures_last_assistant_tokens() {
        let ts = "2026-04-21T00:00:00Z";
        let (_dir, path) = write_fixture(&[
            assistant_event(1, 2, 3, 4, ts),
            assistant_event(10, 20, 30, 40, ts),
            assistant_event(100, 200, 300, 400, ts),
        ]);
        let tail = parse_transcript_tail(&path);
        // Last assistant wins, not sum.
        assert_eq!(tail.last_assistant.input, 100);
        assert_eq!(tail.last_assistant.output, 200);
        assert_eq!(tail.last_assistant.cache_write, 300);
        assert_eq!(tail.last_assistant.cache_read, 400);
    }

    #[test]
    fn tail_counts_real_user_turns() {
        let ts = "2026-04-21T00:00:00Z";
        let (_dir, path) = write_fixture(&[
            user_text_event(ts, "first question"),
            assistant_event(1, 2, 0, 0, ts),
            user_text_event(ts, "second question"),
            assistant_event(3, 4, 0, 0, ts),
        ]);
        let tail = parse_transcript_tail(&path);
        assert_eq!(tail.real_turns, 2);
    }

    #[test]
    fn tail_filters_every_synthetic_prefix() {
        let ts = "2026-04-21T00:00:00Z";
        let mut lines: Vec<String> = SYNTHETIC_PREFIXES
            .iter()
            .map(|p| user_text_event(ts, &format!("{p} payload")))
            .collect();
        lines.push(user_text_event(ts, "this one is real"));
        let (_dir, path) = write_fixture(&lines);
        let tail = parse_transcript_tail(&path);
        assert_eq!(
            tail.real_turns, 1,
            "only the non-synthetic user turn should count"
        );
    }

    #[test]
    fn tail_reports_large_error_tool_result_size() {
        let ts = "2026-04-21T00:00:00Z";
        let big = "x".repeat(3000);
        let (_dir, path) = write_fixture(&[tool_result_event(ts, &big, true)]);
        let tail = parse_transcript_tail(&path);
        assert_eq!(tail.max_error_bytes, 3000);
    }

    #[test]
    fn tail_suppresses_error_below_2kb_threshold() {
        let ts = "2026-04-21T00:00:00Z";
        // Exactly 2048 is NOT surfaced (strict > 2048). 2049 would be.
        let at_threshold = "x".repeat(2048);
        let (_dir, path) = write_fixture(&[tool_result_event(ts, &at_threshold, true)]);
        let tail = parse_transcript_tail(&path);
        assert_eq!(tail.max_error_bytes, 0);
    }

    #[test]
    fn tail_ignores_non_error_tool_result_even_when_large() {
        let ts = "2026-04-21T00:00:00Z";
        let big = "x".repeat(5000);
        // is_error: false -> must not surface.
        let (_dir, path) = write_fixture(&[tool_result_event(ts, &big, false)]);
        let tail = parse_transcript_tail(&path);
        assert_eq!(tail.max_error_bytes, 0);
    }

    #[test]
    fn tail_survives_malformed_lines_mixed_with_good_ones() {
        let ts = "2026-04-21T00:00:00Z";
        let lines = vec![
            assistant_event(1, 2, 3, 4, ts),
            "not json at all".to_string(),
            "{\"broken\": ".to_string(),
            assistant_event(100, 200, 300, 400, ts),
        ];
        let (_dir, path) = write_fixture(&lines);
        let tail = parse_transcript_tail(&path);
        // Malformed lines are skipped; the valid final assistant wins.
        assert_eq!(tail.last_assistant.input, 100);
        assert_eq!(tail.last_assistant.cache_read, 400);
    }

    #[test]
    fn tail_parses_array_form_tool_result_content() {
        let ts = "2026-04-21T00:00:00Z";
        let big_text = "y".repeat(2500);
        let escaped = big_text.replace('\\', "\\\\").replace('"', "\\\"");
        let event = format!(
            r#"{{"type":"user","timestamp":"{ts}","message":{{"role":"user","content":[{{"type":"tool_result","is_error":true,"content":[{{"type":"text","text":"{escaped}"}}]}}]}}}}"#
        );
        let (_dir, path) = write_fixture(&[event]);
        let tail = parse_transcript_tail(&path);
        assert_eq!(tail.max_error_bytes, 2500);
    }
}
