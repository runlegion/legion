/// Hand-rolled JSON-RPC 2.0 stdio server for the MCP protocol.
///
/// Reads newline-delimited JSON from stdin, writes responses to stdout.
/// Implements only the subset of MCP that the legion channel uses:
///   - initialize
///   - tools/list
///   - tools/call
///
/// No Content-Length headers. Each message is a single JSON line.
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::board;
use crate::channel::ChannelEvent;
use crate::db::Database;
use crate::error::{LegionError, Result};
use crate::search::SearchIndex;
use crate::signal as sig;
use crate::task;

/// Protocol version string returned by initialize.
const PROTOCOL_VERSION: &str = "2024-11-05";

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

/// Maximum tool result content length before truncation.
const MAX_TOOL_RESULT_LEN: usize = 2000;

/// Tool definitions returned by tools/list. Shape is a public contract -- external MCP clients pin to these field names.
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

/// Build a tool error response per MCP spec 2024-11-05.
///
/// Tool execution errors are returned in the SUCCESS envelope with `isError: true`,
/// not as JSON-RPC error responses. JSON-RPC errors are reserved for protocol-level
/// failures (parse errors, method not found, invalid request envelope).
fn tool_error(id: &Value, err: &LegionError) -> Value {
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
fn truncate(content: &str) -> String {
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

/// Dispatch a single JSON-RPC 2.0 message, returning the response as a Value.
///
/// Returns None for notifications (which have no id) -- not used here but
/// guards against future notification handling.
///
/// `client_repo_cell` is populated on the `initialize` call so the notification
/// emitter thread can learn the client's repo identity after the handshake.
pub fn dispatch(
    request: &Value,
    data_dir: &std::path::Path,
    version: &str,
    tx: &broadcast::Sender<ChannelEvent>,
    client_repo_cell: Option<&Arc<OnceLock<String>>>,
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
        "initialize" => {
            // Extract clientInfo.name to identify the connecting agent's repo.
            // This is stored for use by the notification emitter thread via
            // the shared client_repo cell passed into run_stdio_loop. OnceLock
            // is deliberate: the MCP subprocess is spawned fresh per Claude
            // Code session, so there is exactly one initialize handshake per
            // process lifetime. A second initialize (unexpected under the
            // current plugin model) would silently no-op -- documented here
            // so future deployment changes catch it.
            if let Some(cell) = client_repo_cell {
                if let Some(name) = request
                    .get("params")
                    .and_then(|p| p.get("clientInfo"))
                    .and_then(|ci| ci.get("name"))
                    .and_then(|n| n.as_str())
                {
                    if cell.set(name.to_string()).is_err() {
                        mcp_trace("mcp.initialize.duplicate", &[("ignored_name", name)]);
                        eprintln!(
                            "[legion mcp] duplicate initialize ignored; client_repo already set (one process = one session)"
                        );
                    } else {
                        mcp_trace("mcp.initialize", &[("client_repo", name)]);
                    }
                } else {
                    mcp_trace("mcp.initialize", &[("client_repo", "<missing>")]);
                }
            }
            Some(success_response(
                &id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {
                        "tools": {},
                        "experimental": {
                            "claude/channel": {}
                        }
                    },
                    "serverInfo": {
                        "name": "legion-channel",
                        "version": version
                    },
                    "instructions": "Incoming bullpen posts and signals arrive as JSON-RPC notifications with method notifications/claude/channel. Each notification params.content is an XML-like <channel> tag: <channel type=\"feed\" post_id=\"<uuid>\" repo=\"<repo>\" is_signal=\"<bool>\"><text><![CDATA[post text]]></text></channel>. Read these notifications and integrate them into your working context. No manual polling needed."
                }),
            ))
        }

        "notifications/initialized" => {
            // Client acknowledgment -- no response needed
            None
        }

        // Per MCP spec 2024-11-05, server must respond to ping with an empty
        // result. Claude Code sends ping at ~5min intervals and SIGTERMs the
        // MCP subprocess if we return an error or fail to respond, which
        // silently breaks channel delivery mid-session. See anthropics/claude-code#54544.
        "ping" => Some(success_response(&id, json!({}))),

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
                    let truncated = truncate(&text);
                    Some(success_response(&id, tool_result(&truncated)))
                }
                // Per MCP spec 2024-11-05: tool execution errors go in the success
                // envelope with isError:true, not as JSON-RPC error responses.
                Err(e) => Some(tool_error(&id, &e)),
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

/// Maximum bytes accepted per input line. Rejects oversized messages to
/// prevent unbounded memory growth from a malicious or misbehaving client.
const MAX_LINE_BYTES: usize = 1024 * 1024; // 1 MiB

