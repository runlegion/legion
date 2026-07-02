//! Integration tests: SCIP index status and doctrine-bypass telemetry.

use crate::common::*;

#[test]
fn index_status_empty_db() {
    // #284: --status on a fresh DB exits 0 and emits the no-indexes message.
    let dir = tempfile::tempdir().unwrap();
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args(["--verbose", "index", "--status"]));
    assert!(
        stderr.contains("no SCIP indexes recorded"),
        "expected no-indexes message on stderr, got: {stderr}"
    );
}

#[test]
fn index_status_and_file_mutually_exclusive() {
    // #284: --status conflicts with --file.
    let dir = tempfile::tempdir().unwrap();
    run_fail(legion_cmd(dir.path()).args(["index", "--status", "--file", "/tmp/x"]));
}

#[test]
fn index_status_json_empty_db_returns_empty_array() {
    // #437: --json output is the contract `_legion-indexed.sh` (and
    // downstream #438/#439 hooks) read with `jq -e 'any(.[]; .repo == $r)'`.
    // The empty case must be a valid JSON array, not "null", not empty
    // stdout, not a wrapped object. Otherwise jq errors and every probe
    // silently degrades to "not indexed", disabling block-state.
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["index", "--status", "--json"]));
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON array, got '{stdout}': {e}"));
    let arr = parsed
        .as_array()
        .unwrap_or_else(|| panic!("expected top-level array, got: {parsed}"));
    assert!(
        arr.is_empty(),
        "expected empty array on empty DB, got: {parsed}"
    );
}

#[test]
fn index_status_json_conflicts_with_banner() {
    // --json and --banner are mutually exclusive: banner is human-readable,
    // json is machine-readable. Combining them makes no sense and must fail
    // at parse time so an operator who tries the wrong combo gets a clear
    // error instead of unexpected output.
    let dir = tempfile::tempdir().unwrap();
    run_fail(legion_cmd(dir.path()).args(["index", "--status", "--json", "--banner", "anything"]));
}

#[test]
fn telemetry_record_and_list_roundtrip() {
    // #437: end-to-end CLI round-trip. Hooks (#438/#439) shell out with
    // long-form flags; if clap arg names or bool-flag semantics drift, the
    // hooks break silently. This pins the surface.
    let data_dir = tempfile::tempdir().unwrap();
    let xdg_state = tempfile::tempdir().unwrap();

    run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args([
                "telemetry",
                "record-bypass",
                "--repo",
                "legion",
                "--session-id",
                "sess-int",
                "--tool",
                "Bash",
                "--pattern",
                "fn main",
                "--bypass-reason",
                "integration test",
                "--had-sym-hits",
                "--agent",
                "legion-prime",
            ]),
    );

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args(["telemetry", "list-bypasses", "--since", "1h"]),
    );
    let rows: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON array, got '{stdout}': {e}"));
    let arr = rows
        .as_array()
        .unwrap_or_else(|| panic!("expected array, got: {rows}"));
    assert_eq!(arr.len(), 1, "expected one bypass row, got: {rows}");
    let row = &arr[0];
    assert_eq!(row["repo"], "legion");
    assert_eq!(row["tool"], "Bash");
    assert_eq!(row["pattern"], "fn main");
    assert_eq!(row["had_sym_hits"], true);
    assert_eq!(row["had_recall_hits"], false);
    assert_eq!(row["agent"], "legion-prime");
}

