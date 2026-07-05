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
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON array");
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
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON array");
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
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON array");
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
}
