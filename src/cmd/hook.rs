//! The legion-cmd PreToolUse hook adapter (#1229): the one place that does
//! I/O over the pure `legion_cmd::route`. Reads one hook payload from
//! stdin, reads and parses the policy file, runs any lookups a matched rule
//! requires, calls `route` once under its own deadline, and writes one hook
//! response to stdout. Always exits 0 with a response (FR-CMD-009): an
//! overrun or any internal error (unreadable/invalid policy, bad hook JSON,
//! a panic inside `route`) becomes a deny naming what failed, never an
//! allow and never a process exit that leaves the harness to fail open on
//! its own hook timeout.
//!
//! Hook contract used here (Claude Code docs, `https://code.claude.com/docs/en/hooks.md`,
//! verified 2026-09-16 -- researched fresh for this issue, not assumed from
//! `plugin/hooks/lib/emit.sh`):
//! - Input: `tool_name`, `tool_input`, `session_id`, `cwd`, `tool_use_id`
//!   (Input JSON Schema). Only the fields this adapter reads are modeled
//!   below; unknown fields are ignored, not rejected.
//! - Output: `hookSpecificOutput` with `hookEventName`, `permissionDecision`,
//!   `permissionDecisionReason`, `additionalContext`, `updatedInput`
//!   (Output JSON Schema).
//! - `permissionDecision`'s documented values are exactly `"allow"` and
//!   `"deny"` (Decision Control table) -- there is no documented `"ask"`
//!   value for PreToolUse. Exit 0 with no `permissionDecision` at all
//!   (either no JSON, or `hookSpecificOutput` carrying only
//!   `additionalContext`) is the documented way to express "no opinion,"
//!   which is how the harness's own permission prompt (interactive `ask`)
//!   is reached: by every PreToolUse hook declining to decide, not by this
//!   adapter emitting a decision value named "ask." This is why
//!   `Decision::Allow` and the confirmed-and-marked `Decision::Ask` path
//!   both omit `permissionDecision` below, rather than sending `"allow"`
//!   for the former (FR-CMD-002's "an allow never grants a permission the
//!   harness would not") -- `Decision::Rewrite` is the one path that must
//!   set `permissionDecision: "allow"` explicitly, since `updatedInput`
//!   has no effect without it (`plugin/hooks/lib/emit.sh`'s `emit_rewrite`
//!   pairs them, and that pairing is the only place this adapter borrows
//!   from emit.sh's shape rather than the docs directly).
//! - Exit codes: 0 reads the JSON response; 2 blocks regardless of JSON
//!   content; any other non-zero is a non-blocking error (Exit Codes).
//!   `run_hook` always returns 0 -- our own deny JSON is the authoritative
//!   fail-closed signal, not the exit code.
//! - A hook that times out does not block the tool call -- it fails open,
//!   silently, with the model never told (Timeout Behavior). This is
//!   exactly the failure `route.deadline_ms` exists to prevent: this
//!   adapter enforces a deadline well under the harness's own hook
//!   timeout and returns its own deny before the harness's timeout could
//!   ever fire.
//! - Whether `"ask"`'s operator-prompt carries the agent's confirmation
//!   reason INSIDE the interactive prompt UI itself is not documented
//!   (there is no `"ask"` output value to carry it on). This adapter
//!   carries the reason in `additionalContext` instead, which is
//!   documented and visible to the agent; #1229's own scope does not
//!   include the confirmation store (#1237), so this path is exercised
//!   here only through a stubbed `confirmed` flag, never a real one.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use legion_cmd::{
    AskDetails, Context, DecidingEntry, Decision, Lookup, ManagedTarget, Policy, ProxyReason,
    RequiredLookup, RequiredQuery, Routed, ToolCall, parse_policy, required_lookups, route,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::cmd::config::RouteSettings;
use crate::cmd::replacement::build_replacement;

/// Test seam + operator override: names the policy file directly. Checked
/// before the plugin-root-relative default, so a test never depends on
/// `CLAUDE_PLUGIN_ROOT` being set.
const POLICY_PATH_ENV: &str = "LEGION_CMD_POLICY";

