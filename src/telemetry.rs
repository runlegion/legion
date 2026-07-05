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
    append_jsonl(&bypass_log_path(), record)
}

/// One row in `etc-usage.jsonl` (#707): a sanctioned `sym etc` / `sym tree`
/// query. The counterpart of `BypassRecord` -- bypass volume says where the
/// index fails agents; usage rows and their zero-result rate say whether the
/// sanctioned surface actually answers (#704's primary success metric).
///
/// The schema is shared across query shapes rather than one struct per
/// shape: `pattern` doubles as `extract`'s dotted `--field` path, and
/// `hit_count` is 0/1 for a single-value lookup instead of a match count.
/// `command` is what tells a reader which shape produced a given row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EtcUsageRecord {
    pub ts: DateTime<Utc>,
    /// Query shape, e.g. "find-content", "tree", "extract", "find-file".
    pub command: String,
    /// Repo filter; `None` means cross-repo. Always `None` for `extract`,
    /// which takes a direct path rather than a watch.toml repo.
    pub repo: Option<String>,
    /// The search pattern (`find-content`), dotted `--field` path
    /// (`extract`), or `query=...[,role=...]` description (`find-file`).
    pub pattern: String,
    /// `find-content` only; always `false` for other shapes.
    pub fixed_strings: bool,
    pub hit_count: u64,
    pub skipped_files: u64,
    /// Error text when the invocation failed before or during the scan
    /// (invalid regex, unknown repo, empty or unscannable corpus). `None` on
    /// success. Lets the #704 metric separate "tool answered zero" from
    /// "tool failed to answer"; defaulted so pre-#719 rows still parse.
    #[serde(default)]
    pub error: Option<String>,
    /// Watched repos whose workdir could not be walked at all. Nonzero with
    /// `error: None` means a partial corpus: the scan succeeded but covered
    /// fewer repos than the scope claimed. Without this, a zero-hit row over
    /// a half-dead corpus reads as "tool answered zero" in the #704 metric
    /// when part of the corpus was never searched. Defaulted so earlier
    /// rows still parse.
    #[serde(default)]
    pub failed_repos: u64,
    /// Detected source format for `extract` rows (json/toml/yaml/frontmatter,
    /// see `etc::SourceFormat`). `None` for `find-content` rows, for an
    /// `extract` invocation whose path had no recognizable format, and for
    /// rows logged before #708. Defaulted so earlier rows still parse.
    #[serde(default)]
    pub format: Option<String>,
}

/// Resolve the canonical etc-usage log path (sibling of `bypass.jsonl`).
pub fn etc_usage_log_path() -> PathBuf {
    bypass_log_dir().join("etc-usage.jsonl")
}

/// Append one usage record. Best-effort at the call site: a telemetry write
/// failure must never fail the query that produced it.
pub fn append_etc_usage(record: &EtcUsageRecord) -> Result<()> {
    append_jsonl(&etc_usage_log_path(), record)
}

/// Append one serializable record as a single JSONL line, creating parent
/// dirs and the file on first use.
///
/// The file is 0600 on unix: these logs persist raw search patterns and
/// query text, which can be secret-shaped (an agent grepping for a token
/// writes the token here). Shell history and Claude transcripts -- the
/// other places such strings land -- are 600/700; this must not be the
/// least-protected copy. The mode is set at open time, not chmodded after
/// create: a create-then-chmod leaves a window where the umask-default
/// file is world-readable, and a reader who opened an fd in that window
/// keeps it past the chmod. The follow-up `set_permissions` repairs files
/// created by earlier builds with default permissions.
fn append_jsonl<T: Serialize>(path: &std::path::Path, record: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
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

/// One row of `legion telemetry summary` output. Aggregates bypass
/// volume by `(tool, repo, pattern)` so an operator (and the uncertainty
/// engine consumer in #354) can see where sym/recall is under-serving.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BypassSummaryRow {
    pub tool: String,
    pub repo: String,
    pub pattern: String,
    pub count: usize,
    /// Fraction (0.0..=1.0) of bypasses where sym had hits the agent
    /// ignored. High value -> agent escapes despite a good answer; the
    /// reason list is the right place to look for the why.
    pub had_sym_hits_pct: f64,
    pub had_recall_hits_pct: f64,
    pub last_bypass_ts: DateTime<Utc>,
    /// Deduped list of bypass_reason strings observed for this group.
    pub bypass_reasons: Vec<String>,
}