#[test]
fn telemetry_summary_rolls_up_groups() {
    // #440: summary groups by (tool, repo, pattern), sorts by count desc.
    // Seed three rows for one (Bash, legion, fn_main) group + one row for a
    // different (Read, legion, src/main.rs) group; assert top row is the
    // first group with count=3.
    let data_dir = tempfile::tempdir().unwrap();
    let xdg_state = tempfile::tempdir().unwrap();

    for _ in 0..3 {
        run_ok(
            legion_cmd(data_dir.path())
                .env("XDG_STATE_HOME", xdg_state.path())
                .args([
                    "telemetry",
                    "record-bypass",
                    "--repo",
                    "legion",
                    "--session-id",
                    "s",
                    "--tool",
                    "Bash",
                    "--pattern",
                    "fn_main",
                    "--bypass-reason",
                    "test",
                    "--had-sym-hits",
                ]),
        );
    }
    run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args([
                "telemetry",
                "record-bypass",
                "--repo",
                "legion",
                "--session-id",
                "s",
                "--tool",
                "Read",
                "--pattern",
                "src/main.rs",
                "--bypass-reason",
                "test",
            ]),
    );

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args(["telemetry", "summary", "--since", "1h", "--json"]),
    );
    let rows: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 2, "two groups expected, got: {rows}");
    assert_eq!(arr[0]["tool"], "Bash");
    assert_eq!(arr[0]["count"], 3);
    assert!((arr[0]["had_sym_hits_pct"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    assert_eq!(arr[1]["tool"], "Read");
    assert_eq!(arr[1]["count"], 1);
}

#[test]
fn telemetry_summary_empty_input_prints_no_bypasses_line() {
    // Human-readable output on empty bypass log emits a clear no-data
    // line so an operator running this on a fresh node sees the empty
    // state rather than nothing.
    let data_dir = tempfile::tempdir().unwrap();
    let xdg_state = tempfile::tempdir().unwrap();

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args(["telemetry", "summary"]),
    );
    assert!(
        stdout.contains("no bypasses recorded"),
        "expected no-bypasses line on empty log, got: {stdout}"
    );
}

#[test]
fn telemetry_list_filters_by_repo_and_since() {
    // Combined --since AND --repo filter: each is unit-tested alone in
    // src/telemetry.rs, but the CLI dispatch path that threads both into
    // list_bypasses is only exercised here.
    let data_dir = tempfile::tempdir().unwrap();
    let xdg_state = tempfile::tempdir().unwrap();

    for repo in ["legion", "smugglr", "legion"] {
        run_ok(
            legion_cmd(data_dir.path())
                .env("XDG_STATE_HOME", xdg_state.path())
                .args([
                    "telemetry",
                    "record-bypass",
                    "--repo",
                    repo,
                    "--session-id",
                    "sess",
                    "--tool",
                    "Bash",
                    "--pattern",
                    "x",
                    "--bypass-reason",
                    "test",
                ]),
        );
    }

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", xdg_state.path())
            .args([
                "telemetry",
                "list-bypasses",
                "--since",
                "1h",
                "--repo",
                "legion",
            ]),
    );
    let rows: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 2, "expected 2 legion rows, got: {rows}");
    for row in arr {
        assert_eq!(row["repo"], "legion");
    }
}

