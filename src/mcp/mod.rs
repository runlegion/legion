//! Hand-rolled JSON-RPC 2.0 stdio server for the MCP protocol.
//!
//! Reads newline-delimited JSON from stdin, writes responses to stdout.
//! Implements only the subset of MCP that the legion channel uses:
//!   - initialize
//!   - tools/list
//!   - tools/call
//!
//! No Content-Length headers. Each message is a single JSON line.

mod log;
// pub(crate): src/deliver.rs (#941) reuses `notifier::should_notify` so the
// hook-drain lane is judged against the delivery filter the retired channel
// push used (#947), rather than duplicating the recipient/self-post logic.
pub(crate) mod notifier;
mod tools;

pub use self::log::{mcp_log_dir, mcp_log_path, mcp_trace, most_recent_mcp_log};

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::channel::ChannelEvent;
use crate::error::Result;

use self::log::redirect_stderr_to_log;
use self::notifier::resolve_session_repo_from_cwd;
use self::tools::{
    error_response, handle_tool_call, success_response, tool_definitions, tool_error, tool_result,
    truncate,
};

/// Protocol version string returned by initialize.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Dispatch a single JSON-RPC 2.0 message, returning the response as a Value.
///
/// Returns None for notifications (which have no id) -- not used here but
/// guards against future notification handling.
///
/// `client_repo_cell` is populated on the `initialize` call so tool calls
/// arriving later in the session can be attributed to the connecting agent
/// rather than to the client software.
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
            // This is stored for tool-call attribution via the shared
            // client_repo cell passed into run_stdio_loop. OnceLock
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
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "legion-channel",
                        "version": version
                    },
                    "instructions": "Bullpen posts and signals reach a live session through the hook-drain lane: plugin/hooks/delivery-drain.sh runs `legion deliver drain --repo <repo>` on UserPromptSubmit, PostToolUse and Stop, and injects any undelivered posts as additionalContext. No MCP push, no manual polling."
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

/// Run the MCP stdio server loop.
///
/// Reads newline-delimited JSON from stdin. Writes JSON-RPC responses to
/// stdout. Strictly request/response: the server never initiates a message.
/// It used to also run a polling thread that pushed
/// `notifications/claude/channel` frames for new bullpen rows; that lane was
/// retired in #947 once the hook drain (#941) was measured at delivery parity
/// with it, so a live session now hears about posts through
/// `legion deliver drain` on the plugin's hook events instead.
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

    // Stdout writer, owned outright by this loop. It was an
    // `Arc<Mutex<..>>` while the notifier thread shared it and the Mutex
    // kept the two writers' lines from interleaving; since #947 retired
    // that thread the request loop is the sole writer, so there is nothing
    // left to serialise against.
    let stdout = std::io::stdout();
    let mut out: std::io::BufWriter<std::io::Stdout> = std::io::BufWriter::new(stdout);

    // Which repo the connected client belongs to, for attributing the tool
    // calls that arrive later in the session.
    //
    // Pre-populated from cwd via watch.toml when possible, so the server
    // knows the *agent* identity (kessel, legion, ...) rather than the
    // *client software* identity (`claude-code`, the literal value every
    // Claude Code session sends in initialize.clientInfo.name). Without this
    // pre-fill every session collapses onto the same `claude-code` name.
    // See #400.
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
            if let Ok(s) = serde_json::to_string(&resp) {
                let _ = writeln!(out, "{s}");
                let _ = out.flush();
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
                if let Ok(s) = serde_json::to_string(&resp) {
                    let _ = writeln!(out, "{s}");
                    let _ = out.flush();
                }
                continue;
            }
        };

        if let Some(response) =
            dispatch(&request, &data_dir, &version, &tx, Some(&client_repo_cell))
        {
            match serde_json::to_string(&response) {
                Ok(s) => {
                    let _ = writeln!(out, "{s}");
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
pub(crate) mod testutil {
    use serde_json::{Value, json};
    use tokio::sync::broadcast;

    use crate::channel::ChannelEvent;
    use crate::db::Database;
    use crate::search::SearchIndex;

    pub(crate) fn make_tx() -> broadcast::Sender<ChannelEvent> {
        let (tx, _rx) = broadcast::channel(16);
        tx
    }

    pub(crate) fn make_request(method: &str, params: Option<Value>) -> Value {
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

    /// Create a temp dir with `legion.db` and `index/` at the expected paths.
    /// The MCP handler always opens `data_dir/legion.db` and `data_dir/index`.
    pub(crate) fn mcp_test_dir() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("legion.db")).expect("open legion.db");
        // Initialize search index so handle_tool_call can open it.
        let _index = SearchIndex::open(&dir.path().join("index")).expect("open index");
        (db, dir)
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{make_request, make_tx};
    use super::*;

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
        // #947: the `experimental.claude/channel` advertisement is what told
        // the Claude Code host to subscribe to server-initiated pushes. It
        // must be gone, not merely empty -- a host that still sees the
        // capability would wait for frames this server no longer sends.
        assert!(
            resp["result"]["capabilities"].get("experimental").is_none(),
            "initialize must not advertise an experimental capability; got: {}",
            resp["result"]["capabilities"]
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
    fn notifier_health_method_is_removed() {
        // #947: `legion/notifier_health` probed the retired push thread. It
        // must now fall through to the generic unknown-method arm rather
        // than answering with a stale `unknown` health verdict -- a method
        // that still responds reads as "the notifier is merely quiet" to
        // any operator or watchdog still calling it.
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        let req = make_request("legion/notifier_health", None);

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None).expect("response");
        assert!(
            resp.get("result").is_none(),
            "retired method must not return a result; got: {resp}"
        );
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
        // #947: instructions are read by the connecting model, so they are
        // the one place a stale description actively misleads -- an agent
        // told to expect pushed frames would sit waiting for delivery
        // instead of reading its injected hook context.
        assert!(
            !instructions.contains("notifications/claude/channel"),
            "instructions must not describe the retired push; got: {instructions}"
        );
        assert!(
            instructions.contains("legion deliver drain"),
            "instructions must point at the hook-drain lane; got: {instructions}"
        );
    }
}
