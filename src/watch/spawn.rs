//! Agent spawning: the print/PTY spawn modes, the polymorphic child
//! handle, and the stop-hook fast path (`record_session_end`).

use std::path::Path;
use std::process::Child;

use crate::db::Database;
use crate::error::{LegionError, Result};

use super::config::default_session_lock_ttl_secs;
use super::locks::SessionLockTracker;

// -- Agent Spawning ----------------------------------------------------------

/// Bracketed-paste start marker (`ESC [ 200 ~`). Wrapping the wake prompt
/// in bracketed paste keeps embedded newlines literal so a multi-line
/// prompt is not fragmented into multiple submits (#649).
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
/// Bracketed-paste end marker (`ESC [ 201 ~`).
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";
/// The submit keystroke retried by the confirmed-submit protocol (#649).
/// `\r` submits on every shipped Claude Code surface; `\n` does not.
pub const SUBMIT_KEY: &[u8] = b"\r";

/// Wrap a prompt in bracketed-paste markers with NO trailing submit (#649).
/// Pulled out as a pure helper so the exact wire bytes are unit-testable
/// without a live `claude` PTY.
///
/// The prompt is stripped of ESC (0x1b) bytes before wrapping. The wake
/// prompt bundles signal text authored by other agents/repos; a stray or
/// hostile `ESC[201~` in that content would close the bracketed paste
/// early and turn the remaining bytes into raw TUI keystrokes -- a
/// keystroke-injection seam. Wake prompts are plain text, so dropping ESC
/// is lossless and closes the seam regardless of how the call site widens.
fn wrap_bracketed_paste(prompt: &str) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(prompt.len() + BRACKETED_PASTE_START.len() + BRACKETED_PASTE_END.len());
    out.extend_from_slice(BRACKETED_PASTE_START);
    out.extend(prompt.bytes().filter(|&b| b != 0x1b));
    out.extend_from_slice(BRACKETED_PASTE_END);
    out
}

/// Selects which spawn implementation `spawn_agent` dispatches to.
///
/// `Print` is the current `claude --print -p <prompt>` path. `Pty` is the
/// in-progress migration to a PTY-spawned interactive REPL (see #495);
/// the branch is stubbed at this stage so subsequent issues can fill it
/// in without risking the production path. Resolved once at watch
/// startup from `WATCH_SPAWN_MODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnMode {
    Print,
    Pty,
}

impl SpawnMode {
    /// Resolve from `WATCH_SPAWN_MODE`. Accepts `"print"` or `"pty"`
    /// (case-insensitive). Empty / unset / any other value falls back
    /// to `Pty` (the v0.16.0 default after #494 -- subscription
    /// billing for `claude --print -p` ended 2026-06-15). Operators
    /// who explicitly want the legacy path set `WATCH_SPAWN_MODE=print`.
    /// Unknown values log a warning so a typo is visible.
    pub fn from_env() -> Self {
        match std::env::var("WATCH_SPAWN_MODE") {
            Ok(raw) => Self::parse(&raw),
            Err(_) => Self::Pty,
        }
    }

    fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Self::Pty;
        }
        match trimmed.to_ascii_lowercase().as_str() {
            "print" => Self::Print,
            "pty" => Self::Pty,
            other => {
                eprintln!(
                    "[legion watch] unknown WATCH_SPAWN_MODE={:?} -- falling back to pty",
                    other
                );
                Self::Pty
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Print => "print",
            Self::Pty => "pty",
        }
    }
}

