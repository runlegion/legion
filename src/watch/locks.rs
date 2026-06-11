//! Process liveness and the lock layers: the watch pid lock, the per-repo
//! session locks (watch-spawned and interactive), and the wake cooldown.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Timelike;

use crate::error::{LegionError, Result};

// -- PID Lock ----------------------------------------------------------------

/// Acquire a PID lock file. Returns an error if another watcher is running.
pub fn acquire_pid_lock(lock_path: &Path) -> Result<()> {
    if lock_path.exists() {
        let contents = std::fs::read_to_string(lock_path).unwrap_or_default();
        if let Ok(pid) = contents.trim().parse::<u32>() {
            // Check if the process is actually running
            if process_alive(pid) {
                return Err(LegionError::WatchAlreadyRunning(pid));
            }
            // Stale lock file -- process is dead, remove it
            eprintln!("[legion watch] removing stale lock (pid {})", pid);
        }
    }

    let pid = std::process::id();
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(lock_path, pid.to_string())?;
    Ok(())
}

/// Release the PID lock file.
pub fn release_pid_lock(lock_path: &Path) {
    let _ = std::fs::remove_file(lock_path);
}

/// RAII guard that releases the PID lock file on drop.
///
/// Holds the lock path and removes the file when dropped, ensuring the lock
/// is always released even on panic or task abort.
pub struct PidLockGuard(pub PathBuf);

impl Drop for PidLockGuard {
    fn drop(&mut self) {
        release_pid_lock(&self.0);
        eprintln!("[legion watch] released lock");
    }
}

/// Check whether a process with the given PID is alive.
///
/// Shells out to `kill -0` on Unix (signal 0 probes existence without
/// delivering a signal; the no-unsafe invariant rules out libc::kill).
/// Always returns `false` on non-Unix platforms where we cannot probe
/// process state. This is the single liveness probe for the whole binary --
/// watch locks and the daemon pidfile machinery share it so they can never
/// disagree about whether a pidfile holder is alive (#611).
pub(crate) fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let result = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        matches!(result, Ok(status) if status.success())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

// -- Session Lock ------------------------------------------------------------

/// Per-repo session lock. Prevents `watch` from spawning a second agent session
/// for a repo while a prior session is still running.
///
/// The lockfile lives at `<data-dir>/sessions/<repo>.lock` and contains the
/// child process PID. Two signals count a lock as held:
/// - The PID in the file is alive (signal-0 check via `process_alive`).
/// - The file's mtime is within `ttl` of now.
///
/// Either condition failing (dead PID, or mtime older than TTL) causes the
/// lock to be treated as abandoned; the next spawn overwrites it. Explicit
/// release is available via `release` and is called both on clean exit
/// (reaper path) and on the stop-hook fast path (`record_session_end`).
pub struct SessionLockTracker {
    lock_dir: PathBuf,
    ttl: Duration,
}

