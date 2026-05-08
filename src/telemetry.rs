//! Bypass telemetry: append-only JSONL log of agent escapes from the grep/Read
//! enforcement hooks (#437/#438/#439). Each row is a single JSON object on its
//! own line. The file lives under `$XDG_STATE_HOME/legion/bypass.jsonl` so it
//! survives a reboot and matches the index-log dir migration shipped in 0.13
//! (#424). Errors during write are surfaced to the caller; the hook layer
//! decides whether to drop them on the floor (it does -- telemetry must never
//! break the agent).
//!
//! Read path is `list_bypasses`, used by `legion telemetry list-bypasses` and
//! by the summary endpoint shipped in #440.
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{LegionError, Result};

/// One row in `bypass.jsonl`. Captures who escaped which enforcement hook on
/// what query, plus whether the index even had hits to redirect to. The last
/// two fields are the load-bearing telemetry signal -- a bypass with
/// `had_sym_hits = false` means the index is missing an answer the agent
/// expected; a bypass with `had_sym_hits = true` means the agent ignored a
/// good answer and we should ask why (reflections 019dd266, 019d766f).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BypassRecord {
    pub ts: DateTime<Utc>,
    pub repo: String,
    pub session_id: String,
    pub agent: String,
    pub tool: String,
    pub pattern: String,
    pub bypass_reason: String,
    pub had_sym_hits: bool,
    pub had_recall_hits: bool,
}

/// Resolve the canonical bypass log path. Uses `XDG_STATE_HOME` if set,
/// otherwise `$HOME/.local/state/legion`, otherwise system temp dir with a
/// stderr warning (matches `scip::index_log_dir`).
pub fn bypass_log_path() -> PathBuf {
    bypass_log_dir().join("bypass.jsonl")
}

fn bypass_log_dir() -> PathBuf {
    if let Ok(state) = std::env::var("XDG_STATE_HOME")
        && !state.is_empty()
    {
        return PathBuf::from(state).join("legion");
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(".local/state/legion");
    }
    let fallback = std::env::temp_dir().join("legion");
    eprintln!(
        "[legion] WARNING: neither XDG_STATE_HOME nor HOME set; bypass log at {} will not survive reboot",
        fallback.display()
    );
    fallback
}

/// Append one record to the bypass log. Creates parent dirs and the file on
/// first use. The write itself is atomic at the line level on local
/// filesystems because each record is a single short JSON line followed by a
/// newline; concurrent writers will not interleave bytes.
pub fn append_bypass(record: &BypassRecord) -> Result<()> {
    append_bypass_to(&bypass_log_path(), record)
}

/// Test-friendly variant: write to an explicit path instead of the resolved
/// XDG location.
fn append_bypass_to(path: &std::path::Path, record: &BypassRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(record)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Read all bypass rows, optionally filtered by `since` (drop rows older than
/// `now - since`) and `repo`. Malformed lines are skipped with a stderr
/// breadcrumb; one bad row never poisons the whole list.
pub fn list_bypasses(since: Option<Duration>, repo: Option<&str>) -> Result<Vec<BypassRecord>> {
    list_bypasses_from(&bypass_log_path(), since, repo)
}

fn list_bypasses_from(
    path: &std::path::Path,
    since: Option<Duration>,
    repo: Option<&str>,
) -> Result<Vec<BypassRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let cutoff = since.map(|d| Utc::now() - d);
    let file = std::fs::File::open(path)?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[legion] bypass.jsonl line {} read error: {e}", i + 1);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let record: BypassRecord = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[legion] bypass.jsonl line {} parse error: {e}", i + 1);
                continue;
            }
        };
        if let Some(c) = cutoff
            && record.ts < c
        {
            continue;
        }
        if let Some(r) = repo
            && record.repo != r
        {
            continue;
        }
        out.push(record);
    }
    Ok(out)
}

