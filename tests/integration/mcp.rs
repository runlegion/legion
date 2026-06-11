//! Integration tests: MCP stdio server and the cross-process push bridge.

use crate::common::*;
use std::process::Command;

/// Verify that `legion mcp` runs as a spec-compliant stdio-only MCP server:
/// no HTTP port bind, no watch loop. Each Claude Code session spawns its own
/// `legion mcp` subprocess via plugin.json mcpServers, so a port bind would
/// conflict across concurrent sessions and a watch loop would spawn recursive
/// agent sessions. The long-lived HTTP + watch process is `legion daemon`,
/// kept as a separate singleton and unrelated to this stdio subprocess.
///
/// This test binds a port first to guarantee that `legion mcp` must skip the
/// HTTP bind entirely (attempting to bind an already-taken port would surface
/// as a startup error).
#[test]
fn legion_mcp_subcommand_is_stdio_only() {
    use std::io::Write;

    let data_dir = tempfile::tempdir().unwrap();

    // Hold a port so that if `legion mcp` ever tries to start an HTTP server,
    // the bind would fail and the subprocess would surface as an error.
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();

    let mut child = legion_cmd(data_dir.path())
        .args(["mcp"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn legion mcp");

    // Send a valid MCP initialize request, then close stdin so the stdio loop
    // returns and the process exits cleanly.
    let stdin = child.stdin.as_mut().expect("failed to open child stdin");
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
        )
        .expect("failed to write initialize to stdin");
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("failed to wait for legion mcp");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "legion mcp exited nonzero\nstatus: {:?}\nstderr: {}",
        output.status,
        stderr
    );

    // Stdout must contain a valid MCP initialize response with the right
    // protocol version, proving the stdio loop actually ran.
    assert!(
        stdout.contains("\"protocolVersion\":\"2024-11-05\""),
        "legion mcp stdout missing initialize response\nstdout: {stdout}"
    );

    // Stderr must NOT mention HTTP server startup or watch loop activity,
    // proving legion mcp is stdio-only and does not start either.
    assert!(
        !stderr.contains("channel server at http://"),
        "legion mcp must not start HTTP server\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("watch active"),
        "legion mcp must not start watch loop\nstderr: {stderr}"
    );

    // Keep the blocker alive until the assertions complete so the conflict
    // surface stays hot for the duration of the test.
    drop(blocker);
}