/// Determine whether a notification for a post should be delivered to this client.
///
/// Rules (applied in order):
/// 1. If the text starts with `@all`, deliver unconditionally (broadcast signal).
/// 2. If the text starts with `@<client_repo>` (direct mention), deliver.
/// 3. If the text starts with `@` but NOT addressed to this client, suppress.
/// 4. If `client_repo` is known and the post's `repo` equals `client_repo`, suppress
///    (the client wrote it; no need to echo a general musing back to its author).
/// 5. Otherwise (general musing, no `@` prefix, from a different agent), deliver.
///
/// Recipient parsing treats anything after a leading `@` up to the first
/// whitespace as the recipient token, with a trailing `:` trimmed. An empty
/// recipient (`@` alone) or a recipient that itself begins with `@` (e.g.
/// `@@all`, which looks like a broadcast but isn't) is NOT treated as `@all`
/// or any named target -- the post falls through the signal branch and is
/// suppressed. This is deliberately strict: if an agent fat-fingers a
/// broadcast as `@@all`, it should silently fail rather than silently succeed
/// with the wrong-looking prefix.
pub fn should_notify(text: &str, repo: &str, client_repo: Option<&str>) -> bool {
    if let Some(rest) = text.strip_prefix('@') {
        // It's a signal. Extract the recipient token (first whitespace word).
        let recipient_raw = rest.split_whitespace().next().unwrap_or("");
        let recipient = recipient_raw.trim_end_matches(':');

        // Reject empty or `@`-prefixed recipients -- `@` alone, `@@all`,
        // `@@`, etc. These are suppressed rather than passed to the @all /
        // named-target branches. See docstring above for the reasoning.
        if recipient.is_empty() || recipient.starts_with('@') {
            return false;
        }

        if recipient == "all" {
            return true;
        }
        if let Some(cr) = client_repo {
            return recipient == cr;
        }
        // No client_repo known -- suppress signals (can't verify recipient).
        return false;
    }

    // General musing: suppress own posts, deliver everything else.
    if let Some(cr) = client_repo
        && repo == cr
    {
        return false;
    }

    true
}

/// Split a CDATA body around any literal `]]>` occurrences so the terminator
/// cannot escape the section. The standard XML trick is to replace every
/// `]]>` with `]]]]><![CDATA[>` -- close the current section after the first
/// `]]`, then reopen with `<![CDATA[` before the stray `>`. An agent post
/// containing the literal substring `]]>` (plausible in code snippets) would
/// otherwise terminate the block early and inject raw content into the XML.
fn escape_cdata(text: &str) -> String {
    text.replace("]]>", "]]]]><![CDATA[>")
}

/// Escape `"`, `<`, `>`, and `&` for use inside an XML attribute value.
/// The attribute values are short (post_id, repo, is_signal) and controlled,
/// but post_id comes from the DB and repo from the user; better to escape than
/// trust.
fn escape_xml_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Build the XML-like channel notification content string. Text goes inside a
/// CDATA block with `]]>` sequences neutralised; attribute values are
/// XML-escaped.
fn build_channel_content(post_id: &str, repo: &str, text: &str, is_signal: bool) -> String {
    let post_id_attr = escape_xml_attr(post_id);
    let repo_attr = escape_xml_attr(repo);
    let text_body = escape_cdata(text);
    format!(
        "<channel type=\"feed\" post_id=\"{post_id_attr}\" repo=\"{repo_attr}\" is_signal=\"{is_signal}\"><text><![CDATA[{text_body}]]></text></channel>"
    )
}

/// Default poll interval for the MCP notifier thread. Overridable via
/// `LEGION_MCP_POLL_MS` for integration tests that want a tighter loop.
const DEFAULT_MCP_POLL_MS: u64 = 500;

/// Maximum rows the notifier reads per poll tick. Bounds memory and stdout
/// mutex hold time if a burst of writes lands between ticks. Anything beyond
/// the cap is picked up on the next poll because the cursor advances to the
/// last delivered row.
const NOTIFIER_BATCH_LIMIT: usize = 100;

/// Read the notifier poll interval from the environment, falling back to the
/// default. Invalid values (non-numeric, zero) fall back silently -- the
/// failure mode is "notifier ticks at the default rate", not crash.
fn mcp_poll_interval() -> std::time::Duration {
    let ms = std::env::var("LEGION_MCP_POLL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MCP_POLL_MS);
    std::time::Duration::from_millis(ms)
}

/// Replay window applied at cold boot when this recipient has no
/// `board_reads` cursor yet -- a fresh agent picks up directed signals filed
/// in the last 24 hours instead of starting from the live watermark and
/// silently swallowing them. Bounded so the first boot after a long absence
/// is not a flood.
const NOTIFIER_COLD_BOOT_REPLAY: chrono::Duration = chrono::Duration::hours(24);

/// Resolve the agent name for the current MCP subprocess from `watch.toml`
/// keyed on cwd.
///
/// The MCP `initialize` handshake reports `clientInfo.name = "claude-code"`
/// for every Claude Code session, which is the *client software* identity,
/// not the *agent* identity. Routing channel notifications by that token
/// breaks every directed signal because every session collides on the same
/// name. The agent identity is what `legion --repo <name>` carries on every
/// CLI call; here we recover it by canonicalising cwd and looking up the
/// matching `WatchRepoConfig.recipient()`.
///
/// Returns `None` (and the caller falls back to the legacy `clientInfo.name`
/// handshake value) when:
///   - watch.toml is missing or empty
///   - cwd cannot be canonicalised
///   - no entry's canonicalised workdir matches the current cwd
///
/// All three failure modes are non-fatal: a misconfigured workstation gets
/// the pre-fix behaviour (broadcasts only) rather than no channel at all.
fn resolve_session_repo_from_cwd(data_dir: &std::path::Path) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    resolve_session_repo_for_cwd(data_dir, &cwd)
}

