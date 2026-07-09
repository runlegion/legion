/// Legion daemon: three tokio tasks in one process.
///
/// 1. Channel (HTTP) -- SSE pub/sub + REST endpoints (/sse, /api/feed, /api/tasks, /api/post)
/// 2. Watch          -- signal polling + auto-wake (the existing watch.rs loop)
/// 3. MCP (optional) -- JSON-RPC 2.0 over stdio for Claude Code tool integration
///
/// The daemon starts all tasks and shuts down cleanly on SIGINT/SIGTERM.
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::channel;
use crate::error::{LegionError, Result};
use crate::mcp;
use crate::watch;

/// PID lock file name for the daemon process (separate from watch.pid).
const DAEMON_PID_FILE: &str = "daemon.pid";

/// Return the platform-appropriate log file path for the daemon.
///
/// - macOS: `~/Library/Logs/legion/daemon.log`
/// - Linux/other: `${XDG_STATE_HOME:-$HOME/.local/state}/legion/daemon.log`
pub fn daemon_log_path() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .map_err(|_| LegionError::NoHomeDir)?;
        Ok(home.join("Library/Logs/legion/daemon.log"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let state_home = if let Ok(d) = std::env::var("XDG_STATE_HOME") {
            PathBuf::from(d)
        } else {
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .map_err(|_| LegionError::NoHomeDir)?;
            home.join(".local/state")
        };
        Ok(state_home.join("legion/daemon.log"))
    }
}

/// Spawn the daemon in the background and exit immediately.
///
/// If a daemon is already running (valid PID in `data_dir/daemon.pid`), prints
/// `"legion daemon already running (pid N)"` to stderr and returns `Ok(())`.
/// Does not attempt a duplicate start.
///
/// When clean, forks the current binary with `daemon` subcommand arguments,
/// redirects stdout and stderr to the platform log file, writes the new PID
/// to `data_dir/daemon.pid`, and returns. The caller should exit 0 after this
/// returns -- this function does NOT call `std::process::exit` so the caller
/// retains control.
pub fn spawn_detached(data_dir: &Path, port: u16) -> Result<()> {
    let pid_path = data_dir.join(DAEMON_PID_FILE);

    // Check whether a daemon is already running.
    if pid_path.exists() {
        let contents = std::fs::read_to_string(&pid_path).unwrap_or_default();
        if let Ok(pid) = contents.trim().parse::<u32>() {
            if watch::process_alive(pid) {
                eprintln!("legion daemon already running (pid {pid})");
                return Ok(());
            }
            // Stale PID file -- process is gone, continue to start a new one.
            let _ = std::fs::remove_file(&pid_path);
        }
    }

    // Port preflight (#599). The pidfile check above already returned for a
    // live daemon we own, so if the port is bound now it is held by a FOREIGN
    // process: a stray `legion serve` (which shares this port and writes the
    // same daemon.pid), or an orphaned daemon the pidfile lost track of.
    // Spawning anyway forks a child that dies on bind ("Address already in
    // use") while we falsely report "daemon started (pid N)" and leave the
    // pidfile pointing at a corpse. Fail loud and name the holder instead.
    if !port_available(port) {
        let holders = port_listener_pids(port);
        let (who, hint) = match holders.first() {
            Some(pid) => (
                format!("pid {pid}"),
                format!("stop it first (e.g. `kill {pid}`), then retry"),
            ),
            None => (
                "another process".to_string(),
                "stop it first, then retry".to_string(),
            ),
        };
        return Err(LegionError::DaemonPortInUse(format!(
            "port {port} is already held by {who}; not starting a second daemon. \
             If this is a stray `legion serve` or an orphaned daemon, {hint}."
        )));
    }

    // Resolve the current binary path so the child runs the same binary.
    let binary = std::env::current_exe().map_err(LegionError::Io)?;

    // Ensure the log directory exists.
    let log_path = daemon_log_path()?;
    if let Some(log_dir) = log_path.parent() {
        std::fs::create_dir_all(log_dir)?;
    }

    // Open (or create+append) the log file for stdout+stderr redirection.
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    // Clone the file handle for stderr.
    let log_file_stderr = log_file.try_clone()?;

    let child = std::process::Command::new(&binary)
        .env("LEGION_DATA_DIR", data_dir)
        .args(["daemon", "--port", &port.to_string()])
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_file_stderr))
        .stdin(std::process::Stdio::null())
        .spawn()?;

    let child_pid = child.id();

    // Write the PID so future calls detect the running daemon.
    std::fs::write(&pid_path, child_pid.to_string())?;

    // Detach: forget the child so it is not waited on by this process.
    // On Unix, the child becomes an orphan adopted by init/launchd, which is
    // the intended behavior for a background daemon.
    // We deliberately call forget here rather than drop so the Child struct
    // is not dropped -- dropping a Child on some platforms sends a signal.
    std::mem::forget(child);

    eprintln!(
        "[legion] daemon started (pid {child_pid}), logging to {}",
        log_path.display()
    );

    Ok(())
}

