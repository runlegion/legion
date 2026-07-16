//! Finding-resolution gate (#773): the business-logic layer for
//! `db::findings`, mirroring how `verify.rs` owns `GateResult` /
//! `GateProvenance` while `db::quality_gates` owns their persistence.
//!
//! Three concerns live here:
//!   - `FindingSeverity` / `FindingStatus`: the closed enums the findings
//!     ledger is typed over.
//!   - Structured extraction: `RawFinding` plus the two parse entry points
//!     (`extract_findings_from_value` for `--details-json`'s `findings` key,
//!     `parse_findings_array` for `--findings-json`'s bare array) that turn a
//!     skill's structured JSON output into rows `db::findings::insert_finding`
//!     can persist. Deliberately does NOT parse prose -- the legion-simplify
//!     articulation's "Verdict: finding (duplication, MED)" convention is not
//!     rigid enough to extract from reliably; that skill must emit
//!     `--findings-json` instead (see plugin/skills/legion-simplify/SKILL.md).
//!   - Resolution + refusal: `reconcile_pending_findings` (git-log-based
//!     resolution detection: a commit after a finding's origin that touches
//!     the flagged file resolves it) and `evaluate_refusal` (the pure
//!     predicate a `clean` verdict must clear against the PENDING set left
//!     over from PRIOR gate runs on this branch+skill -- no non-trivial
//!     finding pending, no un-acked LOW finding). This module's predicate is
//!     necessarily cross-commit (a fix landed on an earlier commit resolves a
//!     finding raised there), but it is NOT sufficient by itself: the CALLER
//!     (`cli::verify::reconcile_and_refuse_if_findings_pending`) additionally
//!     refuses a clean verdict that carries findings of its OWN in the same
//!     call -- legion-review's `approved` decision records `--result clean`
//!     in the very call that reports any surviving non-blocking findings, so
//!     a same-run finding is not hypothetical and this module's cross-commit
//!     predicate alone would let it straight through. See that function's
//!     doc comment for the combined check.

use std::process::Command;
use std::str::FromStr;

use crate::db::Database;
use crate::db::findings::QualityGateFinding;
use crate::error::{LegionError, Result};

/// Severity of a structured finding (#773). Stored as lowercase string in
/// `quality_gate_findings.severity`, mirroring `GateResult`'s
/// Display/FromStr/serde symmetry in `verify.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    High,
    Med,
    Low,
}

impl FindingSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Med => "med",
            Self::Low => "low",
        }
    }

    /// True for HIGH/MED -- a finding at this severity blocks a `clean`
    /// verdict until it is resolved or explicitly dispositioned (#773 AC1/2).
    /// LOW findings block too, but only until batch-acked (AC3) -- see
    /// `evaluate_refusal`, which is where that distinction is applied, not
    /// here; this predicate exists so callers never have to spell out
    /// `!= Low` themselves.
    pub fn is_non_trivial(self) -> bool {
        matches!(self, Self::High | Self::Med)
    }
}

impl std::fmt::Display for FindingSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FindingSeverity {
    type Err = LegionError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "high" => Ok(Self::High),
            "med" | "medium" => Ok(Self::Med),
            "low" => Ok(Self::Low),
            other => Err(LegionError::InvalidFindingSeverity(other.to_string())),
        }
    }
}

/// Lifecycle status of a structured finding (#773). Stored as lowercase
/// string in `quality_gate_findings.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingStatus {
    /// Newly extracted; neither resolved nor dispositioned.
    Pending,
    /// A commit after the finding's origin demonstrably touched the flagged
    /// file (`reconcile_pending_findings`).
    Resolved,
    /// An explicit reason was recorded for not fixing it (`dispose_finding`
    /// / `batch_ack_low_findings`).
    Dispositioned,
}

impl FindingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
            Self::Dispositioned => "dispositioned",
        }
    }
}

