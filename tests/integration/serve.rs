//! Integration tests: the HTTP dashboard server (`legion serve`).
//!
//! First integration coverage for serve.rs (#608 coverage net): bind a
//! real port, answer /health, and write/remove the daemon pidfile.

use crate::common::*;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// Kills the spawned server on drop so a failing assertion cannot leak a
/// listening child process past the test.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One hand-rolled HTTP/1.1 GET. `Connection: close` makes the server end
/// the stream, so read_to_string terminates without a client library.
fn http_get(port: u16, path: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

/// `legion serve` binds the requested port and answers GET /health with
/// the liveness JSON (#319 contract: status, version, started_at,
/// uptime_secs) -- the probe hooks, the MCP reconnect path, and the
/// SessionStart supervisor all poll.
#[test]
fn serve_binds_port_and_answers_health() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");

    // The server child opens the same legion.db; warm the schema with one
    // synchronous CLI call first so no later addition to this test can race
    // first-open migrations against the child (push-bridge doctrine).
    warm_schema(dir.path());

    // A port that is free at this instant. Dropping the listener before the
    // spawn leaves a tiny race window; the same pattern the daemon tests use.
    let port = std::net::TcpListener::bind(("0.0.0.0", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    // XDG_STATE_HOME redirects the daemon pidfile into the tempdir so the
    // test never touches (or depends on) the operator's real state dir.
    let child = legion_cmd(dir.path())
        .env("XDG_STATE_HOME", &state_dir)
        .args(["serve", "--port", &port.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn legion serve");
    let mut guard = ChildGuard(child);

    // Poll /health until the server is up (or the child died / we time out).
    let deadline = Instant::now() + Duration::from_secs(15);
    let response = loop {
        if let Ok(resp) = http_get(port, "/health") {
            break resp;
        }
        if let Ok(Some(status)) = guard.0.try_wait() {
            let mut stderr = String::new();
            if let Some(mut pipe) = guard.0.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!("legion serve exited early ({status}); stderr: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "legion serve did not answer /health on port {port} within 15s"
        );
        std::thread::sleep(Duration::from_millis(100));
    };

    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected 200 from /health, got: {response}"
    );
    let body = response
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_else(|| panic!("no body in response: {response}"));
    let health: serde_json::Value = serde_json::from_str(body.trim())
        .unwrap_or_else(|e| panic!("health body is not JSON ({e}): {body}"));
    assert_eq!(health["status"], "ok");
    assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
    assert!(health["started_at"].is_string());
    assert!(health["uptime_secs"].is_number());

    // The pidfile is written only after a successful bind (#599 contract)
    // and points at the live child.
    let pid_file = state_dir.join("legion").join("daemon.pid");
    let recorded = std::fs::read_to_string(&pid_file)
        .expect("serve must write daemon.pid after binding")
        .trim()
        .to_string();
    assert_eq!(
        recorded,
        guard.0.id().to_string(),
        "daemon.pid must record the serve child's PID"
    );
}
