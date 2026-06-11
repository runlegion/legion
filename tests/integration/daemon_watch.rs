//! Integration tests: daemon spawn lifecycle and watch config CRUD.

use crate::common::*;

/// Verify `legion daemon-spawn` is idempotent: when a live daemon PID is
/// already recorded, a second spawn detects it and prints "already running"
/// instead of forking a duplicate.
///
/// The test writes the current test-process PID into `daemon.pid` before
/// calling `daemon-spawn`. The test process is guaranteed alive for the
/// duration of the test, so `is_process_alive` will return true and the
/// spawn path is correctly skipped. This isolates the idempotency check
/// from the question of whether a real daemon child would survive startup
/// in a CI environment (port conflicts, missing watch.toml, etc.), which
/// is a separate concern and not what this test is guarding.
///
/// Unix-only: `is_process_alive` uses `kill -0` on Unix and returns `false`
/// unconditionally on Windows (there is no portable equivalent), so this
/// idempotency path cannot be exercised on Windows. The entire daemon
/// auto-spawn feature is Unix-targeted anyway -- `setup-binary.sh` is bash
/// and the log paths (`~/Library/Logs`, `$XDG_STATE_HOME`) are Unix-only.
#[cfg(unix)]
#[test]
fn daemon_auto_spawn_is_idempotent() {
    let data_dir = tempfile::tempdir().unwrap();
    let pid_file = data_dir.path().join("daemon.pid");

    // Pre-populate daemon.pid with this test process's own PID. The test
    // process is definitionally alive while this runs, so is_process_alive
    // will return true when daemon-spawn checks it.
    let test_pid = std::process::id();
    std::fs::write(&pid_file, test_pid.to_string()).unwrap();

    let stderr = run_ok_stderr(legion_cmd(data_dir.path()).args(["daemon-spawn"]));
    assert!(
        stderr.contains("already running"),
        "expected 'already running' message, got: {stderr}"
    );

    // The PID file must be unchanged -- daemon-spawn should not overwrite
    // a live PID with a fresh spawn.
    let after = std::fs::read_to_string(&pid_file).unwrap();
    assert_eq!(
        after.trim(),
        test_pid.to_string(),
        "daemon-spawn must not overwrite a live PID file"
    );
}

/// Verify `legion daemon-spawn` clears a stale PID file (one whose recorded
/// PID is no longer alive) before attempting a fresh spawn. This is the
/// recovery path for the "daemon crashed, left its PID file behind" case.
///
/// The test writes an unlikely-to-be-in-use PID into `daemon.pid`, then
/// invokes `daemon-spawn`. Because the PID is not alive, the function must
/// remove the stale file and proceed to spawn. We do not assert anything
/// about whether the spawned child survives -- that is a separate concern.
/// We only assert that the stale PID was cleared (file was either removed
/// or overwritten with a different PID) and the command exited zero.
///
/// Unix-only for the same reason as `daemon_auto_spawn_is_idempotent`: the
/// feature uses Unix-only process semantics and shell plumbing, and this
/// test shells out via `kill` for cleanup. See the doc comment on that
/// test for the full rationale.
#[cfg(unix)]
#[test]
fn daemon_auto_spawn_clears_stale_pid() {
    let data_dir = tempfile::tempdir().unwrap();
    let pid_file = data_dir.path().join("daemon.pid");

    // Use a free ephemeral port rather than the default 3131. The #599 port
    // preflight now refuses to spawn onto an occupied port, and a dev box
    // commonly already runs a daemon on 3131 -- binding port 0 and dropping the
    // listener hands us a port that is free at this instant so the spawn path
    // (and the stale-pid clearing it exercises) is what gets tested, not the
    // collision guard.
    // Bind 0.0.0.0:0 to mirror the production `port_available` probe exactly.
    let port = std::net::TcpListener::bind(("0.0.0.0", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    // Write a stale PID: use 2^31 - 1, the maximum valid POSIX PID, which is
    // almost never a live process on any real system.
    let stale_pid: u32 = (i32::MAX) as u32;
    std::fs::write(&pid_file, stale_pid.to_string()).unwrap();

    run_ok(legion_cmd(data_dir.path()).args(["daemon-spawn", "--port", &port.to_string()]));

    // The stale PID must be gone. Either the file was removed during the
    // stale-cleanup step, or it was overwritten with a new child PID. Either
    // way, the stale value must not still be there.
    if pid_file.exists() {
        let after = std::fs::read_to_string(&pid_file).unwrap();
        assert_ne!(
            after.trim(),
            stale_pid.to_string(),
            "stale PID must be cleared or replaced"
        );

        // Best-effort cleanup: if a real daemon child was spawned, try to
        // kill it so we do not leave orphans behind after the test.
        if let Ok(pid) = after.trim().parse::<i32>() {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .output();
        }
    }
}

/// `legion daemon-spawn` onto a port held by a foreign process must fail
/// loud BEFORE forking (#599/#602 port preflight): non-zero exit, an error
/// naming the holder, and no pidfile pointing at a corpse.
///
/// The unit test `daemon::tests::spawn_detached_refuses_when_port_held`
/// pins the function-level contract; this drives the same preflight through
/// the CLI surface so the wiring from `Commands::DaemonSpawn` cannot
/// regress to start-then-die.
#[test]
fn daemon_spawn_preflight_refuses_held_port() {
    let data_dir = tempfile::tempdir().unwrap();

    // Hold a port for the duration of the test -- this is the "foreign
    // process" the preflight must detect. Bind 0.0.0.0 to mirror the
    // production `port_available` probe.
    let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();

    let (_stdout, stderr) =
        run_fail(legion_cmd(data_dir.path()).args(["daemon-spawn", "--port", &port.to_string()]));
    assert!(
        stderr.contains("already held by"),
        "preflight must name the port holder, got: {stderr}"
    );
    assert!(
        !data_dir.path().join("daemon.pid").exists(),
        "preflight must refuse before forking -- no pidfile may be written"
    );

    drop(listener);
}

// -- legion watch add / remove / list -----------------------------------------

#[test]
fn watch_add_creates_entry() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "watch",
        "add",
        "--name",
        "rafters",
        workdir.path().to_str().unwrap(),
    ]));
    assert!(
        stdout.contains("added:"),
        "expected 'added:' in output: {stdout}"
    );
    assert!(
        stdout.contains("rafters"),
        "expected name in output: {stdout}"
    );
}

