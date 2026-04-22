//! Claude Code statusline subcommand.
//!
//! Consumes the Claude Code statusLine JSON on stdin, persists rate-limit
//! and usage samples to legion's local store, and renders a single-line
//! status chip on stdout.
//!
//! See docs at code.claude.com/docs/en/statusline for the input JSON contract.
//! The mtberlin2023 reference implementation (MIT) informed the threshold
//! calibration and rendering conventions; legion's implementation is native.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::db::Database;
use crate::usage::{RawTokens, TranscriptTail, format_tokens, parse_transcript_tail};

// ---------------------------------------------------------------------------
// Persisted sample types
// ---------------------------------------------------------------------------

/// A rate-limit sample drawn from a single Claude Code statusline render.
/// Mirrors the `rate_limits.{five_hour,seven_day}` blocks in the input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitSample {
    pub id: String,
    pub hostname: String,
    pub session_id: String,
    pub sampled_at: String,
    pub five_hour_pct: Option<f64>,
    pub five_hour_resets_at: Option<i64>,
    pub seven_day_pct: Option<f64>,
    pub seven_day_resets_at: Option<i64>,
    pub model: Option<String>,
}

/// A per-turn usage sample drawn from the most recent assistant message
/// in the session transcript. `effective_tokens` is the weighted total
/// used by the cost estimator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSample {
    pub id: String,
    pub hostname: String,
    pub session_id: String,
    pub turn_index: Option<i64>,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_read_tokens: i64,
    pub effective_tokens: i64,
    pub error_bytes: i64,
    pub sampled_at: String,
}

// ---------------------------------------------------------------------------
// Thresholds (starting calibration; refine from cluster data over time)
// ---------------------------------------------------------------------------

/// Next-turn replay bands driving chip color and action hint.
const REPLAY_YELLOW: u64 = 200_000;
const REPLAY_RED: u64 = 500_000;
const REPLAY_HARD_CAP: u64 = 1_000_000;

/// Rate-limit warn/alert thresholds; below warn the cap segment is hidden.
const FIVE_HOUR_WARN_PCT: f64 = 75.0;
const FIVE_HOUR_ALERT_PCT: f64 = 90.0;
const SEVEN_DAY_WARN_PCT: f64 = 60.0;
const SEVEN_DAY_ALERT_PCT: f64 = 85.0;

/// Cache window: how long a rendered chip stays fresh before re-render.
const CACHE_TTL_SECS: u64 = 10;

/// Subdirectory (under legion's data dir) that holds per-session chip caches.
/// User-scoped by construction: the parent `data_dir()` lives under the
/// current user's XDG data / macOS Application Support tree, so a different
/// user on the same host cannot poison or collide with these files.
const CACHE_SUBDIR: &str = "cache/statusline";

/// Error log path relative to legion's data dir. Same user-scoping rationale
/// as the cache dir -- a `/tmp` log is symlink-attackable on shared hosts
/// because `OpenOptions::append` follows symlinks.
const ERROR_LOG_SUBPATH: &str = "logs/hook-errors.log";

/// Default action slash command surfaced at yellow+ replay thresholds.
/// Overridable via the `LEGION_STATUSLINE_ACTION` environment variable.
const DEFAULT_ACTION: &str = "/log";