/// Full SCIP round-trip at the CLI boundary (#608 coverage net): `legion
/// index` runs the language indexer subprocess, stores the protobuf blob,
/// and `legion sym def` / `legion sym refs` answer lookups from it.
///
/// The "indexer" is a PATH shim named `scip-rust` that copies a
/// pre-built index.scip into the repo root -- the same isolation trick the
/// scip.rs unit tests use -- so the test pins legion's own plumbing
/// (watch.toml resolution, language detection, blob storage, symbol query)
/// without needing a real rust-analyzer on the runner.
#[cfg(unix)]
#[test]
fn index_and_sym_def_refs_roundtrip_against_fixture_repo() {
    use protobuf::Message;
    use scip::types::{Document, Index, Occurrence, SymbolRole};
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    // Fixture repo: a Cargo.toml marker so detect_languages says "rust",
    // plus the src/lib.rs the blob's document refers to -- the inventory
    // walk only rows files that exist on disk, and the symbol-count
    // enrichment joins blob relative_path to inventory path (#705).
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(
        repo.path().join("src/lib.rs"),
        "pub struct Greeter;\npub fn hello() {}\n",
    )
    .unwrap();

    // Register the fixture in watch.toml directly (the `watch add` command
    // would also kick off a background indexer, which this test does not
    // want racing its shim).
    std::fs::write(
        dir.path().join("watch.toml"),
        format!(
            "poll_interval_secs = 30\ncooldown_secs = 300\n\n[[repos]]\nname = \"fixture\"\nworkdir = \"{}\"\n",
            repo.path().display()
        ),
    )
    .unwrap();

    // Build a tiny SCIP index: one definition of Greeter plus two references.
    let symbol = "rust-analyzer cargo fixture 0.1.0 src/lib.rs/Greeter#";
    let occurrence = |range: Vec<i32>, is_def: bool| {
        let mut o = Occurrence::new();
        o.symbol = symbol.to_string();
        o.range = range;
        if is_def {
            o.symbol_roles = SymbolRole::Definition as i32;
        }
        o
    };
    let mut document = Document::new();
    document.relative_path = "src/lib.rs".to_string();
    document.occurrences = vec![
        occurrence(vec![4, 0, 4, 7], true),
        occurrence(vec![10, 8, 10, 15], false),
        occurrence(vec![20, 4, 20, 11], false),
    ];
    let mut index = Index::new();
    index.documents = vec![document];
    let blob = index.write_to_bytes().expect("serialize scip index");
    let blob_path = dir.path().join("fixture-index.scip");
    std::fs::write(&blob_path, &blob).unwrap();

    // PATH shim: `scip-rust` copies the pre-built blob into the repo root,
    // exactly where run_indexer_binary expects index.scip to appear.
    let shim_dir = tempfile::tempdir().unwrap();
    let shim = shim_dir.path().join("scip-rust");
    std::fs::write(
        &shim,
        format!("#!/bin/sh\ncp '{}' index.scip\n", blob_path.display()),
    )
    .unwrap();
    let mut perm = std::fs::metadata(&shim).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&shim, perm).unwrap();
    let shim_path = format!("{}:/usr/bin:/bin", shim_dir.path().display());

    // Index the fixture through the CLI -- synchronous, stores the blob.
    let index_stderr = run_ok_stderr(
        legion_cmd(dir.path())
            .env("PATH", &shim_path)
            .args(["index", "fixture"]),
    );
    assert!(
        index_stderr.contains("indexed fixture (rust)"),
        "expected index confirmation, got: {index_stderr}"
    );
    // #705 end-to-end: the walk inventoried the fixture and the enrichment
    // pass matched the blob's src/lib.rs document to its inventory row --
    // "applied to 1" proves the SCIP relative_path joined the walk's
    // repo-relative path through the real CLI + DB, not just unit halves.
    assert!(
        index_stderr.contains("inventoried"),
        "expected the inventory summary, got: {index_stderr}"
    );
    assert!(
        index_stderr.contains("symbol counts applied to 1 inventoried files for fixture"),
        "expected the symbol-count enrichment line, got: {index_stderr}"
    );

    // def: the single definition occurrence, 1-indexed (range line 4 -> 5).
    let defs = run_ok(
        legion_cmd(dir.path()).args(["sym", "def", "Greeter", "--repo", "fixture", "--json"]),
    );
    let defs: serde_json::Value = serde_json::from_str(defs.trim()).expect("def JSON");
    let defs = defs.as_array().expect("def array");
    assert_eq!(defs.len(), 1, "expected exactly one definition: {defs:?}");
    assert_eq!(defs[0]["file"], "src/lib.rs");
    assert_eq!(defs[0]["line"], 5);
    assert_eq!(defs[0]["repo"], "fixture");
    assert_eq!(defs[0]["lang"], "rust");

    // refs: the two non-definition occurrences, sorted by line.
    let refs = run_ok(
        legion_cmd(dir.path()).args(["sym", "refs", "Greeter", "--repo", "fixture", "--json"]),
    );
    let refs: serde_json::Value = serde_json::from_str(refs.trim()).expect("refs JSON");
    let refs = refs.as_array().expect("refs array");
    assert_eq!(refs.len(), 2, "expected two references: {refs:?}");
    assert_eq!(refs[0]["line"], 11);
    assert_eq!(refs[1]["line"], 21);
}