/// End-to-end test: spawn `legion mcp` as a subprocess, perform the MCP
/// `initialize` handshake, then fire FOUR separate `legion post` CLI
/// invocations covering every branch of the recipient filter:
///
/// - **MUSING_DELIVERED**: plain text from `sender-repo` -- general musing
///   from a different repo, MUST deliver.
/// - **OWN_POST_SUPPRESSED**: plain text from `recv-repo` (same as
///   `clientInfo.name`) -- own-post suppression, MUST NOT deliver.
/// - **NAMED_SIGNAL_DELIVERED**: `@recv-repo` signal from `sender-repo` --
///   targeted signal to this client, MUST deliver with `is_signal="true"`.
/// - **WRONG_SIGNAL_SUPPRESSED**: `@other-repo` signal from `sender-repo` --
///   targeted signal to a different client, MUST NOT deliver.
///
/// For every delivered frame, the test also parses the wire payload (the
/// `<channel>` XML inside `params.content`) and asserts the `repo`,
/// `is_signal`, and CDATA body are correct -- this locks the wire format so
/// a future refactor of `build_channel_content` cannot silently change the
/// shape of the message Claude Code parses on the other end.
///
/// This test is the primary regression guard for issue #220. Prior to the
/// fix, the MCP notifier thread subscribed to an in-process
/// `tokio::sync::broadcast` channel, which cannot cross process boundaries.
/// Any write made from a separate process -- a `legion post` CLI command,
/// a second Claude Code session's MCP subprocess, the standalone HTTP
/// daemon -- was silently invisible to the notifier. This test exercises
/// exactly that path across every filter branch and must fail if any of
/// them regresses (the prior PR #221 review highlighted that a test
/// covering only the general-musing branch would let a `client_repo_cell`
/// wiring break slip through).
#[test]
fn mcp_push_bridge_delivers_cross_process_post() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().expect("tempdir");

    // Warm the database once before spawning the MCP subprocess. Legion's
    // schema migrations are not concurrency-safe at first-open time: two
    // processes racing to ALTER TABLE on a fresh DB produce "duplicate
    // column name" errors. A single synchronous CLI command drives the
    // full migration path to completion, so subsequent openers see a
    // ready schema.
    let warmup = Command::new(env!("CARGO_BIN_EXE_legion"))
        .env("LEGION_DATA_DIR", dir.path())
        .args(["post", "--repo", "warmup-repo", "--text", "schema warmup"])
        .output()
        .expect("spawn legion post (warmup)");
    assert!(
        warmup.status.success(),
        "warmup post failed: {}",
        String::from_utf8_lossy(&warmup.stderr)
    );

    // Spawn the MCP subprocess with a tight poll interval so the test
    // finishes quickly instead of waiting on the 500ms default.
    let mut child = Command::new(env!("CARGO_BIN_EXE_legion"))
        .env("LEGION_DATA_DIR", dir.path())
        .env("LEGION_MCP_POLL_MS", "50")
        .args(["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn legion mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let child_stderr = child.stderr.take().expect("stderr");
    let mut reader = BufReader::new(stdout);

    // Drain subprocess stderr in a background thread. If the notifier spams
    // errors (DB failure, etc.) and fills the stderr pipe, the child can
    // block on `eprintln!`, which interacts badly with the shared stdout
    // mutex. Draining stderr prevents that and captures the lines so the
    // failure message can include them.
    let captured_stderr = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    {
        let captured = std::sync::Arc::clone(&captured_stderr);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(child_stderr);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line) {
                if n == 0 {
                    break;
                }
                if let Ok(mut s) = captured.lock() {
                    s.push_str(&line);
                }
                line.clear();
            }
        });
    }

    // 1. Send initialize. `clientInfo.name = "recv-repo"` is load-bearing:
    //    without it, the notifier cannot suppress own-posts or route
    //    named signals.
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "recv-repo", "version": "0.0.1" }
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&init).unwrap()).expect("write initialize");
    stdin.flush().expect("flush");

    // 2. Read the initialize response.
    let mut init_line = String::new();
    reader
        .read_line(&mut init_line)
        .expect("read initialize response");
    let init_resp: serde_json::Value =
        serde_json::from_str(init_line.trim()).expect("parse initialize response");
    assert_eq!(init_resp["id"], 1, "initialize response id mismatch");
    assert_eq!(
        init_resp["result"]["serverInfo"]["name"], "legion-channel",
        "wrong server name"
    );

    // 3. Fire four cross-process posts covering every filter branch. The
    //    markers are unique per-case so the assertions can distinguish
    //    which frames arrived without parsing text content.
    let musing_marker = "MCP_PUSH_MUSING_DELIVERED_9f2a1b";
    let own_post_marker = "MCP_PUSH_OWN_POST_SUPPRESSED_9f2a1b";
    let named_signal_marker = "MCP_PUSH_NAMED_SIGNAL_DELIVERED_9f2a1b";
    let wrong_signal_marker = "MCP_PUSH_WRONG_SIGNAL_SUPPRESSED_9f2a1b";

    // Order matters: fire the "must not deliver" posts FIRST so that when
    // the later "must deliver" posts arrive, we know the prior ones have
    // already been polled and filtered. If MUSING_DELIVERED arrives and
    // OWN_POST_SUPPRESSED is not in the observed set by then, we can
    // conclude the notifier's filter actively suppressed it, not just
    // that it had not been polled yet.
    let posts = [
        ("recv-repo", own_post_marker.to_string()),
        (
            "sender-repo",
            format!("@other-repo review:approved -- {}", wrong_signal_marker),
        ),
        ("sender-repo", musing_marker.to_string()),
        (
            "sender-repo",
            format!("@recv-repo review:approved -- {}", named_signal_marker),
        ),
    ];

    for (repo, text) in &posts {
        let post_out = Command::new(env!("CARGO_BIN_EXE_legion"))
            .env("LEGION_DATA_DIR", dir.path())
            .args(["post", "--repo", repo, "--text", text])
            .output()
            .expect("spawn legion post");
        assert!(
            post_out.status.success(),
            "legion post failed ({}): {}",
            repo,
            String::from_utf8_lossy(&post_out.stderr)
        );
    }

    // 4. Drain subprocess stdout until BOTH deliverable markers have been
    //    seen OR the deadline expires. Each line is captured regardless
    //    of whether it matches.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed_lines: Vec<String> = Vec::new();
    let mut observed_frames: Vec<serde_json::Value> = Vec::new();

    // Read in a dedicated thread so we can enforce the deadline via
    // channel recv_timeout instead of blocking forever on a dead pipe.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = reader;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut musing_frame: Option<serde_json::Value> = None;
    let mut signal_frame: Option<serde_json::Value> = None;

    while Instant::now() < deadline && (musing_frame.is_none() || signal_frame.is_none()) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                observed_lines.push(line.clone());
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                    continue;
                };
                if v["method"] != "notifications/claude/channel" {
                    continue;
                }
                observed_frames.push(v.clone());
                let content = v["params"]["content"].as_str().unwrap_or("");
                if content.contains(musing_marker) {
                    musing_frame = Some(v);
                } else if content.contains(named_signal_marker) {
                    signal_frame = Some(v);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Always kill the subprocess and collect captured stderr before
    // asserting so a failure does not leave a zombie daemon behind and so
    // the failure message is diagnosable.
    let _ = child.kill();
    let _ = child.wait();
    let stderr_snapshot = captured_stderr
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();

    let failure_context = || {
        format!(
            "observed {} frames:\n{}\ncaptured stderr:\n{}",
            observed_frames.len(),
            observed_lines.join(""),
            stderr_snapshot
        )
    };

    // Positive assertion 1: general-musing branch delivered.
    let musing = musing_frame.unwrap_or_else(|| {
        panic!(
            "did not observe musing notification carrying {}; {}",
            musing_marker,
            failure_context()
        )
    });
    let musing_content = musing["params"]["content"].as_str().expect("content str");
    assert!(
        musing_content.contains(r#"repo="sender-repo""#),
        "musing frame wire repo attribute wrong: {musing_content}"
    );
    assert!(
        musing_content.contains(r#"is_signal="false""#),
        "musing frame is_signal attribute wrong: {musing_content}"
    );
    assert!(
        musing_content.contains(&format!("<![CDATA[{musing_marker}]]>")),
        "musing frame CDATA body does not match marker: {musing_content}"
    );

    // Positive assertion 2: @recv-repo named-signal branch delivered.
    let signal = signal_frame.unwrap_or_else(|| {
        panic!(
            "did not observe named-signal notification carrying {}; {}",
            named_signal_marker,
            failure_context()
        )
    });
    let signal_content = signal["params"]["content"].as_str().expect("content str");
    assert!(
        signal_content.contains(r#"repo="sender-repo""#),
        "signal frame wire repo attribute wrong: {signal_content}"
    );
    assert!(
        signal_content.contains(r#"is_signal="true""#),
        "signal frame is_signal attribute wrong: {signal_content}"
    );
    assert!(
        signal_content.contains(named_signal_marker),
        "signal frame CDATA body does not match marker: {signal_content}"
    );

    // Negative assertion 1: own-post (recv-repo → recv-repo) was suppressed.
    // Both deliverable frames have arrived by this point, so any intervening
    // polls that would have delivered OWN_POST_SUPPRESSED have already run.
    for frame in &observed_frames {
        let content = frame["params"]["content"].as_str().unwrap_or("");
        assert!(
            !content.contains(own_post_marker),
            "own-post suppression regression: frame carrying {own_post_marker} was delivered; {content}"
        );
    }

    // Negative assertion 2: wrong-recipient signal (@other-repo) was suppressed.
    for frame in &observed_frames {
        let content = frame["params"]["content"].as_str().unwrap_or("");
        assert!(
            !content.contains(wrong_signal_marker),
            "wrong-recipient signal suppression regression: frame carrying {wrong_signal_marker} was delivered; {content}"
        );
    }
}
