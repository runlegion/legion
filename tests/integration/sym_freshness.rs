//! CLI end-to-end tests for #1187: `sym def`, `sym refs`, `sym list` and
//! `sym hover` printing the same stderr freshness line `sym tree` / `sym
//! imports` / `sym importers` / `sym etc find-file` already print (#746),
//! reusing `compute_freshness`/`format_freshness_line` unchanged. Unlike
//! those four verbs, these must NOT adopt the `--json` `FreshJsonEnvelope`
//! shape (`sym_tree.rs` pins that shape for `tree`/`find-file`) -- stdout
//! for def/refs/list/hover stays a bare array (or, for hover, the existing
//! bare object/`null`), exactly as before this issue.

use crate::common::{
    legion_cmd, run_git_fixture, run_git_fixture_output, run_ok, run_ok_output, run_ok_stderr,
};

/// Seed a watch.toml in the data dir pointing at `repos` (name, workdir).
/// Mirrors `sym_tree.rs`/`inventory_freshness.rs`'s helper of the same name.
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

/// Build a real git-backed fixture repo carrying one `Greeter` struct
/// (one definition occurrence, one reference occurrence, and a
/// `SymbolInformation` entry so `sym hover` also resolves it), register it
/// as `name` in `data_dir`'s watch.toml, and run `legion index <name>`
/// against it -- via the same PATH-shim trick `index_telemetry.rs` uses, so
/// this pins legion's own query plumbing without needing a real
/// rust-analyzer on the runner.
///
/// Commits the fixture before indexing (`legion index` is what records
/// `head_at_index`; indexing an uncommitted tree would make `head_drift`
/// permanently false and the drift tests below pass vacuously). Returns the
/// repo dir so callers can advance its HEAD afterward without re-indexing,
/// to exercise drift.
#[cfg(unix)]
fn index_fixture_repo(data_dir: &std::path::Path, name: &str) -> tempfile::TempDir {
    use protobuf::Message;
    use scip::types::symbol_information::Kind;
    use scip::types::{Document, Index, Occurrence, SymbolInformation, SymbolRole};
    use std::os::unix::fs::PermissionsExt;

    let repo = tempfile::tempdir().expect("repo dir");
    run_git_fixture(repo.path(), &["init"]);
    std::fs::write(
        repo.path().join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    )
    .expect("write Cargo.toml");
    std::fs::create_dir_all(repo.path().join("src")).expect("mkdir src");
    std::fs::write(
        repo.path().join("src/lib.rs"),
        "pub struct Greeter;\npub fn hello() {}\n",
    )
    .expect("write fixture");
    run_git_fixture(repo.path(), &["add", "."]);
    run_git_fixture(repo.path(), &["commit", "-m", "initial"]);

    seed_watch_toml(data_dir, &[(name, repo.path())]);

    let symbol = format!("rust-analyzer cargo {name} 0.1.0 src/lib.rs/Greeter#");
    let occurrence = |range: Vec<i32>, is_def: bool| {
        let mut o = Occurrence::new();
        o.symbol = symbol.clone();
        o.range = range;
        if is_def {
            o.symbol_roles = SymbolRole::Definition as i32;
        }
        o
    };
    let mut info = SymbolInformation::new();
    info.symbol = symbol.clone();
    info.kind = Kind::Struct.into();
    info.documentation = vec!["A friendly greeter.".to_string()];

    let mut document = Document::new();
    document.relative_path = "src/lib.rs".to_string();
    document.occurrences = vec![
        occurrence(vec![0, 11, 0, 18], true),
        occurrence(vec![1, 4, 1, 9], false),
    ];
    document.symbols = vec![info];

    let mut index = Index::new();
    index.documents = vec![document];
    let blob = index.write_to_bytes().expect("serialize scip index");
    let blob_path = data_dir.join(format!("{name}-index.scip"));
    std::fs::write(&blob_path, &blob).expect("write blob");

    let shim_dir = tempfile::tempdir().expect("shim dir");
    let shim = shim_dir.path().join("scip-rust");
    std::fs::write(
        &shim,
        format!("#!/bin/sh\ncp '{}' index.scip\n", blob_path.display()),
    )
    .expect("write shim");
    let mut perm = std::fs::metadata(&shim)
        .expect("shim metadata")
        .permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&shim, perm).expect("chmod shim");
    let shim_path = format!("{}:/usr/bin:/bin", shim_dir.path().display());

    run_ok(
        legion_cmd(data_dir)
            .env("PATH", &shim_path)
            .args(["index", name]),
    );
    repo
}