/// Spawn a `claude` session for the given repo under the resolved mode.
///
/// `Print` runs `claude --print -p <prompt>` exactly as before. `Pty`
/// is reserved for the PTY migration (#489) and currently returns
/// `LegionError::NotImplemented` -- callers must surface the error and
/// release any holds (the existing `poll_cycle` spawn-failure path does
/// this already).
///
/// Optimistic stop-hook handoff (#493). Writes `exit_observed_at` on
/// the wake_attempts row so the reaper can short-circuit a poll cycle,
/// AND releases the session lock so the wake gate clears immediately
/// rather than waiting for the TTL. PTY EOF + PID-poll remain the
/// authoritative completion signal -- this is a speed-up only.
/// Idempotent: missing rows return `Ok(())` because the hook may fire
/// for operator-attended sessions that watch never spawned (no wake row
/// -> still Ok; no lock -> release is a no-op).
///
/// `repo_hint` is used on the interactive path (#673 fix 2): when no
/// wake_attempts row exists for `attempt_id` (interactive / operator
/// session), the `.session` file for `repo_hint` is removed so the
/// active-session gate is cleared. Ignored when the row is found
/// (the repo is resolved from the row instead).
pub fn record_session_end(
    db: &Database,
    attempt_id: &str,
    repo_hint: Option<&str>,
    data_dir: &Path,
) -> Result<()> {
    match db.mark_wake_attempt_exit_observed(attempt_id) {
        Ok(()) => {}
        Err(LegionError::WakeAttemptNotFound(_)) => {
            // No wake row for this attempt_id -- operator-attended (interactive)
            // session or a hook firing before the daemon wrote the row.
            // Still clean up the .session file if the caller provided a repo
            // name, so the gate is cleared for the next wake cycle (#673 fix 2).
            if let Some(repo) = repo_hint {
                let session_locks =
                    SessionLockTracker::new(data_dir, default_session_lock_ttl_secs());
                if let Err(e) = session_locks.release_interactive(repo) {
                    eprintln!(
                        "[legion watch] session-end: failed to release .session for {}: {}",
                        repo, e
                    );
                }
            }
            return Ok(());
        }
        Err(e) => return Err(e),
    }

    // Resolve the repo from the wake_attempts row so we know which lockfile
    // to delete. The TTL is irrelevant here -- `release` deletes the lockfile
    // by path and never consults `ttl` -- so use the default rather than
    // reading watch.toml. A failure to release is non-fatal: the reaper's
    // authoritative path releases the lock on the next poll cycle.
    let session_locks = SessionLockTracker::new(data_dir, default_session_lock_ttl_secs());

    match db.get_wake_attempt(attempt_id) {
        Ok(Some(attempt)) => {
            if let Err(e) = session_locks.release(&attempt.repo_name) {
                eprintln!(
                    "[legion watch] session-end: failed to release lock for {}: {}",
                    attempt.repo_name, e
                );
            }
            // Remove the interactive .session file as well (#673 fix 2).
            // The wake-spawned path does not write a .session file (only
            // interactive sessions do), but releasing it here is idempotent
            // -- missing file is not an error. This closes the gap where a
            // wake-spawned session that also happens to have an operator-held
            // .session could leave a stale file behind.
            if let Err(e) = session_locks.release_interactive(&attempt.repo_name) {
                eprintln!(
                    "[legion watch] session-end: failed to release .session for {}: {}",
                    attempt.repo_name, e
                );
            }
        }
        Ok(None) => {
            // Row was just written by mark_wake_attempt_exit_observed so
            // this branch should be unreachable in practice. Log and continue.
            eprintln!(
                "[legion watch] session-end: wake attempt {} not found after update (race?)",
                attempt_id
            );
        }
        Err(e) => {
            eprintln!(
                "[legion watch] session-end: could not look up attempt {} for lock release: {}",
                attempt_id, e
            );
        }
    }

    Ok(())
}

/// Polymorphic child handle returned by `spawn_agent`. The Print
/// branch wraps `std::process::Child` (unchanged from the `claude -p`
/// era); the Pty branch wraps `pty::PtySession` so the interactive
/// REPL retains subscription billing under the post-2026-06-15 cutoff.
///
/// Implements the minimal subset of operations the watch reaper +
/// lease release path actually need: `pid` for liveness + log lines,
/// `try_wait` for non-blocking exit detection, `kill` for shutdown.
///
/// `send_keys` is the one sanctioned WRITE control path: the
/// confirmed-submit protocol (#649) injects the prompt paste and retry
/// submit keystrokes through it after spawn. Reading the other
/// direction stays off-limits -- the PTY ring buffer (`output_tail`) is
/// for diagnostics, not control flow; submit confirmation is the
/// filesystem oracle, not output scraping.
pub enum SpawnedChild {
    Print(Child),
    Pty(crate::pty::PtySession),
}

