//! CLI end-to-end tests for `legion whoami --generate` (gather mode) and
//! `legion whoami --generate --apply` (apply mode) (#784).
//!
//! The pure gather/apply/validate logic is unit-tested in
//! `src/identity_generate.rs`; these tests exercise the actual binary
//! surface: flag validation, the real `watch.toml` repo-resolution error
//! path (`Database::data_dir()` caches its result in a process-wide
//! `OnceLock`, so this can only be exercised from a fresh subprocess --
//! see `src/identity_generate.rs::vault_repo_workdir`'s doc comment), a
//! real `legion index` seeded file inventory, and the full gather -> author
//! manifest -> apply --dry-run -> apply round trip.

use crate::common::{legion_cmd, run_fail, run_ok};

/// Seed a watch.toml in the data dir pointing at `repos` (name, workdir).
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
fn generate_without_vault_repo_or_byline_fails_loudly() {
    let data_dir = tempfile::tempdir().expect("data dir");

    let (_stdout, stderr) =
        run_fail(legion_cmd(data_dir.path()).args(["whoami", "--repo", "legion", "--generate"]));
    assert!(
        stderr.contains("--vault-repo") || stderr.contains("--byline"),
        "expected a flag-validation message naming the missing flag, got:\n{stderr}"
    );
}

#[test]
fn generate_apply_without_from_file_fails_loudly() {
    let data_dir = tempfile::tempdir().expect("data dir");

    let (_stdout, stderr) = run_fail(legion_cmd(data_dir.path()).args([
        "whoami",
        "--repo",
        "legion",
        "--generate",
        "--apply",
    ]));
    assert!(
        stderr.contains("--from-file"),
        "expected a flag-validation message naming --from-file, got:\n{stderr}"
    );
}

#[test]
fn generate_unknown_vault_repo_returns_watch_config_error() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let known_dir = tempfile::tempdir().expect("known dir");
    seed_watch_toml(data_dir.path(), &[("known-repo", known_dir.path())]);

    let (_stdout, stderr) = run_fail(legion_cmd(data_dir.path()).args([
        "whoami",
        "--repo",
        "legion",
        "--generate",
        "--vault-repo",
        "ghost-vault-repo",
        "--byline",
        "legion",
    ]));
    assert!(
        stderr.contains("ghost-vault-repo") && stderr.contains("watch.toml"),
        "expected a watch.toml repo-not-found message naming the repo, got:\n{stderr}"
    );
}

#[test]
fn generate_gathers_claimed_and_given_halves() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let vault_dir = tempfile::tempdir().expect("vault dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    // Date-titled filename, no byline substring in the path -- the exact
    // case a filename-glob approach would miss.
    std::fs::write(
        vault_dir.path().join("2026-07-01-persistence.md"),
        "---\nauthor: legion\n---\n\nPersistence is a discipline, not a feature.\n",
    )
    .expect("write claimed fixture");
    std::fs::write(
        vault_dir.path().join("unrelated.md"),
        "---\nauthor: someone-else\n---\n\nnot a match\n",
    )
    .expect("write non-matching fixture");
    seed_watch_toml(data_dir.path(), &[("vault-repo", vault_dir.path())]);

    run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["index", "vault-repo"]),
    );

    // A given-half reflection authored by a different repo about "legion".
    run_ok(legion_cmd(data_dir.path()).args([
        "reflect",
        "--repo",
        "rafters",
        "--text",
        "legion is careful about identity invariants",
    ]));

    let stdout = run_ok(legion_cmd(data_dir.path()).args([
        "whoami",
        "--repo",
        "legion",
        "--generate",
        "--vault-repo",
        "vault-repo",
        "--byline",
        "legion",
    ]));

    let bundle: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout is a JSON GatherBundle");
    assert_eq!(bundle["repo"], "legion");
    assert_eq!(bundle["vault_repo"], "vault-repo");
    let claimed = bundle["claimed"].as_array().expect("claimed array");
    assert_eq!(
        claimed.len(),
        1,
        "expected exactly one claimed match: {claimed:?}"
    );
    assert_eq!(claimed[0]["path"], "2026-07-01-persistence.md");
    assert_eq!(claimed[0]["byline"], "legion");
    assert!(
        claimed[0]["body"]
            .as_str()
            .unwrap()
            .contains("Persistence is a discipline"),
        "expected the full file body, got: {claimed:?}"
    );
}

#[test]
fn apply_dry_run_then_full_apply_round_trip() {
    let data_dir = tempfile::tempdir().expect("data dir");

    // Seed an old identity root so the swap has something to replace.
    run_ok(legion_cmd(data_dir.path()).args([
        "reflect",
        "--repo",
        "gen-test-repo",
        "--text",
        "old identity root",
        "--whoami",
    ]));

    let manifest_path = data_dir.path().join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::json!({
            "root": "a careful builder of durable systems",
            "chain": ["learned that backups come before deletes"]
        })
        .to_string(),
    )
    .expect("write manifest");

    // --dry-run: reports the plan, writes nothing.
    let dry_run_out = run_ok(legion_cmd(data_dir.path()).args([
        "whoami",
        "--repo",
        "gen-test-repo",
        "--generate",
        "--apply",
        "--from-file",
        manifest_path.to_str().unwrap(),
        "--dry-run",
    ]));
    let planned: serde_json::Value =
        serde_json::from_str(dry_run_out.trim()).expect("stdout is a JSON ApplyPlan");
    assert_eq!(planned["would_create"], 2);
    let would_retire = planned["would_retire"]
        .as_array()
        .expect("would_retire array");
    assert_eq!(would_retire.len(), 1);
    let backup_path = planned["backup_path"].as_str().expect("backup_path string");
    assert!(
        !std::path::Path::new(backup_path).exists(),
        "dry-run must not write a backup file"
    );

    // Real apply: root replaced, chain in place, old root retired.
    let apply_out = run_ok(legion_cmd(data_dir.path()).args([
        "whoami",
        "--repo",
        "gen-test-repo",
        "--generate",
        "--apply",
        "--from-file",
        manifest_path.to_str().unwrap(),
    ]));
    let applied: serde_json::Value =
        serde_json::from_str(apply_out.trim()).expect("stdout is a JSON ApplyOutcome");
    let new_ids = applied["new_ids"].as_array().expect("new_ids array");
    assert_eq!(new_ids.len(), 2);
    let retired_ids = applied["retired_ids"]
        .as_array()
        .expect("retired_ids array");
    assert_eq!(retired_ids.len(), 1);
    let applied_backup_path = applied["backup_path"].as_str().expect("backup_path string");
    assert!(
        std::path::Path::new(applied_backup_path).exists(),
        "apply must write the pre-apply backup file"
    );

    let whoami_out =
        run_ok(legion_cmd(data_dir.path()).args(["whoami", "--repo", "gen-test-repo"]));
    assert!(
        whoami_out.contains("a careful builder of durable systems"),
        "expected the new root text in whoami output, got:\n{whoami_out}"
    );
    assert!(
        !whoami_out.contains("old identity root"),
        "old identity root must be retired, got:\n{whoami_out}"
    );
}
