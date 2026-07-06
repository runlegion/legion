//! CLI end-to-end tests for `legion sym tree` (#706).
//!
//! The pure filtering/scoping logic (`filter_and_scope`, `under_matches`,
//! `tree_depth`, `compute_uncovered_message`) is unit-tested in
//! src/cli/index_cmd.rs; these tests exercise the actual binary surface:
//! flag parsing, the `Database::list_file_inventory` read path seeded by a
//! real `legion index` run, cross-repo tagging, and the telemetry side
//! effect landing in etc-usage.jsonl.

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
fn tree_cli_end_to_end_with_json_ext_and_telemetry() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    std::fs::create_dir_all(repo_dir.path().join("src/db")).expect("mkdir src/db");
    std::fs::write(
        repo_dir.path().join("src/db/inventory.rs"),
        "pub fn f() {}\n",
    )
    .expect("write fixture");
    std::fs::write(repo_dir.path().join("README.md"), "# hi\n").expect("write fixture");
    std::fs::write(repo_dir.path().join("deploy.sh"), "#!/bin/sh\n").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("treerepo", repo_dir.path())]);

    // Populate the inventory table via a real (docs-only, no SCIP markers)
    // `legion index` run -- no filesystem walk happens at query time after this.
    run_ok(legion_cmd(data_dir.path()).args(["index", "treerepo"]));

    // --ext filters server-side to just the .rs file.
    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--repo", "treerepo", "--ext", "rs", "--json"]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let entries = envelope["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1, "only the .rs file should match --ext rs");
    assert_eq!(entries[0]["repo"], "treerepo");
    assert_eq!(entries[0]["path"], "src/db/inventory.rs");
    assert_eq!(entries[0]["ext"], "rs");
    assert_eq!(entries[0]["lang"], "rust");

    // No --ext: every file appears; non-symbol files carry lang = null.
    let all_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--repo", "treerepo", "--json"]),
    );
    let all_envelope: serde_json::Value =
        serde_json::from_str(all_out.trim()).expect("stdout is a JSON envelope object");
    let all = all_envelope["entries"].as_array().expect("entries array");
    assert_eq!(all.len(), 3, "every inventoried file must appear: {all:?}");
    let readme = all
        .iter()
        .find(|e| e["path"] == "README.md")
        .expect("README.md present");
    assert!(readme["lang"].is_null(), "README.md must have lang=null");
    let script = all
        .iter()
        .find(|e| e["path"] == "deploy.sh")
        .expect("deploy.sh present");
    assert!(script["lang"].is_null(), "deploy.sh must have lang=null");

    // Telemetry: one row per invocation, command="tree", with result count.
    let usage_path = state_dir.path().join("legion/etc-usage.jsonl");
    let usage = std::fs::read_to_string(&usage_path).expect("etc-usage.jsonl written");
    let rows: Vec<&str> = usage.lines().collect();
    assert_eq!(rows.len(), 2, "one row per completed tree query:\n{usage}");
    let first: serde_json::Value = serde_json::from_str(rows[0]).expect("row is JSON");
    assert_eq!(first["command"], "tree");
    assert_eq!(first["repo"], "treerepo");
    assert_eq!(first["hit_count"], 1);
    let second: serde_json::Value = serde_json::from_str(rows[1]).expect("row is JSON");
    assert_eq!(second["hit_count"], 3);
}

#[test]
fn tree_under_scopes_to_subtree() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    std::fs::create_dir_all(repo_dir.path().join("src/db")).expect("mkdir");
    std::fs::write(repo_dir.path().join("src/db/inventory.rs"), "").expect("write fixture");
    std::fs::write(repo_dir.path().join("src/main.rs"), "").expect("write fixture");
    // A sibling that shares the string prefix "src/db" but is not inside it.
    std::fs::write(repo_dir.path().join("src/dbfoo.rs"), "").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("treerepo", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "treerepo"]));

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym", "tree", "--repo", "treerepo", "--under", "src/db", "--json",
            ]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let entries = envelope["entries"].as_array().expect("entries array");
    assert_eq!(
        entries.len(),
        1,
        "--under src/db must exclude the sibling src/dbfoo.rs: {entries:?}"
    );
    assert_eq!(entries[0]["path"], "src/db/inventory.rs");
}