#[test]
fn watch_add_derives_name_from_path_basename() {
    let dir = tempfile::tempdir().unwrap();
    let workdir_parent = tempfile::tempdir().unwrap();
    let workdir = workdir_parent.path().join("my-project");
    std::fs::create_dir(&workdir).unwrap();

    let stdout = run_ok(legion_cmd(dir.path()).args(["watch", "add", workdir.to_str().unwrap()]));
    assert!(
        stdout.contains("my-project"),
        "expected derived name 'my-project' in output: {stdout}"
    );

    let listed_stdout = run_ok(legion_cmd(dir.path()).args(["watch", "list"]));
    assert!(
        listed_stdout.contains("my-project"),
        "expected 'my-project' in list output: {listed_stdout}"
    );
}

#[test]
fn watch_add_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "watch",
        "add",
        "--name",
        "rafters",
        workdir.path().to_str().unwrap(),
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "watch",
        "add",
        "--name",
        "rafters",
        workdir.path().to_str().unwrap(),
    ]));
    assert!(
        stdout.contains("already present"),
        "expected 'already present' for duplicate: {stdout}"
    );
}

#[test]
fn watch_add_with_agent_flag() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "watch",
        "add",
        "--name",
        "legion",
        "--agent",
        "my-agent",
        workdir.path().to_str().unwrap(),
    ]));
    assert!(
        stdout.contains("my-agent"),
        "expected agent name in output: {stdout}"
    );
}

#[test]
fn watch_list_shows_entries() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();

    // Empty list first.
    let stdout = run_ok(legion_cmd(dir.path()).args(["watch", "list"]));
    assert!(
        stdout.contains("no repos"),
        "empty list should say 'no repos': {stdout}"
    );

    // Add one entry.
    run_ok(legion_cmd(dir.path()).args([
        "watch",
        "add",
        "--name",
        "rafters",
        workdir.path().to_str().unwrap(),
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["watch", "list"]));
    assert!(
        stdout.contains("rafters"),
        "list should show 'rafters': {stdout}"
    );
}

#[test]
fn watch_remove_removes_entry() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "watch",
        "add",
        "--name",
        "rafters",
        workdir.path().to_str().unwrap(),
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["watch", "remove", "rafters"]));
    assert!(
        stdout.contains("removed:"),
        "expected 'removed:' in output: {stdout}"
    );

    // Verify it is gone from list.
    let list_out = run_ok(legion_cmd(dir.path()).args(["watch", "list"]));
    assert!(
        !list_out.contains("rafters"),
        "entry should be gone after remove: {list_out}"
    );
}

#[test]
fn watch_remove_missing_entry_reports_not_found() {
    let dir = tempfile::tempdir().unwrap();

    let stdout = run_ok(legion_cmd(dir.path()).args(["watch", "remove", "nonexistent"]));
    assert!(
        stdout.contains("not found"),
        "expected 'not found' for unknown name: {stdout}"
    );
}

#[test]
fn watch_add_rejects_nonexistent_workdir() {
    let dir = tempfile::tempdir().unwrap();

    run_fail(legion_cmd(dir.path()).args([
        "watch",
        "add",
        "--name",
        "test",
        "/nonexistent/path/that/does/not/exist",
    ]));
}
