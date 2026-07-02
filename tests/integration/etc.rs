//! CLI end-to-end tests for `legion sym etc find-content` (#707).
//!
//! The library core (`etc::find_content`) is unit-tested in src/etc.rs;
//! these tests exercise the actual binary surface the review found
//! uncovered: flag parsing, output shapes, loud error paths, and the
//! telemetry side effect landing in etc-usage.jsonl.

use crate::common::{legion_cmd, run_fail, run_ok};

/// Seed a watch.toml in the data dir pointing at `repos` (name, workdir).
fn seed_watch_toml(data_dir: &std::path::Path, repos: &[(&str, &std::path::Path)]) {
    let mut toml = String::new();
    for (name, workdir) in repos {
        toml.push_str(&format!(
            "[[repos]]\nname = \"{}\"\nworkdir = \"{}\"\n\n",
            name,
            workdir.display()
        ));
    }
    std::fs::write(data_dir.join("watch.toml"), toml).expect("seed watch.toml");
}

#[test]
fn find_content_cli_end_to_end_with_json_and_telemetry() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(
        repo_dir.path().join("style.css"),
        "--spacing-0.5: 4px;\nbody { margin: 0; }\n",
    )
    .expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("etcrepo", repo_dir.path())]);

    // Human output: path:line: text, punctuation literal survives argv + matching.
    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "find-content",
                "--spacing-0.5",
                "--repo",
                "etcrepo",
                "--fixed-strings",
            ]),
    );
    assert!(
        stdout.contains("style.css:1: --spacing-0.5: 4px;"),
        "expected path:line: text hit, got:\n{stdout}"
    );

    // JSON output: structured ContentHit array.
    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "find-content",
                "margin",
                "--repo",
                "etcrepo",
                "--json",
            ]),
    );
    let hits: Vec<serde_json::Value> =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON array");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["repo"], "etcrepo");
    assert_eq!(hits[0]["path"], "style.css");
    assert_eq!(hits[0]["line"], 2);

    // Telemetry side effect: one row per completed search in etc-usage.jsonl.
    let usage_path = state_dir.path().join("legion/etc-usage.jsonl");
    let usage = std::fs::read_to_string(&usage_path).expect("etc-usage.jsonl written");
    let rows: Vec<&str> = usage.lines().collect();
    assert_eq!(rows.len(), 2, "one row per completed search:\n{usage}");
    let first: serde_json::Value = serde_json::from_str(rows[0]).expect("row is JSON");
    assert_eq!(first["command"], "find-content");
    assert_eq!(first["pattern"], "--spacing-0.5");
    assert_eq!(first["fixed_strings"], true);
    assert_eq!(first["hit_count"], 1);
}

#[test]
fn find_content_unknown_repo_fails_loudly() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    // Errored invocations write telemetry too (#719 review fix), so the
    // state dir must be isolated or the test pollutes the real usage log.
    let state_dir = tempfile::tempdir().expect("state dir");
    seed_watch_toml(data_dir.path(), &[("etcrepo", repo_dir.path())]);

    let (_stdout, stderr) = run_fail(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "etc", "find-content", "needle", "--repo", "nonesuch"]),
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
    assert_eq!(row["hit_count"], 0);
    assert!(
        row["error"]
            .as_str()
            .is_some_and(|e| e.contains("nonesuch")),
        "error field should carry the failure, got: {row}"
    );
}

#[test]
fn find_content_empty_corpus_fails_loudly() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(data_dir.path().join("watch.toml"), "").expect("empty watch.toml");

    let (_stdout, stderr) = run_fail(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "etc", "find-content", "needle"]),
    );
    assert!(
        stderr.contains("no repos in watch.toml"),
        "expected the empty-corpus error, got:\n{stderr}"
    );
}

#[test]
fn find_content_all_repos_unscannable_fails_loudly() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    let ghost = data_dir.path().join("no-such-workdir");
    seed_watch_toml(data_dir.path(), &[("ghost", ghost.as_path())]);

    let (_stdout, stderr) = run_fail(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "etc", "find-content", "needle"]),
    );
    assert!(
        stderr.contains("no repo could be scanned") && stderr.contains("ghost"),
        "expected the unscannable-corpus error naming the repo, got:\n{stderr}"
    );
}