#[test]
fn tree_no_repo_is_cross_repo_and_tags_each_entry() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let alpha_dir = tempfile::tempdir().expect("alpha dir");
    let beta_dir = tempfile::tempdir().expect("beta dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    std::fs::write(alpha_dir.path().join("a.rs"), "").expect("write fixture");
    std::fs::write(beta_dir.path().join("b.rs"), "").expect("write fixture");
    seed_watch_toml(
        data_dir.path(),
        &[("alpha", alpha_dir.path()), ("beta", beta_dir.path())],
    );
    run_ok(legion_cmd(data_dir.path()).args(["index", "alpha"]));
    run_ok(legion_cmd(data_dir.path()).args(["index", "beta"]));

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--json"]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let entries = envelope["entries"].as_array().expect("entries array");
    let repos: std::collections::HashSet<&str> = entries
        .iter()
        .map(|e| e["repo"].as_str().unwrap())
        .collect();
    assert!(
        repos.contains("alpha") && repos.contains("beta"),
        "cross-repo tree must tag entries from every indexed repo: {entries:?}"
    );

    // Cross-repo snapshots cover exactly the distinct repos in `entries`,
    // not every watched repo (#746).
    let snapshots = envelope["snapshots"].as_array().expect("snapshots array");
    let snapshot_repos: std::collections::HashSet<&str> = snapshots
        .iter()
        .map(|s| s["repo"].as_str().unwrap())
        .collect();
    assert_eq!(
        snapshot_repos,
        std::collections::HashSet::from(["alpha", "beta"]),
        "cross-repo snapshots must cover exactly the repos represented in entries: {snapshots:?}"
    );

    // Human output prefixes cross-repo lines with the repo name.
    let human_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree"]),
    );
    assert!(human_out.contains("alpha/a.rs"), "got:\n{human_out}");
    assert!(human_out.contains("beta/b.rs"), "got:\n{human_out}");
}

#[test]
fn tree_unknown_repo_fails_loudly() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    seed_watch_toml(data_dir.path(), &[("treerepo", repo_dir.path())]);

    let (_stdout, stderr) = run_fail(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--repo", "nonesuch"]),
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
    assert_eq!(row["command"], "tree");
    assert_eq!(row["hit_count"], 0);
    assert!(
        row["error"]
            .as_str()
            .is_some_and(|e| e.contains("nonesuch")),
        "error field should carry the failure, got: {row}"
    );
}

#[test]
fn tree_uncovered_repo_prints_explicit_message_not_silence() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    // In watch.toml but never indexed -- zero inventory rows.
    seed_watch_toml(data_dir.path(), &[("treerepo", repo_dir.path())]);

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--repo", "treerepo"]),
    );
    assert!(
        stderr.contains("no inventory for 'treerepo'") && stderr.contains("legion index treerepo"),
        "expected the explicit no-inventory hint, got:\n{stderr}"
    );
    // Never-indexed repo also has no snapshot row (#746): the freshness
    // line must name that explicitly, not stay silent about staleness.
    assert!(
        stderr.contains("no inventory snapshot recorded")
            && stderr.contains("legion index treerepo"),
        "expected the no-snapshot-recorded freshness hint, got:\n{stderr}"
    );
}