/// The plugin root env var Claude Code sets for every hook subprocess
/// (see `plugin/hooks/lib/prelude.sh`'s `legion_resolve_bin`); the shipped
/// policy lives at `<plugin root>/legion-cmd/policy.json`.
const PLUGIN_ROOT_ENV: &str = "CLAUDE_PLUGIN_ROOT";

/// A hook response the adapter could not even build (serialization
/// failure). Kept as a fixed string, not built with `serde_json`, so it
/// cannot itself fail to serialize.
const FALLBACK_DENY_JSON: &str = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"legion-cmd adapter failed to serialize its own response"}}"#;

/// The PreToolUse payload fields this adapter reads. `#[serde(default)]`
/// on every non-essential field and no `deny_unknown_fields`: a field the
/// harness adds later, or omits on a call shape this adapter has not seen,
/// must never turn into a parse failure by itself.
#[derive(Debug, Deserialize)]
struct HookPayload {
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
    #[serde(default)]
    cwd: Option<String>,
}

/// Every way the adapter itself can fail closed (FR-CMD-009). Each variant
/// maps to a deny whose reason names the variant and its detail.
#[derive(Debug, thiserror::Error)]
enum AdapterError {
    #[error("could not read the hook payload: {0}")]
    Payload(String),
    #[error("could not read or parse the routing policy: {0}")]
    PolicyRead(String),
    #[error("a required recall or consult lookup failed: {0}")]
    Lookup(String),
    #[error("could not build the replacement command: {0}")]
    Replacement(String),
    #[error("the routing decision exceeded its deadline of {deadline_ms}ms")]
    DeadlineExceeded { deadline_ms: u64 },
    #[error("the router panicked while deciding: {0}")]
    Panic(String),
}

/// Reads one PreToolUse payload from `stdin` and writes one hook response
/// to `stdout`. Always exits 0 (FR-CMD-009): every failure this function
/// can observe becomes a deny JSON body, never a non-zero exit and never a
/// silently empty stdout that leaves the harness's own hook-timeout fail
/// open to decide instead.
pub fn run_hook(mut stdin: impl Read, mut stdout: impl Write) -> ExitCode {
    let mut input = String::new();
    let response = match stdin.read_to_string(&mut input) {
        Ok(_) => build_response(&input),
        Err(e) => deny_for_error(&AdapterError::Payload(e.to_string()), None),
    };
    let body = serde_json::to_string(&response).unwrap_or_else(|_| FALLBACK_DENY_JSON.to_string());
    let _ = writeln!(stdout, "{body}");
    ExitCode::SUCCESS
}

/// Builds the hook response for one raw payload string. Never panics past
/// this boundary: a panic inside `route` itself is caught by `decide`'s
/// channel-disconnect detection, and every other failure is an explicit
/// `Result` this function maps to a deny.
fn build_response(payload_text: &str) -> Value {
    let payload: HookPayload = match serde_json::from_str(payload_text) {
        Ok(payload) => payload,
        Err(e) => return deny_for_error(&AdapterError::Payload(e.to_string()), None),
    };

    let policy_path = match resolve_policy_path() {
        Ok(path) => path,
        Err(e) => return deny_for_error(&e, Some(&payload)),
    };
    let policy_text = match std::fs::read_to_string(&policy_path) {
        Ok(text) => text,
        Err(e) => {
            return deny_for_error(
                &AdapterError::PolicyRead(format!("{}: {e}", policy_path.display())),
                Some(&payload),
            );
        }
    };
    let settings = match RouteSettings::from_policy_text(&policy_text) {
        Ok(settings) => settings,
        Err(e) => return deny_for_error(&AdapterError::PolicyRead(e.to_string()), Some(&payload)),
    };
    let policy = match parse_policy(&policy_text) {
        Ok(policy) => policy,
        Err(e) => return deny_for_error(&AdapterError::PolicyRead(e.to_string()), Some(&payload)),
    };

    let call = ToolCall {
        tool: payload.tool_name.clone(),
        input: payload.tool_input.clone(),
    };
    let repo = repo_from_cwd(payload.cwd.as_deref());
    let runner: Arc<dyn LookupRunner + Send + Sync> = Arc::new(RealLookupRunner { repo });

    let outcome = decide(settings.deadline, move || {
        decide_inner(&policy, &call, runner.as_ref())
    });
    match outcome {
        Ok(routed) => apply_decision(&routed, &payload, false),
        Err(e) => deny_for_error(&e, Some(&payload)),
    }
}

