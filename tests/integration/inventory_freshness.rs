//! CLI end-to-end tests for `legion index`'s inventory-snapshot recording
//! (#746): every `legion index <repo>` run upserts one `inventory_snapshots`
//! row per repo (indexed-at timestamp + HEAD at index time), which `sym
//! tree`/`sym etc find-file --json` then surface via the `snapshots` field
//! of the `--json` envelope (see sym_tree.rs/find_file.rs for the
//! freshness/drift-detection tests built on top of this).

use crate::common::{legion_cmd, run_git_fixture, run_git_fixture_output, run_ok};

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

/// `legion index` in a real git checkout records the checkout's actual
/// HEAD sha alongside the indexed-at timestamp.
#[test]
fn legion_index_records_inventory_snapshot_with_head_in_git_checkout() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    run_git_fixture(repo_dir.path(), &["init"]);
    std::fs::write(repo_dir.path().join("a.rs"), "fn a() {}\n").expect("write fixture");
    run_git_fixture(repo_dir.path(), &["add", "a.rs"]);
    run_git_fixture(repo_dir.path(), &["commit", "-m", "initial"]);

    let expected_head = run_git_fixture_output(repo_dir.path(), &["rev-parse", "HEAD"]);

    seed_watch_toml(data_dir.path(), &[("gitrepo", repo_dir.path())]);
    let before = chrono::Utc::now();
    run_ok(legion_cmd(data_dir.path()).args(["index", "gitrepo"]));
    let after = chrono::Utc::now();

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--repo", "gitrepo", "--json"]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let snapshot = &envelope["snapshots"][0];
    assert_eq!(
        snapshot["head_at_index"].as_str(),
        Some(expected_head.as_str()),
        "recorded head must match `git rev-parse HEAD` for the checkout: {snapshot:?}"
    );

    let indexed_at = snapshot["indexed_at"]
        .as_str()
        .expect("indexed_at must be a string");
    let parsed =
        chrono::DateTime::parse_from_rfc3339(indexed_at).expect("indexed_at must parse as RFC3339");
    assert!(
        parsed >= before && parsed <= after,
        "indexed_at ({indexed_at}) must fall within the index call's window"
    );
}

/// A non-git workdir still gets a snapshot row -- with `head: None`, never
/// an error.
#[test]
fn legion_index_records_snapshot_with_none_head_for_non_git_workdir() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    std::fs::write(repo_dir.path().join("a.rs"), "fn a() {}\n").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("plainrepo", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "plainrepo"]));

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--repo", "plainrepo", "--json"]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let snapshot = &envelope["snapshots"][0];
    assert!(
        snapshot["indexed_at"].is_string(),
        "a snapshot row must still be recorded: {snapshot:?}"
    );
    assert!(
        snapshot["head_at_index"].is_null(),
        "non-git workdir must record head=None, not an error: {snapshot:?}"
    );
    assert_eq!(snapshot["head_drift"], false);
}

/// Re-running `legion index <repo>` replaces the prior snapshot row rather
/// than accumulating one row per run: after a second index run following a
/// HEAD advance, the snapshot reflects only the latest run.
#[test]
fn reindex_replaces_prior_snapshot_row_not_accumulates() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    run_git_fixture(repo_dir.path(), &["init"]);
    std::fs::write(repo_dir.path().join("a.rs"), "fn a() {}\n").expect("write fixture");
    run_git_fixture(repo_dir.path(), &["add", "a.rs"]);
    run_git_fixture(repo_dir.path(), &["commit", "-m", "initial"]);
    seed_watch_toml(data_dir.path(), &[("gitrepo", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "gitrepo"]));

    std::fs::write(repo_dir.path().join("b.rs"), "fn b() {}\n").expect("write fixture");
    run_git_fixture(repo_dir.path(), &["add", "b.rs"]);
    run_git_fixture(repo_dir.path(), &["commit", "-m", "second"]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "gitrepo"]));

    let expected_head = run_git_fixture_output(repo_dir.path(), &["rev-parse", "HEAD"]);

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "tree", "--repo", "gitrepo", "--json"]),
    );
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let snapshots = envelope["snapshots"].as_array().expect("snapshots array");
    assert_eq!(
        snapshots.len(),
        1,
        "one snapshot per repo, not one per index run: {snapshots:?}"
    );
    assert_eq!(
        snapshots[0]["head_at_index"].as_str(),
        Some(expected_head.as_str())
    );
    assert_eq!(
        snapshots[0]["head_drift"], false,
        "no drift right after a fresh index"
    );
}
