//! PTY wrapper for the watch auto-wake migration (#486, part of #495).
//!
//! Wraps `portable-pty` for safe spawn / write-keystrokes / drain-output /
//! kill / wait. A dedicated reader thread drains the master fd into a
//! bounded ring buffer so a chatty child cannot block on a full pipe; the
//! buffer drops oldest bytes on overflow because the harness only needs
//! EOF detection plus occasional diagnostic snapshots, not the full
//! transcript.
//!
//! Pure abstraction -- knows nothing about Claude, the legion plugin, or
//! the wake-attempts FSM. Issue #489 consumes this from
//! `watch::spawn_agent` to launch `claude` interactively.
//!
//! Until #489 wires this in, the module is dead from main's perspective;
//! `#![allow(dead_code)]` keeps clippy quiet during the soak window.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::error::{LegionError, Result};

const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;
const DEFAULT_RING_BYTES: usize = 64 * 1024;
const READ_CHUNK_BYTES: usize = 4 * 1024;
/// Bound for [`PtySession::output_tail_lossy`]'s RAW INPUT slice (#1001) --
/// this caps how much of [`PtySession::output_tail`]'s bytes are read
/// before escaping, NOT the length of the escaped string that comes out.
/// The ring buffer itself caps at `DEFAULT_RING_BYTES` (64 KiB) of raw,
/// unrotated terminal output including every ANSI escape/cursor-movement
/// byte; a caller logging that much on every event (e.g. a courier's
/// stderr line to an unrotated daemon log) would grow the log without
/// bound. 2 KiB of raw input is enough to carry a handful of rendered
/// lines -- the evidence a live verification run actually cites -- without
/// that cost. Because `char::escape_debug` can expand one control byte
/// (e.g. ESC, `0x1b`) into up to six output chars (`\u{1b}`), the escaped
/// string this bounds the INPUT of can be up to `OUTPUT_TAIL_LOSSY_BYTES *
/// 6` long in the worst case (an input that is nothing but control bytes)
/// -- see [`PtySession::output_tail_lossy`]'s doc. No further output-side
/// cap is applied: a daemon log line up to ~12 KiB in that adversarial
/// worst case is judged an acceptable bound, and truncating the ALREADY-
/// escaped string risks cutting a multi-char escape sequence in half.
const OUTPUT_TAIL_LOSSY_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone)]
pub struct PtySpawnOptions<'a> {
    pub bin: &'a str,
    pub args: &'a [&'a str],
    pub cwd: &'a Path,
    /// Extra env to set on the child (appended to inherited env).
    pub env: &'a [(&'a str, &'a str)],
    pub cols: u16,
    pub rows: u16,
    /// Cap for the diagnostic ring buffer. Bytes beyond this are dropped
    /// from the head as new bytes arrive.
    pub ring_buffer_bytes: usize,
}