/// Whether `port` can be bound right now (i.e. it is free).
///
/// Binds and immediately drops a listener on `0.0.0.0:port`. A momentary
/// TOCTOU gap exists between this probe and the daemon child's own bind, but it
/// converts the common "another process already owns the port" case from a
/// silent fork-then-die into a clear up-front error. The same race already
/// existed before this check, so it adds safety without removing any.
fn port_available(port: u16) -> bool {
    std::net::TcpListener::bind(("0.0.0.0", port)).is_ok()
}

/// Best-effort: the PID(s) listening on `port`, for naming the holder in the
/// port-conflict error. Unix-only via `lsof`; returns empty on other platforms
/// or when `lsof` is unavailable -- the caller degrades to a generic message.
pub(crate) fn port_listener_pids(port: u16) -> Vec<u32> {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
            .output();
        match output {
            Ok(out) => String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.trim().parse::<u32>().ok())
                .collect(),
            Err(_) => Vec::new(),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = port;
        Vec::new()
    }
}

/// How long to wait for a SIGTERM'd daemon to exit before escalating to SIGKILL.
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(3);
/// How long to wait for a SIGKILL'd process to be reaped (and release its port).
const SIGKILL_REAP_TIMEOUT: Duration = Duration::from_secs(2);

/// Read the daemon PID from the pidfile, if present and parseable.
fn read_daemon_pid(pid_path: &Path) -> Option<u32> {
    let contents = std::fs::read_to_string(pid_path).ok()?;
    contents.trim().parse::<u32>().ok()
}

/// PID of the live daemon tracked by `data_dir/daemon.pid`, if any.
///
/// #613 (absorbed #601): lets `legion serve` name the daemon as the port
/// holder when its own bind fails, instead of emitting a bare bind error.
/// Stale pidfiles (dead process) read as None.
pub(crate) fn live_daemon_pid(data_dir: &Path) -> Option<u32> {
    let pid = read_daemon_pid(&data_dir.join(DAEMON_PID_FILE))?;
    watch::process_alive(pid).then_some(pid)
}

/// Send a signal (e.g. "TERM", "KILL") to a process. Unix-only; returns false on
/// other platforms. Uses `kill` to avoid a libc dependency, matching
/// `watch::process_alive`.
fn send_signal(pid: u32, signal: &str) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args([format!("-{signal}"), pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, signal);
        false
    }
}

/// Poll until the process exits or `timeout` elapses. Returns true if it exited.
fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if !watch::process_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !watch::process_alive(pid)
}

/// Stop a running daemon and wait for it to release its port.
///
/// Sends SIGTERM, then polls for up to `GRACEFUL_STOP_TIMEOUT`. If the process has
/// not exited by then -- e.g. axum's graceful HTTP shutdown is draining a long-lived
/// SSE connection and will not release the port promptly -- it escalates to SIGKILL,
/// so a restart never blocks on the drain. Removes the pidfile and returns `true` if
/// a running daemon was stopped, `false` if none was running.
pub fn stop_detached(data_dir: &Path) -> Result<bool> {
    let pid_path = data_dir.join(DAEMON_PID_FILE);
    let Some(pid) = read_daemon_pid(&pid_path) else {
        return Ok(false);
    };
    if !watch::process_alive(pid) {
        // Stale pidfile -- nothing to stop, clean it up.
        let _ = std::fs::remove_file(&pid_path);
        return Ok(false);
    }

    if !send_signal(pid, "TERM") {
        eprintln!("[legion] warning: failed to send SIGTERM to daemon {pid}");
    }
    let mut exited = wait_for_exit(pid, GRACEFUL_STOP_TIMEOUT);
    if !exited {
        eprintln!(
            "[legion] daemon {pid} did not exit within {}s; sending SIGKILL",
            GRACEFUL_STOP_TIMEOUT.as_secs()
        );
        if !send_signal(pid, "KILL") {
            eprintln!("[legion] warning: failed to send SIGKILL to daemon {pid}");
        }
        // SIGKILL is uncatchable; a short wait lets the kernel reap it and free
        // the bound port before the caller respawns.
        exited = wait_for_exit(pid, SIGKILL_REAP_TIMEOUT);
    }

    if !exited {
        // Still alive after SIGKILL (D-state, EPERM, a failed signal send, or a slow
        // reap). Do NOT remove the pidfile -- it is the only handle to re-target this
        // process -- and do NOT claim success. Fail loud so restart_detached's `?`
        // short-circuits instead of spawning a duplicate into the still-bound port.
        return Err(LegionError::DaemonStopFailed(format!(
            "pid {pid} survived SIGKILL; pidfile left in place"
        )));
    }

    let _ = std::fs::remove_file(&pid_path);
    Ok(true)
}