// Chip glyphs are part of the output contract -- agents and dashboards
// parse them. Escape sequences keep the source bytes ASCII-only while
// rendering emoji at runtime.
const GREEN: &str = "\u{1F7E2}";
const YELLOW: &str = "\u{1F7E1}";
const RED: &str = "\u{1F534}";
const WARN: &str = "\u{26A0}";
const ALERT: &str = "\u{1F6A8}";

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the statusline subcommand. Reads JSON on stdin, writes samples
/// to the database, and prints either the rendered chip (default) or
/// a JSON summary (when `json` is true).
///
/// Cannot fail: a broken statusline must not surface as an error to the
/// Claude Code UI (which would display a distracting red banner on every
/// assistant turn). Every internal failure path is swallowed, logged to
/// `<legion-data-dir>/logs/hook-errors.log`, and returns cleanly. The
/// signature reflects that contract -- no `Result<()>` lying about
/// failure modes that never surface.
pub fn run(json: bool) {
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        log_error(format!("read stdin: {e}"));
        return;
    }
    let parsed = match parse_input(&raw) {
        Some(p) => p,
        None => {
            log_error("parse stdin JSON failed or payload empty".to_string());
            return;
        }
    };

    let session_id = match parsed.session_id.as_deref() {
        Some(sid) if !sid.is_empty() => sid.to_string(),
        _ => {
            log_error("stdin JSON missing session_id".to_string());
            return;
        }
    };

    let cache_path = cache_path_for(&session_id);

    // Fast path: cache hit, re-emit and exit without DB writes. The cached
    // content already carries its trailing newline from write_cache, so emit
    // it verbatim with print! rather than println!. A missing or unresolvable
    // cache path just skips the fast path; the render below still runs.
    if !json
        && let Some(ref p) = cache_path
        && let Some(cached) = read_cache_if_fresh(p, CACHE_TTL_SECS)
    {
        print!("{cached}");
        return;
    }

    let hostname = resolve_hostname();
    let sampled_at = Utc::now().to_rfc3339();

    let tail: TranscriptTail = parsed
        .transcript_path
        .as_deref()
        .map(parse_transcript_tail)
        .unwrap_or_default();

    let rate_sample = build_rate_sample(&hostname, &session_id, &sampled_at, &parsed);
    let usage_sample = build_usage_sample(&hostname, &session_id, &sampled_at, &parsed, &tail);

    persist_samples(&rate_sample, usage_sample.as_ref());

    if json {
        let payload = serde_json::json!({
            "rate_limit": rate_sample,
            "usage": usage_sample,
            "tail": {
                "last_assistant": &tail.last_assistant,
                "real_turns": tail.real_turns,
                "max_error_bytes": tail.max_error_bytes,
            },
        });
        // Serialisation failures on a well-typed JSON value are effectively
        // impossible, but swallow them anyway to preserve the "never break
        // the Claude Code UI" invariant.
        match serde_json::to_string_pretty(&payload) {
            Ok(s) => println!("{s}"),
            Err(e) => log_error(format!("serialise json output: {e}")),
        }
        return;
    }

    let chip = render_chip(&tail, &parsed);
    // Include the trailing newline in both the stdout emission and the
    // cached bytes so a subsequent cache-hit emits byte-identical output.
    let chip_line = format!("{chip}\n");
    if let Some(ref p) = cache_path {
        write_cache(p, &chip_line);
    }
    print!("{chip_line}");
}

// ---------------------------------------------------------------------------
// Input parsing
// ---------------------------------------------------------------------------

/// Parsed view of the Claude Code statusline JSON input.
/// Fields are all optional since the render layer is robust to partial
/// payloads and the subcommand must never crash the UI.
#[derive(Debug, Default, Clone)]
struct ParsedInput {
    session_id: Option<String>,
    transcript_path: Option<PathBuf>,
    model: Option<String>,
    five_hour_pct: Option<f64>,
    five_hour_resets_at: Option<i64>,
    seven_day_pct: Option<f64>,
    seven_day_resets_at: Option<i64>,
}

fn parse_input(raw: &str) -> Option<ParsedInput> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let session_id = v
        .get("session_id")
        .and_then(|x| x.as_str())
        .map(str::to_owned);
    let transcript_path = v
        .get("transcript_path")
        .and_then(|x| x.as_str())
        .map(PathBuf::from);
    let model = v
        .get("model")
        .and_then(|m| m.get("id"))
        .and_then(|x| x.as_str())
        .map(str::to_owned);

    let rl = v.get("rate_limits");
    let fh = rl.and_then(|r| r.get("five_hour"));
    let sd = rl.and_then(|r| r.get("seven_day"));

    Some(ParsedInput {
        session_id,
        transcript_path,
        model,
        five_hour_pct: fh.and_then(|x| x.get("used_percentage")).and_then(as_f64),
        five_hour_resets_at: fh.and_then(|x| x.get("resets_at")).and_then(as_i64),
        seven_day_pct: sd.and_then(|x| x.get("used_percentage")).and_then(as_f64),
        seven_day_resets_at: sd.and_then(|x| x.get("resets_at")).and_then(as_i64),
    })
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|n| n as f64))
}

fn as_i64(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|n| n as i64))
}

// ---------------------------------------------------------------------------
// Sample construction + persistence
// ---------------------------------------------------------------------------

