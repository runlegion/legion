//! The MCP tool surface: tool definitions, JSON-RPC response envelopes,
//! tool-result truncation, and `handle_tool_call`. Carved from mcp.rs
//! (#612).

use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::board;
use crate::channel::ChannelEvent;
use crate::db::Database;
use crate::error::{LegionError, Result};
use crate::search::SearchIndex;
use crate::signal as sig;
use crate::task;

/// Maximum tool result content length before truncation.
const MAX_TOOL_RESULT_LEN: usize = 2000;

/// Tool definitions returned by tools/list. Shape is a public contract -- external MCP clients pin to these field names.
pub(super) fn tool_definitions() -> Value {
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
pub(super) fn success_response(id: &Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

/// Build a JSON-RPC 2.0 error response.
pub(super) fn error_response(id: &Value, code: i64, message: &str) -> Value {
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
pub(super) fn tool_result(text: &str) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ]
    })
}

/// Build a tool error response per MCP spec 2024-11-05.
///
/// Tool execution errors are returned in the SUCCESS envelope with `isError: true`,
/// not as JSON-RPC error responses. JSON-RPC errors are reserved for protocol-level
/// failures (parse errors, method not found, invalid request envelope).
pub(super) fn tool_error(id: &Value, err: &LegionError) -> Value {
    // Avoid leaking internal details (file paths, DB internals) for non-argument errors.
    let msg = match err {
        LegionError::McpInvalidArgument(m) => m.clone(),
        _ => format!("internal error: {err}"),
    };
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{"type": "text", "text": msg}],
            "isError": true
        }
    })
}

/// Truncate content to MAX_TOOL_RESULT_LEN with a trailing hint.
///
/// Cuts at a UTF-8 codepoint boundary to avoid panicking on multi-byte chars.
pub(super) fn truncate(content: &str) -> String {
    if content.len() <= MAX_TOOL_RESULT_LEN {
        return content.to_string();
    }
    let hint = "\n\n[truncated -- full content on bullpen]";
    let budget = MAX_TOOL_RESULT_LEN.saturating_sub(hint.len());
    let mut cut = 0usize;
    for (i, c) in content.char_indices() {
        if i + c.len_utf8() > budget {
            break;
        }
        cut = i + c.len_utf8();
    }
    format!("{}{hint}", &content[..cut])
}