/// Advance `repo_dir`'s HEAD past what was indexed, without re-running
/// `legion index` -- the exact drift scenario `sym tree`'s equivalent test
/// exercises.
fn advance_head(repo_dir: &std::path::Path) {
    std::fs::write(repo_dir.join("b.rs"), "fn b() {}\n").expect("write fixture");
    run_git_fixture(repo_dir, &["add", "b.rs"]);
    run_git_fixture(repo_dir, &["commit", "-m", "second"]);
}

/// `sym def` prints the loud WARNING freshness line (naming both HEADs) on
/// stderr when the repo's live HEAD has moved past the index-time HEAD --
/// today (before #1187) it prints nothing at all. Also pins that `--json`
/// stdout is unaffected: a bare `SymbolLocation` array, not the
/// `FreshJsonEnvelope` `tree`/`find-file` use, with every field intact --
/// the freshness line still lands on stderr even in `--json` mode, since
/// there is no envelope left to carry it.
#[cfg(unix)]
#[test]
fn def_prints_freshness_warning_on_head_drift_and_json_stays_bare_array() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo = index_fixture_repo(data_dir.path(), "fixture");
    advance_head(repo.path());
    let expected_head = run_git_fixture_output(repo.path(), &["rev-parse", "HEAD"]);

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path()).args(["sym", "def", "Greeter", "--repo", "fixture"]),
    );
    assert!(
        stderr.contains("WARNING: current HEAD is")
            && stderr.contains("inventory may be stale")
            && stderr.contains("re-run 'legion index fixture'"),
        "expected a head-drift freshness warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&expected_head[..7]),
        "warning should name the current HEAD, got:\n{stderr}"
    );

    let output = run_ok_output(
        legion_cmd(data_dir.path()).args(["sym", "def", "Greeter", "--repo", "fixture", "--json"]),
    );
    let json_stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        json_stderr.contains("WARNING: current HEAD is"),
        "freshness warning must still print on stderr in --json mode, got:\n{json_stderr}"
    );
    let json_stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(json_stdout.trim()).expect("stdout is a bare JSON array");
    let arr = parsed
        .as_array()
        .expect("top level is an array, not an object");
    assert_eq!(arr.len(), 1, "expected exactly one definition: {arr:?}");
    assert_eq!(arr[0]["file"], "src/lib.rs");
    assert_eq!(arr[0]["line"], 1);
    assert_eq!(arr[0]["repo"], "fixture");
    assert_eq!(arr[0]["lang"], "rust");
    assert!(arr[0]["column"].is_number(), "column must survive: {arr:?}");
}

/// `sym def` prints the quiet "up to date" line, and never a WARNING, when
/// the repo has not moved since it was indexed.
#[cfg(unix)]
#[test]
fn def_prints_up_to_date_when_head_matches_and_json_unchanged() {
    let data_dir = tempfile::tempdir().expect("data dir");
    index_fixture_repo(data_dir.path(), "fixture");

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path()).args(["sym", "def", "Greeter", "--repo", "fixture"]),
    );
    assert!(
        stderr.contains("fixture: indexed") && stderr.contains("up to date"),
        "expected an up-to-date freshness line, got:\n{stderr}"
    );
    assert!(!stderr.contains("WARNING"), "got:\n{stderr}");

    let json_out = run_ok(
        legion_cmd(data_dir.path()).args(["sym", "def", "Greeter", "--repo", "fixture", "--json"]),
    );
    let parsed: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a bare JSON array");
    assert_eq!(parsed.as_array().expect("array").len(), 1);
}

/// `sym refs` shares `run_location_query` with `sym def`; one drift test
/// pins that the shared freshness call actually fires for this verb too.
#[cfg(unix)]
#[test]
fn refs_prints_freshness_warning_on_head_drift() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo = index_fixture_repo(data_dir.path(), "fixture");
    advance_head(repo.path());

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path()).args(["sym", "refs", "Greeter", "--repo", "fixture"]),
    );
    assert!(
        stderr.contains("WARNING: current HEAD is") && stderr.contains("inventory may be stale"),
        "expected a head-drift freshness warning, got:\n{stderr}"
    );
}