impl SpawnedChild {
    pub fn id(&self) -> u32 {
        match self {
            SpawnedChild::Print(c) => c.id(),
            SpawnedChild::Pty(s) => s.pid(),
        }
    }

    /// Non-blocking exit check. `Ok(Some(_))` once the child has
    /// exited (the inner value is the OS-reported success bit;
    /// callers that need exit code distinguishing use Print's Child
    /// directly or the PtySession's ExitStatus). `Ok(None)` while
    /// still running.
    pub fn try_wait(&mut self) -> Result<Option<bool>> {
        match self {
            SpawnedChild::Print(c) => match c.try_wait().map_err(LegionError::Io)? {
                Some(status) => Ok(Some(status.success())),
                None => Ok(None),
            },
            SpawnedChild::Pty(s) => match s.try_wait()? {
                Some(status) => Ok(Some(status.success)),
                None => Ok(None),
            },
        }
    }

    /// Terminate the child. Used by `reap_finished` to tear down an idle PTY
    /// REPL whose turn is complete, and by the spawn-failure cleanup path.
    pub fn kill(&mut self) -> Result<()> {
        match self {
            SpawnedChild::Print(c) => c.kill().map_err(LegionError::Io),
            SpawnedChild::Pty(s) => s.kill(),
        }
    }

    /// Send raw bytes to the child's interactive input after spawn.
    ///
    /// The confirmed-submit protocol (#649) uses this to bracketed-paste
    /// the wake prompt and to retry the submit keystroke until the TUI
    /// input pipeline is ready. The `Print` variant has no interactive
    /// stdin -- `claude --print -p` consumed its prompt as an argv -- so
    /// it returns `PtyControlUnsupported` rather than silently no-op'ing,
    /// which would mask a spawn-mode mismatch at the call site.
    pub fn send_keys(&mut self, bytes: &[u8]) -> Result<()> {
        match self {
            SpawnedChild::Print(_) => Err(LegionError::PtyControlUnsupported),
            SpawnedChild::Pty(s) => s.write(bytes),
        }
    }

    /// Has a Claude turn started in this child's session? The
    /// confirmed-submit protocol (#649) gates its retry loop on this.
    /// `Print` children never enter that loop (they submit their prompt
    /// as an argv at spawn), so the variant returns `false`; the only
    /// caller skips non-`Pty` children anyway.
    pub fn turn_started(&self) -> bool {
        match self {
            SpawnedChild::Print(_) => false,
            SpawnedChild::Pty(s) => s.saw_turn_start(),
        }
    }
}