impl<'a> PtySpawnOptions<'a> {
    /// Build a minimal options struct with defaults for size and ring.
    pub fn new(bin: &'a str, args: &'a [&'a str], cwd: &'a Path) -> Self {
        Self {
            bin,
            args,
            cwd,
            env: &[],
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            ring_buffer_bytes: DEFAULT_RING_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitStatus {
    pub success: bool,
    pub exit_code: Option<i32>,
    /// Signal number when killed by signal. `portable-pty` does not
    /// surface signal info directly; populated only when we can derive
    /// it from a kill path. `None` means either a normal exit or that
    /// the underlying platform did not report a signal.
    pub signal: Option<i32>,
}

/// Live PTY session. Holds the master writer, child handle, and a
/// background reader thread that drains the master into the ring buffer.
///
/// `Drop` is best-effort: kills the child if still running and joins the
/// reader thread. A panic in the reader thread does not poison the ring
/// buffer (we log and exit the thread; subsequent `output_tail()` returns
/// whatever was captured up to the panic).
pub struct PtySession {
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    /// Hold the master so the slave's exit is the only thing that
    /// closes the channel. Dropping the master before the child exits
    /// causes EIO on the child's stdin and can race with the reader.
    _master: Box<dyn MasterPty + Send>,
    pid: u32,
    ring: Arc<Mutex<RingBuffer>>,
    reader: Option<JoinHandle<()>>,
    eof_observed: Arc<AtomicBool>,
}

struct RingBuffer {
    buf: VecDeque<u8>,
    cap: usize,
}

impl RingBuffer {
    fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap.min(64 * 1024)),
            cap,
        }
    }

    fn extend(&mut self, bytes: &[u8]) {
        // If the incoming chunk is larger than the cap, only the tail
        // matters; drop everything currently buffered and keep the last
        // `cap` bytes of `bytes`.
        if bytes.len() >= self.cap {
            self.buf.clear();
            let start = bytes.len() - self.cap;
            self.buf.extend(bytes[start..].iter().copied());
            return;
        }
        self.buf.extend(bytes.iter().copied());
        while self.buf.len() > self.cap {
            self.buf.pop_front();
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }
}

impl PtySession {
    pub fn spawn(opts: PtySpawnOptions<'_>) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: opts.rows.max(1),
                cols: opts.cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| LegionError::PtyAllocFailed(e.to_string()))?;

        let mut cmd = CommandBuilder::new(opts.bin);
        if !opts.args.is_empty() {
            cmd.args(opts.args);
        }
        cmd.cwd(opts.cwd);
        for (k, v) in opts.env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| LegionError::PtySpawnFailed {
                bin: opts.bin.to_string(),
                source: Box::new(std::io::Error::other(e.to_string())),
            })?;
        // The slave half is only needed to spawn; dropping it lets the
        // child's stdio see EOF when the master is later closed.
        drop(pair.slave);

        let pid = child.process_id().unwrap_or(0);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| LegionError::PtyAllocFailed(format!("clone_reader: {}", e)))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| LegionError::PtyAllocFailed(format!("take_writer: {}", e)))?;

        let ring_cap = opts.ring_buffer_bytes.max(1);
        let ring = Arc::new(Mutex::new(RingBuffer::new(ring_cap)));
        let eof_observed = Arc::new(AtomicBool::new(false));

        let reader_handle =
            spawn_reader_thread(reader, Arc::clone(&ring), Arc::clone(&eof_observed));

        Ok(Self {
            child,
            writer,
            _master: pair.master,
            pid,
            ring,
            reader: Some(reader_handle),
            eof_observed,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer
            .write_all(bytes)
            .and_then(|_| self.writer.flush())
            .map_err(|e| LegionError::PtyWriteFailed(e.to_string()))
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// EOF on the master fd, observed by the reader thread. Useful as
    /// the first indication that the child is on its way out -- the OS
    /// reap may lag this signal by a poll tick. Callers gate on
    /// `try_wait` for the authoritative completion.
    pub fn eof_observed(&self) -> bool {
        self.eof_observed.load(Ordering::SeqCst)
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        match self.child.try_wait() {
            Ok(Some(status)) => Ok(Some(into_exit_status(status))),
            Ok(None) => Ok(None),
            Err(e) => Err(LegionError::PtyWaitFailed(e.to_string())),
        }
    }

    /// Send SIGKILL (or platform equivalent) to the child. Idempotent
    /// against an already-exited child on every platform `portable-pty`
    /// supports; the underlying error is surfaced rather than swallowed
    /// so callers can distinguish "kill landed" from "kill itself failed
    /// (permissions, ESRCH on a reaped pid we did not observe, etc)."
    pub fn kill(&mut self) -> Result<()> {
        self.child
            .kill()
            .map_err(|e| LegionError::PtyWaitFailed(format!("kill: {}", e)))
    }

    pub fn output_tail(&self) -> Vec<u8> {
        match self.ring.lock() {
            Ok(g) => g.snapshot(),
            Err(poisoned) => poisoned.into_inner().snapshot(),
        }
    }

    /// A short, log-safe rendering of the ring buffer's tail (#1001): reads
    /// at most the last [`OUTPUT_TAIL_LOSSY_BYTES`] bytes of
    /// [`Self::output_tail`] (that bound is on the RAW INPUT only -- see
    /// its doc), lossily decoded to UTF-8, with every control character
    /// other than `\n` escaped (`char::escape_debug`) so raw ANSI/cursor-
    /// movement bytes cannot turn a log line into an unreadable terminal-
    /// redraw dump. The escaped OUTPUT string is not itself length-capped,
    /// so it can be up to `OUTPUT_TAIL_LOSSY_BYTES * 6` long in the worst
    /// case where the input is nothing but escapable control bytes (each
    /// can expand to up to 6 chars, e.g. ESC becomes `\u{1b}`) -- typical
    /// terminal output is mostly printable text and escapes far less than
    /// that in practice. For diagnostics only -- callers that need
    /// turn-start control flow still read the raw bytes via
    /// [`Self::saw_turn_start`].
    pub fn output_tail_lossy(&self) -> String {
        lossy_tail(&self.output_tail())
    }

    /// Has a Claude turn started in this session's output?
    ///
    /// The confirmed-submit protocol (#649) uses this to decide when to
    /// STOP re-sending the submit keystroke: once a turn is running, the
    /// prompt landed and further Enters would queue stray empty turns.
    ///
    /// The signal is the live token counter -- Claude renders `N tokens`
    /// in the working spinner only while a turn is processing, never on
    /// the idle input screen. This was chosen over the session-transcript
    /// file (the original #649 plan) because that file is buffered and
    /// written on clean exit, not incrementally -- empirically it never
    /// appears for a live or killed wake (oracle2, 2026-06-13), so it
    /// cannot confirm a turn-start. Scanning the ring buffer crosses the
    /// "diagnostics, not control flow" line deliberately: turn-start
    /// detection is the one sanctioned control-flow read.
    ///
    /// The marker is TUI-version-sensitive (the older `esc to interrupt`
    /// string vanished by 2.1.176), but a miss fails CLOSED -- the wake
    /// ends in `Spawning -> Failed` with a visible reason rather than a
    /// silent hang.
    pub fn saw_turn_start(&self) -> bool {
        has_turn_start_marker(&self.output_tail())
    }
}

/// Detect the live token counter Claude renders only while a turn is
/// processing (#649). The counter is always `<digits> tokens` (e.g.
/// `103 tokens`), so the scan requires a digit immediately before
/// ` tokens` -- a bare `tokens` is NOT enough.
///
/// This precision is load-bearing, not cosmetic: the wake prompt itself
/// contains the prose `waste tokens` (it warns against empty acks), and
/// the TUI echoes the pasted prompt into the input box. A bare-`tokens`
/// match would fire on that echo BEFORE any submit, confirming a turn
/// that never started and hanging the wake. Requiring the leading digit
/// separates the counter (`1 tokens`) from the prose (`waste tokens`).
///
/// Pulled out as a free function so the scan is unit-testable without a
/// live PTY.
fn has_turn_start_marker(bytes: &[u8]) -> bool {
    const NEEDLE: &[u8] = b" tokens";
    // For each ` tokens` occurrence, require the byte before the space to
    // be an ASCII digit -- the counter's `<N> tokens` shape.
    bytes
        .windows(NEEDLE.len())
        .enumerate()
        .any(|(i, w)| w == NEEDLE && i > 0 && bytes[i - 1].is_ascii_digit())
}

/// Truncate-and-escape logic behind [`PtySession::output_tail_lossy`] (#1001).
/// Pulled out as a free function, same convention as [`has_turn_start_marker`]
/// above, so the truncation/escaping rule is unit-testable without a live PTY.
fn lossy_tail(raw: &[u8]) -> String {
    let start = raw.len().saturating_sub(OUTPUT_TAIL_LOSSY_BYTES);
    // No capacity hint: escape_debug can expand a control byte into up to
    // six output chars (see OUTPUT_TAIL_LOSSY_BYTES's doc), so
    // `with_capacity(OUTPUT_TAIL_LOSSY_BYTES)` would undersize the common
    // case where the input needs escaping at all -- String grows as
    // needed regardless, so a wrong guess only costs a reallocation, not
    // correctness, but an accurate guess isn't cheap to compute either.
    let mut out = String::new();
    for c in String::from_utf8_lossy(&raw[start..]).chars() {
        if c == '\n' || !c.is_control() {
            out.push(c);
        } else {
            out.extend(c.escape_debug());
        }
    }
    out
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Best-effort: if the child is anything other than confirmed
        // exited (`Some(_)`), kill it. That covers both still-running
        // (Ok(None)) and try_wait errors (Err) -- treating Err as "do
        // not kill" would leak the child when the wait path itself is
        // the thing broken. Errors here are swallowed because Drop
        // cannot return anything useful and the OS reaps regardless.
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(handle) = self.reader.take() {
            // The reader exits on EOF (which a killed child produces).
            // Joining here keeps tests deterministic; in production a
            // stuck reader would block Drop, which is acceptable -- the
            // alternative is a leaked thread.
            let _ = handle.join();
        }
    }
}