/// `sym list` prints the freshness warning on drift, and its `--json`
/// output stays the existing bare `SymbolEntry` array.
#[cfg(unix)]
#[test]
fn list_prints_freshness_warning_on_head_drift_and_json_stays_bare_array() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo = index_fixture_repo(data_dir.path(), "fixture");
    advance_head(repo.path());

    let stderr =
        run_ok_stderr(legion_cmd(data_dir.path()).args(["sym", "list", "--repo", "fixture"]));
    assert!(
        stderr.contains("WARNING: current HEAD is") && stderr.contains("inventory may be stale"),
        "expected a head-drift freshness warning, got:\n{stderr}"
    );

    let json_out =
        run_ok(legion_cmd(data_dir.path()).args(["sym", "list", "--repo", "fixture", "--json"]));
    let parsed: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a bare JSON array");
    let arr = parsed
        .as_array()
        .expect("top level is an array, not an object");
    assert_eq!(
        arr.len(),
        1,
        "expected exactly one enumerated symbol: {arr:?}"
    );
    assert_eq!(arr[0]["name"], "Greeter");
    assert_eq!(arr[0]["file"], "src/lib.rs");
    assert_eq!(arr[0]["repo"], "fixture");
    assert_eq!(arr[0]["lang"], "rust");
}

/// `sym hover` prints the freshness warning on drift; its output shape
/// (a bare object, or `null`/nothing on no match) is untouched.
#[cfg(unix)]
#[test]
fn hover_prints_freshness_warning_on_head_drift() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo = index_fixture_repo(data_dir.path(), "fixture");
    advance_head(repo.path());

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path()).args(["sym", "hover", "Greeter", "--repo", "fixture"]),
    );
    assert!(
        stderr.contains("WARNING: current HEAD is") && stderr.contains("inventory may be stale"),
        "expected a head-drift freshness warning, got:\n{stderr}"
    );

    let json_out = run_ok(
        legion_cmd(data_dir.path())
            .args(["sym", "hover", "Greeter", "--repo", "fixture", "--json"]),
    );
    let parsed: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a bare hover object");
    assert_eq!(parsed["repo"], "fixture");
    assert_eq!(parsed["docstring"], "A friendly greeter.");
}

/// Requirement 2: a cross-repo query (no `--repo`) with hits in only one of
/// several registered, indexed repos prints exactly one freshness line --
/// naming the repo actually represented in the results -- not one line per
/// registered repo. Before #1187 this printed zero lines regardless, so
/// this also pins that the line for the matching repo actually appears.
#[cfg(unix)]
#[test]
fn cross_repo_query_scopes_freshness_to_repos_in_the_result_not_every_registered_repo() {
    let data_dir = tempfile::tempdir().expect("data dir");
    index_fixture_repo(data_dir.path(), "alpha");
    // `beta` is registered and indexed, but has no `Greeter` symbol, so a
    // cross-repo `sym def Greeter` must not mention it.
    let beta_repo = tempfile::tempdir().expect("beta repo");
    std::fs::write(
        beta_repo.path().join("Cargo.toml"),
        "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::create_dir_all(beta_repo.path().join("src")).expect("mkdir src");
    std::fs::write(
        beta_repo.path().join("src/lib.rs"),
        "pub fn unrelated() {}\n",
    )
    .expect("write fixture");

    // Extend watch.toml (index_fixture_repo already wrote one entry for
    // "alpha") to also register "beta", then index it as a plain docs pass
    // (no SCIP marker needed -- it just needs a `file_inventory` presence
    // so it is a real "registered repo" for the no-`--repo` scope).
    let existing =
        std::fs::read_to_string(data_dir.path().join("watch.toml")).expect("read watch.toml");
    std::fs::write(
        data_dir.path().join("watch.toml"),
        format!(
            "{existing}\n[[repos]]\nname = \"beta\"\nworkdir = \"{}\"\n",
            beta_repo.path().display()
        ),
    )
    .expect("extend watch.toml");
    run_ok(legion_cmd(data_dir.path()).args(["index", "beta"]));

    let stderr = run_ok_stderr(legion_cmd(data_dir.path()).args(["sym", "def", "Greeter"]));
    assert!(
        stderr.contains("alpha: indexed"),
        "expected a freshness line for alpha (has the hit), got:\n{stderr}"
    );
    assert!(
        !stderr.contains("beta:"),
        "beta has no hit and no --repo scope, so it must get no freshness line: {stderr}"
    );
}
