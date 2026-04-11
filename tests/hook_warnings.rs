//! Integration tests for legion hook warnings (#209).
//!
//! Regression guard: prior to #209 every legion hook piped stderr to
//! /tmp/legion-hook-errors.log (or /dev/null) and silently exited 0 on
//! failure. When legion itself was broken (corrupted embedding column,
//! missing data dir, schema mismatch) the agent lost recall + bullpen
//! context mid-session with zero visible signal. Compounding incidents
//! masked as agent idiocy for days.
//!
//! These tests construct two broken legion environments and assert that
//! every hook surfaces a visible `[Legion WARNING]` block in its stdout
//! while still exiting 0 (so Claude Code never blocks on legion failures).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const WARNING_SENTINEL: &str = "[Legion WARNING]";

/// Build a self-contained test CLAUDE_PLUGIN_ROOT in `temp` containing:
///   - bin/legion -- a wrapper script that execs the cargo-built test binary
///   - hooks/<all-under-test>.sh + _legion-warn.sh copied from source tree
fn setup_plugin_root(temp: &Path) -> PathBuf {
    let plugin_root = temp.join("plugin");
    fs::create_dir_all(plugin_root.join("bin")).expect("mkdir bin");
    fs::create_dir_all(plugin_root.join("hooks")).expect("mkdir hooks");

    let legion_bin = plugin_root.join("bin").join("legion");
    let test_binary = env!("CARGO_BIN_EXE_legion");
    fs::write(
        &legion_bin,
        format!("#!/bin/bash\nexec \"{}\" \"$@\"\n", test_binary),
    )
    .expect("write legion wrapper");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&legion_bin)
            .expect("stat wrapper")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&legion_bin, perms).expect("chmod wrapper");
    }

    let src_hooks = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugin")
        .join("hooks");
    for hook in [
        "_legion-warn.sh",
        "session-start.sh",
        "recall-first.sh",
        "bullpen-check.sh",
        "post-compact.sh",
        "precompact.sh",
    ] {
        fs::copy(src_hooks.join(hook), plugin_root.join("hooks").join(hook))
            .unwrap_or_else(|e| panic!("copy {}: {}", hook, e));
    }

    plugin_root
}

