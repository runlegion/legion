//! Integration tests for #917: prose verbs read their body from stdin so a
//! piped body's backticks/$() never reach argv (the shell interprets an
//! inline `--body "...`command`..."` and silently drops the substituted
//! text before legion ever sees it).
//!
//! Covers the distinct shapes: `comment` has no alternative input lane (its
//! only prose flag is `--body`), while `reflect` and `post` have `--transcript`
//! as an alternative that the stdin fallback must never shadow. `commit`'s
//! stdin path (and its `--message-file`-not-shadowed guard) lives in
//! `commit.rs`, where the git fixture harness already exists.

use crate::common::*;
use std::path::Path;

// ---------------------------------------------------------------------------
// comment -- exercised through a stubbed work source plugin, following the
// pattern in worksource_pr.rs. The stub writes the `LEGION_WS_BODY` env var
// it received to a file, using `printf '%s'` (not `echo`) so no trailing
// newline is added and the value is not re-interpreted: `"$VAR"` inside
// double quotes never re-evaluates backticks/$() already present in the
// variable's own content -- only legion's own argv construction, which
// #917 fixes, was ever at risk.
// ---------------------------------------------------------------------------

#[cfg(unix)]
const COMMENT_STUB_PLUGIN: &str = r#"#!/bin/bash
set -e
case "${1:-}" in
  comment)
    printf '%s' "$LEGION_WS_BODY" > "$STUB_RECEIVED_BODY_FILE"
    ;;
  *)
    echo "stub: unknown subcommand $1" >&2
    exit 2
    ;;
esac
"#;

/// Write the stub plugin + a watch.toml pointing repo "stub" at it, mirroring
/// `worksource_pr.rs::setup_pr_read_stub`.
#[cfg(unix)]
fn setup_comment_stub(data_dir: &Path, plugin_root: &Path) {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let worksources = plugin_root.join("worksources");
    fs::create_dir_all(&worksources).unwrap();
    let plugin_path = worksources.join("github");
    fs::write(&plugin_path, COMMENT_STUB_PLUGIN).unwrap();
    let mut perm = fs::metadata(&plugin_path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&plugin_path, perm).unwrap();

    let watch = format!(
        r#"poll_interval_secs = 30
cooldown_secs = 300

[[repos]]
name = "stub"
github = "owner/stub"
workdir = "{}"
worksource = "github"
"#,
        data_dir.display()
    );
    fs::write(data_dir.join("watch.toml"), watch).unwrap();
}

#[cfg(unix)]
fn comment_cmd(
    data_dir: &Path,
    plugin_root: &Path,
    received_body_file: &Path,
) -> std::process::Command {
    let mut cmd = legion_cmd(data_dir);
    cmd.env("CLAUDE_PLUGIN_ROOT", plugin_root)
        .env("STUB_RECEIVED_BODY_FILE", received_body_file);
    cmd
}

/// A body piped on stdin, with no `--body` flag, reaches the plugin
/// byte-identical -- including literal backticks and `$(...)`, the whole
/// point of #917.
#[cfg(unix)]
#[test]
fn comment_body_from_stdin_survives_backticks_and_command_substitution_verbatim() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    setup_comment_stub(data_dir.path(), plugin_root.path());
    let received_body_file = data_dir.path().join("received_body.txt");

    let payload: &[u8] =
        b"see `src/cli/util.rs` and $(cargo test --workspace) for the reproduction";

    let out = run_with_stdin(
        comment_cmd(data_dir.path(), plugin_root.path(), &received_body_file)
            .args(["comment", "--repo", "stub", "--number", "42"]),
        payload,
    );
    assert!(
        out.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let received = std::fs::read(&received_body_file).expect("stub wrote the received body");
    assert_eq!(
        received, payload,
        "the body must survive stdin byte-identical, including backticks and $(...)"
    );
}

/// `--body` inline still works unchanged (regression).
#[cfg(unix)]
#[test]
fn comment_inline_body_still_works() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    setup_comment_stub(data_dir.path(), plugin_root.path());
    let received_body_file = data_dir.path().join("received_body.txt");

    run_ok(
        comment_cmd(data_dir.path(), plugin_root.path(), &received_body_file).args([
            "comment",
            "--repo",
            "stub",
            "--number",
            "42",
            "--body",
            "plain inline body",
        ]),
    );

    let received = std::fs::read_to_string(&received_body_file).unwrap();
    assert_eq!(received, "plain inline body");
}