/// Runs `work` on its own thread and enforces `deadline` against it via a
/// channel, rather than `catch_unwind`: a worker that panics drops its
/// sender without sending, which `recv_timeout` reports as
/// `Disconnected` -- indistinguishable, from the caller's side, from any
/// other reason the worker never produced a value, which is exactly what
/// a panic is. This requires the crate not build with `panic = "abort"`
/// (it does not; see `Cargo.toml`'s `[profile.release]`, which sets
/// neither `panic` nor overrides the default `unwind`).
fn decide<F>(deadline: Duration, work: F) -> Result<Routed, AdapterError>
where
    F: FnOnce() -> Result<Routed, AdapterError> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(work());
    });
    match rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(AdapterError::DeadlineExceeded {
            deadline_ms: deadline.as_millis() as u64,
        }),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(AdapterError::Panic(
            "the router thread ended without producing a decision".to_string(),
        )),
    }
}

/// Runs the lookups the matched rule requires (the pure pre-pass,
/// `legion_cmd::required_lookups`), fills `Context`, and calls `route`
/// exactly once. A lookup failure fails the whole decision closed
/// (`AdapterError::Lookup`) rather than falling back to `NotFetched`,
/// which would silently reach FR-CMD-016's missing-lookup default instead
/// of surfacing why.
fn decide_inner(
    policy: &Policy,
    call: &ToolCall,
    runner: &dyn LookupRunner,
) -> Result<Routed, AdapterError> {
    let mut ctx = Context::default();
    for query in required_lookups(policy, call) {
        let lookup = runner.run(&query).map_err(AdapterError::Lookup)?;
        match query.lookup {
            RequiredLookup::Recall => ctx.recall = lookup,
            RequiredLookup::Consult => ctx.consult = lookup,
        }
    }
    Ok(route(policy, call, &ctx))
}

/// Performs one `RequiredQuery`. A seam (rather than a direct call from
/// `decide_inner`) so tests can inject a lookup that never runs real I/O.
trait LookupRunner {
    fn run(&self, query: &RequiredQuery) -> Result<Lookup, String>;
}

/// The production `LookupRunner`: BM25-only recall/consult (no embedding
/// model load) against the local store, scoped to `repo` for recall and
/// cross-repo for consult. BM25-only, not hybrid, deliberately: loading
/// `model2vec-rs` per hook invocation would itself eat into
/// `route.deadline_ms`, the exact failure mode the deadline exists to
/// catch -- a slow lookup should count against the deadline and deny, not
/// silently make every hook call slower.
struct RealLookupRunner {
    repo: String,
}

impl LookupRunner for RealLookupRunner {
    fn run(&self, query: &RequiredQuery) -> Result<Lookup, String> {
        let (db, index) = crate::cli::util::open_db_and_index().map_err(|e| e.to_string())?;
        let range = crate::timerange::TimeRange::default();
        let result = match query.lookup {
            RequiredLookup::Recall => crate::recall::recall_bm25(
                &db,
                &index,
                &self.repo,
                &query.query,
                5,
                crate::recall::ArchiveMode::Hot,
                &range,
            ),
            RequiredLookup::Consult => {
                crate::recall::consult_bm25(&db, &index, &query.query, 5, &range)
            }
        }
        .map_err(|e| e.to_string())?;

        if result.reflections.is_empty() {
            Ok(Lookup::Empty)
        } else {
            Ok(Lookup::Found(
                result.reflections.into_iter().map(|r| r.text).collect(),
            ))
        }
    }
}

/// Resolves the policy file path: `LEGION_CMD_POLICY` (test seam and
/// operator override) first, then `${CLAUDE_PLUGIN_ROOT}/legion-cmd/policy.json`.
/// Neither set is itself a fail-closed condition (FR-CMD-016's empty-policy
/// spirit extended to "no policy configured at all"): `run_hook` takes no
/// path argument by design, so there is no third way to tell it where to
/// look.
fn resolve_policy_path() -> Result<PathBuf, AdapterError> {
    if let Ok(path) = std::env::var(POLICY_PATH_ENV) {
        return Ok(PathBuf::from(path));
    }
    if let Ok(root) = std::env::var(PLUGIN_ROOT_ENV) {
        return Ok(PathBuf::from(root).join("legion-cmd").join("policy.json"));
    }
    Err(AdapterError::PolicyRead(format!(
        "neither {POLICY_PATH_ENV} nor {PLUGIN_ROOT_ENV} is set; no policy file to read"
    )))
}

