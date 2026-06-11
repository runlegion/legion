//! Shared helpers for the legion integration test crate.
//!
//! Every test in this crate exercises the compiled `legion` binary as a
//! subprocess via `CARGO_BIN_EXE_legion`. The spawn-and-assert plumbing
//! lives here so the ~250 call sites do not each hand-roll the
//! stderr-surfacing failure message (#608).

use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Base `legion` command with `LEGION_DATA_DIR` pointed at an isolated dir.
pub fn legion_cmd(data_dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_legion"));
    cmd.env("LEGION_DATA_DIR", data_dir);
    cmd
}

/// A variant of `legion_cmd` that also overrides the home directory so the
/// usage command reads sessions from the tempdir instead of the real
/// `~/.claude/projects/`.
///
/// `dirs::home_dir()` on Windows uses `SHGetKnownFolderPath(FOLDERID_Profile)`
/// and ignores both `HOME` and `USERPROFILE`, so the usage handler honors a
/// `LEGION_HOME` override that the test sets here.
pub fn legion_cmd_with_home(data_dir: &Path, home_dir: &Path) -> Command {
    let mut cmd = legion_cmd(data_dir);
    cmd.env("LEGION_HOME", home_dir);
    cmd
}

/// Run the command, require success, and return stdout as a `String`.
///
/// Panics with the child's stderr (and stdout) when the command fails, so
/// every call site gets the diagnostic message the old hand-rolled
/// `assert!(out.status.success(), "...: {}", from_utf8_lossy(&out.stderr))`
/// idiom used to re-type by hand.
pub fn run_ok(cmd: &mut Command) -> String {
    let out = cmd.output().expect("failed to execute legion binary");
    assert!(
        out.status.success(),
        "command failed (status {:?})\nstderr:\n{}\nstdout:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run the command, require success, and return stderr as a `String`.
///
/// Several legion surfaces print their human-facing confirmation to stderr
/// (`eprintln!`) while keeping stdout quiet for piping; tests asserting on
/// those messages use this variant.
pub fn run_ok_stderr(cmd: &mut Command) -> String {
    let out = cmd.output().expect("failed to execute legion binary");
    assert!(
        out.status.success(),
        "command failed (status {:?})\nstderr:\n{}\nstdout:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Run the command, require failure (non-zero exit), and return
/// `(stdout, stderr)` so the caller can assert on the error surface.
pub fn run_fail(cmd: &mut Command) -> (String, String) {
    let out = cmd.output().expect("failed to execute legion binary");
    assert!(
        !out.status.success(),
        "expected the command to fail but it succeeded\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout),
    );
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Spawn the command with a piped stdin, write `payload`, close the pipe,
/// and wait for the child. Returns the raw `Output` -- callers assert
/// success or failure themselves (stdin-fed commands are used for both
/// happy-path seeding and malformed-payload rejection tests).
///
/// The pipe discipline matters: stdin must be dropped before waiting or the
/// child blocks on EOF forever. This helper owns that easy-to-miss step.
pub fn run_with_stdin(cmd: &mut Command, payload: &[u8]) -> Output {
    use std::io::Write;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn legion binary");
    child
        .stdin
        .take()
        .expect("child stdin is piped")
        .write_all(payload)
        .expect("failed to write payload to child stdin");
    // The taken stdin handle dropped at the end of the statement above,
    // closing the pipe so the child sees EOF.
    child.wait_with_output().expect("failed to wait for child")
}

/// Assert `s` is a well-formed UUID of version 7 (legion's ID format).
/// Tolerates surrounding whitespace so raw stdout can be passed directly.
pub fn assert_uuid_format(s: &str) {
    let parsed = uuid::Uuid::parse_str(s.trim())
        .unwrap_or_else(|e| panic!("expected a UUID, got {s:?}: {e}"));
    assert_eq!(
        parsed.get_version_num(),
        7,
        "expected a UUIDv7, got version {} in {s:?}",
        parsed.get_version_num()
    );
}

/// Seed a single rate-limit sample by invoking `legion statusline` with a
/// minimal synthetic Claude Code JSON payload. Returns the path to a
/// transcript file we do NOT create -- statusline tolerates a missing
/// transcript (skips usage sample) and still writes the rate-limit row.
pub fn seed_rate_limit_sample(data_dir: &Path, five_hour_pct: f64, seven_day_pct: f64) {
    let session_id = format!("seed-{}", uuid::Uuid::now_v7());
    let payload = serde_json::json!({
        "session_id": session_id,
        "rate_limits": {
            "five_hour": { "used_percentage": five_hour_pct, "resets_at": 0 },
            "seven_day": { "used_percentage": seven_day_pct, "resets_at": 0 },
        },
    });
    let out = run_with_stdin(
        legion_cmd(data_dir).args(["statusline"]),
        payload.to_string().as_bytes(),
    );
    assert!(
        out.status.success(),
        "statusline seed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