/// `--json` emits the `{snapshots, entries}` envelope, not a bare array
/// (#746): the explicit-repo case always yields exactly one snapshot for
/// that repo, with `indexed_at`/`head_at_index` recorded by the `legion
/// index` run above.
#[test]
fn sym_tree_json_wraps_entries_with_snapshot_freshness() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    std::fs::write(repo_dir.path().join("a.rs"), "").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("treerepo", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "treerepo"]));

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--repo", "treerepo", "--json"]),
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
    assert_eq!(snapshots[0]["repo"], "treerepo");
    assert!(
        snapshots[0]["indexed_at"].is_string(),
        "indexed_at must be recorded by `legion index`: {snapshots:?}"
    );
    assert_eq!(snapshots[0]["head_drift"], false);
}

/// Cross-repo `--json` with zero matching entries yields an empty
/// `snapshots` array -- not one row per watch.toml repo (#746).
#[test]
fn sym_tree_json_cross_repo_empty_result_yields_empty_snapshots() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    // In watch.toml but never indexed -- zero inventory rows anywhere.
    seed_watch_toml(data_dir.path(), &[("treerepo", repo_dir.path())]);

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--json"]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    assert_eq!(envelope["entries"].as_array().unwrap().len(), 0);
    assert_eq!(
        envelope["snapshots"].as_array().unwrap().len(),
        0,
        "empty cross-repo result must not synthesize one row per watch.toml repo: {envelope}"
    );
}

/// Human output prints an "up to date" freshness line before the entry
/// table when the repo's live HEAD still matches the index-time HEAD
/// (#746).
#[test]
fn tree_human_output_prints_up_to_date_when_head_matches() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    crate::common::run_git_fixture(repo_dir.path(), &["init"]);
    std::fs::write(repo_dir.path().join("a.rs"), "fn a() {}\n").expect("write fixture");
    crate::common::run_git_fixture(repo_dir.path(), &["add", "a.rs"]);
    crate::common::run_git_fixture(repo_dir.path(), &["commit", "-m", "initial"]);
    seed_watch_toml(data_dir.path(), &[("treerepo", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "treerepo"]));

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--repo", "treerepo"]),
    );
    assert!(
        stderr.contains("treerepo: indexed") && stderr.contains("up to date"),
        "expected an up-to-date freshness line, got:\n{stderr}"
    );
    assert!(!stderr.contains("WARNING"), "got:\n{stderr}");
}

/// Human output prints a loud WARNING freshness line naming both HEADs
/// when the repo's live HEAD has moved past the index-time HEAD (#746) --
/// the exact scenario bullpen post 019f355c reported as silently invisible.
#[test]
fn tree_human_output_prints_freshness_warning_on_head_drift() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    crate::common::run_git_fixture(repo_dir.path(), &["init"]);
    std::fs::write(repo_dir.path().join("a.rs"), "fn a() {}\n").expect("write fixture");
    crate::common::run_git_fixture(repo_dir.path(), &["add", "a.rs"]);
    crate::common::run_git_fixture(repo_dir.path(), &["commit", "-m", "initial"]);
    seed_watch_toml(data_dir.path(), &[("treerepo", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "treerepo"]));

    // The repo moves past the indexed HEAD -- `legion index` is not re-run.
    std::fs::write(repo_dir.path().join("b.rs"), "fn b() {}\n").expect("write fixture");
    crate::common::run_git_fixture(repo_dir.path(), &["add", "b.rs"]);
    crate::common::run_git_fixture(repo_dir.path(), &["commit", "-m", "second"]);

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--repo", "treerepo"]),
    );
    assert!(
        stderr.contains("WARNING: current HEAD is")
            && stderr.contains("inventory may be stale")
            && stderr.contains("re-run 'legion index treerepo'"),
        "expected a head-drift warning, got:\n{stderr}"
    );

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--repo", "treerepo", "--json"]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let snapshots = envelope["snapshots"].as_array().expect("snapshots array");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0]["head_drift"], true);
    assert!(snapshots[0]["head_at_index"].is_string());
    assert!(snapshots[0]["current_head"].is_string());
    assert_ne!(snapshots[0]["head_at_index"], snapshots[0]["current_head"]);
}