/// Best-effort: retrieve the full command-line string for a process on Unix.
///
/// On macOS uses `ps -p <pid> -o args=`; on Linux reads `/proc/<pid>/cmdline`
/// (NUL-separated, converted to spaces). Returns `None` when the process is
/// not found, the command is unavailable, or we are on a non-Unix platform.
/// This is intentionally best-effort -- callers guard against `None` by
/// refusing to kill the holder rather than proceeding blindly.
fn process_cmdline(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }
    #[cfg(target_os = "linux")]
    {
        let raw = std::fs::read(format!("/proc/{}/cmdline", pid)).ok()?;
        let s: String = raw
            .split(|&b| b == 0)
            .filter_map(|chunk| std::str::from_utf8(chunk).ok())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        if s.is_empty() { None } else { Some(s) }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// Return true when the command-line string is a legion daemon process.
///
/// This is the safety boundary for SIGKILL, so the match is structural, not a
/// free-text scan: `argv[0]`'s basename must be exactly `legion`, and its first
/// argument must be the `daemon` (or legacy `serve`) subcommand. A substring
/// scan over the whole cmdline is too loose for a kill decision -- e.g.
/// `node serve --legion-mode` contains both "legion" and "serve" but is not a
/// legion daemon and must never be killed.
///
/// The `serve` subcommand is intentionally accepted: `legion serve` is the
/// legacy daemon form (pre-daemon-spawn split) and is harmless because only the
/// daemon-port holder is ever examined. `daemon-spawn`/`daemon-restart` do NOT
/// match (they are short-lived CLI calls, never the port holder), and neither
/// do other legion subcommands (`recall`, `post`, ...).
fn cmdline_is_legion_daemon(cmdline: &str) -> bool {
    let mut tokens = cmdline.split_whitespace();
    // argv[0]: the program. Its basename must be exactly "legion".
    let prog = match tokens.next() {
        Some(p) => p,
        None => return false,
    };
    let base = prog.rsplit('/').next().unwrap_or(prog);
    if !base.eq_ignore_ascii_case("legion") {
        return false;
    }
    // argv[1]: the subcommand must be exactly "daemon" or "serve".
    match tokens.next() {
        Some(sub) => sub.eq_ignore_ascii_case("daemon") || sub.eq_ignore_ascii_case("serve"),
        None => false,
    }
}

/// Decide whether a port holder should be killed.
///
/// Returns `true` only when `cmdline` is `Some` and identifies a legion daemon
/// process. `None` (cmdline unreadable) and any non-legion cmdline return
/// `false` -- the safety invariant is "refuse to kill an unidentified or
/// non-legion process".
///
/// Extracted from `kill_orphaned_daemon_on_port_with` so this safety contract
/// is independently testable without spawning real processes.
pub(crate) fn should_kill_port_holder(cmdline: Option<&str>) -> bool {
    match cmdline {
        Some(c) => cmdline_is_legion_daemon(c),
        None => false,
    }
}

/// Kill an orphaned daemon holding `port` when we can confirm it is a legion
/// daemon process. Returns `true` when a process was identified and killed,
/// `false` when no holder was found or the holder could not be confirmed as
/// a legion daemon (in which case the holder is left alone).
///
/// Safety invariant: we ONLY kill the process when `cmdline_is_legion_daemon`
/// returns true. An unrelated process holding the same port is never touched.
///
/// DI seam (#675): `list_pids` and `read_cmdline` stand in for
/// `port_listener_pids`/`process_cmdline` (`restart_detached_with` and the
/// production `restart_detached` both wire the real functions through) so the
/// TERM->wait->KILL escalation and the kill/no-kill safety gate can be
/// exercised deterministically in tests, without depending on `lsof`/`ps` or a
/// real killable process.
fn kill_orphaned_daemon_on_port_with(
    port: u16,
    list_pids: impl Fn(u16) -> Vec<u32>,
    read_cmdline: impl Fn(u32) -> Option<String>,
) -> bool {
    let pids = list_pids(port);
    if pids.is_empty() {
        // #675 fix 3: on platforms where `process_cmdline` cannot identify a
        // process (anything other than macOS/Linux -- see its cfg gate) this
        // whole wedge-recovery step degrades to a silent no-op, since
        // `port_listener_pids` is also empty there. Log it so the operator
        // knows auto-recovery was skipped rather than assuming it ran.
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        eprintln!(
            "[legion] daemon-restart: orphan-kill wedge recovery is not supported on this \
             platform (port-holder identification needs macOS or Linux); if port {port} is \
             held by a stale legion daemon, stop it manually, then retry."
        );
        return false;
    }

    let mut killed_any = false;
    for pid in pids {
        // TOCTOU: `list_pids` already observed `pid` holding the port, but
        // between that snapshot and this re-read the holder can exit and the
        // pid be recycled by an unrelated process. This re-read is
        // intentional and safe: `should_kill_port_holder` judges whatever
        // process CURRENTLY answers to `pid`, so a recycled pid is decided on
        // its own (non-legion) cmdline and is never killed on the strength of
        // stale information from the `list_pids` snapshot.
        let cmdline: Option<String> = read_cmdline(pid);
        if !should_kill_port_holder(cmdline.as_deref()) {
            match &cmdline {
                None => eprintln!(
                    "[legion] daemon-restart: cannot read cmdline for port-holder pid {pid} -- \
                     refusing to kill an unidentified process"
                ),
                Some(c) => eprintln!(
                    "[legion] daemon-restart: port {port} is held by pid {pid} ({c:?}), \
                     which does not look like a legion daemon -- refusing to kill it. \
                     Stop it manually, then retry."
                ),
            }
            continue;
        }

        eprintln!("[legion] daemon-restart: killing orphaned daemon pid {pid} holding port {port}");
        if send_signal(pid, "TERM") {
            if !wait_for_exit(pid, GRACEFUL_STOP_TIMEOUT) {
                eprintln!(
                    "[legion] daemon-restart: orphaned daemon {pid} did not exit after SIGTERM; \
                     escalating to SIGKILL"
                );
                send_signal(pid, "KILL");
                wait_for_exit(pid, SIGKILL_REAP_TIMEOUT);
            }
        } else {
            // TERM failed (EPERM or already exiting); try KILL.
            send_signal(pid, "KILL");
            wait_for_exit(pid, SIGKILL_REAP_TIMEOUT);
        }

        if !watch::process_alive(pid) {
            eprintln!("[legion] daemon-restart: orphaned daemon {pid} stopped");
            killed_any = true;
        } else {
            eprintln!(
                "[legion] daemon-restart: failed to stop orphaned daemon {pid} on port {port}"
            );
        }
    }
    killed_any
}

/// Restart the background daemon: stop the running one (bounded), then spawn fresh.
///
/// This is the supported way to bounce the daemon. A bare `daemon-spawn` after a
/// manual kill can race the dying daemon's graceful-shutdown drain (port still
/// bound -> "Address already in use", or "already running" while it exits); restart
/// removes that wait by stopping deterministically before spawning.
///
/// Wedge recovery (#673 fix 3): when the pidfile is stale or missing but the
/// daemon port is held, identifies the holding process and kills it (after
/// confirming it is a legion daemon process). This prevents the common wedge
/// where a daemon whose pidfile was lost still holds the port, and
/// `daemon-spawn` refuses with "port in use" until a manual kill.
pub fn restart_detached(data_dir: &Path, port: u16) -> Result<()> {
    restart_detached_with(data_dir, port, port_listener_pids, process_cmdline)
}

/// DI seam for `restart_detached`'s wedge-recovery step: `list_pids` and
/// `read_cmdline` are threaded straight through to
/// `kill_orphaned_daemon_on_port_with` so a test can drive the whole
/// stop -> detect-orphan -> kill-decision -> spawn sequence deterministically
/// (#675). See `kill_orphaned_daemon_on_port_with` for what each closure
/// stands in for.
fn restart_detached_with(
    data_dir: &Path,
    port: u16,
    list_pids: impl Fn(u16) -> Vec<u32>,
    read_cmdline: impl Fn(u32) -> Option<String>,
) -> Result<()> {
    if stop_detached(data_dir)? {
        eprintln!("[legion] stopped running daemon");
    }

    // FIX 3: after the pidfile-based stop, check whether the port is still
    // held. If so, the pidfile was stale/missing (orphaned daemon). Attempt
    // to identify and kill the holder if it is a legion daemon process.
    if !port_available(port) {
        eprintln!(
            "[legion] daemon-restart: port {port} still held after pidfile-based stop; \
             checking for orphaned daemon"
        );
        kill_orphaned_daemon_on_port_with(port, list_pids, read_cmdline);

        // Give the OS a moment to release the port after the kill.
        if !port_available(port) {
            // Port is still held -- either the kill failed or the holder was
            // not a legion daemon. spawn_detached will produce a clear error.
            eprintln!(
                "[legion] daemon-restart: port {port} is still held; spawn will report the holder"
            );
        }
    }

    spawn_detached(data_dir, port)
}

/// Configuration for the daemon.
pub struct DaemonConfig {
    /// Directory that holds legion.db, watch.toml, etc.
    pub data_dir: PathBuf,
    /// HTTP port for the channel server.
    pub port: u16,
    /// Whether to start the MCP stdio server.
    pub enable_mcp: bool,
}

/// Run the legion daemon.
///
/// Spawns three tokio tasks:
///   - HTTP server (axum) on `config.port`
///   - Watch loop (background)
///   - MCP stdio server (when `config.enable_mcp` is true, in spawn_blocking)
///
/// Blocks until SIGINT/SIGTERM. All tasks are cancelled on shutdown.
pub fn run_daemon(config: DaemonConfig) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| LegionError::Server(format!("failed to create runtime: {e}")))?;

    let result = runtime.block_on(run_daemon_async(config));

    // Give blocking threads (e.g. MCP stdin loop) up to 2 seconds to exit before
    // the runtime forcibly drops them. Without this, a blocking task stuck on
    // read_until() holds the OS thread alive until the process exits.
    runtime.shutdown_timeout(Duration::from_secs(2));

    result
}