/// The repo name recall/consult scope lookups to: the hook payload's `cwd`
/// basename, or a fixed placeholder when `cwd` is absent. This adapter does
/// not replicate `prelude.sh`'s `--git-common-dir` worktree resolution
/// (FR-CMD-016 already fails a rule closed when its lookup errors, so a
/// wrong repo name here costs a possibly-empty recall, not a wrong
/// decision).
fn repo_from_cwd(cwd: Option<&str>) -> String {
    cwd.and_then(|c| std::path::Path::new(c).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("(unknown)")
        .to_string()
}

/// Turns a `Routed` decision into the hook response JSON (FR-CMD-017).
/// `confirmed` is always `false` on the real path -- there is no
/// confirmation store yet (#1237) -- and exists only so a test can exercise
/// the marked-and-confirmed ask path without one.
fn apply_decision(routed: &Routed, payload: &HookPayload, confirmed: bool) -> Value {
    match &routed.decision {
        Decision::Allow { note } => allow_response(note.as_deref()),
        Decision::Proxy { reason } => proxy_response(*reason),
        Decision::Rewrite { target, reason } => {
            match build_replacement(target, &routed.facts, &payload.tool_input) {
                Ok(updated_input) => rewrite_response(&updated_input, reason, target),
                Err(e) => deny_for_error(&AdapterError::Replacement(e.to_string()), Some(payload)),
            }
        }
        Decision::Deny(details) => deny_json(&format!(
            "{} (instead: {})",
            details.reason(),
            details.instead()
        )),
        Decision::Ask(details) => ask_response(details, &routed.entry, payload, confirmed),
    }
}

/// FR-CMD-002: an allow runs unchanged; `permissionDecision` is omitted so
/// the harness's own permission rules decide, and the note (if any) rides
/// along as `additionalContext`.
fn allow_response(note: Option<&str>) -> Value {
    match note {
        Some(note) if !note.is_empty() => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": note
            }
        }),
        _ => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse"
            }
        }),
    }
}

/// FR-CMD-004: a proxy runs unchanged, on the record at zero coverage
/// credit. Same pass-through shape as allow, but always visible: there is
/// no "quiet" proxy the way an allow's note can be `None`.
fn proxy_response(reason: ProxyReason) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": format!(
                "legion-cmd: running unaudited ({}) -- zero coverage credit",
                reason.as_str()
            )
        }
    })
}

/// FR-CMD-003: the rewrite's replacement is patched into the whole original
/// `tool_input` (preserving `description`/`timeout`/`run_in_background`,
/// per `emit.sh`'s `emit_rewrite`), returned as `updatedInput` alongside an
/// explicit `permissionDecision: "allow"` -- required for `updatedInput` to
/// take effect at all, so this is the one Decision arm that sets it.
fn rewrite_response(updated_input: &Value, reason: &str, target: &ManagedTarget) -> Value {
    let message = format!("legion-cmd rewrote this to {} -- {reason}", target.as_str());
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": message.clone(),
            "updatedInput": updated_input,
            "additionalContext": message
        }
    })
}

/// FR-CMD-005: a deny always carries a reason and the command to run
/// instead (both already folded into `reason` by the caller, since the
/// hook contract has no separate "instead" field).
fn deny_json(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }
    })
}