/// Group bypass rows by (tool, repo, pattern), sort by count desc, take
/// the top `n`. Used by both the CLI summary and the HTTP endpoint.
pub fn summarize(rows: &[BypassRecord], top: usize) -> Vec<BypassSummaryRow> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<(String, String, String), Vec<&BypassRecord>> = BTreeMap::new();
    for r in rows {
        groups
            .entry((r.tool.clone(), r.repo.clone(), r.pattern.clone()))
            .or_default()
            .push(r);
    }
    let mut out: Vec<BypassSummaryRow> = groups
        .into_iter()
        .map(|((tool, repo, pattern), rs)| {
            let count = rs.len();
            let sym_hits = rs.iter().filter(|r| r.had_sym_hits).count();
            let recall_hits = rs.iter().filter(|r| r.had_recall_hits).count();
            let last = rs.iter().map(|r| r.ts).max().unwrap_or_else(Utc::now);
            let mut reasons: Vec<String> = rs.iter().map(|r| r.bypass_reason.clone()).collect();
            reasons.sort();
            reasons.dedup();
            BypassSummaryRow {
                tool,
                repo,
                pattern,
                count,
                had_sym_hits_pct: sym_hits as f64 / count as f64,
                had_recall_hits_pct: recall_hits as f64 / count as f64,
                last_bypass_ts: last,
                bypass_reasons: reasons,
            }
        })
        .collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.count));
    if top > 0 && out.len() > top {
        out.truncate(top);
    }
    out
}

/// Read all `sym etc` / `sym tree` usage rows, optionally filtered by
/// `since` (drop rows older than `now - since`) and `command` (e.g.
/// "find-content", "tree", "extract", "find-file"). Malformed lines are
/// skipped with a stderr breadcrumb, mirroring `list_bypasses`.
pub fn list_etc_usage(
    since: Option<Duration>,
    command: Option<&str>,
) -> Result<Vec<EtcUsageRecord>> {
    list_etc_usage_from(&etc_usage_log_path(), since, command)
}

fn list_etc_usage_from(
    path: &std::path::Path,
    since: Option<Duration>,
    command: Option<&str>,
) -> Result<Vec<EtcUsageRecord>> {
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
                eprintln!("[legion] etc-usage.jsonl line {} read error: {e}", i + 1);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let record: EtcUsageRecord = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[legion] etc-usage.jsonl line {} parse error: {e}", i + 1);
                continue;
            }
        };
        if let Some(c) = cutoff
            && record.ts < c
        {
            continue;
        }
        if let Some(cmd) = command
            && record.command != cmd
        {
            continue;
        }
        out.push(record);
    }
    Ok(out)
}

/// One row of `legion telemetry etc-summary` output (#713): the PRIMARY
/// success metric for the sym-etc epic (#704). This issue reworded the
/// grep/find guard, which changes what gets classified as a bypass
/// mid-experiment -- so raw bypass counts are the SECONDARY signal only
/// (see `BypassSummaryRow`). The primary signal is whether the sanctioned
/// surface actually answers: usage volume and zero-result rate per query
/// shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EtcSummaryRow {
    /// Query shape: "find-content", "tree", "extract", or "find-file".
    pub command: String,
    pub count: usize,
    /// Invocations that ran to completion but matched nothing -- the tool
    /// answered, and the answer was "zero". Distinct from `error_count`
    /// (the tool could not even attempt an answer).
    pub zero_result_count: usize,
    /// Fraction (0.0..=1.0) of non-error invocations that returned zero
    /// hits. A high rate on a shape is the same signal bypass volume used
    /// to be: evidence the sanctioned surface under-serves real queries.
    pub zero_result_pct: f64,
    pub error_count: usize,
    pub last_used_ts: DateTime<Utc>,
}