fn spawn_reader_thread(
    mut reader: Box<dyn Read + Send>,
    ring: Arc<Mutex<RingBuffer>>,
    eof_observed: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    eof_observed.store(true, Ordering::SeqCst);
                    return;
                }
                Ok(n) => {
                    // Lock-poisoning here means a writer to the ring
                    // panicked while holding the lock. Recover and keep
                    // draining -- losing transcript bytes is better than
                    // leaking the thread.
                    let mut guard = match ring.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    guard.extend(&chunk[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    // I/O failures on the master fd are usually the
                    // child closing stdout, but a genuine error here is
                    // a silent failure in disguise -- log it so a hung
                    // wake post-mortem has a breadcrumb.
                    eprintln!("[legion pty] reader thread error: {}", e);
                    eof_observed.store(true, Ordering::SeqCst);
                    return;
                }
            }
        }
    })
}

fn into_exit_status(status: portable_pty::ExitStatus) -> ExitStatus {
    let exit_code = status.exit_code();
    ExitStatus {
        success: status.success(),
        exit_code: Some(exit_code as i32),
        signal: None,
    }
}

// Tests exercise `bash -c "..."` plus Unix PTY semantics (master fd
// EIO on slave exit, signal-based kill, /bin/kill -0 for liveness).
// The wrapper itself is cross-platform via portable-pty's ConPTY
// backing, but the test harness leans on Unix-only behavior; Windows
// CI was running Git Bash under ConPTY and hanging in `sleep 30` /
// kill paths (over 25 min run before timeout). Issue #486 follow-up
// tracks restoring Windows coverage with PowerShell-shimmed children.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn turn_start_marker_detects_token_counter_only() {
        // The live token counter (<digits> tokens) is the turn-start signal;
        // the idle input screen never renders it, so a miss keeps the retry
        // loop going.
        assert!(has_turn_start_marker(b"Transmuting... (4s | 103 tokens)"));
        assert!(has_turn_start_marker(
            b"running stop hooks... 0/3 | 1 tokens"
        ));
        assert!(!has_turn_start_marker(
            b"? for shortcuts   accept edits on (shift+tab to cycle)"
        ));
        assert!(!has_turn_start_marker(b""));
    }

    #[test]
    fn turn_start_marker_ignores_prose_tokens_in_echoed_prompt() {
        // The wake prompt warns that empty acks "waste tokens"; the TUI
        // echoes the pasted prompt into the input box. A bare-`tokens`
        // match would false-confirm on that echo before any submit. The
        // leading-digit requirement must reject prose.
        assert!(!has_turn_start_marker(
            b"empty acknowledgments waste tokens and trigger wake storms"
        ));
        assert!(!has_turn_start_marker(b"tokens"));
        assert!(!has_turn_start_marker(b" tokens"));
    }

    // -- lossy_tail (#1001) --------------------------------------------------

    #[test]
    fn lossy_tail_escapes_control_bytes_but_keeps_newlines() {
        // A raw ANSI cursor-movement/color sequence must not survive into a
        // log line unescaped -- ESC (0x1b) is a control byte -- while an
        // ordinary newline stays literal so multi-line evidence (e.g. a
        // courier's delivery line) still reads as separate lines.
        let raw = b"before\x1b[31mred\x1b[0m\nafter";
        let out = lossy_tail(raw);
        assert!(!out.contains('\u{1b}'), "raw ESC bytes must not survive");
        assert!(out.contains("\\u{1b}"), "ESC must appear escaped: {out:?}");
        assert!(
            out.contains('\n'),
            "a literal newline must be preserved, not escaped: {out:?}"
        );
        assert!(out.contains("before") && out.contains("after"));
    }

    #[test]
    fn lossy_tail_caps_raw_input_and_bounds_worst_case_escaped_output() {
        // Plain printable bytes never escape, so they would never actually
        // exercise the OUTPUT-side bound documented on
        // OUTPUT_TAIL_LOSSY_BYTES -- feed ESC (0x1b) instead, the worst
        // case for escape_debug's expansion (six output chars per input
        // byte, e.g. `\u{1b}`), to prove the escaped string stays within
        // the documented `OUTPUT_TAIL_LOSSY_BYTES * 6` worst-case bound
        // rather than being length-capped itself (it is not -- only the
        // RAW INPUT slice taken from output_tail is capped).
        let raw = vec![0x1bu8; OUTPUT_TAIL_LOSSY_BYTES * 4];
        let out = lossy_tail(&raw);
        assert!(
            out.len() <= OUTPUT_TAIL_LOSSY_BYTES * 6,
            "escaped output must stay within the documented worst-case 6x \
             bound on the raw-input cap; got {} bytes",
            out.len()
        );
    }

    #[test]
    fn lossy_tail_keeps_only_the_tail_of_a_longer_buffer() {
        let mut raw = vec![b'a'; OUTPUT_TAIL_LOSSY_BYTES];
        raw.extend_from_slice(b"THE-TAIL-MARKER");
        let out = lossy_tail(&raw);
        assert!(
            out.ends_with("THE-TAIL-MARKER"),
            "must keep the END of the buffer, not the start: {out:?}"
        );
    }

    fn run_until<F: FnMut() -> bool>(deadline_ms: u64, mut f: F) -> bool {
        let deadline = Instant::now() + Duration::from_millis(deadline_ms);
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        f()
    }

    fn wait_for_exit(session: &mut PtySession, deadline_ms: u64) -> Option<ExitStatus> {
        let deadline = Instant::now() + Duration::from_millis(deadline_ms);
        loop {
            match session.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return None,
            }
        }
    }

    fn tmp_cwd() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn spawn_echo_returns_exit_zero_with_output() {
        let cwd = tmp_cwd();
        let opts = PtySpawnOptions::new("bash", &["-c", "echo foo; exit 0"], &cwd);
        let mut session = PtySession::spawn(opts).expect("spawn");
        let status = wait_for_exit(&mut session, 5_000).expect("child exits within deadline");
        assert!(status.success, "exit 0 must be success");
        assert_eq!(status.exit_code, Some(0));
        // Drain stragglers from the reader.
        run_until(500, || !session.output_tail().is_empty());
        let out = String::from_utf8_lossy(&session.output_tail()).to_string();
        assert!(
            out.contains("foo"),
            "ring buffer should contain echoed output, got: {:?}",
            out
        );
    }

    #[test]
    fn spawn_nonzero_exit_propagates() {
        let cwd = tmp_cwd();
        let opts = PtySpawnOptions::new("bash", &["-c", "exit 7"], &cwd);
        let mut session = PtySession::spawn(opts).expect("spawn");
        let status = wait_for_exit(&mut session, 5_000).expect("child exits within deadline");
        assert!(!status.success);
        assert_eq!(status.exit_code, Some(7));
    }

    #[test]
    fn spawn_missing_binary_returns_typed_error() {
        let cwd = tmp_cwd();
        let opts = PtySpawnOptions::new("/nonexistent/legion-pty-test-binary-xyz", &[], &cwd);
        match PtySession::spawn(opts) {
            Ok(_) => panic!("expected PtySpawnFailed for missing binary"),
            Err(LegionError::PtySpawnFailed { bin, .. }) => {
                assert!(bin.contains("legion-pty-test-binary-xyz"));
            }
            Err(other) => panic!("expected PtySpawnFailed, got {other:?}"),
        }
    }

    #[test]
    fn write_to_cat_reflects_echo_in_output() {
        let cwd = tmp_cwd();
        // `cat` echoes stdin to stdout. The PTY is in cooked mode by
        // default, so we expect both the typed input and the echo to
        // appear in the ring buffer.
        let opts = PtySpawnOptions::new("bash", &["-c", "cat"], &cwd);
        let mut session = PtySession::spawn(opts).expect("spawn");
        session.write(b"hello-pty\n").expect("write");
        let saw_echo = run_until(2_000, || {
            let out = String::from_utf8_lossy(&session.output_tail()).to_string();
            out.contains("hello-pty")
        });
        assert!(
            saw_echo,
            "cat should echo the written bytes back; got {:?}",
            String::from_utf8_lossy(&session.output_tail())
        );
        session.kill().expect("kill");
    }

    #[test]
    fn ring_buffer_caps_chatty_child_without_blocking() {
        let cwd = tmp_cwd();
        let mut opts = PtySpawnOptions::new(
            "bash",
            // Emit ~256KB of output; cap is 8KB.
            &["-c", "yes 0123456789ABCDEF | head -c 262144"],
            &cwd,
        );
        opts.ring_buffer_bytes = 8 * 1024;
        let mut session = PtySession::spawn(opts).expect("spawn");
        let status = wait_for_exit(&mut session, 10_000)
            .expect("chatty child must exit; if this hangs the pipe back-pressured the harness");
        assert!(status.success);
        let tail = session.output_tail();
        assert!(
            tail.len() <= 8 * 1024,
            "ring buffer must cap at ring_buffer_bytes; got {} bytes",
            tail.len()
        );
    }

    #[test]
    fn kill_terminates_long_lived_child_and_reader_drains() {
        let cwd = tmp_cwd();
        let opts = PtySpawnOptions::new("bash", &["-c", "sleep 30"], &cwd);
        let mut session = PtySession::spawn(opts).expect("spawn");
        assert!(session.pid() > 0);
        session.kill().expect("kill");
        let status = wait_for_exit(&mut session, 3_000).expect("kill must surface as exit");
        // Killed processes are not successful and report no clean exit code.
        assert!(!status.success);
    }

    #[test]
    fn drop_cleans_up_running_child_without_zombie() {
        let cwd = tmp_cwd();
        let opts = PtySpawnOptions::new("bash", &["-c", "sleep 30"], &cwd);
        let session = PtySession::spawn(opts).expect("spawn");
        let pid = session.pid();
        drop(session);
        // After Drop returns, the child must no longer exist. On Unix
        // we probe via kill -0; the call returns ESRCH for a reaped pid.
        #[cfg(unix)]
        {
            // Give the OS a beat to actually reap; Drop should already
            // have called wait() but signals propagate asynchronously.
            // `kill -0 <pid>` exits 0 if the process exists and is
            // signalable, non-zero otherwise. Shelling out keeps the
            // assertion off the unsafe-libc surface.
            std::thread::sleep(Duration::from_millis(200));
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(!alive, "PtySession Drop must reap the child (pid {})", pid);
        }
        #[cfg(not(unix))]
        {
            // No portable zombie check off-unix; just assert the pid
            // was set so the test is meaningful where it can be.
            assert!(pid > 0);
        }
    }

    #[test]
    fn spawn_honors_explicit_cwd() {
        // pwd should reflect the directory we asked for, not the
        // current working directory of the harness. This pins the
        // contract that #489 will rely on when spawning claude in a
        // watched repo's workdir.
        let tmp = tempfile::tempdir().expect("temp dir");
        let opts = PtySpawnOptions::new("bash", &["-c", "pwd"], tmp.path());
        let mut session = PtySession::spawn(opts).expect("spawn");
        wait_for_exit(&mut session, 5_000).expect("pwd exits");
        run_until(500, || !session.output_tail().is_empty());
        let out = String::from_utf8_lossy(&session.output_tail()).to_string();
        // Use canonical path because macOS resolves /var -> /private/var
        // for tempfile-issued paths.
        let expected = tmp
            .path()
            .canonicalize()
            .expect("canonicalize")
            .display()
            .to_string();
        assert!(
            out.contains(&expected),
            "child pwd should be the requested cwd; expected {:?}, got {:?}",
            expected,
            out
        );
    }

    #[test]
    fn spawn_passes_env_through_to_child() {
        let cwd = tmp_cwd();
        let env = [("LEGION_PTY_TEST", "marker-value-9f8e")];
        let mut opts = PtySpawnOptions::new("bash", &["-c", "echo $LEGION_PTY_TEST"], &cwd);
        opts.env = &env;
        let mut session = PtySession::spawn(opts).expect("spawn");
        wait_for_exit(&mut session, 5_000).expect("child exits");
        run_until(500, || {
            let s = String::from_utf8_lossy(&session.output_tail()).to_string();
            s.contains("marker-value-9f8e")
        });
        let out = String::from_utf8_lossy(&session.output_tail()).to_string();
        assert!(
            out.contains("marker-value-9f8e"),
            "child must see env vars passed via PtySpawnOptions; got {:?}",
            out
        );
    }

    #[test]
    fn eof_observed_flips_after_child_exits() {
        let cwd = tmp_cwd();
        let opts = PtySpawnOptions::new("bash", &["-c", "echo done; exit 0"], &cwd);
        let mut session = PtySession::spawn(opts).expect("spawn");
        wait_for_exit(&mut session, 5_000).expect("child exits");
        let flipped = run_until(1_000, || session.eof_observed());
        assert!(
            flipped,
            "reader thread must set eof_observed once the master sees EOF"
        );
    }

    #[test]
    fn write_after_child_exit_does_not_panic() {
        // The PTY write contract under load is platform-dependent:
        // macOS surfaces EIO once the slave exits and propagates as
        // PtyWriteFailed; Linux silently buffers into the master and
        // never errors until the master fd is closed. Both are
        // acceptable -- what we care about is that the harness never
        // panics on a closed PTY, regardless of OS, and that any error
        // that DOES come back is the typed `PtyWriteFailed` variant
        // (not a generic Io / Unknown surface).
        let cwd = tmp_cwd();
        let opts = PtySpawnOptions::new("bash", &["-c", "exit 0"], &cwd);
        let mut session = PtySession::spawn(opts).expect("spawn");
        wait_for_exit(&mut session, 5_000).expect("child exits");
        std::thread::sleep(Duration::from_millis(200));
        for _ in 0..16 {
            match session.write(b"after-exit\n") {
                Ok(()) => std::thread::sleep(Duration::from_millis(50)),
                Err(LegionError::PtyWriteFailed(_)) => {
                    // macOS-style closed-PTY surface. Contract met.
                    return;
                }
                Err(other) => panic!("unexpected error variant: {other:?}"),
            }
        }
        // Linux-style: every write succeeded. Also acceptable -- the
        // master fd buffer absorbed the writes. No panic = pass.
    }
}