impl std::fmt::Display for FindingStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FindingStatus {
    type Err = LegionError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "resolved" => Ok(Self::Resolved),
            "dispositioned" => Ok(Self::Dispositioned),
            other => Err(LegionError::InvalidFindingStatus(other.to_string())),
        }
    }
}

// --- Structured extraction ---

/// A finding as a skill emits it over JSON, before severity has been parsed
/// into `FindingSeverity`. `line` and `severity` are deliberately loose at
/// this layer (skills emit `"HIGH"`/`"MED"`/`"LOW"`, or occasionally a
/// less-clean value) -- the caller (`cli::verify`) maps an unparseable
/// severity to MED with a warning rather than dropping the finding, since
/// silently discarding a structured finding here would reopen exactly the
/// evaporation hole this issue closes.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RawFinding {
    pub file: String,
    pub line: Option<u32>,
    pub severity: String,
    pub summary: String,
}

/// Pull the `findings` array out of a `--details-json` object, e.g. the
/// legion-review contract (`{"decision": ..., "findings": [...], ...}`).
/// Missing key, wrong shape, or an unparseable entry all degrade to "no
/// finding from that slot" rather than failing the whole gate record --
/// `findings_count` on the gate row itself stays the source of truth for how
/// many findings a run reported; this ledger is additive audit substrate.
pub fn extract_findings_from_value(value: &serde_json::Value) -> Vec<RawFinding> {
    let Some(arr) = value.get("findings").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| serde_json::from_value::<RawFinding>(v.clone()).ok())
        .collect()
}

/// Parse a bare JSON array of findings, e.g. `legion quality-gate check
/// --findings-json`'s payload. Unlike `extract_findings_from_value`, a
/// malformed top-level payload (not a JSON array at all) IS an error -- the
/// caller passed `--findings-json` explicitly, so garbage there should be
/// loud, not silently treated as "no findings".
pub fn parse_findings_array(raw: &str) -> Result<Vec<RawFinding>> {
    serde_json::from_str::<Vec<RawFinding>>(raw).map_err(|e| {
        LegionError::WorkSource(format!(
            "--findings-json is not a JSON array of findings: {e}"
        ))
    })
}

// --- Refusal predicate ---

/// The result of evaluating the PENDING set for a (branch, skill) pair
/// against the refusal predicate (#773 AC1-3).
#[derive(Debug, Default)]
pub struct FindingRefusal {
    /// PENDING findings at HIGH/MED severity -- block `clean` until resolved
    /// or individually dispositioned.
    pub blocking: Vec<QualityGateFinding>,
    /// PENDING findings at LOW severity -- block `clean` until batch-acked,
    /// but never require per-item ceremony (AC3).
    pub trivial_unacked: Vec<QualityGateFinding>,
}

impl FindingRefusal {
    /// True when a `clean` verdict must be refused.
    pub fn blocks(&self) -> bool {
        !self.blocking.is_empty() || !self.trivial_unacked.is_empty()
    }
}

/// Pure predicate: partition an already-PENDING finding set into blocking
/// (HIGH/MED) and trivial-unacked (LOW). Callers pass the PENDING set for a
/// single (branch, skill) pair post-reconcile (`db::list_pending_findings`);
/// a finding whose status is not PENDING is skipped defensively (it should
/// never appear in that set, but this function makes no assumption about its
/// caller having already filtered).
pub fn evaluate_refusal(pending: &[QualityGateFinding]) -> FindingRefusal {
    let mut refusal = FindingRefusal::default();
    for finding in pending {
        if finding.status != FindingStatus::Pending {
            continue;
        }
        if finding.severity.is_non_trivial() {
            refusal.blocking.push(finding.clone());
        } else {
            refusal.trivial_unacked.push(finding.clone());
        }
    }
    refusal
}

// --- Resolution detection ---

/// Pure predicate: given the commit hashes (oldest-first) that touched a
/// file strictly after a finding's origin commit, return the one that
/// resolved it -- the earliest commit in that range, i.e. the first commit
/// that touched the file once the finding existed. `None` means still
/// pending (the range was empty).
pub fn resolving_commit(touching_commits_oldest_first: &[String]) -> Option<&String> {
    touching_commits_oldest_first.first()
}