/// Group etc-usage rows by `command`, sort by count desc. Used by
/// `legion telemetry etc-summary`.
pub fn summarize_etc_usage(rows: &[EtcUsageRecord]) -> Vec<EtcSummaryRow> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, Vec<&EtcUsageRecord>> = BTreeMap::new();
    for r in rows {
        groups.entry(r.command.clone()).or_default().push(r);
    }
    let mut out: Vec<EtcSummaryRow> = groups
        .into_iter()
        .map(|(command, rs)| {
            let count = rs.len();
            let error_count = rs.iter().filter(|r| r.error.is_some()).count();
            let zero_result_count = rs
                .iter()
                .filter(|r| r.error.is_none() && r.hit_count == 0)
                .count();
            let non_error_count = count - error_count;
            let zero_result_pct = if non_error_count == 0 {
                0.0
            } else {
                zero_result_count as f64 / non_error_count as f64
            };
            let last = rs.iter().map(|r| r.ts).max().unwrap_or_else(Utc::now);
            EtcSummaryRow {
                command,
                count,
                zero_result_count,
                zero_result_pct,
                error_count,
                last_used_ts: last,
            }
        })
        .collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.count));
    out
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
        append_jsonl(&path, &rec).unwrap();
        let rows = list_bypasses_from(&path, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], rec);
    }

    #[test]
    fn etc_usage_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("etc-usage.jsonl");
        let rec = EtcUsageRecord {
            ts: Utc::now(),
            command: "find-content".to_string(),
            repo: Some("legion".to_string()),
            pattern: "<<<<<<<".to_string(),
            fixed_strings: true,
            hit_count: 3,
            skipped_files: 1,
            error: None,
            failed_repos: 1,
            format: None,
        };
        append_jsonl(&path, &rec).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: EtcUsageRecord = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(parsed, rec);
    }

    /// `extract` (#708) rows use the same schema: `pattern` carries the
    /// dotted `--field` path and `format` names the detected source format.
    #[test]
    fn etc_usage_extract_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("etc-usage.jsonl");
        let rec = EtcUsageRecord {
            ts: Utc::now(),
            command: "extract".to_string(),
            repo: None,
            pattern: "scripts.build".to_string(),
            fixed_strings: false,
            hit_count: 1,
            skipped_files: 0,
            error: None,
            failed_repos: 0,
            format: Some("json".to_string()),
        };
        append_jsonl(&path, &rec).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: EtcUsageRecord = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(parsed, rec);
    }

    /// Pre-#719 rows have no `error` key; the serde default must keep them
    /// parseable, and an error row must round-trip its text.
    #[test]
    fn etc_usage_error_field_defaults_and_round_trips() {
        let legacy = r#"{"ts":"2026-07-02T01:01:32Z","command":"find-content","repo":null,"pattern":"x","fixed_strings":false,"hit_count":0,"skipped_files":0}"#;
        let parsed: EtcUsageRecord = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.error, None);
        assert_eq!(parsed.failed_repos, 0);
        assert_eq!(parsed.format, None);

        let rec = EtcUsageRecord {
            ts: Utc::now(),
            command: "find-content".to_string(),
            repo: None,
            pattern: "(unclosed".to_string(),
            fixed_strings: false,
            hit_count: 0,
            skipped_files: 0,
            error: Some("invalid regex '(unclosed': ...".to_string()),
            failed_repos: 0,
            format: None,
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: EtcUsageRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);
    }

    /// Pre-#708 rows (find-content only) have no `format` key; the serde
    /// default must keep them parseable, and an extract row's format text
    /// must round-trip.
    #[test]
    fn etc_usage_format_field_defaults_and_round_trips() {
        let legacy = r#"{"ts":"2026-07-02T01:01:32Z","command":"find-content","repo":null,"pattern":"x","fixed_strings":false,"hit_count":0,"skipped_files":0,"error":null,"failed_repos":0}"#;
        let parsed: EtcUsageRecord = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.format, None);

        let rec = EtcUsageRecord {
            ts: Utc::now(),
            command: "extract".to_string(),
            repo: None,
            pattern: "database.port".to_string(),
            fixed_strings: false,
            hit_count: 0,
            skipped_files: 0,
            error: Some("field 'port' not found in 'config.yaml': ...".to_string()),
            failed_repos: 0,
            format: Some("yaml".to_string()),
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: EtcUsageRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);
    }

    fn etc_rec(
        command: &str,
        hit_count: u64,
        error: Option<&str>,
        ts: DateTime<Utc>,
    ) -> EtcUsageRecord {
        EtcUsageRecord {
            ts,
            command: command.to_string(),
            repo: Some("legion".to_string()),
            pattern: "x".to_string(),
            fixed_strings: false,
            hit_count,
            skipped_files: 0,
            error: error.map(|e| e.to_string()),
            failed_repos: 0,
            format: None,
        }
    }

    #[test]
    fn list_etc_usage_filters_by_since_and_command() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("etc-usage.jsonl");
        let old = etc_rec("find-content", 1, None, Utc::now() - Duration::days(10));
        let recent_fc = etc_rec("find-content", 0, None, Utc::now());
        let recent_tree = etc_rec("tree", 3, None, Utc::now());
        for r in [&old, &recent_fc, &recent_tree] {
            append_jsonl(&path, r).unwrap();
        }

        let all = list_etc_usage_from(&path, None, None).unwrap();
        assert_eq!(all.len(), 3);

        let since_7d = list_etc_usage_from(&path, Some(Duration::days(7)), None).unwrap();
        assert_eq!(since_7d.len(), 2, "the 10-day-old row must be dropped");

        let tree_only = list_etc_usage_from(&path, None, Some("tree")).unwrap();
        assert_eq!(tree_only.len(), 1);
        assert_eq!(tree_only[0].command, "tree");
    }

    /// The #713 primary metric: zero-result rate must exclude error rows
    /// from its denominator -- an invalid-regex error is "the tool could
    /// not attempt an answer", not "the tool answered zero", and conflating
    /// the two would understate how often the sanctioned surface actually
    /// works when it gets a chance to run.
    #[test]
    fn summarize_etc_usage_computes_zero_result_rate_excluding_errors() {
        let now = Utc::now();
        let rows = vec![
            etc_rec("find-content", 5, None, now),
            etc_rec("find-content", 0, None, now),
            etc_rec("find-content", 0, None, now),
            etc_rec("find-content", 0, Some("invalid regex"), now),
        ];
        let summary = summarize_etc_usage(&rows);
        assert_eq!(summary.len(), 1);
        let row = &summary[0];
        assert_eq!(row.command, "find-content");
        assert_eq!(row.count, 4);
        assert_eq!(row.error_count, 1);
        assert_eq!(row.zero_result_count, 2);
        assert!(
            (row.zero_result_pct - (2.0 / 3.0)).abs() < 1e-9,
            "2 zero-results out of 3 non-error invocations: {}",
            row.zero_result_pct
        );
    }

    #[test]
    fn summarize_etc_usage_groups_by_command_and_sorts_by_count_desc() {
        let now = Utc::now();
        let rows = vec![
            etc_rec("tree", 1, None, now),
            etc_rec("find-file", 1, None, now),
            etc_rec("find-file", 0, None, now),
        ];
        let summary = summarize_etc_usage(&rows);
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0].command, "find-file");
        assert_eq!(summary[0].count, 2);
        assert_eq!(summary[1].command, "tree");
        assert_eq!(summary[1].count, 1);
    }

    /// All-error groups must report 0.0, not NaN from a 0/0 division.
    #[test]
    fn summarize_etc_usage_all_errors_has_zero_pct_not_nan() {
        let now = Utc::now();
        let rows = vec![etc_rec("extract", 0, Some("bad field"), now)];
        let summary = summarize_etc_usage(&rows);
        assert_eq!(summary[0].zero_result_pct, 0.0);
    }

    #[cfg(unix)]
    #[test]
    fn append_tightens_file_permissions_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bypass.jsonl");
        append_jsonl(&path, &sample("legion", Utc::now())).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "telemetry logs persist raw patterns");
    }

    #[test]
    fn append_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a/b/c/bypass.jsonl");
        append_jsonl(&path, &sample("legion", Utc::now())).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn list_filters_by_repo() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bypass.jsonl");
        append_jsonl(&path, &sample("legion", Utc::now())).unwrap();
        append_jsonl(&path, &sample("smugglr", Utc::now())).unwrap();
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
        append_jsonl(&path, &sample("legion", now - Duration::hours(48))).unwrap();
        append_jsonl(&path, &sample("legion", now - Duration::minutes(5))).unwrap();
        append_jsonl(&path, &sample("smugglr", now - Duration::hours(48))).unwrap();
        append_jsonl(&path, &sample("smugglr", now - Duration::minutes(5))).unwrap();
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
        append_jsonl(&path, &sample("legion", now - Duration::hours(48))).unwrap();
        append_jsonl(&path, &sample("legion", now - Duration::minutes(5))).unwrap();
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
    fn summarize_groups_by_tool_repo_pattern() {
        let now = Utc::now();
        let rows = vec![
            sample("legion", now),
            sample("legion", now - Duration::minutes(5)),
            sample("smugglr", now - Duration::minutes(10)),
        ];
        let out = summarize(&rows, 0);
        assert_eq!(
            out.len(),
            2,
            "two groups (legion, smugglr) since pattern + tool match"
        );
        // Ordering: legion has count=2, smugglr count=1.
        assert_eq!(out[0].repo, "legion");
        assert_eq!(out[0].count, 2);
        assert_eq!(out[1].repo, "smugglr");
        assert_eq!(out[1].count, 1);
    }

    #[test]
    fn summarize_top_truncates() {
        let now = Utc::now();
        let mut rows = Vec::new();
        for i in 0..5 {
            let mut r = sample("legion", now);
            r.pattern = format!("pat-{i}");
            // Repeat each pattern 5-i times so counts are 5,4,3,2,1
            for _ in 0..(5 - i) {
                rows.push(r.clone());
            }
        }
        let out = summarize(&rows, 3);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].count, 5);
        assert_eq!(out[1].count, 4);
        assert_eq!(out[2].count, 3);
    }

    #[test]
    fn summarize_pcts_and_reasons() {
        let now = Utc::now();
        let mut a = sample("legion", now);
        a.bypass_reason = "env:LEGION_BYPASS_GREP=1".to_string();
        a.had_sym_hits = true;
        a.had_recall_hits = false;
        let mut b = sample("legion", now);
        b.bypass_reason = "searching literal".to_string();
        b.had_sym_hits = false;
        b.had_recall_hits = false;
        let mut c = sample("legion", now);
        c.bypass_reason = "env:LEGION_BYPASS_GREP=1".to_string();
        c.had_sym_hits = true;
        c.had_recall_hits = true;
        let out = summarize(&[a, b, c], 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].count, 3);
        assert!((out[0].had_sym_hits_pct - 2.0 / 3.0).abs() < 1e-9);
        assert!((out[0].had_recall_hits_pct - 1.0 / 3.0).abs() < 1e-9);
        // Reasons deduped.
        assert_eq!(out[0].bypass_reasons.len(), 2);
        assert!(
            out[0]
                .bypass_reasons
                .contains(&"env:LEGION_BYPASS_GREP=1".to_string())
        );
        assert!(
            out[0]
                .bypass_reasons
                .contains(&"searching literal".to_string())
        );
    }

    #[test]
    fn summarize_empty_input_is_empty_output() {
        let out = summarize(&[], 10);
        assert!(out.is_empty());
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