/// FR-CMD-006: an ask refuses the command with a question and a reason,
/// naming how to confirm, UNLESS the matched policy entry marked the
/// command as needing the operator AND the agent has already confirmed --
/// only then does the harness's own permission prompt run, by this adapter
/// declining to decide at all. `confirmed` has no real source yet (#1237);
/// it is `false` on every call from `run_hook`.
fn ask_response(
    details: &AskDetails,
    entry: &DecidingEntry,
    payload: &HookPayload,
    confirmed: bool,
) -> Value {
    let needs_operator = matches!(
        entry,
        DecidingEntry::Rule {
            needs_operator: true,
            ..
        }
    );
    if needs_operator && confirmed {
        return json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": format!(
                    "legion-cmd: awaiting operator confirmation -- {}",
                    details.reason()
                )
            }
        });
    }

    let command = payload
        .tool_input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("<command>");
    let reason = format!(
        "{} ({}). To confirm: legion cmd confirm --reason \"{}\" -- {command}",
        details.question(),
        details.reason(),
        details.reason()
    );
    deny_json(&reason)
}

/// Builds the deny response for an adapter-internal failure (FR-CMD-005,
/// FR-CMD-009): the reason names the failing variant, and `instead` is
/// `legion cmd-check -- '<command>'` when the original command is known,
/// or a bare rerun hint when it is not (a payload the adapter could not
/// even parse carries no recoverable command text).
fn deny_for_error(err: &AdapterError, payload: Option<&HookPayload>) -> Value {
    let command = payload
        .and_then(|p| p.tool_input.get("command"))
        .and_then(Value::as_str);
    let instead = match command {
        Some(cmd) if !cmd.is_empty() => {
            format!("legion cmd-check -- '{}'", cmd.replace('\'', r"'\''"))
        }
        _ => "legion cmd-check --hook".to_string(),
    };
    deny_json(&format!(
        "legion-cmd adapter failed closed: {err} (instead: {instead})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use legion_cmd::{AskDetails, DenyDetails, Facts};
    use std::sync::Mutex;

    // Serializes tests that mutate process-wide env vars
    // (`LEGION_CMD_POLICY`/`CLAUDE_PLUGIN_ROOT`); `cargo test` runs a
    // single process with threaded tests, so unsynchronized env mutation
    // across tests is a real race, not a hypothetical one.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn payload_json(tool_name: &str, command: &str) -> String {
        serde_json::json!({
            "tool_name": tool_name,
            "tool_input": {"command": command},
            "cwd": "/repo/legion"
        })
        .to_string()
    }

    fn hook_specific(value: &Value) -> &Value {
        value
            .get("hookSpecificOutput")
            .expect("every response has hookSpecificOutput")
    }

    // -- run_hook / build_response: never allow on internal failure -------

    #[test]
    fn malformed_payload_denies_with_a_reason() {
        let response = build_response("not json");
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "deny");
        assert!(
            out["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("hook payload")
        );
    }

    #[test]
    fn missing_policy_env_vars_deny_with_a_reason() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var(POLICY_PATH_ENV);
            std::env::remove_var(PLUGIN_ROOT_ENV);
        }
        let response = build_response(&payload_json("Bash", "echo hi"));
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "deny");
        assert!(
            out["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("policy")
        );
    }

    #[test]
    fn unreadable_policy_path_denies_with_a_reason() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(POLICY_PATH_ENV, "/nonexistent/legion-cmd-policy.json");
        }
        let response = build_response(&payload_json("Bash", "echo hi"));
        unsafe {
            std::env::remove_var(POLICY_PATH_ENV);
        }
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "deny");
        assert!(
            out["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("legion-cmd adapter failed closed")
        );
        assert!(
            out["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("legion cmd-check -- 'echo hi'")
        );
    }

    #[test]
    fn malformed_policy_json_denies_with_a_reason() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir();
        let policy_path = dir.join("policy.json");
        std::fs::write(&policy_path, "{ this is not json").expect("write fixture policy");
        unsafe {
            std::env::set_var(POLICY_PATH_ENV, &policy_path);
        }
        let response = build_response(&payload_json("Bash", "echo hi"));
        unsafe {
            std::env::remove_var(POLICY_PATH_ENV);
        }
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "deny");
    }

    #[test]
    fn empty_policy_denies_every_command_fr_cmd_016() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir();
        let policy_path = dir.join("policy.json");
        std::fs::write(&policy_path, "{}").expect("write fixture policy");
        unsafe {
            std::env::set_var(POLICY_PATH_ENV, &policy_path);
        }
        let response = build_response(&payload_json("Bash", "echo hi"));
        unsafe {
            std::env::remove_var(POLICY_PATH_ENV);
        }
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "deny");
    }

    #[test]
    fn a_command_no_rule_governs_allows_with_no_permission_decision() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir();
        let policy_path = dir.join("policy.json");
        std::fs::write(
            &policy_path,
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"chmod": {"rules": [
                {"id": "r1", "predicate": {"operand_contains": "777"},
                 "outcome": {"kind": "deny", "reason": "no", "instead": "chmod 755"}}
            ]}}}}}"#,
        )
        .expect("write fixture policy");
        unsafe {
            std::env::set_var(POLICY_PATH_ENV, &policy_path);
        }
        let response = build_response(&payload_json("Bash", "ls -la"));
        unsafe {
            std::env::remove_var(POLICY_PATH_ENV);
        }
        let out = hook_specific(&response);
        assert!(out.get("permissionDecision").is_none());
    }

    #[test]
    fn a_governed_command_denies_with_reason_and_instead() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir();
        let policy_path = dir.join("policy.json");
        std::fs::write(
            &policy_path,
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"chmod": {"rules": [
                {"id": "r1", "predicate": {"operand_contains": "777"},
                 "outcome": {"kind": "deny", "reason": "opens the tree", "instead": "chmod 755"}}
            ]}}}}}"#,
        )
        .expect("write fixture policy");
        unsafe {
            std::env::set_var(POLICY_PATH_ENV, &policy_path);
        }
        let response = build_response(&payload_json("Bash", "chmod 777 x"));
        unsafe {
            std::env::remove_var(POLICY_PATH_ENV);
        }
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "deny");
        let reason = out["permissionDecisionReason"].as_str().unwrap();
        assert!(reason.contains("opens the tree"));
        assert!(reason.contains("chmod 755"));
    }

    #[test]
    fn a_rewrite_patches_updated_input_and_sets_allow() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir();
        let policy_path = dir.join("policy.json");
        std::fs::write(
            &policy_path,
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh issue": {"rules": [
                {"id": "gh-issue-list", "predicate": {"arg_equals": "list"},
                 "outcome": {"kind": "rewrite", "target": "legion issue list", "reason": "duplicate surface"}}
            ]}}}}}"#,
        )
        .expect("write fixture policy");
        unsafe {
            std::env::set_var(POLICY_PATH_ENV, &policy_path);
        }
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": "gh issue list", "description": "list issues", "timeout": 5000},
            "cwd": "/repo/legion"
        })
        .to_string();
        let response = build_response(&payload);
        unsafe {
            std::env::remove_var(POLICY_PATH_ENV);
        }
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "allow");
        assert_eq!(out["updatedInput"]["command"], "legion issue list");
        assert_eq!(out["updatedInput"]["description"], "list issues");
        assert_eq!(out["updatedInput"]["timeout"], 5000);
    }

    #[test]
    fn an_unconfirmed_ask_denies_with_question_reason_and_confirm_hint() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir();
        let policy_path = dir.join("policy.json");
        std::fs::write(
            &policy_path,
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh pr": {"rules": [
                {"id": "gh-pr-merge-admin", "predicate": {"arg_equals": "--admin"},
                 "outcome": {"kind": "ask", "question": "bypass branch protection?", "reason": "skips checks", "needs_operator": true}}
            ]}}}}}"#,
        )
        .expect("write fixture policy");
        unsafe {
            std::env::set_var(POLICY_PATH_ENV, &policy_path);
        }
        let response = build_response(&payload_json("Bash", "gh pr merge 42 --admin"));
        unsafe {
            std::env::remove_var(POLICY_PATH_ENV);
        }
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "deny");
        let reason = out["permissionDecisionReason"].as_str().unwrap();
        assert!(reason.contains("bypass branch protection?"));
        assert!(reason.contains("skips checks"));
        assert!(reason.contains("legion cmd confirm"));
        assert!(reason.contains("gh pr merge 42 --admin"));
    }

    #[test]
    fn a_parse_error_asks_and_is_refused_the_same_as_any_other_ask() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir();
        let policy_path = dir.join("policy.json");
        std::fs::write(
            &policy_path,
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"chmod": {"rules": [
                {"id": "r1", "predicate": "always",
                 "outcome": {"kind": "allow", "note": null}}
            ]}}}}}"#,
        )
        .expect("write fixture policy");
        unsafe {
            std::env::set_var(POLICY_PATH_ENV, &policy_path);
        }
        // An unbalanced single quote cannot be tokenized.
        let response = build_response(&payload_json("Bash", "echo '"));
        unsafe {
            std::env::remove_var(POLICY_PATH_ENV);
        }
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "deny");
    }

    #[test]
    fn overrun_denies_and_names_the_deadline() {
        let outcome = decide(Duration::from_millis(10), || {
            thread::sleep(Duration::from_millis(200));
            Ok(routed_allow())
        });
        let err = outcome.expect_err("a 10ms deadline against a 200ms worker must overrun");
        assert!(matches!(
            err,
            AdapterError::DeadlineExceeded { deadline_ms: 10 }
        ));
        let response = deny_for_error(&err, None);
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "deny");
        assert!(
            out["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("deadline")
        );
    }

    #[test]
    fn a_panic_inside_the_worker_is_caught_as_an_internal_error_not_a_hang() {
        let outcome: Result<Routed, AdapterError> = decide(
            Duration::from_secs(5),
            || -> Result<Routed, AdapterError> { panic!("simulated router panic") },
        );
        let err = outcome.expect_err("a panicked worker must not silently succeed");
        assert!(matches!(err, AdapterError::Panic(_)));
        let response = deny_for_error(&err, None);
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "deny");
    }

    fn routed_allow() -> Routed {
        Routed {
            decision: Decision::Allow { note: None },
            facts: Facts::default(),
            entry: DecidingEntry::Default,
        }
    }

    // -- Ask paths built directly (no confirmation store yet, #1237) -------

    #[test]
    fn unmarked_ask_is_refused_even_when_stubbed_as_confirmed() {
        let details = AskDetails::new("run it?", "reason").expect("valid ask");
        let entry = DecidingEntry::Rule {
            id: "r1".to_string(),
            needs_operator: false,
        };
        let payload: HookPayload =
            serde_json::from_str(&payload_json("Bash", "gh pr merge 1")).expect("valid payload");
        let response = ask_response(&details, &entry, &payload, true);
        let out = hook_specific(&response);
        assert_eq!(out["permissionDecision"], "deny");
    }

    #[test]
    fn marked_but_unconfirmed_ask_is_refused_not_prompted() {
        let details = AskDetails::new("run it?", "reason").expect("valid ask");
        let entry = DecidingEntry::Rule {
            id: "r1".to_string(),
            needs_operator: true,
        };
        let payload: HookPayload =
            serde_json::from_str(&payload_json("Bash", "gh pr merge 1")).expect("valid payload");
        let response = ask_response(&details, &entry, &payload, false);
        let out = hook_specific(&response);
        assert_eq!(
            out["permissionDecision"], "deny",
            "needs_operator alone, with no confirmation, must never reach the harness prompt"
        );
    }

    #[test]
    fn marked_and_confirmed_ask_omits_permission_decision_for_the_harness_prompt() {
        let details = AskDetails::new("run it?", "bypasses review").expect("valid ask");
        let entry = DecidingEntry::Rule {
            id: "r1".to_string(),
            needs_operator: true,
        };
        let payload: HookPayload =
            serde_json::from_str(&payload_json("Bash", "gh pr merge 1")).expect("valid payload");
        let response = ask_response(&details, &entry, &payload, true);
        let out = hook_specific(&response);
        assert!(
            out.get("permissionDecision").is_none(),
            "a confirmed, marked ask must defer to the harness's own permission prompt"
        );
        assert!(
            out["additionalContext"]
                .as_str()
                .unwrap()
                .contains("bypasses review")
        );
    }

    // -- No-go deny carries the fixed instead text unchanged ---------------

    #[test]
    fn a_no_go_style_deny_carries_its_instead_text_through_unchanged() {
        let details = DenyDetails::no_go("never runs").expect("valid no-go deny");
        let response = deny_json(&format!(
            "{} (instead: {})",
            details.reason(),
            details.instead()
        ));
        let out = hook_specific(&response);
        assert!(
            out["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains(legion_cmd::NO_GO_INSTEAD)
        );
    }

    fn tempdir() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "legion-cmd-hook-test-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }
}