/// Parse a duration string like `24h`, `7d`, `30m`, `90s`. Used by
/// `--since` on `list-bypasses`.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return Err(LegionError::Telemetry(
            "duration is empty (try 24h, 7d, 30m)".to_string(),
        ));
    }
    let (num_part, unit) = s
        .split_at(s.find(|c: char| !c.is_ascii_digit()).ok_or_else(|| {
            LegionError::Telemetry(format!("duration '{s}' missing unit suffix"))
        })?);
    let n: i64 = num_part
        .parse()
        .map_err(|e| LegionError::Telemetry(format!("invalid duration number in '{s}': {e}")))?;
    match unit {
        "s" => Ok(Duration::seconds(n)),
        "m" => Ok(Duration::minutes(n)),
        "h" => Ok(Duration::hours(n)),
        "d" => Ok(Duration::days(n)),
        other => Err(LegionError::Telemetry(format!(
            "unknown duration unit '{other}' (use s, m, h, d)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample(repo: &str, ts: DateTime<Utc>) -> BypassRecord {
        BypassRecord {
            ts,
            repo: repo.to_string(),
            session_id: "sess-1".to_string(),
            agent: "legion".to_string(),
            tool: "Bash".to_string(),
            pattern: "fn main".to_string(),
            bypass_reason: "test".to_string(),
            had_sym_hits: true,
            had_recall_hits: false,
        }
    }

    #[test]
    fn round_trip_one_row() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested/bypass.jsonl");
        let rec = sample("legion", Utc::now());
        append_bypass_to(&path, &rec).unwrap();
        let rows = list_bypasses_from(&path, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], rec);
    }

    #[test]
    fn append_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a/b/c/bypass.jsonl");
        append_bypass_to(&path, &sample("legion", Utc::now())).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn list_filters_by_repo() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bypass.jsonl");
        append_bypass_to(&path, &sample("legion", Utc::now())).unwrap();
        append_bypass_to(&path, &sample("smugglr", Utc::now())).unwrap();
        let rows = list_bypasses_from(&path, None, Some("legion")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repo, "legion");
    }

    #[test]
    fn list_filters_by_since_and_repo_combined() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bypass.jsonl");
        let now = Utc::now();
        // 4 rows: 2 legion (one stale, one fresh), 2 smugglr (one stale, one fresh).
        append_bypass_to(&path, &sample("legion", now - Duration::hours(48))).unwrap();
        append_bypass_to(&path, &sample("legion", now - Duration::minutes(5))).unwrap();
        append_bypass_to(&path, &sample("smugglr", now - Duration::hours(48))).unwrap();
        append_bypass_to(&path, &sample("smugglr", now - Duration::minutes(5))).unwrap();
        let rows = list_bypasses_from(&path, Some(Duration::hours(24)), Some("legion")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repo, "legion");
        assert!(rows[0].ts > now - Duration::hours(24));
    }

    #[test]
    fn list_filters_by_since() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bypass.jsonl");
        let now = Utc::now();
        append_bypass_to(&path, &sample("legion", now - Duration::hours(48))).unwrap();
        append_bypass_to(&path, &sample("legion", now - Duration::minutes(5))).unwrap();
        let rows = list_bypasses_from(&path, Some(Duration::hours(24)), None).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].ts > now - Duration::hours(24));
    }

    #[test]
    fn list_missing_file_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("does-not-exist.jsonl");
        let rows = list_bypasses_from(&path, None, None).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn list_skips_malformed_lines() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bypass.jsonl");
        let good = sample("legion", Utc::now());
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{{not json").unwrap();
        writeln!(f, "{}", serde_json::to_string(&good).unwrap()).unwrap();
        writeln!(f).unwrap();
        drop(f);
        let rows = list_bypasses_from(&path, None, None).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn parse_duration_accepts_units() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::seconds(30));
        assert_eq!(parse_duration("15m").unwrap(), Duration::minutes(15));
        assert_eq!(parse_duration("24h").unwrap(), Duration::hours(24));
        assert_eq!(parse_duration("7d").unwrap(), Duration::days(7));
    }

    #[test]
    fn parse_duration_rejects_bad_input() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("24").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("24x").is_err());
    }

    #[test]
    fn bypass_log_path_uses_xdg_state_home() {
        let saved_xdg = std::env::var("XDG_STATE_HOME").ok();
        // SAFETY: this test mutates process env. Cargo runs tests in parallel
        // by default, so a concurrent test reading XDG_STATE_HOME could
        // observe the override. None do today; if that changes, run this
        // module with `cargo test -- --test-threads=1`.
        unsafe {
            std::env::set_var("XDG_STATE_HOME", "/tmp/legion-xdg-test");
        }
        let p = bypass_log_path();
        assert_eq!(p, PathBuf::from("/tmp/legion-xdg-test/legion/bypass.jsonl"));
        unsafe {
            match saved_xdg {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }
}