pub fn spawn_agent(
    workdir: &str,
    prompt: &str,
    mode: SpawnMode,
    attempt_id: Option<&str>,
) -> Result<SpawnedChild> {
    match mode {
        SpawnMode::Print => {
            let mut cmd = std::process::Command::new("claude");
            cmd.args(["--print", "-p", prompt])
                .current_dir(workdir)
                .env("LEGION_AUTO_WAKE", "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            if let Some(id) = attempt_id {
                cmd.env("LEGION_WAKE_ATTEMPT_ID", id);
            }
            match cmd.spawn() {
                Ok(c) => Ok(SpawnedChild::Print(c)),
                Err(e) => {
                    eprintln!("[legion watch] failed to spawn agent: {}", e);
                    Err(LegionError::Io(e))
                }
            }
        }
        SpawnMode::Pty => {
            // PTY-spawned interactive `claude` REPL (#489). Subscription
            // billing applies because the REPL sees a TTY. Prompt is
            // injected via keystrokes through the master fd; the legion
            // plugin remains loaded inside the spawned session so the
            // stop hook can fire `legion watch session-end` (#493) as
            // an optimistic completion signal.
            let env: Vec<(&str, &str)> = {
                let mut e = vec![
                    ("LEGION_AUTO_WAKE", "1"),
                    ("LEGION_SPAWN_SOURCE", "watch-pty"),
                ];
                if let Some(id) = attempt_id {
                    e.push(("LEGION_WAKE_ATTEMPT_ID", id));
                }
                e
            };
            let cwd = std::path::Path::new(workdir);
            let mut opts = crate::pty::PtySpawnOptions::new("claude", &[], cwd);
            opts.env = &env;
            let mut session = crate::pty::PtySession::spawn(opts)?;
            // Inject the prompt as a BRACKETED PASTE, with NO trailing
            // submit keystroke (#649). Two empirical reasons:
            //   1. The wake prompt is multi-line; a raw multi-line write
            //      lets the TUI submit on the first embedded newline,
            //      fragmenting the prompt. Wrapping it in the bracketed
            //      paste markers (ESC[200~ ... ESC[201~) keeps every
            //      newline literal until one explicit Enter submits.
            //   2. A `\r` written here, before the TUI input pipeline is
            //      interactive (~15-22s in plugin-heavy repos), is
            //      swallowed -- the old fire-and-forget submit. The
            //      confirmed-submit protocol sends Enter from the health
            //      tick instead, retrying until the ring buffer shows a
            //      turn started.
            let keystrokes = wrap_bracketed_paste(prompt);
            if let Err(e) = session.write(&keystrokes) {
                eprintln!("[legion watch] PTY prompt paste failed: {}", e);
                // Best-effort kill so we do not leak a half-started
                // child; the spawn-failure path in poll_cycle releases
                // any leases we acquired.
                let _ = session.kill();
                return Err(e);
            }
            Ok(SpawnedChild::Pty(session))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- SpawnMode (#485) ---------------------------------------------------

    #[test]
    fn spawn_mode_parse_print() {
        assert_eq!(SpawnMode::parse("print"), SpawnMode::Print);
        assert_eq!(SpawnMode::parse("PRINT"), SpawnMode::Print);
        assert_eq!(SpawnMode::parse("  print  "), SpawnMode::Print);
    }

    #[test]
    fn spawn_mode_parse_pty() {
        assert_eq!(SpawnMode::parse("pty"), SpawnMode::Pty);
        assert_eq!(SpawnMode::parse("PTY"), SpawnMode::Pty);
    }

    #[test]
    fn spawn_mode_parse_unknown_falls_back_to_pty() {
        // Default flipped to Pty in #494 (post-2026-06-15 billing
        // shift). Empty, whitespace, and unrecognized strings now
        // engage the PTY path; operators who want the legacy
        // print path set WATCH_SPAWN_MODE=print explicitly.
        assert_eq!(SpawnMode::parse(""), SpawnMode::Pty);
        assert_eq!(SpawnMode::parse("   "), SpawnMode::Pty);
        assert_eq!(SpawnMode::parse("nope"), SpawnMode::Pty);
    }

    // The stub-verifying `spawn_agent_pty_returns_not_implemented`
    // test from #485 is intentionally removed in #489 -- the Pty
    // branch now spawns a PTY-backed REPL instead of returning
    // NotImplemented. Behavior is exercised by the integration tests
    // in pty::tests; a full end-to-end spawn here would require a
    // real `claude` binary on PATH, which CI does not have.

    #[test]
    fn spawn_mode_as_str_matches_env_values() {
        assert_eq!(SpawnMode::Print.as_str(), "print");
        assert_eq!(SpawnMode::Pty.as_str(), "pty");
    }

    // -- send_keys (#648) ---------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn send_keys_on_print_child_is_unsupported() {
        // The Print variant has no interactive stdin -- send_keys must
        // surface a typed error, not silently swallow the keystrokes,
        // so a spawn-mode mismatch is visible at the call site.
        let child = std::process::Command::new("sleep")
            .arg("9999")
            .spawn()
            .expect("spawn sleep");
        let mut spawned = SpawnedChild::Print(child);

        let err = spawned
            .send_keys(b"\r")
            .expect_err("send_keys on a print child must error");
        assert!(matches!(err, LegionError::PtyControlUnsupported));

        let _ = spawned.kill();
    }

    // -- bracketed paste (#649) ---------------------------------------------

    #[test]
    fn wrap_bracketed_paste_wraps_exactly_with_no_submit() {
        // The wire bytes must be START + prompt + END, with NO trailing \r:
        // the submit keystroke is sent later by the confirmation loop, and a
        // multi-line prompt must stay inside one paste so embedded newlines
        // do not fragment into multiple submits.
        let wrapped = wrap_bracketed_paste("line one\nline two");
        let mut expected = Vec::new();
        expected.extend_from_slice(b"\x1b[200~");
        expected.extend_from_slice(b"line one\nline two");
        expected.extend_from_slice(b"\x1b[201~");
        assert_eq!(wrapped, expected);
        assert!(
            !wrapped.ends_with(b"\r"),
            "bracketed paste must not carry a submit keystroke"
        );
    }

    #[test]
    fn wrap_bracketed_paste_strips_esc_to_close_injection_seam() {
        // An embedded ESC[201~ in prompt content must not survive -- it
        // would close the paste early and inject raw TUI keystrokes. ESC
        // bytes are dropped; the surrounding plain text is preserved.
        let hostile = "before\x1b[201~rm -rf\x1b after";
        let wrapped = wrap_bracketed_paste(hostile);
        // No interior ESC byte remains (the only ESC bytes are inside the
        // start/end markers we control).
        let body = &wrapped[BRACKETED_PASTE_START.len()..wrapped.len() - BRACKETED_PASTE_END.len()];
        assert!(
            !body.contains(&0x1b),
            "ESC bytes must be stripped from pasted content"
        );
        assert_eq!(body, b"before[201~rm -rf after");
    }

    #[test]
    fn turn_started_is_false_for_print_child() {
        // Print children never enter the confirmation loop; the accessor
        // must report no turn so a stray call cannot mis-confirm them.
        #[cfg(unix)]
        {
            let child = std::process::Command::new("sleep")
                .arg("9999")
                .spawn()
                .expect("spawn sleep");
            let mut spawned = SpawnedChild::Print(child);
            assert!(!spawned.turn_started());
            let _ = spawned.kill();
        }
    }

    // -- record_session_end (#673 fix 2) ------------------------------------

    /// `record_session_end` with a repo_hint and no wake_attempts row
    /// (interactive/operator path) must remove the `.session` file.
    #[test]
    fn session_end_removes_session_file_on_interactive_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path();

        // Write a .session file to simulate an active interactive session.
        let locks = SessionLockTracker::new(data_dir, default_session_lock_ttl_secs());
        locks
            .record_interactive("myrepo", std::process::id())
            .expect("record_interactive");

        // Verify the .session file exists before the call.
        let session_file = data_dir.join("sessions").join("myrepo.session");
        assert!(
            session_file.exists(),
            "precondition: .session file must exist before session-end"
        );

        // Open a fresh DB -- no wake_attempts row exists.
        let db_path = data_dir.join("legion.db");
        let db = crate::db::Database::open(&db_path).expect("open db");

        // Call with an attempt_id that has no matching row and a repo_hint.
        let result = record_session_end(&db, "nonexistent-attempt-id", Some("myrepo"), data_dir);
        assert!(
            result.is_ok(),
            "record_session_end must return Ok on missing wake attempt"
        );

        // The .session file must be gone.
        assert!(
            !session_file.exists(),
            ".session file must be removed on the interactive path when repo_hint is provided"
        );
    }

    /// When no wake_attempts row exists AND no repo_hint is provided,
    /// `record_session_end` returns Ok but does not crash.
    #[test]
    fn session_end_no_repo_hint_is_ok_with_no_wake_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("legion.db");
        let db = crate::db::Database::open(&db_path).expect("open db");

        let result = record_session_end(&db, "nonexistent-attempt-id", None, dir.path());
        assert!(
            result.is_ok(),
            "record_session_end must return Ok even with no row and no repo_hint"
        );
    }
}
