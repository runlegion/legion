//! CLI end-to-end tests for `legion sym etc find-content` (#707) and
//! `legion sym etc extract` (#708).
//!
//! The library core (`etc::find_content`, `etc::extract_field`) is
//! unit-tested in src/etc.rs; these tests exercise the actual binary
//! surface the review found uncovered: flag parsing, output shapes, loud
//! error paths, and the telemetry side effect landing in etc-usage.jsonl.

use crate::common::{legion_cmd, run_fail, run_ok};

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

/// One dead repo among live ones is a warning, not a failure -- but it must
/// land in three places: stderr (for the human), exit 0 with hits (the scan
/// still answered), and the telemetry row's failed_repos count (so the #704
/// metric can tell "answered zero over the full corpus" from "answered zero
/// but part of the corpus was never searched").
#[test]
fn find_content_partial_corpus_failure_warns_and_lands_in_telemetry() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(repo_dir.path().join("ok.txt"), "needle\n").expect("write fixture");
    let ghost = data_dir.path().join("no-such-workdir");
    seed_watch_toml(
        data_dir.path(),
        &[("alive", repo_dir.path()), ("ghost", ghost.as_path())],
    );

    let out = legion_cmd(data_dir.path())
        .env("XDG_STATE_HOME", state_dir.path())
        .args(["sym", "etc", "find-content", "needle"])
        .output()
        .expect("run legion");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "partial failure must not fail the scan\nstderr:\n{stderr}"
    );
    // Cross-repo scan over >1 repo prefixes hits with the repo name.
    assert!(
        stdout.contains("alive/ok.txt:1: needle"),
        "expected the live repo's hit, got:\n{stdout}"
    );
    assert!(
        stderr.contains("repo 'ghost' could not be scanned"),
        "expected the dead repo named on stderr, got:\n{stderr}"
    );

    let usage = std::fs::read_to_string(state_dir.path().join("legion/etc-usage.jsonl"))
        .expect("etc-usage.jsonl written");
    let row: serde_json::Value =
        serde_json::from_str(usage.lines().next().expect("one row")).expect("row is JSON");
    assert_eq!(row["hit_count"], 1);
    assert_eq!(
        row["failed_repos"], 1,
        "partial corpus must be visible: {row}"
    );
    assert!(
        row["error"].is_null(),
        "partial failure is not an error: {row}"
    );
}

/// The --hidden flag end-to-end: default walk skips dotfiles, --hidden
/// admits them, and .git/ stays excluded either way. The library behavior
/// is unit-tested; this pins the clap-to-ContentScope wiring, where a
/// dropped negation would invert the default silently.
#[test]
fn find_content_hidden_flag_cli_end_to_end() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    let workflows = repo_dir.path().join(".github/workflows");
    std::fs::create_dir_all(&workflows).expect("mkdir .github/workflows");
    std::fs::write(workflows.join("ci.yml"), "needle\n").expect("write fixture");
    let git_dir = repo_dir.path().join(".git");
    std::fs::create_dir_all(&git_dir).expect("mkdir .git");
    std::fs::write(git_dir.join("config"), "needle\n").expect("write fixture");
    std::fs::write(repo_dir.path().join("seen.txt"), "needle\n").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("etcrepo", repo_dir.path())]);

    let default_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["sym", "etc", "find-content", "needle", "--repo", "etcrepo"]),
    );
    assert!(default_out.contains("seen.txt:1: needle"));
    assert!(
        !default_out.contains(".github"),
        "default walk must skip hidden files, got:\n{default_out}"
    );

    let hidden_out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "find-content",
                "needle",
                "--repo",
                "etcrepo",
                "--hidden",
            ]),
    );
    assert!(
        hidden_out.contains(".github/workflows/ci.yml:1: needle"),
        "--hidden must admit dotdirs, got:\n{hidden_out}"
    );
    assert!(
        !hidden_out.contains(".git/config"),
        ".git must stay excluded even under --hidden, got:\n{hidden_out}"
    );
}

