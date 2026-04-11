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
            if let Some(cell) = client_repo_cell
                && let Some(name) = request
                    .get("params")
                    .and_then(|p| p.get("clientInfo"))
                    .and_then(|ci| ci.get("name"))
                    .and_then(|n| n.as_str())
                && cell.set(name.to_string()).is_err()
            {
                eprintln!(
                    "[legion mcp] duplicate initialize ignored; client_repo already set (one process = one session)"
                );
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

    // Seed the cursor to "now" so the notifier only emits for posts that land
    // after the MCP subprocess started. Replaying historical bullpen content
    // would flood the session with messages the user has already seen.
    let mut last_seen_at = chrono::Utc::now().to_rfc3339();

    let poll_interval = mcp_poll_interval();

    loop {
        std::thread::sleep(poll_interval);

        let new_posts = match db.get_board_posts_since(&last_seen_at) {
            Ok(posts) => posts,
            Err(e) => {
                eprintln!("[legion mcp notif] db poll failed: {e}; continuing");
                continue;
            }
        };

        if new_posts.is_empty() {
            continue;
        }

        // Advance the cursor to the newest row we saw. Rows are ordered
        // ascending by `created_at`, so the last element is the newest.
        // This must happen unconditionally regardless of whether individual
        // rows are delivered or suppressed, or a suppressed post (own-post,
        // wrong signal target) would be re-scanned forever.
        if let Some(last) = new_posts.last() {
            last_seen_at = last.created_at.clone();
        }

        let client_repo = client_repo_cell.get().map(|s| s.as_str());

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

            if !should_notify(&post.text, &post.repo, client_repo) {
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

            let Ok(mut locked) = out.lock() else {
                eprintln!("[legion mcp notif] stdout mutex poisoned; notifier thread exiting");
                return;
            };

            // A write or flush failure on stdout almost always means the
            // client hung up (EPIPE). Continuing to loop against a dead pipe
            // would burn CPU silently forever -- exit the notifier thread
            // instead.
            if let Err(e) = writeln!(locked, "{s}") {
                eprintln!("[legion mcp notif] stdout write failed ({e}); notifier thread exiting");
                return;
            }
            if let Err(e) = locked.flush() {
                eprintln!("[legion mcp notif] stdout flush failed ({e}); notifier thread exiting");
                return;
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
    // Shared stdout writer -- both the request loop and the notification thread
    // write to stdout. The Mutex serialises their writes so lines never interleave.
    let stdout = std::io::stdout();
    let out: Arc<Mutex<std::io::BufWriter<std::io::Stdout>>> =
        Arc::new(Mutex::new(std::io::BufWriter::new(stdout)));

    // Shared cell so the request loop can inform the notification thread which
    // repo the connected client belongs to (extracted from initialize clientInfo.name).
    let client_repo_cell: Arc<OnceLock<String>> = Arc::new(OnceLock::new());

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