async fn run_daemon_async(config: DaemonConfig) -> Result<()> {
    let data_dir = config.data_dir.clone();

    // --mcp mode: stdio-only per-session subprocess spawned by Claude Code via
    // plugin.json mcpServers. Skip HTTP server bind and watch loop -- those are
    // singleton concerns that run via `legion daemon` without --mcp, independently.
    // Running them per-session causes :3131 bind conflicts across concurrent
    // sessions and duplicate watch loops that can trigger recursive agent spawns.
    if config.enable_mcp {
        return run_mcp_stdio_only(data_dir).await;
    }

    let port = config.port;

    eprintln!("[legion daemon] starting on port {port}");
    eprintln!(
        "[legion daemon] note: embed model not loaded -- posts via /api/post will not be similarity-searchable until card 019d7991-2eab lands"
    );

    // Build the broadcast channel for SSE notifications.
    let (tx, _rx) = channel::new_broadcast();

    let channel_state = channel::ChannelState {
        data_dir: data_dir.clone(),
        tx: tx.clone(),
        started_at: chrono::Utc::now(),
        role: channel::ServerRole::Daemon,
    };
    let app = channel::router(channel_state);

    // Bind the TCP listener.
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| LegionError::Server(format!("failed to bind {addr}: {e}")))?;

    eprintln!("[legion daemon] channel server at http://localhost:{port}");

    // Schedules fire under the daemon too (#613 decision): the daemon is
    // the long-lived server on a watch node, so cron posts must not depend
    // on someone running `legion serve`. Spawned only after the bind
    // succeeded -- same invariant as run_server: a process that failed to
    // become the server must not produce side effects. The task is
    // deliberately not in the select! below -- it is a side loop, not a
    // liveness-critical component, and it is cancelled when the runtime
    // shuts down.
    let _firing = channel::spawn_schedule_firing(data_dir.clone(), tx);

    // Spawn the watch loop as a background task.
    let watch_handle = tokio::spawn(async move {
        run_watch_task(&data_dir).await;
    });

    // Build the axum server future.
    let serve_future = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    // Race the HTTP server and watch task. Any task exiting -- success, error,
    // or panic -- triggers the other to stop so background failures surface
    // immediately instead of silently continuing.
    tokio::select! {
        result = serve_future => {
            if let Err(e) = result {
                eprintln!("[legion daemon] http server error: {e}");
            }
            eprintln!("[legion daemon] http server exited; shutting down");
        }
        result = watch_handle => {
            match result {
                Ok(()) => eprintln!("[legion daemon] watch task exited; shutting down"),
                Err(e) => eprintln!("[legion daemon] watch task exited: {e}; shutting down"),
            }
        }
    }

    eprintln!("[legion daemon] shutdown complete");
    Ok(())
}