fn build_rate_sample(
    hostname: &str,
    session_id: &str,
    sampled_at: &str,
    parsed: &ParsedInput,
) -> RateLimitSample {
    RateLimitSample {
        id: Uuid::now_v7().to_string(),
        hostname: hostname.to_owned(),
        session_id: session_id.to_owned(),
        sampled_at: sampled_at.to_owned(),
        five_hour_pct: parsed.five_hour_pct,
        five_hour_resets_at: parsed.five_hour_resets_at,
        seven_day_pct: parsed.seven_day_pct,
        seven_day_resets_at: parsed.seven_day_resets_at,
        model: parsed.model.clone(),
    }
}

fn build_usage_sample(
    hostname: &str,
    session_id: &str,
    sampled_at: &str,
    parsed: &ParsedInput,
    tail: &TranscriptTail,
) -> Option<UsageSample> {
    // Only emit a usage sample when the transcript produced a tail. An empty
    // or unreadable transcript leaves all tokens zero, which would create
    // misleading cluster-wide aggregates.
    parsed.transcript_path.as_ref()?;
    let raw = &tail.last_assistant;
    if raw.input == 0 && raw.output == 0 && raw.cache_write == 0 && raw.cache_read == 0 {
        return None;
    }
    // Token counts and real_turns are u64 from the parser; SQLite stores i64.
    // `as i64` would silently truncate above 2^63 (impossible in practice for
    // tokens, but the idiom is wrong and removes the upper-bound guarantee);
    // `try_from().unwrap_or(i64::MAX)` makes the clamp explicit so an
    // unexpectedly huge value pins at i64::MAX rather than wrapping to a
    // negative-looking gigantic number in the DB.
    Some(UsageSample {
        id: Uuid::now_v7().to_string(),
        hostname: hostname.to_owned(),
        session_id: session_id.to_owned(),
        turn_index: if tail.real_turns > 0 {
            Some(i64::try_from(tail.real_turns).unwrap_or(i64::MAX))
        } else {
            None
        },
        model: parsed.model.clone(),
        input_tokens: i64::try_from(raw.input).unwrap_or(i64::MAX),
        output_tokens: i64::try_from(raw.output).unwrap_or(i64::MAX),
        cache_write_tokens: i64::try_from(raw.cache_write).unwrap_or(i64::MAX),
        cache_read_tokens: i64::try_from(raw.cache_read).unwrap_or(i64::MAX),
        effective_tokens: i64::try_from(raw.effective()).unwrap_or(i64::MAX),
        error_bytes: i64::try_from(tail.max_error_bytes).unwrap_or(i64::MAX),
        sampled_at: sampled_at.to_owned(),
    })
}

fn persist_samples(rate: &RateLimitSample, usage: Option<&UsageSample>) {
    let db_path = match database_path() {
        Some(p) => p,
        None => return,
    };
    let db = match Database::open(&db_path) {
        Ok(d) => d,
        Err(e) => {
            log_error(format!("open database: {e}"));
            return;
        }
    };
    if let Err(e) = db.insert_rate_limit_sample(rate) {
        log_error(format!("insert rate_limit_samples: {e}"));
    }
    if let Some(u) = usage
        && let Err(e) = db.insert_usage_sample(u)
    {
        log_error(format!("insert usage_samples: {e}"));
    }
}

fn database_path() -> Option<PathBuf> {
    // Resolve through the same `data_dir()` the rest of the binary uses so
    // statusline samples land in the one canonical legion.db. A previous
    // revision constructed its own ProjectDirs tuple here, which pointed at
    // a different path on macOS and silently split the sample stream off
    // from the rest of legion's state -- the exact split-brain the
    // `data_dir()` doc warns against. Swallow errors: the statusline must
    // never crash the Claude Code UI just because the home dir is weird.
    crate::data_dir().ok().map(|d| d.join("legion.db"))
}

// ---------------------------------------------------------------------------
// Chip rendering
// ---------------------------------------------------------------------------

/// Render the statusline chip for the given tail + parsed input.
fn render_chip(tail: &TranscriptTail, parsed: &ParsedInput) -> String {
    let replay = next_turn_replay(&tail.last_assistant);
    let (emoji, action_suffix) = replay_band(replay, &action_command());
    let err_suffix = error_suffix(tail.max_error_bytes);
    let cap_suffix = cap_segments(parsed);

    format!(
        "{emoji} [{turns}] {replay} next turn{action}{err}{cap}",
        emoji = emoji,
        turns = tail.real_turns,
        replay = format_tokens(replay),
        action = action_suffix,
        err = err_suffix,
        cap = cap_suffix,
    )
}