/// An empty piped body (no `--body`, stdin closed with nothing written) is
/// refused with a clear error rather than posting a blank comment.
#[cfg(unix)]
#[test]
fn comment_empty_piped_body_is_rejected() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    setup_comment_stub(data_dir.path(), plugin_root.path());
    let received_body_file = data_dir.path().join("received_body.txt");

    let out = run_with_stdin(
        comment_cmd(data_dir.path(), plugin_root.path(), &received_body_file)
            .args(["comment", "--repo", "stub", "--number", "42"]),
        b"   \n",
    );
    assert!(!out.status.success(), "expected the command to fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--body is empty"),
        "expected empty-body error, got: {stderr}"
    );
    assert!(
        !received_body_file.exists(),
        "the plugin must never be invoked with a blank body"
    );
}

// ---------------------------------------------------------------------------
// reflect -- round-tripped through `legion recall --id` rather than a stub,
// since reflect has no plugin hop: the stored text is the artifact.
// ---------------------------------------------------------------------------

/// A reflection body piped on stdin, with neither `--text` nor
/// `--transcript`, is stored and recalls back byte-identical -- literal
/// backticks and `$(...)` included.
#[test]
fn reflect_body_from_stdin_survives_backticks_and_command_substitution_verbatim() {
    let dir = tempfile::tempdir().unwrap();

    let payload: &[u8] =
        b"the regression lives in `src/cli/util.rs` -- repro with $(cargo test --bin legion)";

    let out = run_with_stdin(
        legion_cmd(dir.path()).args(["reflect", "--repo", "prose-stdin-test"]),
        payload,
    );
    assert!(
        out.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_uuid_format(&id);

    let recalled =
        run_ok(legion_cmd(dir.path()).args(["recall", "--repo", "prose-stdin-test", "--id", &id]));
    assert!(
        recalled.contains("`src/cli/util.rs`"),
        "expected the literal backticked path to survive, got: {recalled}"
    );
    assert!(
        recalled.contains("$(cargo test --bin legion)"),
        "expected the literal $(...) to survive uninterpreted, got: {recalled}"
    );
}

/// `--text` inline still works unchanged (regression).
#[test]
fn reflect_inline_text_still_works() {
    let dir = tempfile::tempdir().unwrap();

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "prose-stdin-test-inline",
        "--text",
        "plain inline reflection",
    ]));
    assert_uuid_format(stdout.trim());
}

// ---------------------------------------------------------------------------
// post -- stored to the bullpen (no plugin hop), read back cross-repo via
// `legion bullpen`, mirroring bullpen.rs::post_and_bullpen_roundtrip. post
// shares reflect's `--transcript` alternative and the same
// `inline_or_stdin_unless` guard, already covered by the reflect transcript
// test, so post asserts the stdin round-trip only.
// ---------------------------------------------------------------------------

/// A post body piped on stdin, with neither `--text` nor `--transcript`, is
/// stored and reads back verbatim in the bullpen -- backticks and `$(...)`
/// intact.
#[test]
fn post_body_from_stdin_survives_backticks_and_command_substitution_verbatim() {
    let dir = tempfile::tempdir().unwrap();

    let payload: &[u8] = b"regression in `src/cli/util.rs` -- repro $(cargo test)";

    let out = run_with_stdin(
        legion_cmd(dir.path()).args(["post", "--repo", "prose-stdin-post"]),
        payload,
    );
    assert!(
        out.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "expected a post id on stdout"
    );

    let bullpen = run_ok(legion_cmd(dir.path()).args(["bullpen", "--repo", "prose-stdin-reader"]));
    assert!(
        bullpen.contains("`src/cli/util.rs`"),
        "expected the literal backticked path to survive into the bullpen, got: {bullpen}"
    );
    assert!(
        bullpen.contains("$(cargo test)"),
        "expected the literal $(...) to survive uninterpreted, got: {bullpen}"
    );
}

/// `--transcript` is not shadowed by the stdin fallback: when a transcript
/// path is given, the (missing-file) transcript error surfaces -- the
/// command never blocks trying to also read stdin.
#[test]
fn reflect_transcript_flag_is_not_shadowed_by_stdin_fallback() {
    let dir = tempfile::tempdir().unwrap();

    let out = run_with_stdin(
        legion_cmd(dir.path()).args([
            "reflect",
            "--repo",
            "prose-stdin-test",
            "--transcript",
            "/nonexistent/legion/transcript.jsonl",
        ]),
        b"this must be ignored -- --transcript was given",
    );
    assert!(!out.status.success(), "expected the command to fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("transcript file not found"),
        "expected the transcript-not-found error, got: {stderr}"
    );
}
