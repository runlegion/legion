//! CLI end-to-end tests for `legion sym etc find-file` (#709).
//!
//! The pure matching/heuristic logic (`matches_name`, `FileRole::matches`)
//! is unit-tested in src/db/inventory.rs, and the scan/message plumbing
//! (`scan_find_file`, `compute_find_file_uncovered_message`,
//! `describe_find_file_scope`) in src/cli/index_cmd.rs; these tests
//! exercise the actual binary surface: flag parsing, the
//! `Database::list_file_inventory` read path seeded by a real `legion
//! index` run, cross-repo tagging, `--repo` scoping (which needs a real
//! watch.toml to validate against), and the telemetry side effect landing
//! in etc-usage.jsonl.

use crate::common::{legion_cmd, run_fail, run_ok, run_ok_stderr};

/// Seed a watch.toml in the data dir pointing at `repos` (name, workdir).
/// Backslashes are TOML escape syntax, so Windows paths interpolated raw
/// into a basic string make the whole file unparseable -- normalize to
/// forward slashes, which Windows path APIs accept.
fn seed_watch_toml(data_dir: &std::path::Path, repos: &[(&str, &std::path::Path)]) {
    let mut toml = String::new();
    for (name, workdir) in repos {
        toml.push_str(&format!(
            "[[repos]]\nname = \"{}\"\nworkdir = \"{}\"\n\n",
            name,
            workdir.display().to_string().replace('\\', "/")
        ));
    }
    std::fs::write(data_dir.join("watch.toml"), toml).expect("seed watch.toml");
}

#[test]
fn find_file_matches_by_basename_across_repos() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let alpha_dir = tempfile::tempdir().expect("alpha dir");
    let beta_dir = tempfile::tempdir().expect("beta dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    std::fs::create_dir_all(alpha_dir.path().join("web")).expect("mkdir");
    std::fs::write(alpha_dir.path().join("web/components.json"), "{}").expect("write fixture");
    std::fs::write(beta_dir.path().join("components.json"), "{}").expect("write fixture");
    seed_watch_toml(
        data_dir.path(),
        &[("alpha", alpha_dir.path()), ("beta", beta_dir.path())],
    );
    run_ok(legion_cmd(data_dir.path()).args(["index", "alpha"]));
    run_ok(legion_cmd(data_dir.path()).args(["index", "beta"]));

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "etc", "find-file", "components.json", "--json"]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let entries = envelope["entries"].as_array().expect("entries array");
    assert_eq!(
        entries.len(),
        2,
        "must find the file in both repos: {entries:?}"
    );
    let repos: std::collections::HashSet<&str> = entries
        .iter()
        .map(|e| e["repo"].as_str().unwrap())
        .collect();
    assert!(repos.contains("alpha") && repos.contains("beta"));

    // Human output tags each hit with its repo since the query is cross-repo.
    let human_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "etc", "find-file", "components.json"]),
    );
    assert!(
        human_out.contains("alpha/web/components.json"),
        "got:\n{human_out}"
    );
    assert!(
        human_out.contains("beta/components.json"),
        "got:\n{human_out}"
    );
}

#[test]
fn find_file_repo_flag_scopes_to_one_repo() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let alpha_dir = tempfile::tempdir().expect("alpha dir");
    let beta_dir = tempfile::tempdir().expect("beta dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    std::fs::write(alpha_dir.path().join("components.json"), "{}").expect("write fixture");
    std::fs::write(beta_dir.path().join("components.json"), "{}").expect("write fixture");
    seed_watch_toml(
        data_dir.path(),
        &[("alpha", alpha_dir.path()), ("beta", beta_dir.path())],
    );
    run_ok(legion_cmd(data_dir.path()).args(["index", "alpha"]));
    run_ok(legion_cmd(data_dir.path()).args(["index", "beta"]));

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "find-file",
                "components.json",
                "--repo",
                "alpha",
                "--json",
            ]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let entries = envelope["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1, "must scope to alpha only: {entries:?}");
    assert_eq!(entries[0]["repo"], "alpha");
}

#[test]
fn find_file_role_filters_to_config_files() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    std::fs::write(repo_dir.path().join("watch.toml"), "").expect("write fixture");
    std::fs::write(repo_dir.path().join("main.rs"), "").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("rolerepo", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "rolerepo"]));

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "etc", "find-file", "*", "--role", "config", "--json"]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let entries = envelope["entries"].as_array().expect("entries array");
    assert_eq!(
        entries.len(),
        1,
        "only the .toml file is role=config: {entries:?}"
    );
    assert_eq!(entries[0]["path"], "watch.toml");
}

#[test]
fn find_file_unknown_repo_fails_loudly() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    seed_watch_toml(data_dir.path(), &[("realrepo", repo_dir.path())]);

    let (_stdout, stderr) = run_fail(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "etc", "find-file", "x", "--repo", "nonesuch"]),
    );
    assert!(
        stderr.contains("not in watch.toml"),
        "expected the fix-hint error, got:\n{stderr}"
    );

    // The failed invocation itself lands in telemetry with the error text.
    let usage = std::fs::read_to_string(state_dir.path().join("legion/etc-usage.jsonl"))
        .expect("errored invocation still writes a usage row");
    let row: serde_json::Value =
        serde_json::from_str(usage.lines().next().expect("one row")).expect("row is JSON");
    assert_eq!(row["command"], "find-file");
    assert_eq!(row["hit_count"], 0);
    assert!(
        row["error"]
            .as_str()
            .is_some_and(|e| e.contains("nonesuch")),
        "error field should carry the failure, got: {row}"
    );
}