/// Next-turn replay cost: the tokens Claude Code will re-read on the
/// next API call. Sum of cache read, cache creation, and raw input.
/// Output is deliberately excluded: it is output by the last turn,
/// not input for the next.
fn next_turn_replay(raw: &RawTokens) -> u64 {
    raw.cache_read + raw.cache_write + raw.input
}

/// Return the action-command slash phrase, overridable via env.
fn action_command() -> String {
    std::env::var("LEGION_STATUSLINE_ACTION").unwrap_or_else(|_| DEFAULT_ACTION.to_owned())
}

/// Map replay tokens to (emoji, action-suffix).
fn replay_band(replay: u64, action: &str) -> (&'static str, String) {
    if replay < REPLAY_YELLOW {
        (GREEN, String::new())
    } else if replay < REPLAY_RED {
        (YELLOW, format!(" \u{00B7} {action} after task"))
    } else if replay < REPLAY_HARD_CAP {
        (RED, format!(" \u{00B7} {action} now"))
    } else {
        (RED, format!(" \u{00B7} {action} before next msg"))
    }
}

/// " · ⚠ NKB err" suffix when a large cached error was observed.
fn error_suffix(bytes: u64) -> String {
    if bytes == 0 {
        return String::new();
    }
    let kb = bytes / 1024;
    format!(" \u{00B7} {WARN} {kb}KB err")
}

/// Concatenated cap segments (5h, weekly) in order.
fn cap_segments(parsed: &ParsedInput) -> String {
    let mut out = String::new();
    if let Some(s) = cap_segment(
        parsed.five_hour_pct,
        parsed.five_hour_resets_at,
        &FIVE_HOUR_CONFIG,
    ) {
        out.push_str(&format!(" \u{00B7} {s}"));
    }
    if let Some(s) = cap_segment(
        parsed.seven_day_pct,
        parsed.seven_day_resets_at,
        &SEVEN_DAY_CONFIG,
    ) {
        out.push_str(&format!(" \u{00B7} {s}"));
    }
    out
}

/// Per-window calibration used by [`cap_segment`].
struct CapConfig {
    warn_pct: f64,
    alert_pct: f64,
    label: &'static str,
    format_reset: fn(i64) -> String,
}

const FIVE_HOUR_CONFIG: CapConfig = CapConfig {
    warn_pct: FIVE_HOUR_WARN_PCT,
    alert_pct: FIVE_HOUR_ALERT_PCT,
    label: "5h",
    format_reset: format_reset_same_day,
};

const SEVEN_DAY_CONFIG: CapConfig = CapConfig {
    warn_pct: SEVEN_DAY_WARN_PCT,
    alert_pct: SEVEN_DAY_ALERT_PCT,
    label: "wk",
    format_reset: format_reset_day_time,
};

/// Produce one cap segment (e.g. "WARN 78%·5h→14:32") or `None` when the
/// percentage sits below the warn threshold for this window.
fn cap_segment(pct: Option<f64>, resets_at: Option<i64>, cfg: &CapConfig) -> Option<String> {
    let pct = pct?;
    let marker = if pct >= cfg.alert_pct {
        ALERT
    } else if pct >= cfg.warn_pct {
        WARN
    } else {
        return None;
    };
    let suffix = resets_at
        .filter(|&t| t > 0)
        .map(cfg.format_reset)
        .filter(|s| !s.is_empty())
        .map(|s| format!("\u{2192}{s}"))
        .unwrap_or_default();
    // Round rather than truncate so the displayed integer matches what
    // the user expects from the float we are showing (e.g. 89.9 displays
    // as "90%", not "89%"). The marker is still driven by the raw float
    // so band semantics do not change.
    Some(format!(
        "{marker} {pct}%\u{00B7}{label}{suffix}",
        pct = pct.round() as i64,
        label = cfg.label,
    ))
}