/// Inner form of [`resolve_session_repo_from_cwd`] with the cwd injected.
/// Split out so unit tests can exercise the watch.toml lookup against a
/// fixture directory without mutating the global process cwd.
fn resolve_session_repo_for_cwd(
    data_dir: &std::path::Path,
    cwd: &std::path::Path,
) -> Option<String> {
    let watch_path = data_dir.join("watch.toml");
    let repos = match crate::watch::list_repos_in_config(&watch_path) {
        Ok(r) if !r.is_empty() => r,
        _ => return None,
    };

    let cwd_canon = std::fs::canonicalize(cwd).ok()?;

    for repo in repos {
        let workdir = std::path::Path::new(&repo.workdir);
        if let Ok(workdir_canon) = std::fs::canonicalize(workdir)
            && workdir_canon == cwd_canon
        {
            return Some(repo.recipient().to_string());
        }
    }
    None
}

/// Compute the initial `(last_seen_at, last_seen_id)` cursor for the
/// notifier thread.
///
/// Three-way resolution (#400):
///
///   1. **Known recipient with a `board_reads` cursor** -- seed from
///      that timestamp. The cursor advances on every successful delivery
///      so subsequent boots see only what arrived since the last delivery.
///   2. **Known recipient, no cursor yet** -- seed at
///      `now - NOTIFIER_COLD_BOOT_REPLAY` so a fresh agent picks up the
///      recent past instead of starting at the live watermark and
///      silently swallowing offline-window posts. `should_notify` and
///      `resolved_at IS NULL` keep replay narrow.
///   3. **Unknown recipient** (no watch.toml entry for cwd) -- fall back
///      to the pre-#400 watermark. The notifier cannot route directed
///      signals in that state anyway, so behaviour matches the prior
///      version: live posts only, no replay.
///
/// In case 1 the id comes from `board_reads.last_read_id`, written
/// alongside the timestamp on every successful delivery, so the
/// strict-`>` comparator in `get_board_posts_since` excludes the
/// already-delivered row even when its `created_at` collides with a
/// neighbour. In case 2 the id is empty (no prior delivery exists), and
/// in case 3 the id comes from the watermark row.
fn seed_notifier_cursor(db: &Database, client_repo: Option<&str>) -> Result<(String, String)> {
    if let Some(recipient) = client_repo {
        match db.get_board_read_cursor(recipient)? {
            Some((ts, id)) => {
                mcp_trace(
                    "notifier.cursor.seed",
                    &[
                        ("at", &ts),
                        ("id", &id),
                        ("source", "board_reads"),
                        ("recipient", recipient),
                    ],
                );
                Ok((ts, id))
            }
            None => {
                let backstop = (chrono::Utc::now() - NOTIFIER_COLD_BOOT_REPLAY).to_rfc3339();
                mcp_trace(
                    "notifier.cursor.seed",
                    &[
                        ("at", &backstop),
                        ("source", "cold_boot_replay"),
                        ("recipient", recipient),
                    ],
                );
                Ok((backstop, String::new()))
            }
        }
    } else {
        match db.get_board_cursor_watermark()? {
            Some((ts, id)) => {
                mcp_trace(
                    "notifier.cursor.seed",
                    &[("at", &ts), ("id", &id), ("source", "watermark")],
                );
                Ok((ts, id))
            }
            None => {
                let now = chrono::Utc::now().to_rfc3339();
                mcp_trace(
                    "notifier.cursor.seed",
                    &[("at", &now), ("source", "now_empty_table")],
                );
                Ok((now, String::new()))
            }
        }
    }
}

