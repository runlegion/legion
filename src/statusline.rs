//! Claude Code statusline subcommand.
//!
//! Consumes the Claude Code statusLine JSON on stdin, persists rate-limit
//! and usage samples to legion's local store, and prints the operator's
//! status line on stdout: model, directory, branch, context and rate-limit
//! percentages.
//!
//! See docs at code.claude.com/docs/en/statusline for the input JSON contract.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::db::Database;
use crate::usage::{TranscriptTail, parse_transcript_tail};

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
// Constants
// ---------------------------------------------------------------------------

/// Sample throttle window: a session whose stamp file is younger than this
/// skips sample persistence. The line itself is rendered on every call.
const CACHE_TTL_SECS: u64 = 10;

/// Subdirectory (under legion's data dir) that holds per-session stamp files.
/// User-scoped by construction: the parent `data_dir()` lives under the
/// current user's XDG data / macOS Application Support tree, so a different
/// user on the same host cannot poison or collide with these files.
const CACHE_SUBDIR: &str = "cache/statusline";

/// Error log path relative to legion's data dir. Same user-scoping rationale
/// as the cache dir -- a `/tmp` log is symlink-attackable on shared hosts
/// because `OpenOptions::append` follows symlinks.
const ERROR_LOG_SUBPATH: &str = "logs/hook-errors.log";

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the statusline subcommand. Reads JSON on stdin, writes samples
/// to the database, and prints either the status line (default) or
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

    if !json {
        record_samples(&parsed, true);
        let branch = parsed.dir.as_deref().and_then(git_branch);
        println!("{}", render_line(&parsed, branch.as_deref()));
        return;
    }

    if let Some((rate_sample, usage_sample, tail)) = record_samples(&parsed, false) {
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
    }
}

