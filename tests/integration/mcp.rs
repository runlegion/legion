//! Integration tests: the MCP stdio server's request/response surface, and
//! the absence of the retired server-initiated channel push (#947).

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

/// The MCP subprocess must never initiate a message (#947).
///
/// This is the inverse of the test it replaces
/// (`mcp_push_bridge_delivers_cross_process_post`), which asserted that a
/// cross-process `legion post` produced `notifications/claude/channel`
/// frames on the subprocess's stdout. That lane is retired; the hook drain
/// (#941) delivers to live sessions now.
///
/// The post fired here is deliberately the case the retired notifier was
/// most certain to deliver: a plain musing from a repo other than
/// `clientInfo.name`, which passes every branch of `should_notify` and lands
/// after the subprocess's boot watermark, so it took the live path rather
/// than the narrower cold-boot replay filter. Against the pre-#947 binary
/// this test fails: the notifier polled at a 500ms default, so the quiet
/// window below spans several ticks.
///
/// Two things make the verdict trustworthy rather than vacuous:
///
///   1. **A dead subprocess cannot pass.** After the quiet window the test
///      round-trips `tools/list` and requires the four legion tools back. A
///      subprocess that crashed, wedged, or exited would fail there instead
///      of silently satisfying "no frames arrived."
///   2. **No kill, no reap race.** The child is retired by closing stdin and
///      blocking on `wait()`, never by `kill()` -- the #959/PR960 lesson
///      that a signal sent is not a process reaped, and that test
///      determinism is a property of the fixture rather than the assert.
///      Every line the subprocess writes, in every phase, is inspected for a
///      push frame; none is skipped past.
///
/// The one residual timing exposure is a false PASS (a reintroduced push
/// slower than the window), never a false fail: with the emitter deleted
/// there is no code path that could produce a frame late.
#[test]
fn mcp_subprocess_never_emits_channel_push_frame() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::sync::mpsc::{RecvTimeoutError, channel};
    use std::time::{Duration, Instant};

    /// Long enough to span several ticks of the retired notifier's 500ms
    /// default poll interval, so the pre-#947 binary fails this test.
    const QUIET_WINDOW: Duration = Duration::from_millis(2500);
    /// Generous ceiling for a single request/response round trip. Only
    /// reached when the subprocess is wedged, which is a real failure.
    const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

    let dir = tempfile::tempdir().expect("tempdir");

    // Isolate telemetry state: an `mcp_notification` row appended to the
    // developer's real delivery.jsonl would corrupt the very parity dataset
    // that authorized this retirement. The override also lets the test
    // assert the file stays clean.
    let state_home = dir.path().join("state");

    // Warm the database once before spawning the MCP subprocess. Legion's
    // schema migrations are not concurrency-safe at first-open time: two
    // processes racing to ALTER TABLE on a fresh DB produce "duplicate
    // column name" errors. A single synchronous CLI command drives the full
    // migration path to completion, so subsequent openers see a ready schema.
    let warmup = Command::new(env!("CARGO_BIN_EXE_legion"))
        .env("LEGION_DATA_DIR", dir.path())
        .env("XDG_STATE_HOME", &state_home)
        .args(["post", "--repo", "warmup-repo", "--text", "schema warmup"])
        .output()
        .expect("spawn legion post (warmup)");
    assert!(
        warmup.status.success(),
        "warmup post failed: {}",
        String::from_utf8_lossy(&warmup.stderr)
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_legion"))
        .env("LEGION_DATA_DIR", dir.path())
        .env("XDG_STATE_HOME", &state_home)
        .args(["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn legion mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let child_stderr = child.stderr.take().expect("stderr");

    // Drain subprocess stderr in a background thread. A full stderr pipe
    // blocks the child inside eprintln!, which would look like "quiet"
    // here for the wrong reason.
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

    // One reader thread forwards every stdout line for the whole test, so
    // no phase can read past a frame another phase would have caught.
    let (tx, rx) = channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
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

    // Every line the subprocess emits, recorded in order for diagnostics.
    let mut observed: Vec<String> = Vec::new();
    // Lines that parsed as a server-initiated channel push. Must stay empty.
    let mut push_frames: Vec<String> = Vec::new();

    let note = |line: String, observed: &mut Vec<String>, pushes: &mut Vec<String>| {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim())
            && v["method"] == "notifications/claude/channel"
        {
            pushes.push(line.clone());
        }
        observed.push(line);
    };

    // 1. Handshake. `clientInfo.name = "recv-repo"` is load-bearing: it is
    //    the identity the retired notifier routed against, so naming it
    //    keeps the post below inside the delivered branch.
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
    writeln!(
        stdin,
        "{}",
        serde_json::to_string(&init).expect("serialize initialize")
    )
    .expect("write initialize");
    stdin.flush().expect("flush initialize");

    let init_line = rx
        .recv_timeout(RESPONSE_TIMEOUT)
        .expect("no initialize response from legion mcp");
    note(init_line.clone(), &mut observed, &mut push_frames);
    let init_resp: serde_json::Value =
        serde_json::from_str(init_line.trim()).expect("parse initialize response");
    assert_eq!(init_resp["id"], 1, "initialize response id mismatch");
    // The capability advertisement is what made the host subscribe to
    // pushes; assert its absence on the wire, not just in the unit test.
    assert!(
        init_resp["result"]["capabilities"]
            .get("experimental")
            .is_none(),
        "initialize still advertises an experimental capability: {}",
        init_resp["result"]["capabilities"]
    );

    // 2. Fire the cross-process post the retired lane would have pushed.
    let marker = "MCP_PUSH_RETIRED_MUSING_9f2a1b";
    let post_out = Command::new(env!("CARGO_BIN_EXE_legion"))
        .env("LEGION_DATA_DIR", dir.path())
        .env("XDG_STATE_HOME", &state_home)
        .args(["post", "--repo", "sender-repo", "--text", marker])
        .output()
        .expect("spawn legion post");
    assert!(
        post_out.status.success(),
        "legion post failed: {}",
        String::from_utf8_lossy(&post_out.stderr)
    );

    // 3. Quiet window: the subprocess must say nothing at all on its own.
    let window_end = Instant::now() + QUIET_WINDOW;
    loop {
        let remaining = window_end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(line) => note(line, &mut observed, &mut push_frames),
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let unsolicited: Vec<String> = observed[1..].to_vec();

    // 4. Liveness proof: a normal request must still get a normal response,
    //    so an exited or wedged subprocess cannot pass as "retired."
    let list_req = serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    writeln!(
        stdin,
        "{}",
        serde_json::to_string(&list_req).expect("serialize tools/list")
    )
    .expect("write tools/list");
    stdin.flush().expect("flush tools/list");

    let list_deadline = Instant::now() + RESPONSE_TIMEOUT;
    let mut list_resp: Option<serde_json::Value> = None;
    while list_resp.is_none() {
        let remaining = list_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                note(line.clone(), &mut observed, &mut push_frames);
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim())
                    && v["id"] == 2
                {
                    list_resp = Some(v);
                }
            }
            Err(_) => break,
        }
    }

    // 5. Retire the child deterministically: EOF on stdin ends its read
    //    loop, and wait() blocks until the kernel has actually reaped it.
    //    No kill(), so there is no signal-delivery race to lose.
    drop(stdin);
    let status = child.wait().expect("wait for legion mcp");
    let stderr_snapshot = captured_stderr
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();

    let context = || {
        format!(
            "observed {} stdout lines:\n{}\ncaptured stderr:\n{}",
            observed.len(),
            observed.join(""),
            stderr_snapshot
        )
    };

    assert!(
        push_frames.is_empty(),
        "MCP subprocess emitted {} notifications/claude/channel frame(s) after #947 retired the push:\n{}\n{}",
        push_frames.len(),
        push_frames.join(""),
        context()
    );
    assert!(
        unsolicited.is_empty(),
        "MCP subprocess wrote unsolicited output during the quiet window:\n{}\n{}",
        unsolicited.join(""),
        context()
    );

    let list = list_resp.unwrap_or_else(|| {
        panic!(
            "no tools/list response -- subprocess is not alive; {}",
            context()
        )
    });
    let tools = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list response has no tools array: {list}"));
    assert_eq!(
        tools.len(),
        4,
        "the four legion tools must survive the push retirement; got: {list}"
    );

    assert!(
        status.success(),
        "legion mcp exited nonzero on stdin EOF: {status:?}; {}",
        context()
    );

    // Nothing was delivered, so nothing may be recorded as delivered. An
    // absent file is the expected shape; a present one must carry no
    // mcp_notification rows.
    let delivery_log = state_home.join("legion").join("delivery.jsonl");
    if let Ok(raw) = std::fs::read_to_string(&delivery_log) {
        let mcp_rows: Vec<&str> = raw
            .lines()
            .filter(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .map(|v| v["lane"] == "mcp_notification")
                    .unwrap_or(false)
            })
            .collect();
        assert!(
            mcp_rows.is_empty(),
            "retired lane still wrote delivery telemetry:\n{}",
            mcp_rows.join("\n")
        );
    }
}