/// Handle a single MCP tools/call dispatch.
///
/// Opens a fresh DB connection on each call (consistent with legion's
/// single-connection model for the CLI). The MCP server is long-lived but
/// calls are infrequent.
pub(super) fn handle_tool_call(
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
            let repo = get_str("repo")
                .ok_or_else(|| LegionError::McpInvalidArgument("repo is required".into()))?;
            let text = get_str("text")
                .ok_or_else(|| LegionError::McpInvalidArgument("text is required".into()))?;

            let db = Database::open(&data_dir.join("legion.db"))?;
            let index = SearchIndex::open(&data_dir.join("index"))?;

            // TODO(019d7991-2eab): compute and store embedding so this post is similarity-searchable
            let id = board::post_from_text_with_meta(
                &db,
                &index,
                &repo,
                text.trim(),
                &crate::db::ReflectionMeta::default(),
            )?;

            let _ = tx.send(ChannelEvent::Feed);

            Ok(format!("posted (id: {})", id))
        }

        "legion_reply" => {
            let repo = get_str("repo")
                .ok_or_else(|| LegionError::McpInvalidArgument("repo is required".into()))?;
            let post_id = get_str("post_id")
                .ok_or_else(|| LegionError::McpInvalidArgument("post_id is required".into()))?;
            let text = get_str("text")
                .ok_or_else(|| LegionError::McpInvalidArgument("text is required".into()))?;

            // Reply format "re:<post_id> -- <text>" is part of the bullpen text protocol; other agents parse this prefix.
            let reply_text = format!("re:{} -- {}", post_id, text.trim());

            let db = Database::open(&data_dir.join("legion.db"))?;
            let index = SearchIndex::open(&data_dir.join("index"))?;

            // TODO(019d7991-2eab): compute and store embedding so this post is similarity-searchable
            let id = board::post_from_text_with_meta(
                &db,
                &index,
                &repo,
                &reply_text,
                &crate::db::ReflectionMeta::default(),
            )?;

            let _ = tx.send(ChannelEvent::Feed);

            Ok(format!("replied (id: {})", id))
        }

        "legion_signal" => {
            let repo = get_str("repo")
                .ok_or_else(|| LegionError::McpInvalidArgument("repo is required".into()))?;
            let to = get_str("to")
                .ok_or_else(|| LegionError::McpInvalidArgument("to is required".into()))?;
            let verb = get_str("verb")
                .ok_or_else(|| LegionError::McpInvalidArgument("verb is required".into()))?;
            let status = get_str("status");
            let note = get_str("note");
            let details_str = get_str("details");

            // Guard: repo is the authoring context; to is the routing target.
            // When they are the same (case-insensitive) the signal is silently
            // dropped by the poll query, so reject it here the same way the
            // CLI arm does (#673 fix 1). Broadcasts (bare "all", "everyone",
            // or @-prefixed forms) are exempt. is_self_address is shared with
            // the CLI guard so the sentinel set cannot drift.
            if crate::signal::is_self_address(std::slice::from_ref(&repo), &to) {
                return Err(LegionError::McpInvalidArgument(format!(
                    "--repo and --to must differ: '{}' is the authoring repo context, not the \
                     recipient. To signal {}, use a different --repo value.",
                    repo, repo
                )));
            }

            // One compose/validate entry point shared with the CLI signal
            // arm (#612): details wire parsing, the #587 required-fields
            // gate, and the note length cap all live in signal::compose.
            // Validation failures are argument errors, not internal ones --
            // surface the message to the MCP client verbatim.
            let signal_text = sig::compose(
                &to,
                &verb,
                status.as_deref(),
                note.as_deref(),
                details_str.as_deref(),
                crate::verbs::active_manifest(),
            )
            .map_err(|e| match e {
                LegionError::SignalMissingRequiredFields { .. }
                | LegionError::SignalNoteTooLong { .. } => {
                    LegionError::McpInvalidArgument(e.to_string())
                }
                other => other,
            })?;

            let db = Database::open(&data_dir.join("legion.db"))?;
            let index = SearchIndex::open(&data_dir.join("index"))?;

            // TODO(019d7991-2eab): compute and store embedding so this post is similarity-searchable
            let id = board::post_from_text_with_meta(
                &db,
                &index,
                &repo,
                &signal_text,
                &crate::db::ReflectionMeta::default(),
            )?;

            let _ = tx.send(ChannelEvent::Feed);

            Ok(format!("signaled (id: {})", id))
        }

        "legion_task_respond" => {
            let task_id = get_str("task_id")
                .ok_or_else(|| LegionError::McpInvalidArgument("task_id is required".into()))?;
            let action = get_str("action")
                .ok_or_else(|| LegionError::McpInvalidArgument("action is required".into()))?;
            let note = get_str("note");

            let db = Database::open(&data_dir.join("legion.db"))?;

            match action.as_str() {
                "accept" => task::accept_task(&db, &task_id)?,
                "done" => task::complete_task(&db, &task_id, note.as_deref())?,
                "block" => task::block_task(&db, &task_id, note.as_deref())?,
                other => {
                    return Err(LegionError::McpInvalidArgument(format!(
                        "unknown action: {other}; expected accept, done, or block"
                    )));
                }
            }

            let _ = tx.send(ChannelEvent::Tasks);

            Ok(format!("task {}: {}", action, task_id))
        }

        other => Err(LegionError::McpInvalidArgument(format!(
            "unknown tool: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::dispatch;
    use crate::mcp::testutil::{make_request, make_tx, mcp_test_dir};

    #[test]
    fn tools_list_returns_four_legion_tools() {
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        let req = make_request("tools/list", None);

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");

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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");

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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
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
    fn legion_signal_enforces_required_fields() {
        // The #587 required-fields gate must hold on the MCP surface too --
        // it lived only in the CLI arm before #612, so an MCP rfc with no
        // budget sailed through. signal::compose is now the shared gate.
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
                    "verb": "rfc",
                    "note": "proposal with no budget"
                }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
        assert!(
            resp.get("error").is_none(),
            "validation failures are tool errors, not JSON-RPC errors"
        );
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("budget"),
            "error must name the missing field: {text}"
        );
        assert!(
            !text.starts_with("internal error:"),
            "missing required fields is an argument error, not internal: {text}"
        );

        // Nothing must have been posted.
        let db2 = Database::open(&dir.path().join("legion.db")).expect("open");
        assert!(db2.get_board_posts().expect("posts").is_empty());
    }

    #[test]
    fn legion_signal_with_required_fields_succeeds() {
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
                    "verb": "rfc",
                    "note": "proposal",
                    "details": "budget: 2h"
                }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
        assert!(resp.get("error").is_none());
        assert!(resp["result"].get("isError").is_none());

        let db2 = Database::open(&dir.path().join("legion.db")).expect("open");
        let posts = db2.get_board_posts().expect("posts");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].text, "@legion rfc {budget: 2h} -- proposal");
    }

    #[test]
    fn legion_signal_rejects_overlong_note_as_argument_error() {
        let (db, dir) = mcp_test_dir();
        drop(db);

        let tx = make_tx();
        let long_note = "a".repeat(sig::MAX_SIGNAL_NOTE_LENGTH + 1);
        let req = make_request(
            "tools/call",
            Some(json!({
                "name": "legion_signal",
                "arguments": {
                    "repo": "kelex",
                    "to": "legion",
                    "verb": "question",
                    "note": long_note
                }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            !text.starts_with("internal error:"),
            "note-too-long is an argument error, not internal: {text}"
        );
    }

    #[test]
    fn unknown_tool_returns_is_error() {
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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
        // Per MCP spec: tool errors go in the success envelope with isError:true,
        // NOT as a JSON-RPC error response.
        assert!(
            resp.get("error").is_none(),
            "tool errors must not be JSON-RPC errors"
        );
        assert_eq!(
            resp["result"]["isError"], true,
            "expected isError: true in result"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("nonexistent_tool"),
            "error text should name the tool"
        );
    }

    #[test]
    fn legion_post_missing_repo_returns_is_error() {
        let (db, dir) = mcp_test_dir();
        drop(db);

        let tx = make_tx();
        let req = make_request(
            "tools/call",
            Some(json!({
                "name": "legion_post",
                "arguments": {
                    "text": "missing repo"
                }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
        // Per MCP spec: McpInvalidArgument is a tool error, not a protocol error.
        // Must be in the success envelope with isError:true.
        assert!(
            resp.get("error").is_none(),
            "tool argument errors must not be JSON-RPC errors; got: {:?}",
            resp.get("error")
        );
        assert_eq!(
            resp["result"]["isError"], true,
            "expected isError: true in result"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("repo is required"),
            "expected 'repo is required' in error text, got: {text}"
        );
    }

    #[test]
    fn legion_signal_rejects_self_address_via_shared_guard() {
        // The MCP guard now calls crate::signal::is_self_address (same as the
        // CLI guard). Sending repo="legion" to="legion" must be rejected.
        let (db, dir) = mcp_test_dir();
        drop(db);

        let tx = make_tx();
        let req = make_request(
            "tools/call",
            Some(json!({
                "name": "legion_signal",
                "arguments": {
                    "repo": "legion",
                    "to": "legion",
                    "verb": "review",
                    "status": "approved"
                }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
        assert_eq!(
            resp["result"]["isError"], true,
            "self-address signal must be rejected"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("legion"),
            "error must name the conflicting repo: {text}"
        );
    }

    #[test]
    fn legion_signal_allows_broadcast_with_at_prefix() {
        // "@all" with a leading @ must be treated as a broadcast, not a
        // self-address, by the shared is_self_address guard.
        let (db, dir) = mcp_test_dir();
        drop(db);

        let tx = make_tx();
        let req = make_request(
            "tools/call",
            Some(json!({
                "name": "legion_signal",
                "arguments": {
                    "repo": "legion",
                    "to": "@all",
                    "verb": "announce",
                    "note": "broadcast test"
                }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
        // The signal itself should succeed (not be rejected as self-address).
        // It may fail for other reasons (e.g., missing required fields on the
        // verb) but must NOT fail with a self-address error.
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        let is_self_address_error =
            text.contains("authoring repo context") || text.contains("--repo and --to must differ");
        assert!(
            !is_self_address_error,
            "@all broadcast must not be rejected as a self-address: {text}"
        );
    }

    #[test]
    fn mcp_invalid_argument_is_not_json_rpc_error() {
        // Specifically assert McpInvalidArgument produces isError:true, not -32603.
        let (db, dir) = mcp_test_dir();
        drop(db);

        let tx = make_tx();
        // legion_signal requires "repo", "to", and "verb".
        let req = make_request(
            "tools/call",
            Some(json!({
                "name": "legion_signal",
                "arguments": {
                    "repo": "kelex"
                    // missing "to" and "verb"
                }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
        assert!(
            resp.get("error").is_none(),
            "McpInvalidArgument must not produce a JSON-RPC error envelope"
        );
        assert_eq!(resp["result"]["isError"], true);
        // The error text should describe what is missing (not internal server error).
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            !text.starts_with("internal error:"),
            "McpInvalidArgument should show the validation message, not 'internal error'"
        );
    }

    #[test]
    fn truncate_short_content_unchanged() {
        let short = "hello world";
        assert_eq!(truncate(short), short);
    }

    #[test]
    fn truncate_long_content_with_hint() {
        let long = "a".repeat(MAX_TOOL_RESULT_LEN + 100);
        let result = truncate(&long);
        assert!(result.len() <= MAX_TOOL_RESULT_LEN);
        assert!(result.contains("truncated"));
    }

    #[test]
    fn truncate_multibyte_safe() {
        // Build a string with multi-byte chars near the boundary.
        // Each Chinese character is 3 bytes in UTF-8.
        let repeated: String = "\u{4e2d}".repeat(MAX_TOOL_RESULT_LEN); // well over limit
        let result = truncate(&repeated);
        // Must not panic and must be valid UTF-8
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        assert!(result.len() <= MAX_TOOL_RESULT_LEN);
    }
}