/// Run the MCP stdio server without HTTP bind or watch loop.
///
/// Each Claude Code session spawns its own `legion daemon --mcp` subprocess via
/// plugin.json mcpServers, so this path must be cheap and isolated: no network
/// port, no cross-session coordination, no watch-loop side effects. When stdin
/// closes (CC session ends), `run_stdio_loop` returns and this process exits.
///
/// The broadcast channel is constructed locally even though no HTTP subscribers
/// exist in this mode, because the MCP tool handlers in `mcp.rs` call
/// `tx.send()` unconditionally to notify would-be channel listeners. The sender
/// must be live for those calls to be no-op sends instead of panics.
async fn run_mcp_stdio_only(data_dir: PathBuf) -> Result<()> {
    eprintln!("[legion daemon] MCP stdio-only mode (no HTTP, no watch)");
    eprintln!(
        "[legion daemon] note: embed model not loaded -- posts via MCP will not be similarity-searchable until card 019d7991-2eab lands"
    );

    let (tx, _rx) = channel::new_broadcast();
    let version = env!("CARGO_PKG_VERSION").to_string();

    let handle = tokio::task::spawn_blocking(move || mcp::run_stdio_loop(data_dir, version, tx));

    match handle.await {
        Ok(Ok(())) => {
            eprintln!("[legion daemon] mcp loop exited; shutting down");
            Ok(())
        }
        Ok(Err(e)) => {
            eprintln!("[legion daemon] mcp loop error: {e}; shutting down");
            Err(e)
        }
        Err(e) => {
            eprintln!("[legion daemon] mcp task panic: {e}; shutting down");
            Err(LegionError::Server(format!("mcp task panic: {e}")))
        }
    }
}