impl SessionLockTracker {
    pub fn new(data_dir: &Path, ttl_secs: u64) -> Self {
        Self {
            lock_dir: data_dir.join("sessions"),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    fn lock_path(&self, repo: &str) -> PathBuf {
        self.lock_dir.join(format!("{}.lock", repo))
    }

    /// Path for the interactive-session lock file.
    ///
    /// Distinct from the TTL-gated `.lock` path used by watch-spawned children.
    /// Interactive sessions write their pid here; the gate is PID-liveness alone
    /// (no TTL) because an idle-but-open interactive session must still hold the
    /// gate -- a live process IS the authoritative signal.
    fn session_path(&self, repo: &str) -> PathBuf {
        self.lock_dir.join(format!("{}.session", repo))
    }

    /// Returns the holding PID when any live session (watch-spawned or interactive)
    /// holds the gate. Returns `None` when no live holder exists in either lock
    /// file; callers can safely spawn in all `None` cases.
    ///
    /// Two independent sources are checked:
    ///
    /// 1. `<repo>.lock` -- watch-spawned children. Held when BOTH mtime is within
    ///    `ttl` AND the PID is alive. The TTL guards against PID-reuse on
    ///    un-released locks from crashed spawns.
    ///
    /// 2. `<repo>.session` -- interactive (human-started) sessions. Held when
    ///    the PID is alive, regardless of mtime. No TTL is applied because a
    ///    live interactive session may be idle for hours and must still gate.
    ///    When the PID is dead the stale file is deleted opportunistically.
    ///
    /// The `.lock` holder is preferred when both sources report a live pid.
    /// The existing `.lock` mtime-TTL semantics are unchanged.
    pub fn active_pid(&self, repo: &str) -> Option<u32> {
        // -- .lock path (watch-spawned; TTL-gated) ----------------------------
        let lock_pid: Option<u32> = (|| {
            let path = self.lock_path(repo);
            let meta = std::fs::metadata(&path).ok()?;
            let mtime = meta.modified().ok()?;
            let age = mtime.elapsed().unwrap_or(Duration::ZERO);
            if age > self.ttl {
                return None;
            }
            let contents = std::fs::read_to_string(&path).ok()?;
            let pid = contents.trim().parse::<u32>().ok()?;
            if process_alive(pid) { Some(pid) } else { None }
        })();

        // -- .session path (interactive; PID-liveness only) -------------------
        let session_pid: Option<u32> = (|| {
            let path = self.session_path(repo);
            let contents = std::fs::read_to_string(&path).ok()?;
            let pid = contents.trim().parse::<u32>().ok()?;
            if process_alive(pid) {
                Some(pid)
            } else {
                // Opportunistically clean up the stale file so it does not
                // accumulate after the interactive session exits.
                let _ = std::fs::remove_file(&path);
                None
            }
        })();

        // Prefer .lock if both are held; fall back to .session.
        lock_pid.or(session_pid)
    }

    /// Write or overwrite the lockfile for `repo` with `pid`. Creates the
    /// parent directory if needed. Caller is responsible for having checked
    /// `active_pid` first if they want to respect an existing lock.
    pub fn record_spawn(&self, repo: &str, pid: u32) -> Result<()> {
        std::fs::create_dir_all(&self.lock_dir)?;
        std::fs::write(self.lock_path(repo), pid.to_string())?;
        Ok(())
    }

    /// Write or overwrite the interactive-session lock for `repo` with `pid`.
    ///
    /// Called from the `legion watch session-start` subcommand, which the
    /// SessionStart hook invokes with `$PPID` (the Claude session process).
    /// Unlike `record_spawn`, there is no TTL -- `active_pid` checks the
    /// written PID for liveness directly, so the lock is released only when
    /// the process exits (or when `release_interactive` is called explicitly).
    pub fn record_interactive(&self, repo: &str, pid: u32) -> Result<()> {
        std::fs::create_dir_all(&self.lock_dir)?;
        std::fs::write(self.session_path(repo), pid.to_string())?;
        Ok(())
    }

    /// Delete the interactive-session lock for `repo`.
    ///
    /// Idempotent: a missing file is not an error. Removes only the
    /// `.session` file; the `.lock` file (watch-spawned) is untouched.
    ///
    /// Available for explicit cleanup; the passive path is `active_pid`
    /// deleting stale files opportunistically when it reads a dead PID.
    #[allow(dead_code)] // public API completion -- no production call site yet
    pub fn release_interactive(&self, repo: &str) -> Result<()> {
        let path = self.session_path(repo);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(LegionError::Io(e)),
        }
    }

    /// Delete the lockfile for `repo`, freeing the per-repo wake gate so
    /// the next poll cycle can spawn a new agent session immediately rather
    /// than waiting for the TTL to expire.
    ///
    /// Idempotent: a missing lockfile is not an error. Called from two sites:
    /// - `reap_finished` when the tracked child has exited or its
    ///   `exit_observed_at` is set (stop-hook fired) -- authoritative path.
    /// - `record_session_end` immediately on the stop-hook fast path so the
    ///   gate clears before the next poll cycle.
    pub fn release(&self, repo: &str) -> Result<()> {
        let path = self.lock_path(repo);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(LegionError::Io(e)),
        }
    }
}

// -- Cooldown ----------------------------------------------------------------

/// Tracks per-repo cooldown to prevent wake storms.
pub struct CooldownTracker {
    last_wake: HashMap<String, Instant>,
    cooldown: Duration,
    work_hours_start: Option<u8>,
    work_hours_end: Option<u8>,
}

impl CooldownTracker {
    pub fn new(
        cooldown_secs: u64,
        work_hours_start: Option<u8>,
        work_hours_end: Option<u8>,
    ) -> Self {
        Self {
            last_wake: HashMap::new(),
            cooldown: Duration::from_secs(cooldown_secs),
            work_hours_start,
            work_hours_end,
        }
    }

    /// Check whether we are in work hours (no cooldown applies).
    fn is_work_hours(&self) -> bool {
        if let (Some(start), Some(end)) = (self.work_hours_start, self.work_hours_end) {
            let hour = chrono::Local::now().hour() as u8;
            if start <= end {
                hour >= start && hour < end
            } else {
                // Overnight range (e.g., 22-06)
                hour >= start || hour < end
            }
        } else {
            false
        }
    }

