/// Hand-rolled JSON-RPC 2.0 stdio server for the MCP protocol.
///
/// Reads newline-delimited JSON from stdin, writes responses to stdout.
/// Implements only the subset of MCP that the legion channel uses:
///   - initialize
///   - tools/list
///   - tools/call
///
/// No Content-Length headers. Each message is a single JSON line.
use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::channel::ChannelEvent;
use crate::db::{Database, ReflectionMeta};
use crate::error::{LegionError, Result};
use crate::search::SearchIndex;
use crate::signal as sig;
use crate::task;

/// Protocol version string returned by initialize.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Maximum notification content length before truncation.
const MAX_NOTIFICATION_LEN: usize = 2000;

/// Tool definitions -- match the TypeScript tools.ts shapes exactly.
fn tool_definitions() -> Value {
    json!([
        {
            "name": "legion_post",
            "description": "Post a message to the Legion team bullpen. All agents will see it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo": {
                        "type": "string",
                        "description": "Your repo name (identifies the poster)"
                    },
                    "text": {
                        "type": "string",
                        "description": "The message to post"
                    }
                },
                "required": ["repo", "text"]
            }
        },
        {
            "name": "legion_reply",
            "description": "Reply to a specific bullpen post or signal by ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo": {
                        "type": "string",
                        "description": "Your repo name (identifies the poster)"
                    },
                    "post_id": {
                        "type": "string",
                        "description": "The post/signal ID to reply to"
                    },
                    "text": {
                        "type": "string",
                        "description": "Your reply"
                    }
                },
                "required": ["repo", "post_id", "text"]
            }
        },
        {
            "name": "legion_signal",
            "description": "Send a structured signal to another agent (@recipient verb:status).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo": {
                        "type": "string",
                        "description": "Your repo name (identifies the sender)"
                    },
                    "to": {
                        "type": "string",
                        "description": "Recipient agent name, or \"all\""
                    },
                    "verb": {
                        "type": "string",
                        "description": "Action: review, request, announce, question, answer, etc."
                    },
                    "status": {
                        "type": "string",
                        "description": "Status: approved, help, blocked, etc."
                    },
                    "note": {
                        "type": "string",
                        "description": "Free-text note"
                    },
                    "details": {
                        "type": "string",
                        "description": "Comma-separated key:value detail pairs"
                    }
                },
                "required": ["repo", "to", "verb"]
            }
        },
        {
            "name": "legion_task_respond",
            "description": "Respond to a task assigned to you. Accept, complete, or block it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Task ID"
                    },
                    "action": {
                        "type": "string",
                        "enum": ["accept", "done", "block"],
                        "description": "What to do with the task"
                    },
                    "note": {
                        "type": "string",
                        "description": "Optional note (completion summary or block reason)"
                    }
                },
                "required": ["task_id", "action"]
            }
        }
    ])
}

/// Build a JSON-RPC 2.0 success response.
fn success_response(id: &Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

/// Build a JSON-RPC 2.0 error response.
fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

/// Build a tool result content array (the MCP tools/call response shape).
fn tool_result(text: &str) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ]
    })
}

/// Truncate content to MAX_NOTIFICATION_LEN with a trailing hint.
fn truncate(content: &str, id: Option<&str>) -> String {
    if content.len() <= MAX_NOTIFICATION_LEN {
        return content.to_string();
    }
    let hint = match id {
        Some(post_id) => format!(
            "\n\n[truncated -- full content on bullpen, id: {}]",
            post_id
        ),
        None => "\n\n[truncated -- full content on bullpen]".to_string(),
    };
    let cut = MAX_NOTIFICATION_LEN.saturating_sub(hint.len());
    format!("{}{}", &content[..cut], hint)
}