/// Run the watch loop inside a tokio task.
///
/// All pre-loop scaffolding (sync-actor spawn, config load, pid lock, db
/// open, host id) lives in `WatchLoop::bootstrap`, shared with the
/// standalone `watch::run` driver. When the watch loop cannot start but
/// cluster sync is enabled, the task parks instead of returning so sync
/// keeps running; with no sync configured it returns, which shuts the
/// daemon down via the select! in `run_daemon_async`.
///
/// The PID lock is held via a RAII guard so it is always released when the
/// task exits -- whether by normal return, abort, or panic.
async fn run_watch_task(data_dir: &Path) {
    let spawn_mode = watch::SpawnMode::from_env();
    let boot = watch::WatchLoop::bootstrap(data_dir, spawn_mode, "[legion daemon]");

    // Keep the sync actor alive for the lifetime of this task; sync is
    // optional and never fatal (#536).
    let sync_handle = boot.sync;

    let (mut state, _pid_guard) = match boot.watch {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[legion daemon] watch not started: {e}");
            if sync_handle.is_some() {
                // Cluster sync is already running even though the watch loop
                // cannot start (broken watch.toml, pid-lock held elsewhere,
                // db open error). Park this task instead of returning: a
                // return drops the sync handle AND shuts the whole daemon
                // down via the select! in run_daemon_async. Dispositioned
                // from the PR #624 review (#611): a node with cluster sync
                // enabled but a broken watch config must still sync.
                eprintln!("[legion daemon] cluster sync stays up without watch");
                std::future::pending::<()>().await;
            }
            return;
        }
    };

    // Timer intervals are read from the loop-owned config.
    let poll_interval = std::time::Duration::from_secs(state.config.poll_interval_secs);
    let health_interval = std::time::Duration::from_secs(state.config.health_poll_secs);
    // Auto-reconcile (#654) runs on a slow cadence; 0 disables it entirely.
    let reconcile_interval = std::time::Duration::from_secs(state.config.reconcile_interval_secs);

    let mut poll_timer = tokio::time::Instant::now()
        .checked_sub(poll_interval)
        .unwrap_or_else(tokio::time::Instant::now);
    let mut health_timer = tokio::time::Instant::now()
        .checked_sub(health_interval)
        .unwrap_or_else(tokio::time::Instant::now);
    // Unlike poll/health, the reconcile timer starts at `now` (not
    // now - interval) so the first pass fires one full interval after
    // startup. Reconcile probes the work source once per linked card, so
    // firing it immediately on every daemon bounce would storm `gh`; one
    // interval of staleness after a restart is the cheaper tradeoff.
    let mut reconcile_timer = tokio::time::Instant::now();

    loop {
        // Yield to tokio scheduler each iteration.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        if health_timer.elapsed() >= health_interval {
            state.tick_health();
            health_timer = tokio::time::Instant::now();
        }

        if poll_timer.elapsed() >= poll_interval {
            state.tick_poll();
            poll_timer = tokio::time::Instant::now();
        }

        if state.config.reconcile_interval_secs > 0
            && reconcile_timer.elapsed() >= reconcile_interval
        {
            state.tick_reconcile();
            reconcile_timer = tokio::time::Instant::now();
        }
    }
}