/// #705: a docs-only repo (no language markers) must exit 0 from
/// `legion index`, skip SCIP loudly, and still populate the inventory.
/// Pins the exit-code contract flip at the CLI boundary -- the old code
/// hard-errored on `detect_languages() == []`; walk-level unit tests
/// cannot catch a regression here.
#[test]
fn index_docs_only_repo_succeeds_and_inventories() {
    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("README.md"), "# docs only\n").unwrap();
    std::fs::write(repo.path().join("guide.md"), "content\n").unwrap();

    // Direct watch.toml seed: `watch add` would spawn a background indexer
    // this test does not want racing it.
    std::fs::write(
        dir.path().join("watch.toml"),
        format!(
            // Forward slashes: backslash Windows paths are invalid TOML
            // string escapes, and Windows path APIs accept slashes.
            "poll_interval_secs = 30\ncooldown_secs = 300\n\n[[repos]]\nname = \"docsrepo\"\nworkdir = \"{}\"\n",
            repo.path().display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();

    let stderr = run_ok_stderr(legion_cmd(dir.path()).args(["index", "docsrepo"]));
    assert!(
        stderr.contains("no SCIP-supported language detected"),
        "expected the SCIP-skip notice, got: {stderr}"
    );
    assert!(
        stderr.contains("inventoried 2 files for docsrepo"),
        "expected the inventory summary, got: {stderr}"
    );
}

/// #705 review (PR #718): a missing or unmounted workdir must abort the
/// index run loudly BEFORE any walk. Without the guard, walk_repo on a
/// vanished root returns zero entries and the prune pass deletes every
/// inventory row for the repo -- a transient mount failure silently
/// destroying derived state.
#[test]
fn index_missing_workdir_fails_loudly_instead_of_wiping_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let gone = tempfile::tempdir().unwrap();
    let gone_path = gone.path().to_path_buf();
    drop(gone); // the workdir existed once, then the mount vanished

    std::fs::write(
        dir.path().join("watch.toml"),
        format!(
            // Forward slashes: see index_docs_only_repo_succeeds_and_inventories.
            "poll_interval_secs = 30\ncooldown_secs = 300\n\n[[repos]]\nname = \"ghostrepo\"\nworkdir = \"{}\"\n",
            gone_path.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();

    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args(["index", "ghostrepo"]));
    assert!(
        stderr.contains("does not exist") && stderr.contains("ghostrepo"),
        "expected the missing-workdir refusal naming the repo, got: {stderr}"
    );
}

/// #705 re-review (smugglr, PR #718): a workdir that EXISTS but cannot be
/// read (permission-mangled remount, present-but-unreadable mount point)
/// passes the `is_dir()` guard, so the walk runs, yields a root-level error
/// and zero entries. The prune must be refused: a transient permission
/// failure must not evict rows for files that still exist. This is the
/// CLI-level repro of the chmod-000 wipe demonstrated against 0bdfcb7.
#[cfg(unix)]
#[test]
fn index_unreadable_workdir_skips_prune_instead_of_wiping_inventory() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("README.md"), "# docs only\n").unwrap();
    std::fs::write(repo.path().join("guide.md"), "content\n").unwrap();

    std::fs::write(
        dir.path().join("watch.toml"),
        format!(
            "poll_interval_secs = 30\ncooldown_secs = 300\n\n[[repos]]\nname = \"permrepo\"\nworkdir = \"{}\"\n",
            repo.path().display()
        ),
    )
    .unwrap();

    let stderr = run_ok_stderr(legion_cmd(dir.path()).args(["index", "permrepo"]));
    assert!(
        stderr.contains("inventoried 2 files for permrepo"),
        "expected two files inventoried, got: {stderr}"
    );

    // The mount goes unreadable: the root still stats as a directory but
    // readdir on it fails. (Assumes a non-root test runner -- root ignores
    // mode 000 and would see a readable directory.)
    let mut perm = std::fs::metadata(repo.path()).unwrap().permissions();
    perm.set_mode(0o000);
    std::fs::set_permissions(repo.path(), perm).unwrap();

    let stderr = run_ok_stderr(legion_cmd(dir.path()).args(["index", "permrepo"]));

    // Restore perms before asserting so the tempdir can drop even on failure.
    let mut perm = std::fs::metadata(repo.path()).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(repo.path(), perm).unwrap();

    assert!(
        stderr.contains("prune skipped"),
        "expected the partial-walk prune refusal, got: {stderr}"
    );

    // Row-survival proof: delete one file and re-index. Exactly one stale
    // row gets pruned, so both rows survived the unreadable run -- a wipe
    // would have left nothing to prune.
    std::fs::remove_file(repo.path().join("guide.md")).unwrap();
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args(["index", "permrepo"]));
    assert!(
        stderr.contains("inventoried 1 files for permrepo (1 stale rows pruned)"),
        "expected exactly one stale row pruned after the recovery run, got: {stderr}"
    );
}