/// Body of the notifier thread spawned by `run_stdio_loop`.
///
/// Polls the `reflections` table for new bullpen rows and writes a
/// `notifications/claude/channel` JSON-RPC frame to the shared stdout writer
/// for each row that passes the recipient filter. Shared state (stdout,
/// client_repo cell) is passed in by the caller so the thread can be tested
/// independently from stdio wiring.
///
/// Exits cleanly on a stdout write failure (client hung up, EPIPE) or on a
/// poisoned stdout mutex. Transient database errors are logged and the loop
/// continues -- the notifier is a best-effort push channel, not a strict
/// delivery guarantee. The `last_seen_at` cursor advances only when a poll
/// succeeds, so a transient failure does not lose events on recovery.
fn run_notifier_loop(
    data_dir: PathBuf,
    out: Arc<Mutex<std::io::BufWriter<std::io::Stdout>>>,
    client_repo_cell: Arc<OnceLock<String>>,
) {
    let db = match Database::open(&data_dir.join("legion.db")) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[legion mcp notif] failed to open db: {e}; notifier thread exiting");
            return;
        }
    };

    let (mut last_seen_at, mut last_seen_id): (String, String) = match seed_notifier_cursor(
        &db,
        client_repo_cell.get().map(String::as_str),
    ) {
        Ok(seed) => seed,
        Err(e) => {
            mcp_trace("notifier.seed.failed", &[("err", &e.to_string())]);
            eprintln!(
                "[legion mcp notif] failed to seed cursor: {e}; notifier thread exiting (channel push is now inoperative for this session)"
            );
            return;
        }
    };
    mcp_trace(
        "notifier.start",
        &[(
            "poll_interval_ms",
            &format!("{}", mcp_poll_interval().as_millis()),
        )],
    );

    let poll_interval = mcp_poll_interval();
    let mut consecutive_cap_hits: u32 = 0;
    // #393: a transient stdout write/flush blip used to kill the notifier
    // thread permanently, leaving the MCP responsive to JSON-RPC but unable
    // to push any further notifications -- the symptom is "agents miss every
    // post except the one immediately after their fresh session start."
    // Track consecutive write failures and only exit after the configured
    // threshold so a brief EPIPE or back-pressure event does not silently
    // dark the channel forever.
    let mut consecutive_write_failures: u32 = 0;
    const MAX_CONSECUTIVE_WRITE_FAILURES: u32 = 5;

    loop {
        std::thread::sleep(poll_interval);

        let new_posts =
            match db.get_board_posts_since(&last_seen_at, &last_seen_id, NOTIFIER_BATCH_LIMIT) {
                Ok(posts) => posts,
                Err(e) => {
                    mcp_trace("notifier.poll.failed", &[("err", &e.to_string())]);
                    eprintln!("[legion mcp notif] db poll failed: {e}; continuing");
                    continue;
                }
            };

        if mcp_verbose() {
            mcp_trace(
                "notifier.poll",
                &[
                    ("cursor_at", &last_seen_at),
                    ("cursor_id", &last_seen_id),
                    ("returned", &new_posts.len().to_string()),
                ],
            );
        }

        if new_posts.is_empty() {
            consecutive_cap_hits = 0;
            continue;
        }

        // Surface a breadcrumb when the notifier hits the batch cap on
        // back-to-back ticks. Hitting the cap occasionally is expected
        // under normal activity bursts; hitting it repeatedly means the
        // notifier is falling behind (a misbehaving spammer, or a batch
        // import). This is the only diagnostic for "delivery is minutes
        // behind because we are saturated," which would otherwise be
        // indistinguishable from "the team is quiet."
        if new_posts.len() == NOTIFIER_BATCH_LIMIT {
            consecutive_cap_hits = consecutive_cap_hits.saturating_add(1);
            if consecutive_cap_hits >= 3 {
                eprintln!(
                    "[legion mcp notif] hit NOTIFIER_BATCH_LIMIT ({}) on {} consecutive polls; delivery may be lagging real time",
                    NOTIFIER_BATCH_LIMIT, consecutive_cap_hits
                );
            }
        } else {
            consecutive_cap_hits = 0;
        }

        // Advance the cursor to the newest row we saw. Rows are ordered
        // ascending by `(created_at, id)`, so the last element is the
        // newest. This must happen unconditionally regardless of whether
        // individual rows are delivered or suppressed, or a suppressed
        // post (own-post, wrong signal target) would be re-scanned
        // forever.
        if let Some(last) = new_posts.last() {
            last_seen_at = last.created_at.clone();
            last_seen_id = last.id.clone();
        }

        let client_repo = client_repo_cell.get().map(String::as_str);

        for post in new_posts {
            let is_signal = crate::signal::is_signal(&post.text);

            // Log the "named signal suppressed because client_repo is
            // unknown" case exactly once per post, so that a stuck
            // initialize (or a client that omitted clientInfo.name) is
            // visible in the breadcrumb log instead of manifesting as
            // silent delivery failures. Other suppression cases (own post,
            // signal to a different agent) are expected and not logged.
            if client_repo.is_none() && is_signal && !post.text.starts_with("@all") {
                eprintln!(
                    "[legion mcp notif] suppressing signal {} -- client_repo unknown (initialize handshake missing or clientInfo.name absent)",
                    post.id
                );
            }

            let deliver = should_notify(&post.text, &post.repo, client_repo);
            if mcp_verbose() {
                let preview: String = post.text.chars().take(40).collect();
                mcp_trace(
                    "notifier.decision",
                    &[
                        ("post_id", &post.id),
                        ("from_repo", &post.repo),
                        ("client_repo", client_repo.unwrap_or("<unset>")),
                        ("is_signal", &is_signal.to_string()),
                        ("deliver", &deliver.to_string()),
                        ("text_prefix", &preview.replace('\n', " ")),
                    ],
                );
            }
            if !deliver {
                continue;
            }

            let content = build_channel_content(&post.id, &post.repo, &post.text, is_signal);
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "notifications/claude/channel",
                "params": {
                    "content": content
                }
            });

            let Ok(s) = serde_json::to_string(&notification) else {
                eprintln!("[legion mcp notif] failed to serialize notification");
                continue;
            };

            // Mutex poisoning here is catastrophic, not recoverable. The
            // same `Arc<Mutex<BufWriter<Stdout>>>` is shared with the
            // request loop running on the main thread; a poisoned mutex
            // means every subsequent `out.lock()` on EITHER side returns
            // Err, which would leave the MCP subprocess alive (still
            // accepting requests on stdin) but silently unable to write
            // any response or notification. That is strictly worse than
            // a dead subprocess: Claude Code can recover from a dead MCP
            // server by respawning, but it cannot detect a server that
            // accepts initialize, accepts tool calls, and quietly drops
            // every response. Abort the process so the client gets a
            // clean disconnect and can respawn.
            let Ok(mut locked) = out.lock() else {
                eprintln!(
                    "[legion mcp notif] stdout mutex poisoned; aborting process so claude code can respawn the mcp subprocess"
                );
                std::process::abort();
            };

            // A write or flush failure on stdout is usually EPIPE (client
            // hung up) but can also be a transient back-pressure event. The
            // historical behaviour was to exit the notifier on the first
            // failure, which silently darked the channel for the rest of
            // the session even when stdout recovered. Track consecutive
            // failures and only exit after MAX_CONSECUTIVE_WRITE_FAILURES
            // -- a long-dead pipe still gets us out of the loop, while a
            // single hiccup no longer kills delivery permanently. The
            // mutex-poisoned case above stays as `abort()` because that
            // one is genuinely unrecoverable.
            //
            // The cursor was already advanced for this batch (at the top
            // of the for-loop's enclosing scope), so failed posts are not
            // retried -- accept the loss in exchange for keeping the
            // thread alive. Loss-tolerant beats dead-tolerant.
            let write_ok = writeln!(locked, "{s}").is_ok() && locked.flush().is_ok();
            drop(locked);
            if write_ok {
                consecutive_write_failures = 0;
                // Advance the per-recipient delivery cursor so the next cold
                // boot resumes from this post rather than replaying it. The
                // upsert is forward-only; concurrent writers (e.g. the HTTP
                // backlog path's mark_board_read) cannot move the cursor
                // backwards and race us into re-delivery. Best-effort: a
                // failure here is logged but does not kill the loop -- worst
                // case is one redundant replay on the next boot.
                if let Some(recipient) = client_repo
                    && let Err(e) =
                        db.advance_board_read_cursor(recipient, &post.created_at, &post.id)
                {
                    mcp_trace(
                        "notifier.cursor.advance.failed",
                        &[
                            ("recipient", recipient),
                            ("post_id", &post.id),
                            ("err", &e.to_string()),
                        ],
                    );
                }
            } else {
                consecutive_write_failures = consecutive_write_failures.saturating_add(1);
                eprintln!(
                    "[legion mcp notif] stdout write failed ({}/{}) for post {}",
                    consecutive_write_failures, MAX_CONSECUTIVE_WRITE_FAILURES, post.id
                );
                if consecutive_write_failures >= MAX_CONSECUTIVE_WRITE_FAILURES {
                    eprintln!(
                        "[legion mcp notif] {} consecutive write failures; notifier thread exiting (channel push is now inoperative for this session)",
                        consecutive_write_failures
                    );
                    return;
                }
            }
        }
    }
}