/// Invoke a hook by name, write `stdin_json` to its stdin, and return
/// (stdout, stderr, exit_code). The hook is run under bash with the
/// supplied CLAUDE_PLUGIN_ROOT and LEGION_DATA_DIR environment.
/// LEGION_NO_SYNC=1 is set to skip the sync path that would otherwise
/// hit the network and muddy the test.
fn run_hook(
    hook: &str,
    plugin_root: &Path,
    data_dir: &Path,
    stdin_json: &str,
) -> (String, String, i32) {
    let hook_path = plugin_root.join("hooks").join(hook);
    let mut child = Command::new("bash")
        .arg(&hook_path)
        .env("CLAUDE_PLUGIN_ROOT", plugin_root)
        .env("LEGION_DATA_DIR", data_dir)
        .env("LEGION_NO_SYNC", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {}: {}", hook, e));

    child
        .stdin
        .as_mut()
        .expect("stdin handle")
        .write_all(stdin_json.as_bytes())
        .expect("write stdin");

    let output = child.wait_with_output().expect("wait hook");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

/// Create a broken LEGION_DATA_DIR by writing a regular file where a
/// directory is expected. Every legion invocation that tries to read or
/// create subpaths inside will fail.
fn broken_data_dir_as_file(temp: &Path) -> PathBuf {
    let path = temp.join("broken-data-dir");
    fs::write(&path, b"not-a-directory").expect("write decoy file");
    path
}

/// Initialize a legion data dir via a real `legion reflect` call, then
/// corrupt it catastrophically by replacing `legion.db` with a directory
/// of the same name. Every subsequent SQLite open fails because sqlite3
/// cannot treat a directory as a database file.
///
/// Less catastrophic corruption (DROP TABLE, UPDATE to wrong type, write
/// junk bytes) did not work reliably: legion's `init_schema` re-runs on
/// every open with `CREATE TABLE IF NOT EXISTS`, so schema-level damage
/// silently heals itself. Type-affinity damage only triggers on paths
/// that actually deserialize the embedding column as BLOB, which is a
/// subset of legion reads. The directory-in-place-of-file trick is
/// path-independent and survives every legion recovery attempt -- the
/// same failure class as the 2026-04-11 incident where a data dir was
/// unusable but legion hooks gave no visible signal. See #209.
fn poison_schema(data_dir: &Path) {
    let init = Command::new(env!("CARGO_BIN_EXE_legion"))
        .env("LEGION_DATA_DIR", data_dir)
        .args([
            "reflect",
            "--repo",
            "legion",
            "--text",
            "sentinel reflection so the data dir is fully initialized",
        ])
        .output()
        .expect("spawn legion reflect");
    assert!(
        init.status.success(),
        "legion reflect init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );

    let db_path = data_dir.join("legion.db");
    assert!(
        db_path.exists(),
        "legion.db missing after reflect -- schema changed?"
    );

    // Remove the db and its WAL/SHM sidecars, then put a directory in
    // place of the main file. Legion cannot open a directory as a db.
    fs::remove_file(&db_path).expect("rm legion.db");
    let _ = fs::remove_file(data_dir.join("legion.db-shm"));
    let _ = fs::remove_file(data_dir.join("legion.db-wal"));
    fs::create_dir(&db_path).expect("mkdir in place of legion.db");
}

/// Build stdin JSON shaped for a hook that only needs `cwd`.
fn cwd_json(cwd: &Path) -> String {
    format!(r#"{{"cwd": "{}"}}"#, cwd.display())
}

/// Build stdin JSON for recall-first.sh: cwd + tool metadata.
fn grep_tool_json(cwd: &Path) -> String {
    format!(
        r#"{{"cwd": "{}", "tool_name": "Grep", "tool_input": {{"pattern": "some-query-longer-than-four"}}}}"#,
        cwd.display()
    )
}

/// Build stdin JSON for precompact.sh: cwd + transcript_path.
fn precompact_json(cwd: &Path, transcript: &Path) -> String {
    format!(
        r#"{{"cwd": "{}", "transcript_path": "{}"}}"#,
        cwd.display(),
        transcript.display()
    )
}

/// Assert that hook output contains the warning sentinel and the hook
/// still exited 0 (never break Claude Code).
fn assert_warned(hook: &str, stdout: &str, exit_code: i32) {
    assert_eq!(
        exit_code, 0,
        "{hook} must exit 0 even on legion failure (got {exit_code})\nstdout: {stdout}"
    );
    assert!(
        stdout.contains(WARNING_SENTINEL),
        "{hook} stdout missing {WARNING_SENTINEL}:\n{stdout}"
    );
}

// --- TEST 1: corrupted embedding column (the regression from 2026-04-11) ---

#[test]
fn session_start_warns_on_corrupted_schema() {
    let temp = tempfile::tempdir().unwrap();
    let plugin_root = setup_plugin_root(temp.path());
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    poison_schema(&data_dir);

    let cwd = temp.path().join("legion");
    fs::create_dir_all(&cwd).unwrap();

    let (stdout, _stderr, exit) =
        run_hook("session-start.sh", &plugin_root, &data_dir, &cwd_json(&cwd));
    assert_warned("session-start.sh", &stdout, exit);
}

#[test]
fn recall_first_warns_on_corrupted_schema() {
    let temp = tempfile::tempdir().unwrap();
    let plugin_root = setup_plugin_root(temp.path());
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    poison_schema(&data_dir);

    let cwd = temp.path().join("legion");
    fs::create_dir_all(&cwd).unwrap();

    let (stdout, _stderr, exit) = run_hook(
        "recall-first.sh",
        &plugin_root,
        &data_dir,
        &grep_tool_json(&cwd),
    );
    assert_warned("recall-first.sh", &stdout, exit);
}

#[test]
fn bullpen_check_warns_on_corrupted_schema() {
    let temp = tempfile::tempdir().unwrap();
    let plugin_root = setup_plugin_root(temp.path());
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    poison_schema(&data_dir);

    let cwd = temp.path().join("legion");
    fs::create_dir_all(&cwd).unwrap();

    let (stdout, _stderr, exit) =
        run_hook("bullpen-check.sh", &plugin_root, &data_dir, &cwd_json(&cwd));
    assert_warned("bullpen-check.sh", &stdout, exit);
}

#[test]
fn post_compact_warns_on_corrupted_schema() {
    let temp = tempfile::tempdir().unwrap();
    let plugin_root = setup_plugin_root(temp.path());
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    poison_schema(&data_dir);

    let cwd = temp.path().join("legion");
    fs::create_dir_all(&cwd).unwrap();

    let (stdout, _stderr, exit) =
        run_hook("post-compact.sh", &plugin_root, &data_dir, &cwd_json(&cwd));
    assert_warned("post-compact.sh", &stdout, exit);
}

#[test]
fn precompact_failure_propagates_to_post_compact_warning() {
    // precompact.sh cannot surface additionalContext itself -- the session
    // is about to compact. When its reflect call fails it touches a marker
    // that post-compact.sh reads on the next invocation and surfaces via
    // its own warning block. This is the end-to-end path.
    let temp = tempfile::tempdir().unwrap();
    let plugin_root = setup_plugin_root(temp.path());
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    poison_schema(&data_dir);

    let cwd = temp.path().join("legion");
    fs::create_dir_all(&cwd).unwrap();
    let transcript = temp.path().join("transcript.jsonl");
    fs::write(&transcript, b"").unwrap();

    // Run precompact first -- reflect should fail on the poisoned DB and
    // the marker file should get dropped. precompact exits 0 with no stdout.
    let (_pre_stdout, _pre_stderr, pre_exit) = run_hook(
        "precompact.sh",
        &plugin_root,
        &data_dir,
        &precompact_json(&cwd, &transcript),
    );
    assert_eq!(pre_exit, 0, "precompact must exit 0");

    // The marker path is /tmp/legion-checkpoint-failed-<md5(cwd)>. We can't
    // easily recompute the hash cross-platform here, so assert by running
    // post-compact and inspecting its output -- if the marker was dropped,
    // post-compact will include "precompact reflect (checkpoint not saved)"
    // in its warning block.
    let (stdout, _stderr, exit) =
        run_hook("post-compact.sh", &plugin_root, &data_dir, &cwd_json(&cwd));
    assert_warned("post-compact.sh", &stdout, exit);
    assert!(
        stdout.contains("precompact reflect"),
        "post-compact stdout should surface the precompact failure chain:\n{stdout}"
    );
}

// --- TEST 2: missing / broken data directory (file where dir expected) ---

#[test]
fn session_start_warns_on_missing_data_dir() {
    let temp = tempfile::tempdir().unwrap();
    let plugin_root = setup_plugin_root(temp.path());
    let broken = broken_data_dir_as_file(temp.path());

    let cwd = temp.path().join("legion");
    fs::create_dir_all(&cwd).unwrap();

    let (stdout, _stderr, exit) =
        run_hook("session-start.sh", &plugin_root, &broken, &cwd_json(&cwd));
    assert_warned("session-start.sh", &stdout, exit);
}

#[test]
fn recall_first_warns_on_missing_data_dir() {
    let temp = tempfile::tempdir().unwrap();
    let plugin_root = setup_plugin_root(temp.path());
    let broken = broken_data_dir_as_file(temp.path());

    let cwd = temp.path().join("legion");
    fs::create_dir_all(&cwd).unwrap();

    let (stdout, _stderr, exit) = run_hook(
        "recall-first.sh",
        &plugin_root,
        &broken,
        &grep_tool_json(&cwd),
    );
    assert_warned("recall-first.sh", &stdout, exit);
}

#[test]
fn bullpen_check_warns_on_missing_data_dir() {
    let temp = tempfile::tempdir().unwrap();
    let plugin_root = setup_plugin_root(temp.path());
    let broken = broken_data_dir_as_file(temp.path());

    let cwd = temp.path().join("legion");
    fs::create_dir_all(&cwd).unwrap();

    let (stdout, _stderr, exit) =
        run_hook("bullpen-check.sh", &plugin_root, &broken, &cwd_json(&cwd));
    assert_warned("bullpen-check.sh", &stdout, exit);
}

#[test]
fn post_compact_warns_on_missing_data_dir() {
    let temp = tempfile::tempdir().unwrap();
    let plugin_root = setup_plugin_root(temp.path());
    let broken = broken_data_dir_as_file(temp.path());

    let cwd = temp.path().join("legion");
    fs::create_dir_all(&cwd).unwrap();

    let (stdout, _stderr, exit) =
        run_hook("post-compact.sh", &plugin_root, &broken, &cwd_json(&cwd));
    assert_warned("post-compact.sh", &stdout, exit);
}