    /// Check whether the repo is on cooldown. Returns true if we should skip.
    /// During work hours, cooldown is disabled.
    pub fn is_cooling_down(&self, repo: &str) -> bool {
        if self.is_work_hours() {
            return false;
        }
        self.last_wake
            .get(repo)
            .is_some_and(|t| t.elapsed() < self.cooldown)
    }

    /// Record that a repo was just woken.
    pub fn record_wake(&mut self, repo: &str) {
        self.last_wake.insert(repo.to_string(), Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_tracker_prevents_rapid_wake() {
        let mut tracker = CooldownTracker::new(300, None, None);
        assert!(!tracker.is_cooling_down("rafters"));

        tracker.record_wake("rafters");
        assert!(tracker.is_cooling_down("rafters"));
        assert!(!tracker.is_cooling_down("legion"));
    }

    /// Run a short-lived child to completion and return its (now-dead) PID.
    /// Used to test the dead-PID branch of `SessionLockTracker::active_pid`
    /// without racing against PID reuse on the test host.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let pid = child.id();
        let _ = child.wait();
        pid
    }

    // These tests depend on `process_alive` returning true for our own PID,
    // which is currently Unix-only (see `process_alive` at the top of this
    // file). Gating them keeps Windows CI green while the session lock gate
    // degrades to "always allow spawn" on Windows -- a known pre-existing
    // limitation of the PID-lock code, not a regression from this change.
    // Windows support is tracked separately.
    #[cfg(unix)]
    #[test]
    fn session_lock_active_for_fresh_live_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks
            .record_spawn("legion", std::process::id())
            .expect("record");
        assert!(
            locks.active_pid("legion").is_some(),
            "own PID + fresh mtime should read as active"
        );
    }

