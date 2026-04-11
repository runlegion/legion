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
            if is_process_alive(pid) {
                eprintln!("legion daemon already running (pid {pid})");
                return Ok(());
            }
            // Stale PID file -- process is gone, continue to start a new one.
            let _ = std::fs::remove_file(&pid_path);
        }
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

/// Check whether a process with the given PID is alive on this platform.
///
/// Uses `kill -0` on Unix (no signal sent, just existence check). Always
/// returns `false` on non-Unix platforms where we cannot probe process state.
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
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
        tx,
    };
    let app = channel::router(channel_state);

    // Bind the TCP listener.
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| LegionError::Server(format!("failed to bind {addr}: {e}")))?;

    eprintln!("[legion daemon] channel server at http://localhost:{port}");

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
/// Loads watch.toml from data_dir. If the config is missing or has no repos,
/// logs a warning and exits the task (does not crash the daemon).
///
/// The PID lock is held via a RAII guard so it is always released when the
/// task exits -- whether by normal return, abort, or panic.
async fn run_watch_task(data_dir: &Path) {
    let config_path = data_dir.join("watch.toml");
    let lock_path = data_dir.join("watch.pid");
    let db_path = data_dir.join("legion.db");

    let config = match watch::load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[legion daemon] watch not started: {e}");
            return;
        }
    };

    // Acquire PID lock -- if another watcher is running, skip gracefully.
    if let Err(e) = watch::acquire_pid_lock(&lock_path) {
        eprintln!("[legion daemon] watch skipped ({})", e);
        return;
    }

    // RAII guard releases the lock when this task exits (abort, panic, or return).
    let _pid_guard = watch::PidLockGuard(lock_path);

    eprintln!(
        "[legion daemon] watch active: {} repo(s), poll every {}s",
        config.repos.len(),
        config.poll_interval_secs
    );

    let db = match crate::db::Database::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[legion daemon] watch: db open error: {e}");
            return;
        }
    };

    let mut cooldown = watch::CooldownTracker::new(
        config.cooldown_secs,
        config.work_hours_start,
        config.work_hours_end,
    );
    let mut tracker = watch::AgentTracker::new();
    let mut sampler = crate::health::HealthSampler::new(config.health_window_size);

    let poll_interval = std::time::Duration::from_secs(config.poll_interval_secs);
    let health_interval = std::time::Duration::from_secs(config.health_poll_secs);
    let retention_cutoff = chrono::Duration::days(config.retention_days as i64);
    let lookback = (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339();

    let mut poll_timer = tokio::time::Instant::now()
        .checked_sub(poll_interval)
        .unwrap_or_else(tokio::time::Instant::now);
    let mut health_timer = tokio::time::Instant::now()
        .checked_sub(health_interval)
        .unwrap_or_else(tokio::time::Instant::now);

    loop {
        // Yield to tokio scheduler each iteration.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        if health_timer.elapsed() >= health_interval {
            sampler.sample();
            tracker.reap_finished();

            match sampler.to_health_sample(tracker.active_count()) {
                Ok(sample) => {
                    if let Err(e) = db.insert_health_sample(&sample) {
                        eprintln!("[legion daemon] health persist error: {e}");
                    }
                }
                Err(e) => {
                    eprintln!("[legion daemon] health sample error: {e}");
                }
            }

            health_timer = tokio::time::Instant::now();
        }

        if poll_timer.elapsed() >= poll_interval {
            if sampler.can_spawn(config.health_threshold_pct) {
                match watch::poll_cycle(&db, &config, &mut cooldown, &mut tracker, Some(&lookback))
                {
                    Ok(n) if n > 0 => {
                        eprintln!("[legion daemon] watch: {} agent(s) spawned", n);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[legion daemon] watch poll error: {e}");
                    }
                }
            }

            let cutoff = (chrono::Utc::now() - retention_cutoff).to_rfc3339();
            if let Err(e) = db.prune_health_samples(&cutoff) {
                eprintln!("[legion daemon] health prune error: {e}");
            }
            if let Err(e) = db.prune_watch_handled(&cutoff) {
                eprintln!("[legion daemon] watch_handled prune error: {e}");
            }

            poll_timer = tokio::time::Instant::now();
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
}