/// Build and persist this render's samples. Returns `None`, persisting
/// nothing, when the input has no `session_id` or, with `throttle`, when the
/// session's stamp file is younger than [`CACHE_TTL_SECS`].
fn record_samples(
    parsed: &ParsedInput,
    throttle: bool,
) -> Option<(RateLimitSample, Option<UsageSample>, TranscriptTail)> {
    let Some(session_id) = parsed.session_id.as_deref().filter(|s| !s.is_empty()) else {
        log_error("stdin JSON missing session_id".to_string());
        return None;
    };

    // A missing or unresolvable stamp path just skips the throttle.
    let cache_path = cache_path_for(session_id);
    if throttle
        && let Some(ref p) = cache_path
        && read_cache_if_fresh(p, CACHE_TTL_SECS).is_some()
    {
        return None;
    }

    let hostname = resolve_hostname();
    let sampled_at = Utc::now().to_rfc3339();

    let tail: TranscriptTail = parsed
        .transcript_path
        .as_deref()
        .map(parse_transcript_tail)
        .unwrap_or_default();

    let rate_sample = build_rate_sample(&hostname, session_id, &sampled_at, parsed);
    let usage_sample = build_usage_sample(&hostname, session_id, &sampled_at, parsed, &tail);

    persist_samples(&rate_sample, usage_sample.as_ref());

    if throttle && let Some(ref p) = cache_path {
        write_cache(p, &sampled_at);
    }
    Some((rate_sample, usage_sample, tail))
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
    display_name: Option<String>,
    dir: Option<String>,
    context_pct: Option<f64>,
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
    let text = |x: Option<&Value>| {
        x.and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let display_name = text(v.get("model").and_then(|m| m.get("display_name")));
    let dir =
        text(v.get("workspace").and_then(|w| w.get("current_dir"))).or_else(|| text(v.get("cwd")));
    let cw = v.get("context_window");
    let context_pct = cw
        .and_then(|c| c.get("used_percentage"))
        .and_then(as_f64)
        .or_else(|| {
            cw.and_then(|c| c.get("remaining_percentage"))
                .and_then(as_f64)
                .map(|r| 100.0 - r)
        });

    let rl = v.get("rate_limits");
    let fh = rl.and_then(|r| r.get("five_hour"));
    let sd = rl.and_then(|r| r.get("seven_day"));

    Some(ParsedInput {
        session_id,
        transcript_path,
        model,
        display_name,
        dir,
        context_pct,
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
// Line rendering
// ---------------------------------------------------------------------------

/// Render the status line: model, directory, branch, `ctx:`, `s:`, `w:`,
/// joined by two spaces. A part whose input is absent is left out.
fn render_line(parsed: &ParsedInput, branch: Option<&str>) -> String {
    let model = parsed.display_name.as_deref().unwrap_or("Claude");
    let dir = parsed
        .dir
        .as_deref()
        .and_then(|d| Path::new(d).file_name())
        .map(|n| n.to_string_lossy().into_owned());
    let pct = |label: &str, v: Option<f64>| v.map(|n| format!("{label}:{n:.0}%"));
    [
        Some(model.to_owned()),
        dir,
        branch.map(str::to_owned),
        pct("ctx", parsed.context_pct),
        pct("s", parsed.five_hour_pct),
        pct("w", parsed.seven_day_pct),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("  ")
}

/// The branch checked out in `dir`, read the way the operator's script read
/// it: `symbolic-ref` with optional locks off, so it never waits on or takes
/// the index lock. `None` for a detached HEAD or a directory outside git.
fn git_branch(dir: &str) -> Option<String> {
    let out = match Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(dir)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            log_error(format!("run git symbolic-ref: {e}"));
            return None;
        }
    };
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!branch.is_empty()).then_some(branch)
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
    // half-written stamp. All steps are best-effort: a failure just means
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

pub fn resolve_hostname() -> String {
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
    use crate::usage::RawTokens;

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

    // --- status line ---------------------------------------------------

    const FULL: &str = r#"{
        "session_id": "abc-123",
        "model": {"id": "claude-opus-5-5", "display_name": "Opus 5.5"},
        "workspace": {"current_dir": "/work/legion"},
        "cwd": "/elsewhere/other",
        "context_window": {"used_percentage": 42.4, "remaining_percentage": 10},
        "rate_limits": {
            "five_hour": {"used_percentage": 12.6},
            "seven_day": {"used_percentage": 61}
        }
    }"#;

    /// Render `FULL` with `key` removed from the object at pointer `parent`.
    fn line_without(parent: &str, key: &str, branch: Option<&str>) -> String {
        let mut v: Value = serde_json::from_str(FULL).unwrap();
        if let Some(obj) = v.pointer_mut(parent).and_then(Value::as_object_mut) {
            obj.remove(key);
        }
        render_line(&parse_input(&v.to_string()).unwrap(), branch)
    }

    #[test]
    fn line_with_every_part_present() {
        assert_eq!(
            line_without("", "none", Some("feat/x")),
            "Opus 5.5  legion  feat/x  ctx:42%  s:13%  w:61%"
        );
    }

    #[test]
    fn line_drops_each_absent_part() {
        let b = Some("feat/x");
        assert_eq!(
            line_without("/model", "display_name", b),
            "Claude  legion  feat/x  ctx:42%  s:13%  w:61%"
        );
        assert_eq!(
            line_without("/rate_limits/five_hour", "used_percentage", b),
            "Opus 5.5  legion  feat/x  ctx:42%  w:61%"
        );
        assert_eq!(
            line_without("/rate_limits/seven_day", "used_percentage", b),
            "Opus 5.5  legion  feat/x  ctx:42%  s:13%"
        );
        assert_eq!(
            line_without("", "none", None),
            "Opus 5.5  legion  ctx:42%  s:13%  w:61%"
        );
        let mut v: Value = serde_json::from_str(FULL).unwrap();
        v.as_object_mut().unwrap().remove("context_window");
        assert_eq!(
            render_line(&parse_input(&v.to_string()).unwrap(), b),
            "Opus 5.5  legion  feat/x  s:13%  w:61%"
        );
        v.as_object_mut().unwrap().remove("workspace");
        v.as_object_mut().unwrap().remove("cwd");
        assert_eq!(
            render_line(&parse_input(&v.to_string()).unwrap(), b),
            "Opus 5.5  feat/x  s:13%  w:61%"
        );
    }

    #[test]
    fn line_dir_falls_back_to_cwd() {
        assert_eq!(
            line_without("", "workspace", None),
            "Opus 5.5  other  ctx:42%  s:13%  w:61%"
        );
    }

    #[test]
    fn line_ctx_from_remaining_when_used_absent() {
        let line = line_without("/context_window", "used_percentage", None);
        assert_eq!(line, "Opus 5.5  legion  ctx:90%  s:13%  w:61%");
    }

    #[test]
    fn line_from_partial_input_without_session_id() {
        assert_eq!(render_line(&parse_input("{}").unwrap(), None), "Claude");
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn git_branch_reads_branch_worktree_detached_and_non_git() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        assert_eq!(git_branch(repo.to_str().unwrap()).as_deref(), Some("main"));

        let wt = tmp.path().join("wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat/x",
                wt.to_str().unwrap(),
            ],
        );
        assert_eq!(git_branch(wt.to_str().unwrap()).as_deref(), Some("feat/x"));

        git(&repo, &["checkout", "-q", "--detach"]);
        assert_eq!(git_branch(repo.to_str().unwrap()), None);

        let plain = tmp.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert_eq!(git_branch(plain.to_str().unwrap()), None);
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

    /// #831: the uncertainty emit path resolves the producing model through
    /// this accessor instead of a hardcoded default. It must pick the newest
    /// sample for the requested session and nobody else's.
    #[test]
    fn db_latest_model_for_session_picks_newest_and_scopes_to_session() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("legion.db")).unwrap();

        let sample = |session: &str, at: &str, model: Option<&str>| RateLimitSample {
            id: Uuid::now_v7().to_string(),
            hostname: "puck".into(),
            session_id: session.into(),
            sampled_at: at.into(),
            five_hour_pct: None,
            five_hour_resets_at: None,
            seven_day_pct: None,
            seven_day_resets_at: None,
            model: model.map(Into::into),
        };

        db.insert_rate_limit_sample(&sample(
            "sess-1",
            "2026-07-01T00:00:00Z",
            Some("claude-opus-4-7"),
        ))
        .unwrap();
        db.insert_rate_limit_sample(&sample(
            "sess-1",
            "2026-07-30T00:00:00Z",
            Some("claude-opus-5"),
        ))
        .unwrap();
        // A different session must not leak into sess-1's answer.
        db.insert_rate_limit_sample(&sample(
            "sess-2",
            "2026-07-31T00:00:00Z",
            Some("claude-sonnet-4-6"),
        ))
        .unwrap();

        assert_eq!(
            db.latest_model_for_session("sess-1").unwrap().as_deref(),
            Some("claude-opus-5"),
        );
        assert_eq!(
            db.latest_model_for_session("unknown-session").unwrap(),
            None,
        );
    }

    /// A newer sample carrying no model must not shadow an older one that
    /// does -- otherwise a single null render would push the emit path onto
    /// the unknown marker despite the session's model being known.
    #[test]
    fn db_latest_model_for_session_skips_null_models() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("legion.db")).unwrap();

        let sample = |at: &str, model: Option<&str>| RateLimitSample {
            id: Uuid::now_v7().to_string(),
            hostname: "puck".into(),
            session_id: "sess-1".into(),
            sampled_at: at.into(),
            five_hour_pct: None,
            five_hour_resets_at: None,
            seven_day_pct: None,
            seven_day_resets_at: None,
            model: model.map(Into::into),
        };

        db.insert_rate_limit_sample(&sample("2026-07-01T00:00:00Z", Some("claude-opus-5")))
            .unwrap();
        db.insert_rate_limit_sample(&sample("2026-07-30T00:00:00Z", None))
            .unwrap();

        assert_eq!(
            db.latest_model_for_session("sess-1").unwrap().as_deref(),
            Some("claude-opus-5"),
        );
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