/// Format a 5-hour reset timestamp. Same-day resets use HH:MM local time;
/// resets that cross a day boundary use "Day HH:MM".
fn format_reset_same_day(epoch_secs: i64) -> String {
    let dt = match Local.timestamp_opt(epoch_secs, 0).single() {
        Some(d) => d,
        None => return String::new(),
    };
    let today = Local::now();
    if dt.format("%Y-%m-%d").to_string() == today.format("%Y-%m-%d").to_string() {
        dt.format("%H:%M").to_string()
    } else {
        dt.format("%a %H:%M").to_string()
    }
}

/// Format a weekly reset timestamp as "Day HH:MM".
fn format_reset_day_time(epoch_secs: i64) -> String {
    let dt = match Local.timestamp_opt(epoch_secs, 0).single() {
        Some(d) => d,
        None => return String::new(),
    };
    dt.format("%a %H:%M").to_string()
}

// ---------------------------------------------------------------------------
// Cache helpers
// ---------------------------------------------------------------------------

fn cache_path_for(session_id: &str) -> Option<PathBuf> {
    cache_path_in(&crate::data_dir().ok()?, session_id)
}

/// Compute the statusline cache path for `session_id` under an explicit
/// data dir. Split out from [`cache_path_for`] so unit tests can exercise
/// the sanitiser and path layout without relying on the process-wide
/// `data_dir()` cache, which resolves once per process and can leak state
/// between tests run in the same binary.
fn cache_path_in(data_dir: &Path, session_id: &str) -> Option<PathBuf> {
    // session_id is untrusted; keep only safe chars to avoid path escapes.
    // Claude Code session IDs in practice are UUIDv7 strings (hyphens and
    // hex digits only) so the sanitiser is a no-op on real input. It still
    // defends against malformed input that could collide through character
    // stripping if Anthropic ever changes the ID shape.
    let safe: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe.is_empty() {
        return None;
    }
    let dir = data_dir.join(CACHE_SUBDIR);
    // Best-effort: if mkdir fails the write path will fail-soft too.
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join(safe))
}