/// Thin git wrapper: commit hashes (oldest-first) that touched `file`
/// strictly between `origin_commit` (exclusive) and `head_commit`
/// (inclusive). File-level touch detection, not line-level -- `git log -L`
/// line-tracking is fragile as unrelated edits shift line numbers, so this
/// checks "did any commit since the finding was raised modify this file at
/// all", the same granularity `cli::util::git_changed_files` already uses
/// for the simplify coverage set.
///
/// Returns an empty vec (not an error) when the range is empty
/// (`origin_commit == head_commit`) or git reports no matching commits. A
/// git invocation failure IS an error -- it must never be silently read as
/// "nothing touched it".
///
/// `repo_dir` sets the git invocation's working directory explicitly when
/// `Some` (tests, against an isolated fixture repo); `None` runs against the
/// process's current directory, which is always correct in production since
/// every `legion` CLI invocation already runs from inside the repo it is
/// operating on (matching `cli::util::git_changed_files`'s convention).
pub fn commits_touching_file_in_range(
    repo_dir: Option<&std::path::Path>,
    origin_commit: &str,
    head_commit: &str,
    file: &str,
) -> Result<Vec<String>> {
    if origin_commit == head_commit {
        return Ok(Vec::new());
    }
    let range = format!("{origin_commit}..{head_commit}");
    let mut cmd = Command::new("git");
    if let Some(dir) = repo_dir {
        cmd.current_dir(dir);
    }
    let out = cmd
        .args(["log", "--format=%H", "--reverse", &range, "--", file])
        .output()
        .map_err(|e| LegionError::WorkSource(format!("failed to run git log for {file}: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(LegionError::WorkSource(format!(
            "git log --format=%H --reverse {range} -- {file} failed: {stderr}"
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .filter(|l| !l.trim().is_empty())
        .collect())
}

/// Reconcile every PENDING finding on `branch`/`skill` against
/// `head_commit`: for each whose flagged file was touched by a commit after
/// its origin, mark it RESOLVED with that commit
/// (`Database::mark_finding_resolved`). Best-effort per finding -- a git
/// failure on one finding is logged to stderr and does not abort
/// reconciling the rest, since a transient git error on one stale finding
/// must not itself block a gate record/check that has nothing to do with
/// it. Returns the number of findings resolved. Called unconditionally at
/// the top of both `quality-gate record` and `quality-gate check`, before
/// `evaluate_refusal` ever runs, so a fix landed in an earlier commit is
/// never mistaken for still-pending.
pub fn reconcile_pending_findings(
    database: &Database,
    repo_dir: Option<&std::path::Path>,
    branch: &str,
    skill: &str,
    head_commit: &str,
) -> Result<usize> {
    let pending = database.list_pending_findings(branch, skill)?;
    let mut resolved_count = 0usize;
    for finding in pending {
        match commits_touching_file_in_range(
            repo_dir,
            &finding.origin_commit,
            head_commit,
            &finding.file,
        ) {
            Ok(touching) => {
                if let Some(commit) = resolving_commit(&touching) {
                    match database.mark_finding_resolved(&finding.id, commit) {
                        Ok(_) => resolved_count += 1,
                        Err(e) => eprintln!(
                            "[legion] warning: failed to mark finding {} resolved: {e}",
                            finding.id
                        ),
                    }
                }
            }
            Err(e) => eprintln!(
                "[legion] warning: resolution check failed for finding {} ({}): {e}",
                finding.id, finding.file
            ),
        }
    }
    Ok(resolved_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::findings::NewFindingInput;
    use crate::db::testutil::test_db;

    // --- FindingSeverity / FindingStatus ---

    #[test]
    fn severity_round_trips_through_str() {
        for (s, expected) in [
            ("high", FindingSeverity::High),
            ("HIGH", FindingSeverity::High),
            ("med", FindingSeverity::Med),
            ("medium", FindingSeverity::Med),
            ("MED", FindingSeverity::Med),
            ("low", FindingSeverity::Low),
        ] {
            let parsed: FindingSeverity = s.parse().unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(
                parsed.as_str().parse::<FindingSeverity>().unwrap(),
                expected
            );
        }
    }

    #[test]
    fn severity_invalid_str_errors() {
        assert!("critical".parse::<FindingSeverity>().is_err());
    }

    #[test]
    fn severity_is_non_trivial() {
        assert!(FindingSeverity::High.is_non_trivial());
        assert!(FindingSeverity::Med.is_non_trivial());
        assert!(!FindingSeverity::Low.is_non_trivial());
    }

    #[test]
    fn status_round_trips_through_str() {
        for (s, expected) in [
            ("pending", FindingStatus::Pending),
            ("resolved", FindingStatus::Resolved),
            ("dispositioned", FindingStatus::Dispositioned),
        ] {
            let parsed: FindingStatus = s.parse().unwrap();
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn status_invalid_str_errors() {
        assert!("waived".parse::<FindingStatus>().is_err());
    }

    // --- extraction ---

    #[test]
    fn extract_findings_from_value_pulls_findings_array() {
        let value = serde_json::json!({
            "decision": "changes_requested",
            "findings": [
                {"file": "src/foo.rs", "line": 12, "severity": "HIGH", "summary": "unchecked input"},
                {"file": "src/bar.rs", "line": null, "severity": "LOW", "summary": "naming nit"},
            ],
            "refuted_count": 1,
        });
        let findings = extract_findings_from_value(&value);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].file, "src/foo.rs");
        assert_eq!(findings[0].line, Some(12));
        assert_eq!(findings[1].line, None);
    }

    #[test]
    fn extract_findings_from_value_missing_key_returns_empty() {
        let value = serde_json::json!({"decision": "approved"});
        assert!(extract_findings_from_value(&value).is_empty());
    }

    #[test]
    fn extract_findings_from_value_skips_malformed_entries() {
        let value = serde_json::json!({
            "findings": [
                {"file": "src/foo.rs", "severity": "HIGH", "summary": "ok"},
                {"file": "src/missing-fields.rs"},
            ]
        });
        let findings = extract_findings_from_value(&value);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/foo.rs");
    }

    #[test]
    fn parse_findings_array_parses_bare_array() {
        let raw =
            r#"[{"file": "src/foo.rs", "line": 10, "severity": "MED", "summary": "dup logic"}]"#;
        let findings = parse_findings_array(raw).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, "MED");
    }

    #[test]
    fn parse_findings_array_rejects_non_array_payload() {
        let err = parse_findings_array(r#"{"not": "an array"}"#).unwrap_err();
        assert!(err.to_string().contains("--findings-json"));
    }

    // --- refusal predicate ---

    fn finding(severity: FindingSeverity, status: FindingStatus) -> QualityGateFinding {
        QualityGateFinding {
            id: uuid::Uuid::now_v7().to_string(),
            gate_id: "gate-1".to_string(),
            branch: "feat/x".to_string(),
            skill: "legion-simplify".to_string(),
            origin_commit: "commit-a".to_string(),
            file: "src/foo.rs".to_string(),
            line: Some(10),
            severity,
            summary: "test finding".to_string(),
            status,
            disposition_reason: None,
            resolved_by_commit: None,
            created_at: "2026-07-14T00:00:00Z".to_string(),
            updated_at: "2026-07-14T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn evaluate_refusal_empty_set_does_not_block() {
        let refusal = evaluate_refusal(&[]);
        assert!(!refusal.blocks());
    }

    #[test]
    fn evaluate_refusal_high_blocks() {
        let refusal = evaluate_refusal(&[finding(FindingSeverity::High, FindingStatus::Pending)]);
        assert!(refusal.blocks());
        assert_eq!(refusal.blocking.len(), 1);
        assert!(refusal.trivial_unacked.is_empty());
    }

    #[test]
    fn evaluate_refusal_med_blocks() {
        let refusal = evaluate_refusal(&[finding(FindingSeverity::Med, FindingStatus::Pending)]);
        assert!(refusal.blocks());
        assert_eq!(refusal.blocking.len(), 1);
    }

    #[test]
    fn evaluate_refusal_low_blocks_until_acked() {
        let refusal = evaluate_refusal(&[finding(FindingSeverity::Low, FindingStatus::Pending)]);
        assert!(
            refusal.blocks(),
            "a LOW finding must block until batch-acked, not silently pass (#773 AC3)"
        );
        assert!(refusal.blocking.is_empty());
        assert_eq!(refusal.trivial_unacked.len(), 1);
    }

    #[test]
    fn evaluate_refusal_skips_non_pending_defensively() {
        let refusal = evaluate_refusal(&[finding(FindingSeverity::High, FindingStatus::Resolved)]);
        assert!(!refusal.blocks());
    }

    // --- resolving_commit (pure) ---

    #[test]
    fn resolving_commit_empty_range_is_none() {
        assert!(resolving_commit(&[]).is_none());
    }

    #[test]
    fn resolving_commit_returns_the_oldest_touching_commit() {
        let commits = vec!["commit-1".to_string(), "commit-2".to_string()];
        assert_eq!(resolving_commit(&commits), Some(&"commit-1".to_string()));
    }

    // --- reconcile_pending_findings (DB-level, no real git) ---

    #[test]
    fn reconcile_skips_findings_whose_origin_equals_head() {
        // origin_commit == head_commit short-circuits before any git call --
        // a finding just raised on this exact commit cannot be resolved by
        // itself.
        let db = test_db();
        let finding = db
            .insert_finding(&NewFindingInput {
                gate_id: "gate-1",
                branch: "feat/x",
                skill: "legion-simplify",
                origin_commit: "same-commit",
                file: "src/foo.rs",
                line: Some(1),
                severity: FindingSeverity::High,
                summary: "test",
            })
            .unwrap();

        let resolved_count =
            reconcile_pending_findings(&db, None, "feat/x", "legion-simplify", "same-commit")
                .unwrap();
        assert_eq!(resolved_count, 0);
        let refetched = db.get_finding_by_id(&finding.id).unwrap().unwrap();
        assert_eq!(refetched.status, FindingStatus::Pending);
    }

    #[test]
    fn commits_touching_file_in_range_empty_when_origin_equals_head() {
        let touching =
            commits_touching_file_in_range(None, "same-hash", "same-hash", "src/foo.rs").unwrap();
        assert!(touching.is_empty());
    }

    // --- real-git integration tests ---
    //
    // Isolated per-repo git identity/config, mirroring `inventory.rs`'s
    // `fixture_git_command`/`run_git_fixture` -- this is a separate module in
    // the same crate target, but that helper is `#[cfg(test)] mod`-private to
    // `inventory.rs`, so it is not importable here; duplicating the isolation
    // helper (rather than threading a new shared export through `testutil`
    // for one more call site) matches the precedent that file's own comment
    // already sets for the same tradeoff against the integration crate.

    fn isolated_git_config_paths() -> &'static (std::path::PathBuf, std::path::PathBuf) {
        static ISOLATED_GIT_CONFIG: std::sync::OnceLock<(std::path::PathBuf, std::path::PathBuf)> =
            std::sync::OnceLock::new();
        ISOLATED_GIT_CONFIG.get_or_init(|| {
            let dir = tempfile::tempdir().expect("create isolated git config dir");
            let global = dir.path().join("global.gitconfig");
            let system = dir.path().join("system.gitconfig");
            std::fs::write(&global, "").expect("write isolated global gitconfig");
            std::fs::write(&system, "").expect("write isolated system gitconfig");
            std::mem::forget(dir);
            (global, system)
        })
    }

    fn fixture_git_command(dir: &std::path::Path) -> Command {
        let (global_config, system_config) = isolated_git_config_paths();
        let mut cmd = Command::new("git");
        cmd.current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", global_config)
            .env("GIT_CONFIG_SYSTEM", system_config)
            .env("GIT_DIR", dir.join(".git"))
            .env("GIT_WORK_TREE", dir);
        cmd
    }

    fn run_git_fixture(dir: &std::path::Path, args: &[&str]) -> String {
        let mut full_args: Vec<&str> = vec![
            "-c",
            "user.name=Legion Test Fixture",
            "-c",
            "user.email=legion-test-fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ];
        full_args.extend_from_slice(args);
        let out = fixture_git_command(dir)
            .args(&full_args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn in {dir:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} exited non-zero in {dir:?}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit_file(dir: &std::path::Path, path: &str, contents: &str, message: &str) -> String {
        std::fs::write(dir.join(path), contents).unwrap();
        run_git_fixture(dir, &["add", path]);
        run_git_fixture(dir, &["commit", "-q", "-m", message]);
        run_git_fixture(dir, &["rev-parse", "HEAD"])
    }

    #[test]
    fn commits_touching_file_in_range_finds_a_later_touch() {
        let dir = tempfile::tempdir().unwrap();
        run_git_fixture(dir.path(), &["init", "-q"]);
        let origin = commit_file(dir.path(), "foo.rs", "fn foo() {}\n", "initial");
        // An unrelated file changes first -- must not count as touching foo.rs.
        commit_file(dir.path(), "bar.rs", "fn bar() {}\n", "unrelated change");
        let fix_commit = commit_file(
            dir.path(),
            "foo.rs",
            "fn foo() { /* fixed */ }\n",
            "fix foo",
        );

        let touching =
            commits_touching_file_in_range(Some(dir.path()), &origin, &fix_commit, "foo.rs")
                .unwrap();
        assert_eq!(touching, vec![fix_commit.clone()]);
        assert_eq!(resolving_commit(&touching), Some(&fix_commit));
    }

    #[test]
    fn commits_touching_file_in_range_empty_when_file_untouched_since_origin() {
        let dir = tempfile::tempdir().unwrap();
        run_git_fixture(dir.path(), &["init", "-q"]);
        let origin = commit_file(dir.path(), "foo.rs", "fn foo() {}\n", "initial");
        let head = commit_file(dir.path(), "bar.rs", "fn bar() {}\n", "unrelated change");

        let touching =
            commits_touching_file_in_range(Some(dir.path()), &origin, &head, "foo.rs").unwrap();
        assert!(touching.is_empty());
    }

    #[test]
    fn reconcile_pending_findings_marks_resolved_when_a_later_commit_touches_the_file() {
        let dir = tempfile::tempdir().unwrap();
        run_git_fixture(dir.path(), &["init", "-q"]);
        let origin = commit_file(dir.path(), "foo.rs", "fn foo() {}\n", "initial");

        let db = test_db();
        let raised = db
            .insert_finding(&NewFindingInput {
                gate_id: "gate-1",
                branch: "feat/x",
                skill: "legion-simplify",
                origin_commit: &origin,
                file: "foo.rs",
                line: Some(1),
                severity: FindingSeverity::High,
                summary: "duplicate logic",
            })
            .unwrap();

        let fix_commit = commit_file(
            dir.path(),
            "foo.rs",
            "fn foo() { /* fixed */ }\n",
            "fix foo",
        );

        let resolved_count = reconcile_pending_findings(
            &db,
            Some(dir.path()),
            "feat/x",
            "legion-simplify",
            &fix_commit,
        )
        .unwrap();

        assert_eq!(resolved_count, 1);
        let refetched = db.get_finding_by_id(&raised.id).unwrap().unwrap();
        assert_eq!(refetched.status, FindingStatus::Resolved);
        assert_eq!(
            refetched.resolved_by_commit.as_deref(),
            Some(fix_commit.as_str())
        );
    }
}