/// Handle a single MCP tools/call dispatch.
///
/// Opens a fresh DB connection on each call (consistent with legion's
/// single-connection model for the CLI). The MCP server is long-lived but
/// calls are infrequent.
fn handle_tool_call(
    data_dir: &std::path::Path,
    name: &str,
    args: &Value,
    tx: &broadcast::Sender<ChannelEvent>,
) -> Result<String> {
    let get_str = |key: &str| -> Option<String> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    match name {
        "legion_post" => {
            let repo = get_str("repo").unwrap_or_else(|| "unknown".to_string());
            let text = get_str("text").ok_or_else(|| {
                LegionError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "text is required",
                ))
            })?;

            let db = Database::open(&data_dir.join("legion.db"))?;
            let index = SearchIndex::open(&data_dir.join("index"))?;

            let reflection = db.insert_reflection_with_meta(
                &repo,
                text.trim(),
                "team",
                &ReflectionMeta::default(),
            )?;

            if let Err(e) = index.add(&reflection.id, &repo, text.trim()) {
                eprintln!("[legion mcp] search index add failed: {e}");
            }

            let _ = tx.send(ChannelEvent::Feed);

            Ok(format!("posted (id: {})", reflection.id))
        }

        "legion_reply" => {
            let repo = get_str("repo").unwrap_or_else(|| "unknown".to_string());
            let post_id = get_str("post_id").ok_or_else(|| {
                LegionError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "post_id is required",
                ))
            })?;
            let text = get_str("text").ok_or_else(|| {
                LegionError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "text is required",
                ))
            })?;

            // Format: re:<post_id> -- <text>  (matches TS tools.ts::legion_reply)
            let reply_text = format!("re:{} -- {}", post_id, text.trim());

            let db = Database::open(&data_dir.join("legion.db"))?;
            let index = SearchIndex::open(&data_dir.join("index"))?;

            let reflection = db.insert_reflection_with_meta(
                &repo,
                &reply_text,
                "team",
                &ReflectionMeta::default(),
            )?;

            if let Err(e) = index.add(&reflection.id, &repo, &reply_text) {
                eprintln!("[legion mcp] search index add failed: {e}");
            }

            let _ = tx.send(ChannelEvent::Feed);

            Ok(format!("replied (id: {})", reflection.id))
        }

        "legion_signal" => {
            let repo = get_str("repo").unwrap_or_else(|| "unknown".to_string());
            let to = get_str("to").ok_or_else(|| {
                LegionError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "to is required",
                ))
            })?;
            let verb = get_str("verb").ok_or_else(|| {
                LegionError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "verb is required",
                ))
            })?;
            let status = get_str("status");
            let note = get_str("note");
            let details_str = get_str("details");

            // Validate note length
            if let Some(ref n) = note {
                sig::validate_note(n)?;
            }

            // Parse details string "key:value,key:value" into Vec<(String, String)>
            let details: Vec<(String, String)> = details_str
                .as_deref()
                .unwrap_or("")
                .split(',')
                .filter_map(|pair| {
                    let pair = pair.trim();
                    let pos = pair.find(':')?;
                    Some((
                        pair[..pos].trim().to_string(),
                        pair[pos + 1..].trim().to_string(),
                    ))
                })
                .collect();

            let signal_text =
                sig::format_signal(&to, &verb, status.as_deref(), note.as_deref(), &details);

            let db = Database::open(&data_dir.join("legion.db"))?;
            let index = SearchIndex::open(&data_dir.join("index"))?;

            let reflection = db.insert_reflection_with_meta(
                &repo,
                &signal_text,
                "team",
                &ReflectionMeta::default(),
            )?;

            if let Err(e) = index.add(&reflection.id, &repo, &signal_text) {
                eprintln!("[legion mcp] search index add failed: {e}");
            }

            let _ = tx.send(ChannelEvent::Feed);

            Ok(format!("signaled (id: {})", reflection.id))
        }

        "legion_task_respond" => {
            let task_id = get_str("task_id").ok_or_else(|| {
                LegionError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "task_id is required",
                ))
            })?;
            let action = get_str("action").ok_or_else(|| {
                LegionError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "action is required",
                ))
            })?;
            let note = get_str("note");

            let db = Database::open(&data_dir.join("legion.db"))?;

            match action.as_str() {
                "accept" => task::accept_task(&db, &task_id)?,
                "done" => task::complete_task(&db, &task_id, note.as_deref())?,
                "block" => task::block_task(&db, &task_id, note.as_deref())?,
                other => {
                    return Err(LegionError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("unknown action: {other}; expected accept, done, or block"),
                    )));
                }
            }

            let _ = tx.send(ChannelEvent::Tasks);

            Ok(format!("task {}: {}", action, task_id))
        }

        other => Err(LegionError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("unknown tool: {other}"),
        ))),
    }
}

