/// Hand-rolled JSON-RPC 2.0 stdio server for the MCP protocol.
///
/// Reads newline-delimited JSON from stdin, writes responses to stdout.
/// Implements only the subset of MCP that the legion channel uses:
///   - initialize
///   - tools/list
///   - tools/call
///
/// No Content-Length headers. Each message is a single JSON line.

mod log;
mod notifier;
mod tools;

pub use self::log::{mcp_log_dir, mcp_log_path, mcp_trace, most_recent_mcp_log};
pub use self::notifier::{NotifierHealth, NotifierHeartbeat, classify_notifier_health};
// should_notify / replay_should_deliver are public mcp:: API consumed only
// inside the notifier; the re-export exists to keep the pre-split
// `mcp::` paths addressable (`mod mcp` is private in main.rs, so an
// un-called re-export would otherwise trip unused_imports).
#[allow(unused_imports)]
pub use self::notifier::{replay_should_deliver, should_notify};

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::channel::ChannelEvent;
use crate::error::Result;

use self::log::redirect_stderr_to_log;
use self::notifier::{mcp_poll_interval, resolve_session_repo_from_cwd, run_notifier_loop};
use self::tools::{
    error_response, handle_tool_call, success_response, tool_definitions, tool_error,
    tool_result, truncate,
};

/// Protocol version string returned by initialize.
const PROTOCOL_VERSION: &str = "2024-11-05";

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
    heartbeat: Option<&Arc<NotifierHeartbeat>>,
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

        // Legion-specific extension (#391): report the notifier thread's
        // health so an external diagnostic can tell whether channel push is
        // alive without round-tripping a real post. Returns `unknown` when
        // the heartbeat Arc was not provided (test / unit-dispatch path).
        "legion/notifier_health" => {
            let health = match heartbeat {
                Some(hb) => classify_notifier_health(
                    chrono::Utc::now().timestamp(),
                    hb,
                    mcp_poll_interval(),
                ),
                None => NotifierHealth::Unknown,
            };
            Some(success_response(
                &id,
                serde_json::to_value(&health).unwrap_or(json!({"state": "unknown"})),
            ))
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

    // Notifier heartbeat (#391): shared between the notifier thread (writer)
    // and the dispatch handler for `legion/notifier_health` (reader).
    let heartbeat: Arc<NotifierHeartbeat> = Arc::new(NotifierHeartbeat::new());

    // Spawn the notification emitter thread before entering the blocking read loop.
    let notif_out = Arc::clone(&out);
    let notif_data_dir = data_dir.clone();
    let notif_client_repo = Arc::clone(&client_repo_cell);
    let notif_heartbeat = Arc::clone(&heartbeat);

    std::thread::spawn(move || {
        run_notifier_loop(
            notif_data_dir,
            notif_out,
            notif_client_repo,
            notif_heartbeat,
        );
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

        if let Some(response) = dispatch(
            &request,
            &data_dir,
            &version,
            &tx,
            Some(&client_repo_cell),
            Some(&heartbeat),
        ) {
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
pub(crate) mod testutil {
    use serde_json::{Value, json};
    use tokio::sync::broadcast;

    use crate::channel::ChannelEvent;
    use crate::db::Database;
    use crate::search::SearchIndex;

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

    /// Create a temp dir with `legion.db` and `index/` at the expected paths.
    /// The MCP handler always opens `data_dir/legion.db` and `data_dir/index`.
    fn mcp_test_dir() -> (Database, tempfile::TempDir) {
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
    fn dispatch_notifier_health_returns_unknown_without_heartbeat() {
        // #391: when the heartbeat is not threaded in (unit-test path), the
        // method must still respond with a well-formed JSON-RPC success
        // result so callers can't crash on shape drift.
        let dir = tempfile::tempdir().expect("data dir");
        let tx = make_tx();
        let req = make_request("legion/notifier_health", None);
        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
        let result = resp.get("result").expect("result field");
        assert_eq!(
            result.get("state").and_then(|v| v.as_str()),
            Some("unknown")
        );
    }

    #[test]
    fn dispatch_notifier_health_returns_alive_with_fresh_heartbeat() {
        let dir = tempfile::tempdir().expect("data dir");
        let tx = make_tx();
        let hb = Arc::new(NotifierHeartbeat::new());
        hb.touch();
        let req = make_request("legion/notifier_health", None);
        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, Some(&hb)).expect("response");
        let result = resp.get("result").expect("result field");
        assert_eq!(result.get("state").and_then(|v| v.as_str()), Some("alive"));
        assert!(result.get("last_tick_secs_ago").is_some());
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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(!resp["result"]["capabilities"].is_null());
        assert_eq!(resp["result"]["serverInfo"]["name"], "legion-channel");
        assert_eq!(resp["result"]["serverInfo"]["version"], "0.6.0");
    }

    #[test]
    fn ping_returns_empty_result() {
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        let req = make_request("ping", None);

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
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
            let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
            assert!(resp.get("error").is_none());
            assert_eq!(resp["result"], json!({}));
        }
    }

    #[test]
    fn unknown_method_returns_error() {
        let tx = make_tx();
        let dir = tempfile::tempdir().expect("tempdir");
        let req = make_request("some/unknown/method", None);

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");
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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None);
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

        let resp = dispatch(&req, dir.path(), "0.6.0", &tx, None, None).expect("response");

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

}