/// Run the MCP stdio server loop.
///
/// Reads newline-delimited JSON from stdin. Writes JSON-RPC responses to stdout.
/// A concurrent notification emitter thread polls the legion database for new
/// bullpen rows and pushes `notifications/claude/channel` messages to stdout
/// when the post passes the recipient filter.
///
/// The polling design (rather than an in-process broadcast subscription) is
/// deliberate: MCP subprocesses are spawned one per Claude Code session, so
/// writes originating in a different session's MCP process, in a `legion post`
/// CLI invocation, or in the standalone HTTP daemon all need to reach this
/// notifier. `tokio::sync::broadcast` only fans out inside a single process,
/// so the previous broadcast-driven implementation silently missed every
/// cross-process write. SQLite polling is the lowest-friction bridge: no new
/// dependencies, no IPC primitive, and every write path already lands in the
/// same `reflections` table.
///
/// Blocks the calling thread (meant to run in spawn_blocking or a dedicated thread).
/// Lines larger than MAX_LINE_BYTES are rejected with a JSON-RPC parse error.
pub fn run_stdio_loop(
    data_dir: PathBuf,
    version: String,
    tx: broadcast::Sender<ChannelEvent>,
) -> Result<()> {
    // Redirect stderr to the per-PID log file (#395) before any other code
    // emits a diagnostic. Without this, every eprintln! is swallowed by
    // Claude Code's MCP transport and channel-darkness debugging is blind.
    redirect_stderr_to_log();
    mcp_trace(
        "mcp.start",
        &[
            ("data_dir", &data_dir.display().to_string()),
            ("version", &version),
        ],
    );

    // Shared stdout writer -- both the request loop and the notification thread
    // write to stdout. The Mutex serialises their writes so lines never interleave.
    let stdout = std::io::stdout();
    let out: Arc<Mutex<std::io::BufWriter<std::io::Stdout>>> =
        Arc::new(Mutex::new(std::io::BufWriter::new(stdout)));

    // Shared cell so the request loop can inform the notification thread which
    // repo the connected client belongs to.
    //
    // Pre-populated from cwd via watch.toml when possible, so the notifier
    // knows the *agent* identity (kessel, legion, ...) rather than the
    // *client software* identity (`claude-code`, the literal value every
    // Claude Code session sends in initialize.clientInfo.name). Without this
    // pre-fill, every directed signal collapses onto the same `claude-code`
    // recipient and is suppressed in every session. See #400.
    //
    // OnceLock is set first by cwd-resolution; the subsequent `initialize`
    // handler's set is a no-op (logged as duplicate), so the cwd answer
    // wins. Falls back to the handshake value when watch.toml has no entry
    // for cwd.
    let client_repo_cell: Arc<OnceLock<String>> = Arc::new(OnceLock::new());
    if let Some(name) = resolve_session_repo_from_cwd(&data_dir) {
        mcp_trace(
            "mcp.client_repo.resolved",
            &[("name", &name), ("source", "watch_toml_cwd")],
        );
        let _ = client_repo_cell.set(name);
    } else {
        mcp_trace("mcp.client_repo.resolved", &[("source", "unresolved")]);
    }

    // Spawn the notification emitter thread before entering the blocking read loop.
    let notif_out = Arc::clone(&out);
    let notif_data_dir = data_dir.clone();
    let notif_client_repo = Arc::clone(&client_repo_cell);

    std::thread::spawn(move || {
        run_notifier_loop(notif_data_dir, notif_out, notif_client_repo);
    });

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut buf: Vec<u8> = Vec::with_capacity(4096);

    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("[legion mcp] stdin read error: {e}");
                break;
            }
        }

        if buf.len() > MAX_LINE_BYTES {
            eprintln!(
                "[legion mcp] oversized message ({} bytes), rejecting",
                buf.len()
            );
            let id = Value::Null;
            let resp = error_response(&id, -32700, "message too large");
            if let Ok(s) = serde_json::to_string(&resp)
                && let Ok(mut locked) = out.lock()
            {
                let _ = writeln!(locked, "{s}");
                let _ = locked.flush();
            }
            continue;
        }

        let line = String::from_utf8_lossy(&buf);
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
                if let Ok(s) = serde_json::to_string(&resp)
                    && let Ok(mut locked) = out.lock()
                {
                    let _ = writeln!(locked, "{s}");
                    let _ = locked.flush();
                }
                continue;
            }
        };

        if let Some(response) =
            dispatch(&request, &data_dir, &version, &tx, Some(&client_repo_cell))
        {
            match serde_json::to_string(&response) {
                Ok(s) => {
                    if let Ok(mut locked) = out.lock() {
                        let _ = writeln!(locked, "{s}");
                        let _ = locked.flush();
                    }
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

    #[test]
    fn resolve_session_repo_returns_none_when_watch_toml_missing() {
        let data_dir = tempfile::tempdir().expect("data dir");
        let cwd = tempfile::tempdir().expect("cwd dir");
        assert_eq!(
            resolve_session_repo_for_cwd(data_dir.path(), cwd.path()),
            None
        );
    }

    #[test]
    fn resolve_session_repo_matches_canonicalized_workdir() {
        let data_dir = tempfile::tempdir().expect("data dir");
        let cwd = tempfile::tempdir().expect("cwd dir");
        let watch_path = data_dir.path().join("watch.toml");

        crate::watch::add_repo_to_config(&watch_path, "kessel", cwd.path(), None)
            .expect("add repo");

        assert_eq!(
            resolve_session_repo_for_cwd(data_dir.path(), cwd.path()).as_deref(),
            Some("kessel")
        );
    }

    #[test]
    fn resolve_session_repo_prefers_agent_alias_over_name() {
        let data_dir = tempfile::tempdir().expect("data dir");
        let cwd = tempfile::tempdir().expect("cwd dir");
        let watch_path = data_dir.path().join("watch.toml");

        crate::watch::add_repo_to_config(&watch_path, "kessel", cwd.path(), Some("kessel-agent"))
            .expect("add repo");

        assert_eq!(
            resolve_session_repo_for_cwd(data_dir.path(), cwd.path()).as_deref(),
            Some("kessel-agent")
        );
    }

    #[test]
    fn seed_notifier_cursor_unknown_client_uses_watermark_when_table_empty() {
        let (db, _dir) = mcp_test_dir();
        let (ts, id) = seed_notifier_cursor(&db, None).expect("seed");
        // Empty board -> seed at now() with empty id.
        assert!(id.is_empty());
        // Cannot equality-test `now`, but it parses as RFC3339.
        chrono::DateTime::parse_from_rfc3339(&ts).expect("valid rfc3339");
    }

    #[test]
    fn seed_notifier_cursor_known_recipient_no_history_uses_cold_boot_replay() {
        let (db, _dir) = mcp_test_dir();
        let (ts, id) = seed_notifier_cursor(&db, Some("kessel")).expect("seed");
        assert!(id.is_empty());
        let parsed = chrono::DateTime::parse_from_rfc3339(&ts).expect("valid rfc3339");
        let age = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
        // Should be roughly 24h old; allow 1h slack for slow runners.
        let lo = NOTIFIER_COLD_BOOT_REPLAY - chrono::Duration::hours(1);
        let hi = NOTIFIER_COLD_BOOT_REPLAY + chrono::Duration::hours(1);
        assert!(
            age >= lo && age <= hi,
            "expected ~24h backstop, got {}",
            age
        );
    }

    #[test]
    fn seed_notifier_cursor_known_recipient_with_history_uses_board_reads() {
        let (db, _dir) = mcp_test_dir();
        let pinned_ts = "2026-04-01T12:00:00Z";
        let pinned_id = "019dabcd-0000-7000-8000-000000000001";
        db.advance_board_read_cursor("kessel", pinned_ts, pinned_id)
            .unwrap();
        let (ts, id) = seed_notifier_cursor(&db, Some("kessel")).expect("seed");
        assert_eq!(ts, pinned_ts);
        assert_eq!(id, pinned_id);
    }

    #[test]
    fn cold_boot_replay_picks_up_offline_signal_then_advance_prevents_redelivery() {
        // End-to-end #400: signal filed while kessel is offline lands on
        // first poll; advance_board_read_cursor on delivery means second
        // boot does not re-replay the same row.
        let (db, _dir) = mcp_test_dir();

        // Pretend a directed signal was filed an hour ago, before kessel boots.
        let post = db
            .insert_reflection("legion", "@kessel ping:open from a test", "team")
            .expect("insert");
        let post_id = post.id.clone();
        // Sanity: board_reads is empty for kessel.
        assert!(db.get_board_read_cursor("kessel").unwrap().is_none());

        // First boot: cold replay seed should be in the past, so the post is
        // visible via get_board_posts_since.
        let (seed_at, seed_id) = seed_notifier_cursor(&db, Some("kessel")).expect("seed");
        let visible = db
            .get_board_posts_since(&seed_at, &seed_id, NOTIFIER_BATCH_LIMIT)
            .expect("posts");
        let found = visible.iter().any(|p| p.id == post_id);
        assert!(found, "cold-boot replay must surface the offline signal");

        // Simulate successful delivery: advance the cursor to the post's
        // (created_at, id).
        let delivered = visible
            .iter()
            .find(|p| p.id == post_id)
            .expect("post present");
        db.advance_board_read_cursor("kessel", &delivered.created_at, &delivered.id)
            .unwrap();

        // Second boot: seed comes from board_reads now, equal to the post's
        // created_at. Strict-`>` comparator means the post is NOT re-emitted.
        let (seed_at_2, seed_id_2) = seed_notifier_cursor(&db, Some("kessel")).expect("seed");
        let visible_2 = db
            .get_board_posts_since(&seed_at_2, &seed_id_2, NOTIFIER_BATCH_LIMIT)
            .expect("posts");
        assert!(
            !visible_2.iter().any(|p| p.id == post_id),
            "advanced cursor must not re-replay an already-delivered post"
        );
    }

    #[test]
    fn resolve_session_repo_returns_none_for_unmatched_cwd() {
        let data_dir = tempfile::tempdir().expect("data dir");
        let cwd = tempfile::tempdir().expect("cwd dir");
        let other = tempfile::tempdir().expect("other dir");
        let watch_path = data_dir.path().join("watch.toml");

        crate::watch::add_repo_to_config(&watch_path, "kessel", other.path(), None)
            .expect("add repo");

        assert_eq!(
            resolve_session_repo_for_cwd(data_dir.path(), cwd.path()),
            None
        );
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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");

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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");

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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");

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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");
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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");
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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");
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
    fn ping_returns_empty_result() {
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        let req = make_request("ping", None);

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert!(resp.get("error").is_none(), "ping must not return an error");
        assert_eq!(resp["result"], json!({}));
    }

    #[test]
    fn ping_is_idempotent_across_many_calls() {
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        for _ in 0..100 {
            let req = make_request("ping", None);
            let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");
            assert!(resp.get("error").is_none());
            assert_eq!(resp["result"], json!({}));
        }
    }

    #[test]
    fn unknown_method_returns_error() {
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        let req = make_request("some/unknown/method", None);

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");
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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None);
        assert!(resp.is_none(), "notifications should return None");
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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");
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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");
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

    // ---- notification tests ----

    #[test]
    fn initialize_response_includes_instructions() {
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        let req = make_request(
            "initialize",
            Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "kelex", "version": "0.0.1" }
            })),
        );

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");

        let instructions = resp["result"]["instructions"].as_str().unwrap_or("");
        assert!(
            !instructions.is_empty(),
            "instructions field must be present and non-empty"
        );
        assert!(
            instructions.contains("notifications/claude/channel"),
            "instructions must mention notifications/claude/channel; got: {instructions}"
        );
    }

    #[test]
    fn notification_filter_passes_at_all() {
        // @all signals should reach every client regardless of repo.
        assert!(
            should_notify("@all hello team", "smugglr", Some("kelex")),
            "@all must pass filter for kelex"
        );
        assert!(
            should_notify("@all hello team", "smugglr", Some("smugglr")),
            "@all must pass even for the poster's own client if the post repo differs"
        );
    }

    #[test]
    fn notification_filter_suppresses_wrong_recipient() {
        // A signal to @vault must not reach @kelex.
        assert!(
            !should_notify("@vault review:approved", "smugglr", Some("kelex")),
            "@vault signal must be suppressed for kelex client"
        );
        // A signal to @kelex MUST reach kelex.
        assert!(
            should_notify("@kelex review:approved", "smugglr", Some("kelex")),
            "@kelex signal must reach kelex client"
        );
        // Own post must be suppressed.
        assert!(
            !should_notify("hello team", "kelex", Some("kelex")),
            "own posts must be suppressed"
        );
        // General musing from another agent must reach the client.
        assert!(
            should_notify("just thinking about things", "smugglr", Some("kelex")),
            "general musings from others must reach kelex"
        );
    }

    #[test]
    fn notification_filter_rejects_malformed_signal_prefixes() {
        // `@` alone is not a broadcast -- no recipient token at all.
        assert!(
            !should_notify("@ hello", "smugglr", Some("kelex")),
            "lone @ must be suppressed"
        );
        // `@@all foo` looks like a broadcast but recipient parses as `@all`,
        // which starts with `@` -- rejected as malformed rather than silently
        // routed as if the user meant @all.
        assert!(
            !should_notify("@@all urgent", "smugglr", Some("kelex")),
            "@@all must be suppressed, not routed as @all"
        );
        // `@@` alone with no recipient.
        assert!(
            !should_notify("@@", "smugglr", Some("kelex")),
            "@@ alone must be suppressed"
        );
        // Trailing colon is stripped, so `@kelex:` still reaches kelex.
        assert!(
            should_notify("@kelex: review:approved", "smugglr", Some("kelex")),
            "trailing colon on recipient must still reach the target"
        );
    }

    #[test]
    fn build_channel_content_escapes_cdata_terminator() {
        // A post text containing the CDATA terminator `]]>` would otherwise
        // close the CDATA block early and leak raw content into the XML.
        // escape_cdata splits the terminator across a close/reopen using the
        // canonical `]]]]><![CDATA[>` pattern. An XML parser then sees the
        // original `]]>` in the reassembled CDATA content.
        let content = build_channel_content(
            "019d-test-id",
            "legion",
            "here is the literal terminator ]]> in a code example",
            false,
        );
        assert!(
            content.contains("]]]]><![CDATA[>"),
            "CDATA escape should split ]]> across a close-and-reopen; got: {content}"
        );
        // The legitimate final closer is still `]]></text></channel>`. That
        // is the ONE allowed occurrence of the `]]>` sequence -- and it
        // must close a balanced pair of CDATA opens.
        assert!(
            content.ends_with("]]></text></channel>"),
            "content must end with the correct closer; got: {content}"
        );
        let cdata_opens = content.matches("<![CDATA[").count();
        let cdata_closes = content.matches("]]>").count();
        assert_eq!(
            cdata_opens, cdata_closes,
            "CDATA opens/closes must balance after escape (opens={cdata_opens}, closes={cdata_closes}); got: {content}"
        );
    }

    #[test]
    fn build_channel_content_escapes_xml_attributes() {
        // Post id / repo go into attribute positions. A post from a repo
        // named with a literal quote or ampersand would otherwise break the
        // attribute quoting. Not expected in practice, but cheap to enforce.
        let content = build_channel_content("id\"with'quote", "repo&name", "plain text body", true);
        assert!(
            content.contains("id&quot;with"),
            "post_id attribute must be XML-escaped; got: {content}"
        );
        assert!(
            content.contains("repo&amp;name"),
            "repo attribute must be XML-escaped; got: {content}"
        );
    }
}