/// Dispatch a single JSON-RPC 2.0 message, returning the response as a Value.
///
/// Returns None for notifications (which have no id) -- not used here but
/// guards against future notification handling.
pub fn dispatch(
    request: &Value,
    data_dir: &std::path::Path,
    version: &str,
    tx: &broadcast::Sender<ChannelEvent>,
) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(|m| m.as_str());

    let method = match method {
        Some(m) => m,
        None => {
            // Notification (no id) -- ignore
            return None;
        }
    };

    match method {
        "initialize" => Some(success_response(
            &id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "legion-channel",
                    "version": version
                }
            }),
        )),

        "notifications/initialized" => {
            // Client acknowledgment -- no response needed
            None
        }

        "tools/list" => Some(success_response(
            &id,
            json!({
                "tools": tool_definitions()
            }),
        )),

        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or(Value::Null);
            let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let tool_args = params
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));

            match handle_tool_call(data_dir, tool_name, &tool_args, tx) {
                Ok(text) => {
                    let truncated = truncate(&text, None);
                    Some(success_response(&id, tool_result(&truncated)))
                }
                Err(e) => Some(error_response(&id, -32603, &e.to_string())),
            }
        }

        other => {
            eprintln!("[legion mcp] unknown method: {other}");
            Some(error_response(
                &id,
                -32601,
                &format!("method not found: {other}"),
            ))
        }
    }
}

