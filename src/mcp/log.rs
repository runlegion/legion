//! MCP process logging and trace infrastructure (#395): per-PID log
//! files, stderr redirection, the `mcp_trace` event format, and the
//! verbose-trace gate. Carved from mcp.rs (#612).

use std::path::PathBuf;

use crate::error::{LegionError, Result};

/// Resolve the MCP-process log file (#395). One file per running MCP
/// subprocess so tailing one repo's MCP does not require de-interleaving
/// from another's. macOS uses `~/Library/Logs/legion/mcp/<pid>.log`;
/// other platforms use XDG state.
pub fn mcp_log_path(pid: u32) -> Result<PathBuf> {
    let base = mcp_log_dir()?;
    Ok(base.join(format!("{pid}.log")))
}

/// Directory holding all MCP process log files. Created if missing.
pub fn mcp_log_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .map_err(|_| LegionError::NoHomeDir)?;
        Ok(home.join("Library/Logs/legion/mcp"))
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
        Ok(state_home.join("legion/mcp"))
    }
}

/// Redirect this process's stderr to the per-PID MCP log file (#395).
/// Existing eprintln! calls keep working unchanged; their output now lands
/// in the file instead of being swallowed by Claude Code. Failures here
/// are non-fatal -- without redirection the MCP still functions, we just
/// lose observability.
#[cfg(unix)]
fn redirect_stderr_to_log() {
    let pid = std::process::id();
    let Ok(path) = mcp_log_path(pid) else {
        return;
    };
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    use std::os::unix::io::AsRawFd;
    // Safety: dup2 is async-signal-safe and redirects fd 2 (stderr) to the
    // file. The OwnedFd is leaked via Box::leak so the underlying fd stays
    // open for the process lifetime; closing it would invalidate stderr.
    unsafe {
        if libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) >= 0 {
            Box::leak(Box::new(file));
        }
    }
}

#[cfg(not(unix))]
fn redirect_stderr_to_log() {}

/// Emit a trace event to stderr (which #395 redirects to the per-PID log
/// file). Lifecycle and error events use this unconditionally; verbose
/// events (per-poll, per-post-decision) gate on `LEGION_MCP_TRACE=1` so
/// production sessions do not pay the log-volume cost. Format is
/// `<rfc3339> [legion mcp pid=<pid>] <event> <key>=<value> ...`.
pub fn mcp_trace(event: &str, kvs: &[(&str, &str)]) {
    let now = chrono::Utc::now().to_rfc3339();
    let pid = std::process::id();
    let mut line = format!("{now} [legion mcp pid={pid}] {event}");
    for (k, v) in kvs {
        line.push(' ');
        line.push_str(k);
        line.push('=');
        line.push_str(v);
    }
    eprintln!("{line}");
}

/// Whether verbose tracing is enabled (per-poll, per-post-decision).
fn mcp_verbose() -> bool {
    std::env::var("LEGION_MCP_TRACE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Tail-friendly path lookup helper used by `legion mcp-logs --tail`.
pub fn most_recent_mcp_log() -> Result<Option<PathBuf>> {
    let dir = mcp_log_dir()?;
    if !dir.exists() {
        return Ok(None);
    }
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("log") {
            continue;
        }
        let mtime = entry.metadata()?.modified()?;
        if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
            newest = Some((mtime, path));
        }
    }
    Ok(newest.map(|(_, p)| p))
}
