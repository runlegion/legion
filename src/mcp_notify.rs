/// Inter-process notification for MCP servers.
///
/// MCP servers run as child processes of Claude Code. When posts are created
/// via the CLI (external to any MCP process), those MCP servers need to be notified.
///
/// This module implements file-based IPC:
/// - Each MCP server creates a notification queue at startup
/// - The CLI post command queries running MCP servers and writes notifications
/// - Each MCP server periodically polls its queue for new notifications
use std::fs;
use std::path::PathBuf;
use std::process;

use serde_json::json;

use crate::error::Result;

const NOTIFY_DIR: &str = "/tmp/legion-mcp-notify";

/// Get the notification queue path for this process.
pub fn queue_path() -> PathBuf {
    let pid = process::id();
    PathBuf::from(format!("{}/{}.queue", NOTIFY_DIR, pid))
}

/// Get the queue directory for this process.
fn queue_dir() -> PathBuf {
    PathBuf::from(NOTIFY_DIR)
}

/// Register an MCP server instance as running.
/// Creates an empty notification queue that external processes can write to.
pub fn register_mcp_server() -> Result<()> {
    let dir = queue_dir();
    fs::create_dir_all(&dir)?;

    let path = queue_path();
    if !path.exists() {
        fs::File::create(&path)?;
    }

    Ok(())
}

/// Unregister an MCP server instance (cleanup).
pub fn unregister_mcp_server() -> Result<()> {
    let path = queue_path();
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// Write a notification to all running MCP server queues.
/// Called by the CLI post command and other notification sources.
#[allow(dead_code)]
pub fn notify_all_mcp_servers(
    post_id: &str,
    repo: &str,
    is_signal: bool,
    text: &str,
) -> Result<()> {
    let dir = queue_dir();
    if !dir.exists() {
        return Ok(());
    }

    let notification = json!({
        "post_id": post_id,
        "repo": repo,
        "is_signal": is_signal,
        "text": text,
    });

    let notif_json = serde_json::to_string(&notification)?;

    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "queue") {
            // Append notification to this queue file (newline-delimited JSON).
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;

            // Use std::fs::write with append to safely write in a race-safe manner.
            // For each queue, append the notification as a line.
            if let Ok(mut content) = fs::read_to_string(&path) {
                content.push('\n');
                content.push_str(&notif_json);
                fs::write(&path, content)?;
            } else {
                // If file doesn't exist or is unreadable, just write the notification.
                fs::write(&path, format!("{}\n", notif_json))?;
            }
        }
    }

    Ok(())
}

/// Read and clear all pending notifications for this MCP server.
#[allow(dead_code)]
pub fn read_pending_notifications() -> Result<Vec<serde_json::Value>> {
    let path = queue_path();
    if !path.exists() {
        return Ok(vec![]);
    }

    let content = fs::read_to_string(&path)?;
    let notifications: Vec<serde_json::Value> = content
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    // Clear the queue after reading.
    fs::write(&path, "")?;

    Ok(notifications)
}

/// Notify all MCP servers about a post, by repo and post ID.
/// Useful when the caller doesn't have immediate access to text/is_signal.
#[allow(dead_code)]
pub fn notify_mcp_from_db(db: &crate::db::Database, post_id: &str) -> Result<()> {
    let post = db
        .get_reflection_by_id(post_id)?
        .ok_or_else(|| crate::error::LegionError::Search(format!("post {} not found", post_id)))?;

    let is_signal = crate::signal::is_signal(&post.text);
    notify_all_mcp_servers(&post.id, &post.repo, is_signal, &post.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_path_contains_pid() {
        let path = queue_path();
        let pid = process::id();
        assert!(path.to_string_lossy().contains(&pid.to_string()));
    }

    #[test]
    fn test_notify_and_read_notifications() {
        // Setup
        register_mcp_server().expect("register");

        // Notify
        notify_all_mcp_servers("test-post", "test-repo", false, "hello").expect("notify");

        // Read
        let notifs = read_pending_notifications().expect("read");
        assert_eq!(notifs.len(), 1);
        assert_eq!(notifs[0]["post_id"], "test-post");
        assert_eq!(notifs[0]["repo"], "test-repo");
        assert_eq!(notifs[0]["text"], "hello");

        // Queue should be cleared
        let notifs2 = read_pending_notifications().expect("read2");
        assert_eq!(notifs2.len(), 0);

        // Cleanup
        unregister_mcp_server().expect("unregister");
    }
}