    #[test]
    fn session_lock_inactive_for_dead_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks.record_spawn("legion", dead_pid()).expect("record");
        assert!(
            locks.active_pid("legion").is_none(),
            "dead PID should be treated as abandoned"
        );
    }

    #[test]
    fn session_lock_inactive_when_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        // TTL=1s + sleep > TTL proves the stale-mtime branch fires for a
        // non-zero TTL -- distinguishing it from the TTL=0 disable sentinel.
        let locks = SessionLockTracker::new(dir.path(), 1);
        locks
            .record_spawn("legion", std::process::id())
            .expect("record");
        std::thread::sleep(Duration::from_millis(1_100));
        assert!(
            locks.active_pid("legion").is_none(),
            "stale mtime should be treated as abandoned even with live PID"
        );
    }

    // See the cfg(unix) note on `session_lock_active_for_fresh_live_pid`:
    // the overwrite assertion relies on our own PID reading as alive.
    #[cfg(unix)]
    #[test]
    fn session_lock_record_spawn_overwrites_abandoned_lock() {
        // Acceptance criterion 5 from issue #274: a dead-PID / stale-mtime
        // lock must be overwritten on the next spawn and the fresh lock then
        // read as active. Proves the abandon-and-replace path end-to-end.
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);

        // Seed with an abandoned (dead-PID) lock.
        locks.record_spawn("legion", dead_pid()).expect("seed");
        assert!(
            locks.active_pid("legion").is_none(),
            "precondition: seeded lock must read as abandoned"
        );

        // Re-record with our own (live) PID, simulating a successful respawn.
        locks
            .record_spawn("legion", std::process::id())
            .expect("overwrite");
        assert_eq!(
            locks.active_pid("legion"),
            Some(std::process::id()),
            "overwrite must leave a live lock holding the new PID"
        );
    }

    #[test]
    fn session_lock_inactive_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        assert!(
            locks.active_pid("legion").is_none(),
            "missing lockfile should read as inactive"
        );
    }

    // -- SessionLockTracker::release tests ------------------------------------

    #[test]
    fn session_lock_release_clears_active_pid() {
        // After record_spawn, active_pid returns Some. After release it returns
        // None -- the gate is open again for the next spawn.
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks
            .record_spawn("legion", std::process::id())
            .expect("record spawn");
        // Pre-condition: lock is active (our own live PID).
        #[cfg(unix)]
        assert!(
            locks.active_pid("legion").is_some(),
            "precondition: lock must be active after record_spawn"
        );
        locks.release("legion").expect("release");
        assert!(
            locks.active_pid("legion").is_none(),
            "active_pid must return None after release"
        );
    }

    #[test]
    fn session_lock_release_of_missing_lock_is_ok() {
        // release on a repo that was never locked (or already released) is idempotent.
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        // No record_spawn called -- lockfile does not exist.
        let result = locks.release("nonexistent-repo");
        assert!(
            result.is_ok(),
            "release of a missing lockfile must return Ok, got {:?}",
            result
        );
    }

    // -- record_interactive / release_interactive / active_pid (.session) ----

    /// A .session file written with the test process's (live) PID makes
    /// active_pid return Some, even with no .lock file present.
    #[cfg(unix)]
    #[test]
    fn interactive_lock_active_for_live_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks
            .record_interactive("myrepo", std::process::id())
            .expect("record_interactive");
        let held = locks.active_pid("myrepo");
        assert!(
            held.is_some(),
            "a .session with a live pid must make active_pid return Some"
        );
        assert_eq!(
            held,
            Some(std::process::id()),
            "active_pid must return the pid from the .session file"
        );
    }

    /// A .session file with a dead PID returns None.
    #[test]
    fn interactive_lock_inactive_for_dead_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks
            .record_interactive("myrepo", dead_pid())
            .expect("record_interactive");
        assert!(
            locks.active_pid("myrepo").is_none(),
            "a .session with a dead pid must return None"
        );
    }

    /// A .session with a live PID gates even when mtime is not fresh (no TTL
    /// applied). This is the key property: an idle interactive session must
    /// still hold the gate regardless of how old the file is.
    /// We cannot easily age the mtime in a test, so we assert that a freshly
    /// written .session with a live pid -- and no .lock file present -- gates.
    /// The absence of a TTL check in active_pid for the .session path is the
    /// structural guarantee; this test confirms the live-pid gate works.
    #[cfg(unix)]
    #[test]
    fn interactive_lock_gates_without_lock_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        // TTL=1s so any .lock would expire quickly; .session must still hold.
        let locks = SessionLockTracker::new(dir.path(), 1);
        locks
            .record_interactive("myrepo", std::process::id())
            .expect("record_interactive");
        // No .lock file written; gate must still be held via .session alone.
        assert!(
            locks.active_pid("myrepo").is_some(),
            "live .session with no .lock must still gate (no TTL on .session)"
        );
    }

    /// release_interactive removes only the .session file; the .lock is untouched.
    #[cfg(unix)]
    #[test]
    fn interactive_lock_release_removes_only_session_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        // Write both a .lock and a .session for the same repo.
        locks
            .record_spawn("myrepo", std::process::id())
            .expect("record_spawn");
        locks
            .record_interactive("myrepo", std::process::id())
            .expect("record_interactive");
        assert!(
            locks.active_pid("myrepo").is_some(),
            "precondition: both locks active"
        );

        // release_interactive only.
        locks
            .release_interactive("myrepo")
            .expect("release_interactive");

        // The .lock must still hold the gate.
        assert!(
            locks.active_pid("myrepo").is_some(),
            ".lock must still gate after release_interactive"
        );

        // The .session file must be gone.
        assert!(
            !locks.session_path("myrepo").exists(),
            ".session file must be deleted by release_interactive"
        );
    }

    /// release_interactive on a missing .session file is idempotent.
    #[test]
    fn interactive_lock_release_of_missing_session_is_ok() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        let result = locks.release_interactive("nonexistent-repo");
        assert!(
            result.is_ok(),
            "release_interactive on missing .session must return Ok, got {:?}",
            result
        );
    }

    /// Coexistence: a live .lock and no .session still gates (existing behavior
    /// preserved by the refactored active_pid).
    #[cfg(unix)]
    #[test]
    fn lock_file_alone_still_gates_after_active_pid_refactor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let locks = SessionLockTracker::new(dir.path(), 3600);
        locks
            .record_spawn("myrepo", std::process::id())
            .expect("record_spawn");
        assert!(
            locks.active_pid("myrepo").is_some(),
            "a live .lock with no .session must still gate (existing semantics preserved)"
        );
    }

    /// The single liveness probe shared by watch locks and the daemon
    /// pidfile machinery: our own PID reads as alive, a reaped child's PID
    /// reads as dead.
    #[cfg(unix)]
    #[test]
    fn process_alive_distinguishes_live_and_dead_pids() {
        assert!(
            process_alive(std::process::id()),
            "our own PID must read as alive"
        );
        assert!(
            !process_alive(dead_pid()),
            "a reaped child's PID must read as dead"
        );
    }

    #[test]
    fn pid_lock_acquire_and_release() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("test.pid");

        acquire_pid_lock(&lock_path).expect("acquire lock");
        assert!(lock_path.exists());

        release_pid_lock(&lock_path);
        assert!(!lock_path.exists());
    }

    #[test]
    fn pid_lock_detects_stale_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("test.pid");

        // Write a fake PID that is very unlikely to be running
        std::fs::write(&lock_path, "999999999").expect("write stale lock");

        // Should succeed because the process is not running
        acquire_pid_lock(&lock_path).expect("acquire lock over stale");
    }
}