#[test]
fn find_file_no_match_prints_explicit_message_not_silence() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(repo_dir.path().join("a.rs"), "").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("findrepo", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "findrepo"]));

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "etc", "find-file", "nope.json", "--repo", "findrepo"]),
    );
    assert!(
        stderr.contains("no file named 'nope.json'"),
        "expected the explicit no-match hint, got:\n{stderr}"
    );
}

#[test]
fn find_file_uncovered_repo_prints_explicit_message_not_silence() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    // In watch.toml but never indexed -- zero inventory rows.
    seed_watch_toml(data_dir.path(), &[("findrepo", repo_dir.path())]);

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "etc", "find-file", "x", "--repo", "findrepo"]),
    );
    assert!(
        stderr.contains("no inventory for 'findrepo'") && stderr.contains("legion index findrepo"),
        "expected the explicit no-inventory hint, got:\n{stderr}"
    );
    // Never-indexed repo also has no snapshot row (#746): the freshness
    // line must name that explicitly, not stay silent about staleness.
    assert!(
        stderr.contains("no inventory snapshot recorded")
            && stderr.contains("legion index findrepo"),
        "expected the no-snapshot-recorded freshness hint, got:\n{stderr}"
    );
}

/// `--json` emits the `{snapshots, entries}` envelope, not a bare array
/// (#746).
#[test]
fn find_file_json_wraps_entries_with_snapshot_freshness() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    std::fs::write(repo_dir.path().join("a.rs"), "").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("findrepo", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "findrepo"]));

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "find-file",
                "a.rs",
                "--repo",
                "findrepo",
                "--json",
            ]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    assert!(envelope["entries"].is_array());
    let snapshots = envelope["snapshots"].as_array().expect("snapshots array");
    assert_eq!(
        snapshots.len(),
        1,
        "explicit --repo yields exactly one snapshot: {snapshots:?}"
    );
    assert_eq!(snapshots[0]["repo"], "findrepo");
    assert!(snapshots[0]["indexed_at"].is_string());
    assert_eq!(snapshots[0]["head_drift"], false);
}

/// Human output prints an "up to date" freshness line before the entry
/// list when the repo's live HEAD still matches the index-time HEAD
/// (#746).
#[test]
fn find_file_human_output_prints_up_to_date_when_head_matches() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    crate::common::run_git_fixture(repo_dir.path(), &["init"]);
    std::fs::write(repo_dir.path().join("a.rs"), "fn a() {}\n").expect("write fixture");
    crate::common::run_git_fixture(repo_dir.path(), &["add", "a.rs"]);
    crate::common::run_git_fixture(repo_dir.path(), &["commit", "-m", "initial"]);
    seed_watch_toml(data_dir.path(), &[("findrepo", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "findrepo"]));

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "etc", "find-file", "a.rs", "--repo", "findrepo"]),
    );
    assert!(
        stderr.contains("findrepo: indexed") && stderr.contains("up to date"),
        "expected an up-to-date freshness line, got:\n{stderr}"
    );
    assert!(!stderr.contains("WARNING"), "got:\n{stderr}");
}

/// Regression test built directly from the field report (bullpen post
/// 019f355c): a file's inventory row can be current while the repo's live
/// HEAD has moved past the index-time HEAD, with nothing in the old output
/// hinting the row might be stale. Seeds a real git repo, indexes it,
/// advances HEAD without re-indexing, then asserts `find-file` surfaces
/// the drift instead of staying silent (#746).
#[test]
fn find_file_regression_head_drift_surfaced_instead_of_silent() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    crate::common::run_git_fixture(repo_dir.path(), &["init"]);
    std::fs::write(repo_dir.path().join("CHANGELOG.md"), "# v1\n").expect("write fixture");
    crate::common::run_git_fixture(repo_dir.path(), &["add", "CHANGELOG.md"]);
    crate::common::run_git_fixture(repo_dir.path(), &["commit", "-m", "v1"]);
    seed_watch_toml(data_dir.path(), &[("legion", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "legion"]));

    // The repo moves past the indexed HEAD -- the release edit the field
    // report named -- without a follow-up `legion index` run.
    std::fs::write(repo_dir.path().join("CHANGELOG.md"), "# v2\n").expect("write fixture");
    crate::common::run_git_fixture(repo_dir.path(), &["add", "CHANGELOG.md"]);
    crate::common::run_git_fixture(repo_dir.path(), &["commit", "-m", "v2"]);

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "find-file",
                "CHANGELOG.md",
                "--repo",
                "legion",
                "--json",
            ]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let snapshots = envelope["snapshots"].as_array().expect("snapshots array");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0]["head_drift"], true,
        "the field report's exact scenario must now be visible: {snapshots:?}"
    );
    assert!(snapshots[0]["head_at_index"].is_string());
    assert!(snapshots[0]["current_head"].is_string());
    assert_ne!(snapshots[0]["head_at_index"], snapshots[0]["current_head"]);

    // Human output warns loudly instead of staying silent.
    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "find-file",
                "CHANGELOG.md",
                "--repo",
                "legion",
            ]),
    );
    assert!(
        stderr.contains("WARNING: current HEAD is")
            && stderr.contains("re-run 'legion index legion'"),
        "expected a head-drift warning, got:\n{stderr}"
    );
}