/// The hit-cap warning is a human-facing contract: truncation is never
/// silent. Unit tests pin the counters; this pins the stderr note itself.
#[test]
fn find_content_hit_cap_warns_on_stderr() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    // MAX_HITS is 500; 505 matching lines leaves 5 suppressed.
    std::fs::write(repo_dir.path().join("many.txt"), "needle\n".repeat(505))
        .expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("etcrepo", repo_dir.path())]);

    let out = legion_cmd(data_dir.path())
        .env("XDG_STATE_HOME", state_dir.path())
        .args(["sym", "etc", "find-content", "needle", "--repo", "etcrepo"])
        .output()
        .expect("run legion");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "capped scan still succeeds:\n{stderr}"
    );
    assert_eq!(stdout.lines().count(), 500, "output capped at MAX_HITS");
    assert!(
        stderr.contains("5 more matches suppressed (cap 500)"),
        "expected the suppression note, got:\n{stderr}"
    );
}

/// The --no-ignore flag parses independently of --hidden (#745): a bare
/// `--no-ignore` invocation with no `--hidden` present must be accepted by
/// clap and reach the scan, not be rejected as a missing-flag combination.
/// The library-level semantics (no_ignore alone vs. no_ignore+hidden) are
/// pinned in src/etc.rs; this pins the clap wiring the review found
/// uncovered for --hidden (per reflection 019f2068).
#[test]
fn find_content_no_ignore_flag_parses_independently_of_hidden() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(repo_dir.path().join(".gitignore"), "ignored.txt\n").expect("write fixture");
    std::fs::write(repo_dir.path().join("ignored.txt"), "needle\n").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("etcrepo", repo_dir.path())]);

    // --no-ignore alone (no --hidden) must be accepted and reach ignored.txt,
    // an ordinary (non-dot) gitignored file.
    let out = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "find-content",
                "needle",
                "--repo",
                "etcrepo",
                "--no-ignore",
            ]),
    );
    assert!(
        out.contains("ignored.txt:1: needle"),
        "--no-ignore alone must be accepted by clap and reach a gitignored file, got:\n{out}"
    );
}

/// End-to-end pin of the load-bearing behavior (#745): --no-ignore alone
/// does not reach a gitignored DOT-directory; --no-ignore + --hidden
/// together do. This is the exact rafters field report (bullpen 019f355c).
#[test]
fn find_content_no_ignore_and_hidden_together_reach_gitignored_dot_dir() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(repo_dir.path().join(".gitignore"), ".rafters/\n").expect("write fixture");
    let rafters_dir = repo_dir.path().join(".rafters/output");
    std::fs::create_dir_all(&rafters_dir).expect("mkdir .rafters/output");
    std::fs::write(rafters_dir.join("rafters.css"), "needle\n").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("etcrepo", repo_dir.path())]);

    // --no-ignore alone: the dot-dir is still pruned by the hidden filter.
    let no_ignore_only = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "find-content",
                "needle",
                "--repo",
                "etcrepo",
                "--no-ignore",
            ]),
    );
    assert!(
        !no_ignore_only.contains(".rafters"),
        "--no-ignore alone must not reach a gitignored dot-dir, got:\n{no_ignore_only}"
    );

    // --no-ignore + --hidden together: the dot-dir is reached.
    let both = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "find-content",
                "needle",
                "--repo",
                "etcrepo",
                "--no-ignore",
                "--hidden",
            ]),
    );
    assert!(
        both.contains(".rafters/output/rafters.css:1: needle"),
        "--no-ignore + --hidden together must reach the gitignored dot-dir, got:\n{both}"
    );
}

// -- extract (#708) --