/// Wait for SIGINT or SIGTERM.
///
/// If signal handler installation fails, logs the error and returns immediately
/// rather than panicking. The daemon continues running but loses graceful
/// shutdown; it can still be killed via SIGKILL.
async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        match signal::ctrl_c().await {
            Ok(()) => {}
            Err(e) => {
                // Log the failure but never return -- returning here would cause
                // the select! to fire the ctrl_c arm on startup, shutting down
                // the daemon immediately. Instead park until SIGTERM arrives.
                eprintln!("[legion daemon] failed to install Ctrl+C handler, ignoring: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                eprintln!("[legion daemon] failed to install SIGTERM handler: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    eprintln!("[legion daemon] received shutdown signal");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_daemon_pid_parses_valid_pidfile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_path = dir.path().join(DAEMON_PID_FILE);
        std::fs::write(&pid_path, "12345\n").expect("write");
        assert_eq!(read_daemon_pid(&pid_path), Some(12345));
    }

    #[test]
    fn read_daemon_pid_none_for_missing_or_garbage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_path = dir.path().join(DAEMON_PID_FILE);
        assert_eq!(read_daemon_pid(&pid_path), None, "missing file -> None");
        std::fs::write(&pid_path, "not-a-pid").expect("write");
        assert_eq!(read_daemon_pid(&pid_path), None, "garbage -> None");
    }

    #[test]
    fn stop_detached_no_pidfile_is_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            !stop_detached(dir.path()).expect("stop"),
            "no pidfile -> nothing stopped"
        );
    }

    #[test]
    fn stop_detached_removes_stale_pidfile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_path = dir.path().join(DAEMON_PID_FILE);
        // A PID far above any real process on this host -- not alive.
        std::fs::write(&pid_path, "2147483646").expect("write");
        assert!(
            !stop_detached(dir.path()).expect("stop"),
            "stale (dead) pid -> nothing stopped"
        );
        assert!(!pid_path.exists(), "stale pidfile should be removed");
    }

    #[test]
    fn port_available_false_for_bound_port() {
        let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind ephemeral");
        let port = listener.local_addr().expect("addr").port();
        assert!(
            !port_available(port),
            "a held port must read as unavailable"
        );
    }

    #[test]
    fn spawn_detached_refuses_when_port_held() {
        // Hold the port the way a stray `legion serve` or orphaned daemon would.
        let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind ephemeral");
        let port = listener.local_addr().expect("addr").port();
        let dir = tempfile::tempdir().expect("tempdir");

        let err = spawn_detached(dir.path(), port).expect_err("must refuse a held port");
        assert!(
            matches!(err, LegionError::DaemonPortInUse(_)),
            "expected DaemonPortInUse, got {err:?}"
        );
        // The preflight returns before forking, so no doomed child is spawned
        // and no pidfile is left pointing at a corpse.
        assert!(
            !dir.path().join(DAEMON_PID_FILE).exists(),
            "no pidfile should be written when the port is held"
        );
    }

    #[tokio::test]
    async fn daemon_starts_channel_and_responds_to_feed() {
        // Create a database at the path the channel handler expects: data_dir/legion.db
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let _db = crate::db::Database::open(&data_dir.join("legion.db")).expect("open db");

        let (tx, _rx) = channel::new_broadcast();
        let state = channel::ChannelState {
            data_dir: data_dir.clone(),
            tx,
            started_at: chrono::Utc::now(),
            role: channel::ServerRole::Daemon,
        };

        let app = channel::router(state);

        // Bind on an ephemeral port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let port = addr.port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        // Wait briefly for the server to start.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Make a raw HTTP/1.1 GET request using std blocking TCP.
        // This avoids needing tokio's io-util feature for AsyncReadExt.
        let response = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            use std::net::TcpStream;

            let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).expect("connect");
            stream
                .write_all(
                    b"GET /api/feed HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                )
                .expect("write");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read");
            buf
        })
        .await
        .expect("spawn_blocking");

        let response_str = String::from_utf8_lossy(&response);
        assert!(
            response_str.starts_with("HTTP/1.1 200"),
            "expected 200 OK, got: {}",
            &response_str[..response_str.len().min(200)]
        );
        assert!(
            response_str.contains('['),
            "expected JSON array body in response"
        );
    }

    // -- FIX 3: cmdline-based daemon identification (#673) ------------------

    #[test]
    fn cmdline_is_legion_daemon_matches_daemon_args() {
        // Typical production forms that must be identified as a legion daemon.
        assert!(
            cmdline_is_legion_daemon("/usr/local/bin/legion daemon --port 3131"),
            "absolute path legion daemon must match"
        );
        assert!(
            cmdline_is_legion_daemon("legion daemon"),
            "bare 'legion daemon' must match"
        );
        assert!(
            cmdline_is_legion_daemon("legion serve"),
            "legacy 'legion serve' must match (same port, same binary)"
        );
        assert!(
            cmdline_is_legion_daemon("/home/user/.cargo/bin/legion daemon"),
            "cargo-installed binary must match"
        );
    }

    #[test]
    fn cmdline_is_legion_daemon_rejects_unrelated_processes() {
        // Processes that happen to be on the same port but are not a legion daemon.
        assert!(
            !cmdline_is_legion_daemon("node server.js"),
            "node server must not be identified as a legion daemon"
        );
        assert!(
            !cmdline_is_legion_daemon("python -m http.server 3131"),
            "python http server must not be identified as a legion daemon"
        );
        assert!(
            !cmdline_is_legion_daemon(""),
            "empty cmdline must not match"
        );
        // A process named legion but running a non-daemon subcommand must not
        // be killed -- it could be a `legion recall` or `legion post` call.
        assert!(
            !cmdline_is_legion_daemon("legion recall --repo legion"),
            "legion CLI (non-daemon subcommand) must not be identified as a daemon"
        );
        assert!(
            !cmdline_is_legion_daemon("legion post --repo legion"),
            "legion post must not be identified as a daemon"
        );
        // Substring-match false positive that the structural argv[0]+subcommand
        // check must reject: contains both "legion" and "serve" but argv[0] is
        // node, not legion. This was the pre-push review's HIGH-1 example.
        assert!(
            !cmdline_is_legion_daemon("node serve --legion-mode"),
            "a non-legion argv[0] must never match, even if later args mention legion/serve"
        );
        // daemon-spawn / daemon-restart are short-lived CLI calls, never the
        // port holder; the exact-subcommand match must not treat them as the daemon.
        assert!(
            !cmdline_is_legion_daemon("legion daemon-spawn"),
            "daemon-spawn is a CLI call, not the daemon"
        );
        assert!(
            !cmdline_is_legion_daemon("legion daemon-restart"),
            "daemon-restart is a CLI call, not the daemon"
        );
    }

    #[test]
    fn cmdline_is_legion_daemon_is_case_insensitive() {
        // Platform cmdlines can have varying case.
        assert!(
            cmdline_is_legion_daemon("Legion Daemon"),
            "uppercase forms must also match"
        );
    }

    // -- should_kill_port_holder (#673 review fix) ---------------------------
    //
    // Tests lock in the safety invariant: never kill an unidentified process
    // (None cmdline) or a non-legion process, only a confirmed legion daemon.

    #[test]
    fn should_kill_port_holder_refuses_unidentified_process() {
        // None: cmdline could not be read (permission error, proc gone, etc.).
        // Safety: refuse -- we cannot identify the holder.
        assert!(
            !should_kill_port_holder(None),
            "None cmdline must return false (refuse to kill unidentified process)"
        );
    }

    #[test]
    fn should_kill_port_holder_refuses_non_legion_process() {
        // A real process holding the port that is NOT a legion daemon.
        assert!(
            !should_kill_port_holder(Some("node server.js")),
            "node server must not be killed"
        );
        assert!(
            !should_kill_port_holder(Some("python -m http.server 3131")),
            "python server must not be killed"
        );
        assert!(
            !should_kill_port_holder(Some("legion recall --repo foo")),
            "legion non-daemon CLI invocation must not be killed"
        );
    }

    #[test]
    fn should_kill_port_holder_approves_legion_daemon() {
        // A confirmed legion daemon process should be killed.
        assert!(
            should_kill_port_holder(Some("legion daemon --port 3131")),
            "legion daemon must be approved for kill"
        );
        assert!(
            should_kill_port_holder(Some("/usr/local/bin/legion daemon")),
            "absolute-path legion daemon must be approved"
        );
        assert!(
            should_kill_port_holder(Some("legion serve")),
            "legacy 'legion serve' form must be approved"
        );
    }

    // -- kill orchestration smoke tests via the DI seam (#675) ---------------
    //
    // `kill_orphaned_daemon_on_port_with` sends real SIGTERM/SIGKILL, so it is
    // the highest-leverage gap in the whole wedge-recovery path: a bug here
    // kills an unrelated process. These tests bind a REAL listener to prove the
    // port is genuinely held, but inject `list_pids`/`read_cmdline` (rather
    // than relying on `lsof`/`ps` to introspect the real holder) so the
    // safety-gate assertion is deterministic and independent of what tooling
    // happens to be on the test-runner's PATH.

    #[test]
    fn kill_orphaned_daemon_on_port_with_refuses_non_legion_holder() {
        // Hold a real port -- the "orphan" the wedge-recovery path is asked
        // to look at -- but never actually spawn a killable process; the
        // injected closures fully control what the kill decision sees.
        let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind ephemeral");
        let port = listener.local_addr().expect("addr").port();
        // Maximum valid POSIX PID -- almost never a live process on any real
        // system (same convention as `daemon_auto_spawn_clears_stale_pid`).
        let fake_holder_pid = i32::MAX as u32;

        let killed = kill_orphaned_daemon_on_port_with(
            port,
            |_port| vec![fake_holder_pid],
            |pid| {
                assert_eq!(pid, fake_holder_pid, "must query the injected pid");
                Some("node server.js".to_string())
            },
        );

        assert!(
            !killed,
            "a holder whose cmdline is not a legion daemon must never be killed"
        );
        assert!(
            !port_available(port),
            "the real listener must still hold the port -- nothing was signaled"
        );
        drop(listener);
    }

    #[test]
    fn kill_orphaned_daemon_on_port_with_approves_legion_holder() {
        // Complementary case: when the injected cmdline DOES look like a
        // legion daemon, `should_kill_port_holder` approves it and the
        // escalation loop attempts a signal. There is no real process behind
        // `fake_pid`, so `send_signal`/`wait_for_exit` observe it as already
        // gone -- this pins that the approve-path is reached and returns
        // `true` (via `watch::process_alive` reading false for a nonexistent
        // pid) without needing a real spawned daemon to kill.
        let fake_pid = (i32::MAX - 1) as u32;
        let killed = kill_orphaned_daemon_on_port_with(
            0,
            |_port| vec![fake_pid],
            |pid| {
                assert_eq!(pid, fake_pid);
                Some("legion daemon --port 3131".to_string())
            },
        );
        assert!(
            killed,
            "a confirmed legion daemon holder (already-gone pid) must report killed"
        );
    }

    /// Drives the full `restart_detached` sequence (stop -> detect-orphan ->
    /// kill-decision -> spawn) end to end. The real listener proves the port
    /// is genuinely held (not a mocked `port_available`); the injected
    /// cmdline proves the safety gate refuses to kill it; and the
    /// `DaemonPortInUse` error proves `spawn_detached` was still attempted
    /// afterward, exactly as production `restart_detached` always does
    /// regardless of whether the kill step succeeded.
    #[test]
    fn restart_detached_refuses_to_kill_non_legion_holder_but_still_attempts_spawn() {
        let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind ephemeral");
        let port = listener.local_addr().expect("addr").port();
        let dir = tempfile::tempdir().expect("tempdir");
        let fake_holder_pid = (i32::MAX - 2) as u32;

        let cmdline_lookups = std::cell::Cell::new(0u32);
        let err = restart_detached_with(
            dir.path(),
            port,
            |_port| vec![fake_holder_pid],
            |pid| {
                cmdline_lookups.set(cmdline_lookups.get() + 1);
                assert_eq!(pid, fake_holder_pid, "must query the injected pid");
                Some("node server.js".to_string())
            },
        )
        .expect_err("the port is genuinely held, so spawn must fail loud");

        assert!(
            matches!(err, LegionError::DaemonPortInUse(_)),
            "expected DaemonPortInUse (spawn was attempted and hit the real held port), \
             got {err:?}"
        );
        assert_eq!(
            cmdline_lookups.get(),
            1,
            "cmdline lookup must run exactly once for the one reported pid"
        );
        assert!(
            !port_available(port),
            "the real (non-legion) listener must still hold the port -- it was never killed"
        );
        assert!(
            !dir.path().join(DAEMON_PID_FILE).exists(),
            "no pidfile should be written when spawn's own preflight refuses"
        );

        drop(listener);
    }
}