fn read_cache_if_fresh(path: &Path, ttl_secs: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let now = SystemTime::now();
    let age = now.duration_since(modified).ok()?;
    if age.as_secs() >= ttl_secs {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn write_cache(path: &Path, content: &str) {
    // Atomic write via tmp file + rename: a concurrent reader either sees
    // the previous complete file or the new complete file, never a partial
    // write. The previous revision used `File::create(path)` which truncates
    // in place -- a reader racing the writer could observe zero bytes or a
    // half-written chip. All steps are best-effort: a failure just means
    // the next render recomputes instead of hitting the cache.
    let Some(parent) = path.parent() else {
        return;
    };
    let file_name = match path.file_name().and_then(|s| s.to_str()) {
        Some(s) => s,
        None => return,
    };
    let tmp = parent.join(format!(".{file_name}.tmp.{pid}", pid = std::process::id()));
    let Ok(mut f) = std::fs::File::create(&tmp) else {
        return;
    };
    if f.write_all(content.as_bytes()).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    // Flush before rename so a crash between rename and fsync still leaves
    // a file whose bytes are on disk (rename is atomic on the same fs).
    let _ = f.sync_data();
    drop(f);
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

fn resolve_hostname() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "unknown".to_owned())
}

fn log_error(msg: String) {
    let line = format!(
        "{ts} [statusline] {msg}\n",
        ts = Utc::now().to_rfc3339(),
        msg = msg,
    );
    // Resolve the error log path through legion's data dir so shared hosts
    // cannot poison the file or race symlink attacks through `/tmp`.
    // If the data dir can't be resolved (unusual -- no HOME, filesystem
    // unwritable) swallow the error: the statusline must never surface a
    // failure to the Claude Code UI.
    let Some(path) = error_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

fn error_log_path() -> Option<PathBuf> {
    crate::data_dir().ok().map(|d| d.join(ERROR_LOG_SUBPATH))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(input: u64, output: u64, cache_write: u64, cache_read: u64) -> RawTokens {
        RawTokens {
            input,
            output,
            cache_write,
            cache_read,
        }
    }

    fn tail(raw_tokens: RawTokens, real_turns: u64, max_error_bytes: u64) -> TranscriptTail {
        TranscriptTail {
            last_assistant: raw_tokens,
            real_turns,
            max_error_bytes,
        }
    }

    #[test]
    fn replay_band_green_below_yellow() {
        let (emoji, suffix) = replay_band(150_000, "/log");
        assert_eq!(emoji, GREEN);
        assert!(suffix.is_empty());
    }

    #[test]
    fn replay_band_yellow_suggests_after_task() {
        let (emoji, suffix) = replay_band(300_000, "/log");
        assert_eq!(emoji, YELLOW);
        assert!(suffix.contains("/log"));
        assert!(suffix.contains("after task"));
    }

    #[test]
    fn replay_band_red_says_now() {
        let (emoji, suffix) = replay_band(700_000, "/log");
        assert_eq!(emoji, RED);
        assert!(suffix.contains("now"));
    }

    #[test]
    fn replay_band_hard_cap_says_before_next() {
        let (emoji, suffix) = replay_band(1_500_000, "/compact");
        assert_eq!(emoji, RED);
        assert!(suffix.contains("/compact"));
        assert!(suffix.contains("before next msg"));
    }

    #[test]
    fn five_hour_segment_hidden_below_warn() {
        assert!(cap_segment(Some(60.0), Some(1_800_000_000), &FIVE_HOUR_CONFIG).is_none());
    }

    #[test]
    fn five_hour_segment_warn_at_75() {
        let s = cap_segment(Some(78.0), None, &FIVE_HOUR_CONFIG).expect("warn segment present");
        assert!(s.contains(WARN));
        assert!(s.contains("78%"));
        assert!(s.contains("5h"));
    }

    #[test]
    fn five_hour_segment_alert_at_90() {
        let s = cap_segment(Some(92.0), None, &FIVE_HOUR_CONFIG).expect("alert segment present");
        assert!(s.contains(ALERT));
        assert!(s.contains("92%"));
    }

    #[test]
    fn seven_day_segment_warn_at_60() {
        let s = cap_segment(Some(62.0), None, &SEVEN_DAY_CONFIG).expect("warn segment present");
        assert!(s.contains(WARN));
        assert!(s.contains("62%"));
        assert!(s.contains("wk"));
    }

    #[test]
    fn seven_day_segment_alert_at_85() {
        let s = cap_segment(Some(88.0), None, &SEVEN_DAY_CONFIG).expect("alert segment present");
        assert!(s.contains(ALERT));
        assert!(s.contains("88%"));
    }

    #[test]
    fn error_suffix_hidden_below_threshold() {
        // Tail detection already filters <2KB; this fn treats 0 as absent.
        assert!(error_suffix(0).is_empty());
    }

    #[test]
    fn error_suffix_shows_kb_above_threshold() {
        let s = error_suffix(5120);
        assert!(s.contains("5KB err"));
        assert!(s.contains(WARN));
    }

    #[test]
    fn render_chip_healthy() {
        let t = tail(raw(100, 200, 0, 10_000), 3, 0);
        let parsed = ParsedInput::default();
        let chip = render_chip(&t, &parsed);
        assert!(chip.starts_with(GREEN), "expected green chip, got {chip}");
        assert!(chip.contains("[3]"));
        assert!(chip.contains("next turn"));
        // No cap segments shown when nothing is set.
        assert!(!chip.contains("5h"));
        assert!(!chip.contains("wk"));
    }

    #[test]
    fn render_chip_with_all_warnings() {
        let t = tail(raw(0, 0, 250_000, 300_000), 12, 4096);
        let parsed = ParsedInput {
            five_hour_pct: Some(92.0),
            seven_day_pct: Some(87.0),
            ..Default::default()
        };
        let chip = render_chip(&t, &parsed);
        assert!(chip.starts_with(RED), "expected red chip");
        assert!(chip.contains("now"));
        assert!(chip.contains("4KB err"));
        assert!(chip.contains("92%"));
        assert!(chip.contains("87%"));
    }

    #[test]
    fn parse_input_accepts_full_payload() {
        let raw = r#"{
            "session_id": "abc-123",
            "transcript_path": "/tmp/x.jsonl",
            "model": {"id": "claude-opus-4-7"},
            "rate_limits": {
                "five_hour": {"used_percentage": 45.5, "resets_at": 1714000000},
                "seven_day": {"used_percentage": 72.1, "resets_at": 1714400000}
            }
        }"#;
        let p = parse_input(raw).expect("parse succeeds");
        assert_eq!(p.session_id.as_deref(), Some("abc-123"));
        assert_eq!(p.model.as_deref(), Some("claude-opus-4-7"));
        assert!((p.five_hour_pct.unwrap() - 45.5).abs() < f64::EPSILON);
        assert_eq!(p.five_hour_resets_at, Some(1714000000));
        assert_eq!(p.seven_day_resets_at, Some(1714400000));
    }

    #[test]
    fn parse_input_tolerates_missing_rate_limits() {
        let raw = r#"{
            "session_id": "abc-123",
            "transcript_path": "/tmp/x.jsonl"
        }"#;
        let p = parse_input(raw).expect("parse succeeds");
        assert_eq!(p.session_id.as_deref(), Some("abc-123"));
        assert!(p.five_hour_pct.is_none());
        assert!(p.seven_day_pct.is_none());
    }

    #[test]
    fn parse_input_returns_none_for_garbage() {
        assert!(parse_input("not json at all").is_none());
    }

    #[test]
    fn cache_path_sanitises_session_id() {
        // Run the sanitiser through `cache_path_in` with an explicit tmp data
        // dir so the test stays hermetic: `cache_path_for` resolves through
        // the process-wide `data_dir()` cache which is not safely swappable
        // from within a unit test.
        let dir = tempfile::tempdir().unwrap();
        let p = cache_path_in(dir.path(), "../../../etc/passwd").expect("path resolves");
        let s = p.to_string_lossy();
        assert!(s.ends_with("etcpasswd"), "path should be sanitised: {s}");
        assert!(
            s.contains(CACHE_SUBDIR),
            "path should live under cache subdir: {s}"
        );
        // And it must stay under the data dir we passed in -- no `/tmp` leakage.
        assert!(
            p.starts_with(dir.path()),
            "path must live under the passed-in data dir: {s}"
        );
    }

    #[test]
    fn cache_path_returns_none_for_empty_sanitised_id() {
        // After sanitising, "/////" collapses to the empty string. The path
        // must resolve to None rather than a dir-only path that would point
        // the cache write at the cache directory itself.
        let dir = tempfile::tempdir().unwrap();
        assert!(cache_path_in(dir.path(), "/////").is_none());
    }

    #[test]
    fn write_cache_is_atomic_against_concurrent_readers() {
        // The previous revision used `File::create(path)` which truncates
        // in place -- a reader racing the writer could observe zero bytes.
        // The atomic rename flow guarantees the target file either has the
        // old content or the new content, never a partial write.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chip");
        std::fs::write(&path, "OLD\n").unwrap();
        write_cache(&path, "NEW-CONTENT\n");
        let got = std::fs::read_to_string(&path).unwrap();
        assert_eq!(got, "NEW-CONTENT\n");
        // No stray tmp file left behind on the success path.
        let stray: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".chip.tmp."))
            .collect();
        assert!(stray.is_empty(), "tmp files left behind: {stray:?}");
    }

    #[test]
    fn cache_read_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent");
        assert!(read_cache_if_fresh(&path, CACHE_TTL_SECS).is_none());
    }

    #[test]
    fn cache_read_returns_none_when_ttl_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache");
        std::fs::write(&path, "hello").unwrap();
        // A TTL of zero means any non-zero age is stale.
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(read_cache_if_fresh(&path, 0).is_none());
    }

    #[test]
    fn cache_read_returns_content_when_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache");
        std::fs::write(&path, "hello").unwrap();
        let got = read_cache_if_fresh(&path, CACHE_TTL_SECS);
        assert_eq!(got.as_deref(), Some("hello"));
    }

    #[test]
    fn next_turn_replay_excludes_output() {
        let r = raw(100, 999_999, 200, 300);
        assert_eq!(next_turn_replay(&r), 600);
    }

    // --- build_usage_sample contract ---------------------------------

    #[test]
    fn build_usage_sample_returns_none_when_transcript_absent() {
        let parsed = ParsedInput::default();
        let tail = tail(raw(100, 200, 0, 0), 1, 0);
        assert!(build_usage_sample("host", "sid", "now", &parsed, &tail).is_none());
    }

    #[test]
    fn build_usage_sample_returns_none_when_all_tokens_zero() {
        let parsed = ParsedInput {
            transcript_path: Some(PathBuf::from("/nonexistent")),
            ..Default::default()
        };
        let tail = tail(raw(0, 0, 0, 0), 0, 0);
        assert!(build_usage_sample("host", "sid", "now", &parsed, &tail).is_none());
    }

    #[test]
    fn build_usage_sample_populated_when_tokens_and_transcript_present() {
        let parsed = ParsedInput {
            transcript_path: Some(PathBuf::from("/nonexistent")),
            model: Some("claude-opus-4-7".into()),
            ..Default::default()
        };
        let tail = tail(raw(10, 20, 30, 40), 3, 5120);
        let sample = build_usage_sample("host", "sid", "2026-04-21T00:00:00Z", &parsed, &tail)
            .expect("populated sample");
        assert_eq!(sample.input_tokens, 10);
        assert_eq!(sample.output_tokens, 20);
        assert_eq!(sample.cache_write_tokens, 30);
        assert_eq!(sample.cache_read_tokens, 40);
        // 10 + 20*1.0 + 30*1.25 + 40*0.1 = 10 + 20 + 37.5 + 4 = 71.5 -> 72
        assert_eq!(sample.effective_tokens, 72);
        assert_eq!(sample.error_bytes, 5120);
        assert_eq!(sample.turn_index, Some(3));
        assert_eq!(sample.model.as_deref(), Some("claude-opus-4-7"));
    }

    // --- DB round-trip ------------------------------------------------

    #[test]
    fn db_rate_limit_sample_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("legion.db")).unwrap();
        let sample = RateLimitSample {
            id: Uuid::now_v7().to_string(),
            hostname: "puck".into(),
            session_id: "sess-1".into(),
            sampled_at: "2026-04-21T00:00:00Z".into(),
            five_hour_pct: Some(42.5),
            five_hour_resets_at: Some(1714500000),
            seven_day_pct: Some(68.0),
            seven_day_resets_at: Some(1714900000),
            model: Some("claude-opus-4-7".into()),
        };
        db.insert_rate_limit_sample(&sample).unwrap();

        let got = db
            .latest_rate_limit_sample()
            .unwrap()
            .expect("sample present");
        assert_eq!(got.id, sample.id);
        assert_eq!(got.hostname, "puck");
        assert_eq!(got.five_hour_pct, Some(42.5));
        assert_eq!(got.seven_day_resets_at, Some(1714900000));
        assert_eq!(got.model.as_deref(), Some("claude-opus-4-7"));
    }

    #[test]
    fn db_usage_sample_insert_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("legion.db")).unwrap();
        let sample = UsageSample {
            id: Uuid::now_v7().to_string(),
            hostname: "puck".into(),
            session_id: "sess-1".into(),
            turn_index: Some(5),
            model: Some("claude-sonnet-4-6".into()),
            input_tokens: 100,
            output_tokens: 200,
            cache_write_tokens: 300,
            cache_read_tokens: 400,
            effective_tokens: 715,
            error_bytes: 0,
            sampled_at: "2026-04-21T00:00:00Z".into(),
        };
        // Schema drift would panic here; presence of an id in the return
        // proves the insert hit every NOT NULL column correctly.
        let id = db.insert_usage_sample(&sample).unwrap();
        assert_eq!(id, sample.id);
    }

    #[test]
    fn db_latest_returns_most_recent_rate_limit_sample() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("legion.db")).unwrap();
        let mk = |id: &str, sampled: &str, pct: f64| RateLimitSample {
            id: id.to_string(),
            hostname: "puck".into(),
            session_id: "sess".into(),
            sampled_at: sampled.to_string(),
            five_hour_pct: Some(pct),
            five_hour_resets_at: None,
            seven_day_pct: None,
            seven_day_resets_at: None,
            model: None,
        };
        db.insert_rate_limit_sample(&mk("a", "2026-04-21T00:00:00Z", 30.0))
            .unwrap();
        db.insert_rate_limit_sample(&mk("b", "2026-04-21T01:00:00Z", 80.0))
            .unwrap();
        db.insert_rate_limit_sample(&mk("c", "2026-04-21T00:30:00Z", 55.0))
            .unwrap();

        let got = db.latest_rate_limit_sample().unwrap().expect("has sample");
        assert_eq!(got.id, "b", "most recent by sampled_at should win");
        assert_eq!(got.five_hour_pct, Some(80.0));
    }
}