#[test]
fn extract_cli_default_output_and_telemetry() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(
        repo_dir.path().join("package.json"),
        r#"{"scripts": {"build": "tsc -p ."}}"#,
    )
    .expect("write fixture");

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "extract",
                repo_dir.path().join("package.json").to_str().unwrap(),
                "--field",
                "scripts.build",
            ]),
    );
    // Default output is the bare scalar, not a JSON-quoted string.
    assert_eq!(stdout.trim(), "tsc -p .");

    let usage = std::fs::read_to_string(state_dir.path().join("legion/etc-usage.jsonl"))
        .expect("etc-usage.jsonl written");
    let row: serde_json::Value =
        serde_json::from_str(usage.lines().next().expect("one row")).expect("row is JSON");
    assert_eq!(row["command"], "extract");
    assert_eq!(row["pattern"], "scripts.build");
    assert_eq!(row["hit_count"], 1);
    assert_eq!(row["format"], "json");
    assert!(row["error"].is_null());
}

#[test]
fn extract_cli_json_flag_emits_json_value() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(
        repo_dir.path().join("config.yaml"),
        "database:\n  port: 5432\n",
    )
    .expect("write fixture");

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "extract",
                repo_dir.path().join("config.yaml").to_str().unwrap(),
                "--field",
                "database.port",
                "--json",
            ]),
    );
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert_eq!(value, serde_json::json!(5432));
}

#[test]
fn extract_cli_array_field_prints_one_element_per_line() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(
        repo_dir.path().join("package.json"),
        r#"{"keywords": ["cli", "agents", "memory"]}"#,
    )
    .expect("write fixture");

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "extract",
                repo_dir.path().join("package.json").to_str().unwrap(),
                "--field",
                "keywords",
            ]),
    );
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        vec!["cli", "agents", "memory"]
    );
}

#[test]
fn extract_cli_mdx_frontmatter_field() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(
        repo_dir.path().join("doc.mdx"),
        "---\ntitle: Hello\n---\nimport { Foo } from './foo';\n\n<Foo />\n",
    )
    .expect("write fixture");

    let stdout = run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "extract",
                repo_dir.path().join("doc.mdx").to_str().unwrap(),
                "--field",
                "title",
            ]),
    );
    assert_eq!(stdout.trim(), "Hello");
}

#[test]
fn extract_cli_missing_field_fails_loudly_and_lands_in_telemetry() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(
        repo_dir.path().join("package.json"),
        r#"{"scripts": {"build": "tsc"}}"#,
    )
    .expect("write fixture");

    let (_stdout, stderr) = run_fail(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "extract",
                repo_dir.path().join("package.json").to_str().unwrap(),
                "--field",
                "scripts.test",
            ]),
    );
    assert!(
        stderr.contains("'test'") && stderr.contains("'scripts'"),
        "expected the deepest-resolved-segment error, got:\n{stderr}"
    );

    let usage = std::fs::read_to_string(state_dir.path().join("legion/etc-usage.jsonl"))
        .expect("errored invocation still writes a usage row");
    let row: serde_json::Value =
        serde_json::from_str(usage.lines().next().expect("one row")).expect("row is JSON");
    assert_eq!(row["hit_count"], 0);
    assert_eq!(row["format"], "json");
    assert!(row["error"].as_str().is_some_and(|e| e.contains("scripts")));
}

#[test]
fn extract_cli_unsupported_extension_fails_loudly() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");
    std::fs::write(repo_dir.path().join("notes.txt"), "plain text\n").expect("write fixture");

    let (_stdout, stderr) = run_fail(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args([
                "sym",
                "etc",
                "extract",
                repo_dir.path().join("notes.txt").to_str().unwrap(),
                "--field",
                "anything",
            ]),
    );
    assert!(
        stderr.contains("unsupported format"),
        "expected the unsupported-format error, got:\n{stderr}"
    );

    // Format could not be detected, so the telemetry row's format is null,
    // not a guess.
    let usage = std::fs::read_to_string(state_dir.path().join("legion/etc-usage.jsonl"))
        .expect("errored invocation still writes a usage row");
    let row: serde_json::Value =
        serde_json::from_str(usage.lines().next().expect("one row")).expect("row is JSON");
    assert!(row["format"].is_null());
}