/// Run the MCP stdio server loop.
///
/// Reads newline-delimited JSON from stdin. Writes responses to stdout.
/// Blocks the calling thread (meant to run in spawn_blocking or a dedicated thread).
pub fn run_stdio_loop(
    data_dir: PathBuf,
    version: String,
    tx: broadcast::Sender<ChannelEvent>,
) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[legion mcp] stdin read error: {e}");
                break;
            }
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[legion mcp] parse error: {e}");
                let id = Value::Null;
                let resp = error_response(&id, -32700, "parse error");
                if let Ok(s) = serde_json::to_string(&resp) {
                    let _ = writeln!(out, "{}", s);
                    let _ = out.flush();
                }
                continue;
            }
        };

        if let Some(response) = dispatch(&request, &data_dir, &version, &tx) {
            match serde_json::to_string(&response) {
                Ok(s) => {
                    let _ = writeln!(out, "{}", s);
                    let _ = out.flush();
                }
                Err(e) => {
                    eprintln!("[legion mcp] serialize error: {e}");
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tx() -> broadcast::Sender<ChannelEvent> {
        let (tx, _rx) = broadcast::channel(16);
        tx
    }

    fn make_request(method: &str, params: Option<Value>) -> Value {
        let mut req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method
        });
        if let Some(p) = params {
            req["params"] = p;
        }
        req
    }

    #[test]
    fn initialize_response_shape() {
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        let req = make_request(
            "initialize",
            Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.0.1" }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx).expect("response");

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(!resp["result"]["capabilities"].is_null());
        assert_eq!(resp["result"]["serverInfo"]["name"], "legion-channel");
        assert_eq!(resp["result"]["serverInfo"]["version"], "0.6.0");
    }

    #[test]
    fn tools_list_returns_four_legion_tools() {
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        let req = make_request("tools/list", None);

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx).expect("response");

        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 4);

        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();

        assert!(names.contains(&"legion_post"), "missing legion_post");
        assert!(names.contains(&"legion_reply"), "missing legion_reply");
        assert!(names.contains(&"legion_signal"), "missing legion_signal");
        assert!(
            names.contains(&"legion_task_respond"),
            "missing legion_task_respond"
        );
    }

    /// Create a temp dir with `legion.db` and `index/` at the expected paths.
    /// The MCP handler always opens `data_dir/legion.db` and `data_dir/index`.
    fn mcp_test_dir() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("legion.db")).expect("open legion.db");
        // Initialize search index so handle_tool_call can open it.
        let _index = SearchIndex::open(&dir.path().join("index")).expect("open index");
        (db, dir)
    }

    #[test]
    fn legion_post_inserts_reflection() {
        let (db, dir) = mcp_test_dir();
        drop(db); // close so the mcp handler opens it fresh

        let tx = make_tx();
        let req = make_request(
            "tools/call",
            Some(json!({
                "name": "legion_post",
                "arguments": {
                    "repo": "kelex",
                    "text": "hello from MCP test"
                }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx).expect("response");

        // Should succeed
        assert!(
            resp.get("error").is_none(),
            "unexpected error: {:?}",
            resp["error"]
        );
        let content = &resp["result"]["content"];
        assert!(content.is_array());

        // Verify the DB has the post
        let db2 = Database::open(&dir.path().join("legion.db")).expect("open");
        let posts = db2.get_board_posts().expect("posts");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].repo, "kelex");
        assert_eq!(posts[0].text, "hello from MCP test");
        assert_eq!(posts[0].audience, "team");
    }

    #[test]
    fn legion_reply_formats_re_prefix() {
        let (db, dir) = mcp_test_dir();
        drop(db);

        let tx = make_tx();
        let req = make_request(
            "tools/call",
            Some(json!({
                "name": "legion_reply",
                "arguments": {
                    "repo": "rafters",
                    "post_id": "abc-123",
                    "text": "acknowledged"
                }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx).expect("response");
        assert!(
            resp.get("error").is_none(),
            "unexpected error: {:?}",
            resp["error"]
        );

        let db2 = Database::open(&dir.path().join("legion.db")).expect("open");
        let posts = db2.get_board_posts().expect("posts");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].text, "re:abc-123 -- acknowledged");
    }

    #[test]
    fn legion_signal_formats_signal_text() {
        let (db, dir) = mcp_test_dir();
        drop(db);

        let tx = make_tx();
        let req = make_request(
            "tools/call",
            Some(json!({
                "name": "legion_signal",
                "arguments": {
                    "repo": "kelex",
                    "to": "legion",
                    "verb": "review",
                    "status": "approved"
                }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx).expect("response");
        assert!(
            resp.get("error").is_none(),
            "unexpected error: {:?}",
            resp["error"]
        );

        let db2 = Database::open(&dir.path().join("legion.db")).expect("open");
        let posts = db2.get_board_posts().expect("posts");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].text, "@legion review:approved");
        assert!(crate::signal::is_signal(&posts[0].text));
    }

    #[test]
    fn unknown_tool_returns_error() {
        let (db, dir) = mcp_test_dir();
        drop(db);

        let tx = make_tx();
        let req = make_request(
            "tools/call",
            Some(json!({
                "name": "nonexistent_tool",
                "arguments": {}
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx).expect("response");
        assert!(resp.get("error").is_some(), "expected error response");
    }

    #[test]
    fn unknown_method_returns_error() {
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        let req = make_request("some/unknown/method", None);

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx).expect("response");
        assert!(resp.get("error").is_some());
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn notification_returns_none() {
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        // Notification: has method but no id
        let req = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx);
        assert!(resp.is_none(), "notifications should return None");
    }

    #[test]
    fn truncate_short_content_unchanged() {
        let short = "hello world";
        assert_eq!(truncate(short, None), short);
    }

    #[test]
    fn truncate_long_content_with_hint() {
        let long = "a".repeat(MAX_NOTIFICATION_LEN + 100);
        let result = truncate(&long, Some("test-id"));
        assert!(result.len() <= MAX_NOTIFICATION_LEN);
        assert!(result.contains("truncated"));
        assert!(result.contains("test-id"));
    }
}
