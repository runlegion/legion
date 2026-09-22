//! The legion-cmd PreToolUse hook adapter (#1229): the one place that does
//! I/O over the pure `legion_cmd::route`.
//!
//! `legion cmd-check --hook` reads one PreToolUse payload from stdin, reads
//! the policy file, runs the lookups the matched rules require, calls `route`
//! once, and writes one hook response to stdout (FR-CMD-017). It always
//! answers with a response and exits 0 (FR-CMD-009): a harness hook that
//! times out or exits non-zero fails OPEN -- its output is discarded, the
//! tool call proceeds, and the model is never told -- so the adapter never
//! lets the harness time it out. It enforces its own deadline
//! (`route.deadline_ms`, `crate::cmd::config`) on a worker thread and turns an
//! overrun, a lookup failure, a replacement failure, an unreadable payload or
//! policy, and a panic anywhere into a deny with a reason. Nothing here
//! falls through to running the raw command.
//!
//! # The response shapes (Claude Code hook contract)
//!
//! `hookSpecificOutput.permissionDecision` is `allow`, `deny` or `ask`; its
//! `permissionDecisionReason` reaches the agent only on `deny` (on `allow`
//! and `ask` it is shown to the operator). `additionalContext` reaches the
//! agent on every decision. `updatedInput` replaces the whole `tool_input`.
//! Those facts fix the arms below:
//!
//! - allow: no `permissionDecision`, so the harness's own rules decide and
//!   an allow never grants a permission the harness would not (FR-CMD-002);
//!   route's note, if any, rides along as `additionalContext`.
//! - rewrite: `allow` plus `updatedInput` built by
//!   `crate::cmd::replacement`, with what the command became and why in
//!   `additionalContext` (FR-CMD-003).
//! - proxy: the command runs unchanged (FR-CMD-004).
//! - deny: `deny` with the reason and the command to run instead (FR-CMD-005).
//! - ask without the operator mark: `deny` carrying route's question and
//!   reason and how to confirm (FR-CMD-006, FR-CMD-026). `deny` rather than
//!   `ask` because only a deny's reason reaches the agent, and the agent is
//!   who must answer the question. The operator is not prompted.
//! - ask with the operator mark set on `Routed` (#1227; route sets it only
//!   when the agent's confirmation is in `Context`, #1237): `ask`, the
//!   harness's own permission prompt, carrying the reason. This is the only
//!   path that prompts the operator. The adapter holds no routing branch for
//!   confirmations (FR-CMD-011); it reads the mark route set.
//!
//! # What the adapter does not do
//!
//! It never scans or splits the command string; `legion_cmd::required_lookups`
//! and `route` read the one scan (FR-CMD-017). It never touches the command's
//! stdout or stderr (FR-CMD-012). It runs no external routing binary
//! (FR-CMD-014); the only process it may spawn is `git`, to name the repo a
//! recall lookup is scoped to, and that runs inside the deadline.
//!
//! # The repo
//!
//! `Context::repo` is derived once, in [`repo_for`], and validated there:
//! `LEGION_REPO` when set, else the main checkout's directory name for the
//! payload's `cwd` (a linked worktree's own folder name is never the repo),
//! else `cwd`'s basename -- and whatever that yields must be a plain name
//! (ASCII letters, digits, `-`, `_`, `.`, not starting with `.`) or it is
//! `None`. `cwd` is attacker-shaped input; a name that fails validation is
//! not a repo, and no consumer re-validates or re-sanitizes it. Nothing in
//! this issue splices the repo into a command: it scopes recall and rides in
//! `Context` for route, which performs no I/O.

use std::any::Any;
use std::io::{Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use legion_cmd::{
    AskDetails, Context, Deciding, Decision, Lookup, ManagedTarget, Policy, Routed, ToolCall,
    parse_policy, required_lookups, route,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::cmd::config::RouteSettings;
use crate::cmd::replacement::build_replacement;
use crate::error;
use crate::recall::{ArchiveMode, RecallResult, consult_bm25, recall_bm25};
use crate::timerange::TimeRange;

/// Names the policy file directly. Checked before the plugin-root default so
/// an operator can point the adapter at a policy of their own and a test
/// never depends on `CLAUDE_PLUGIN_ROOT`.
const POLICY_PATH_ENV: &str = "LEGION_CMD_POLICY";

/// The plugin root Claude Code sets for every hook subprocess; the shipped
/// policy lives at `<plugin root>/legion-cmd/policy.json`.
const PLUGIN_ROOT_ENV: &str = "CLAUDE_PLUGIN_ROOT";

/// `plugin/hooks/lib/prelude.sh`'s repo override (#614): when set, every
/// hook resolves the same repo identity from it, and so does this adapter.
const LEGION_REPO_ENV: &str = "LEGION_REPO";

const HOOK_EVENT: &str = "PreToolUse";

/// How many reflections a required recall or consult lookup fetches into
/// `Context`. Small on purpose: the lookup runs inside the decision deadline.
const LOOKUP_LIMIT: usize = 5;

/// The response written when the adapter cannot serialize its own response.
/// A fixed string, not built with `serde_json`, so it cannot itself fail.
const FALLBACK_DENY_JSON: &str = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"legion-cmd could not serialize its response -- instead: legion cmd-check -- <command>"}}"#;

/// The PreToolUse payload fields the adapter acts on. Unknown fields are
/// ignored, not rejected: the harness may add fields, and `session_id` and
/// `tool_use_id` are present in the payload but nothing in this slice
/// consumes them (the confirmation store and the ledger will).
#[derive(Debug, Deserialize)]
struct HookPayload {
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
    #[serde(default)]
    cwd: Option<String>,
}

impl HookPayload {
    /// The Bash command this payload carries, if any. Every message that
    /// names the command back to the agent goes through here.
    fn command(&self) -> Option<&str> {
        self.tool_input
            .get("command")
            .and_then(Value::as_str)
            .filter(|command| !command.is_empty())
    }
}

/// Every way the adapter itself fails, each closed (FR-CMD-009). The deny
/// the agent sees names the variant and its detail.
#[derive(Debug, thiserror::Error)]
enum AdapterError {
    #[error("payload: {0}")]
    Payload(String),
    #[error("policy: {0}")]
    PolicyRead(String),
    #[error("lookup: {0}")]
    Lookup(String),
    #[error("replacement: {0}")]
    Replacement(String),
    #[error("deadline exceeded: no decision within {deadline_ms} ms")]
    DeadlineExceeded { deadline_ms: u64 },
    #[error("panic: {0}")]
    Panic(String),
}

/// `legion cmd-check --hook`: reads one PreToolUse payload (JSON) on stdin and
/// writes one hook response (JSON) on stdout. Always exits 0 with a response.
///
/// The response, not the exit code, is the fail-closed signal: an unreadable
/// stdin, a panic anywhere in the adapter, and every failure inside it become
/// a deny body. A failed write to stdout has no recovery in-process;
/// `plugin/hooks/legion-cmd.sh` treats empty output as a broken adapter and
/// prints its own static deny.
pub fn run_hook(mut stdin: impl Read, mut stdout: impl Write) -> ExitCode {
    let mut input = String::new();
    let response: Value = match stdin.read_to_string(&mut input) {
        Ok(_) => guarded(|| respond(&input)),
        Err(e) => deny_for_error(&AdapterError::Payload(e.to_string()), None),
    };
    let body: String =
        serde_json::to_string(&response).unwrap_or_else(|_| FALLBACK_DENY_JSON.to_string());
    let _ = writeln!(stdout, "{body}");
    ExitCode::SUCCESS
}

/// Runs `build` and turns a panic anywhere inside it into a deny, so the
/// process never exits without a response over an adapter bug.
fn guarded(build: impl FnOnce() -> Value) -> Value {
    match panic::catch_unwind(AssertUnwindSafe(build)) {
        Ok(response) => response,
        Err(payload) => deny_for_error(&AdapterError::Panic(panic_message(&payload)), None),
    }
}

fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "unknown panic".to_string()
}

/// The production path: the policy from the environment, lookups against the
/// local store, the repo override from `LEGION_REPO`.
fn respond(input: &str) -> Value {
    let legion_repo: Option<String> = std::env::var(LEGION_REPO_ENV).ok();
    respond_with(
        input,
        read_policy_text(),
        Arc::new(StoreLookups),
        legion_repo,
    )
}

/// The adapter over injected sources, so a test drives every branch without
/// touching the environment or the store. Reads and parses the policy first
/// (a local file, outside the deadline), then runs repo derivation, the
/// lookups, and route on a worker thread under `route.deadline_ms`.
fn respond_with(
    input: &str,
    policy_text: Result<String, AdapterError>,
    lookups: Arc<dyn LookupRunner>,
    legion_repo: Option<String>,
) -> Value {
    let payload: HookPayload = match serde_json::from_str(input) {
        Ok(payload) => payload,
        Err(e) => return deny_for_error(&AdapterError::Payload(e.to_string()), None),
    };
    let policy_text: String = match policy_text {
        Ok(text) => text,
        Err(e) => return deny_for_error(&e, Some(&payload)),
    };
    let settings: RouteSettings = match RouteSettings::from_policy_text(&policy_text) {
        Ok(settings) => settings,
        Err(e) => {
            return deny_for_error(&AdapterError::PolicyRead(e.to_string()), Some(&payload));
        }
    };
    let policy: Policy = match parse_policy(&policy_text) {
        Ok(policy) => policy,
        Err(e) => {
            return deny_for_error(&AdapterError::PolicyRead(e.to_string()), Some(&payload));
        }
    };

    let call = ToolCall {
        tool: payload.tool_name.clone(),
        input: payload.tool_input.clone(),
    };
    let cwd: Option<String> = payload.cwd.clone();
    let outcome: Result<Routed, AdapterError> = decide(settings.deadline, move || {
        let repo: Option<String> = repo_for(legion_repo.as_deref(), cwd.as_deref());
        let ctx: Context = fetch_context(&policy, &call, repo, lookups.as_ref())?;
        Ok(route(&policy, &call, &ctx))
    });
    match outcome {
        Ok(routed) => apply(&routed, &payload),
        Err(e) => deny_for_error(&e, Some(&payload)),
    }
}

/// Runs `work` on a worker thread and waits at most `deadline` for its
/// result. An overrun is `DeadlineExceeded`; a panic inside `work` is
/// `Panic`, with the panic's message. On an overrun the worker is left
/// running: `legion cmd-check --hook` is a one-shot process, so the leaked
/// thread ends when the process does. This function must not be reused from
/// a long-lived process without real cancellation.
fn decide<T, F>(deadline: Duration, work: F) -> Result<T, AdapterError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, AdapterError> + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Result<T, AdapterError>>();
    let spawned = thread::Builder::new()
        .name("legion-cmd-decide".to_string())
        .spawn(move || {
            let result: Result<T, AdapterError> = match panic::catch_unwind(AssertUnwindSafe(work))
            {
                Ok(result) => result,
                Err(payload) => Err(AdapterError::Panic(panic_message(&payload))),
            };
            let _ = tx.send(result);
        });
    if let Err(e) = spawned {
        return Err(AdapterError::Panic(format!(
            "could not start the decision thread: {e}"
        )));
    }
    match rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(AdapterError::DeadlineExceeded {
            deadline_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
        }),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(AdapterError::Panic(
            "the decision thread ended without a result".to_string(),
        )),
    }
}

/// Runs the lookups the matched rules require (the pure pre-pass) and builds
/// the `Context` route reads. A lookup that fails denies the whole decision
/// (`AdapterError::Lookup`) rather than leaving the lookup `NotFetched`, which
/// would reach FR-CMD-016's missing-lookup deny without saying why. A recall
/// with no derivable repo is such a failure: recall is scoped to a repo.
fn fetch_context(
    policy: &Policy,
    call: &ToolCall,
    repo: Option<String>,
    lookups: &dyn LookupRunner,
) -> Result<Context, AdapterError> {
    let required = required_lookups(policy, call);
    let mut ctx = Context {
        repo: repo.clone(),
        ..Context::default()
    };
    if let Some(query) = required.recall {
        let repo: &str = repo.as_deref().ok_or_else(|| {
            AdapterError::Lookup(format!(
                "recall is scoped to a repo, and none could be derived from cwd or {LEGION_REPO_ENV}"
            ))
        })?;
        ctx.recall = lookups
            .recall(repo, &query)
            .map_err(|e| AdapterError::Lookup(e.to_string()))?;
    }
    if let Some(query) = required.consult {
        ctx.consult = lookups
            .consult(&query)
            .map_err(|e| AdapterError::Lookup(e.to_string()))?;
    }
    Ok(ctx)
}

/// Performs the recall and consult lookups a rule requires. A seam so tests
/// drive the adapter without the store.
trait LookupRunner: Send + Sync {
    fn recall(&self, repo: &str, query: &str) -> error::Result<Lookup>;
    fn consult(&self, query: &str) -> error::Result<Lookup>;
}

/// The production runner: BM25 over the local store. BM25 only, no embedding
/// model -- loading the model per hook invocation would spend the decision
/// deadline on setup, which is the failure the deadline exists to catch.
struct StoreLookups;

impl LookupRunner for StoreLookups {
    fn recall(&self, repo: &str, query: &str) -> error::Result<Lookup> {
        let (db, index) = crate::cli::util::open_db_and_index()?;
        let result: RecallResult = recall_bm25(
            &db,
            &index,
            repo,
            query,
            LOOKUP_LIMIT,
            ArchiveMode::Hot,
            &TimeRange::default(),
        )?;
        Ok(lookup_from(result))
    }

    fn consult(&self, query: &str) -> error::Result<Lookup> {
        let (db, index) = crate::cli::util::open_db_and_index()?;
        let result: RecallResult =
            consult_bm25(&db, &index, query, LOOKUP_LIMIT, &TimeRange::default())?;
        Ok(lookup_from(result))
    }
}

fn lookup_from(result: RecallResult) -> Lookup {
    if result.reflections.is_empty() {
        Lookup::Empty
    } else {
        Lookup::Found(result.reflections.into_iter().map(|r| r.text).collect())
    }
}

/// Reads the policy file: `LEGION_CMD_POLICY`, else
/// `${CLAUDE_PLUGIN_ROOT}/legion-cmd/policy.json`. Neither set is a
/// `PolicyRead` failure, which denies: no policy is not an empty policy, but
/// it is refused the same way (FR-CMD-016).
fn read_policy_text() -> Result<String, AdapterError> {
    let path: PathBuf = policy_path()?;
    std::fs::read_to_string(&path)
        .map_err(|e| AdapterError::PolicyRead(format!("{}: {e}", path.display())))
}

fn policy_path() -> Result<PathBuf, AdapterError> {
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

/// The repo a recall lookup is scoped to (see the module doc). `LEGION_REPO`
/// when set and non-empty, else the name derived from `cwd`; `None` when
/// neither yields a name that passes [`is_safe_repo_name`]. An unsafe
/// `LEGION_REPO` is `None`, not a fall-through to `cwd`: an explicit override
/// that fails validation is a misconfiguration, not a hint.
fn repo_for(legion_repo: Option<&str>, cwd: Option<&str>) -> Option<String> {
    let candidate: Option<String> = match legion_repo.filter(|value| !value.is_empty()) {
        Some(value) => Some(value.to_string()),
        None => cwd.and_then(repo_name_from_cwd),
    };
    candidate.filter(|name| is_safe_repo_name(name))
}

/// The main checkout's directory name for `cwd`, following
/// `prelude.sh`'s `legion_hook_parse`: the parent of `git rev-parse
/// --git-common-dir` when it holds a `.git` (a linked worktree resolves to
/// its primary; a submodule's common dir sits under `.git/modules` and fails
/// the check), else `cwd`'s own basename.
fn repo_name_from_cwd(cwd: &str) -> Option<String> {
    let path = Path::new(cwd);
    let root: PathBuf = crate::inventory::git_common_dir(path)
        .and_then(|common| common.parent().map(Path::to_path_buf))
        .filter(|root| root.join(".git").exists())
        .unwrap_or_else(|| path.to_path_buf());
    root.file_name()?.to_str().map(str::to_string)
}

/// True when `name` is a plain repo name: ASCII letters, digits, `-`, `_`,
/// `.`, and not starting with `.` (rules out `.`, `..`, and hidden-shaped
/// names). The one gate every derived repo name passes.
fn is_safe_repo_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Applies route's Decision to the hook response (FR-CMD-017).
fn apply(routed: &Routed, payload: &HookPayload) -> Value {
    match &routed.decision {
        Decision::Allow { note } => pass_through(note.as_deref()),
        Decision::Proxy { .. } => pass_through(None),
        Decision::Rewrite { target, reason } => {
            match build_replacement(target, &routed.facts, &payload.tool_input) {
                Ok(updated) => rewrite_response(payload.command(), target, reason, updated),
                Err(e) => deny_for_error(&AdapterError::Replacement(e.to_string()), Some(payload)),
            }
        }
        Decision::Deny(details) => deny_response(&format!(
            "{} -- instead: {}",
            details.reason(),
            details.instead()
        )),
        Decision::Ask(details) => ask_response(details, &routed.deciding, payload),
    }
}

/// The command runs unchanged and the harness's own rules decide the
/// permission (FR-CMD-002): no `permissionDecision`. A note reaches the agent
/// as `additionalContext`.
fn pass_through(note: Option<&str>) -> Value {
    let mut fields = Map::new();
    if let Some(note) = note.filter(|note| !note.is_empty()) {
        fields.insert(
            "additionalContext".to_string(),
            Value::String(note.to_string()),
        );
    }
    hook_output(fields)
}

/// The rewrite (FR-CMD-003): `allow` so the audited replacement runs, the
/// patched `tool_input` as `updatedInput`, and what the command became and
/// why as `additionalContext`, so the agent is never left believing its
/// original command ran (FR-CMD-009).
fn rewrite_response(
    original: Option<&str>,
    target: &ManagedTarget,
    reason: &str,
    updated: Value,
) -> Value {
    let message: String = format!(
        "legion-cmd rewrote `{}` to `{}`: {reason}",
        original.unwrap_or("<command>"),
        target.as_str()
    );
    let mut fields = decision_fields("allow", &message);
    fields.insert("updatedInput".to_string(), updated);
    fields.insert("additionalContext".to_string(), Value::String(message));
    hook_output(fields)
}

/// The ask (FR-CMD-006). With the operator mark set on `Routed`, the
/// harness's own permission prompt carrying the reason. Without it, a
/// refusal the agent reads: the question, the reason, and how to confirm
/// (`legion cmd confirm`, #1237). The confirm hint does not pre-fill the
/// reason: the agent must state its own, not copy the policy's.
fn ask_response(details: &AskDetails, deciding: &Deciding, payload: &HookPayload) -> Value {
    if matches!(
        deciding,
        Deciding::Rule {
            needs_operator: true,
            ..
        }
    ) {
        return hook_output(decision_fields("ask", details.reason()));
    }
    deny_response(&format!(
        "{} -- {}. To confirm: legion cmd confirm --reason <why> -- {}",
        details.question(),
        details.reason(),
        quoted_command(payload.command())
    ))
}

/// The deny for an adapter failure (FR-CMD-005, FR-CMD-009): the reason names
/// the failure, and the command to run instead is `legion cmd-check` over the
/// same command, so the agent and the operator can see what route decides.
fn deny_for_error(err: &AdapterError, payload: Option<&HookPayload>) -> Value {
    deny_response(&format!(
        "legion-cmd could not decide this command ({err}) -- instead: legion cmd-check -- {}",
        quoted_command(payload.and_then(HookPayload::command))
    ))
}

fn deny_response(reason: &str) -> Value {
    hook_output(decision_fields("deny", reason))
}

/// The two fields every explicit permission decision carries.
fn decision_fields(decision: &str, reason: &str) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert(
        "permissionDecision".to_string(),
        Value::String(decision.to_string()),
    );
    fields.insert(
        "permissionDecisionReason".to_string(),
        Value::String(reason.to_string()),
    );
    fields
}

fn hook_output(fields: Map<String, Value>) -> Value {
    let mut output = Map::new();
    output.insert(
        "hookEventName".to_string(),
        Value::String(HOOK_EVENT.to_string()),
    );
    output.extend(fields);
    json!({ "hookSpecificOutput": Value::Object(output) })
}

/// The command as a single shell word for a message the agent may paste and
/// run, or the `<command>` placeholder when the payload carries none.
fn quoted_command(command: Option<&str>) -> String {
    match command {
        Some(command) => shell_single_quote(command),
        None => "<command>".to_string(),
    }
}

/// Single-quotes `text` for a shell command line, closing and reopening the
/// quote around every embedded single quote (`'\''`), so a command carrying
/// quotes or metacharacters stays one word.
fn shell_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use legion_cmd::{DenyDetails, Facts};
    use std::sync::Mutex;

    /// Answers every lookup with the same result.
    struct StubLookups(Lookup);

    impl LookupRunner for StubLookups {
        fn recall(&self, _repo: &str, _query: &str) -> error::Result<Lookup> {
            Ok(self.0.clone())
        }
        fn consult(&self, _query: &str) -> error::Result<Lookup> {
            Ok(self.0.clone())
        }
    }

    /// Fails every lookup.
    struct FailingLookups;

    impl LookupRunner for FailingLookups {
        fn recall(&self, _repo: &str, _query: &str) -> error::Result<Lookup> {
            Err(error::LegionError::Search("db unavailable".to_string()))
        }
        fn consult(&self, _query: &str) -> error::Result<Lookup> {
            Err(error::LegionError::Search("db unavailable".to_string()))
        }
    }

    /// Records what it was asked, and takes `delay` to answer.
    struct RecordingLookups {
        calls: Mutex<Vec<(String, String)>>,
        delay: Duration,
    }

    impl RecordingLookups {
        fn new(delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                delay,
            })
        }
        fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl LookupRunner for RecordingLookups {
        fn recall(&self, repo: &str, query: &str) -> error::Result<Lookup> {
            thread::sleep(self.delay);
            self.calls
                .lock()
                .expect("calls lock")
                .push((format!("recall:{repo}"), query.to_string()));
            Ok(Lookup::Empty)
        }
        fn consult(&self, query: &str) -> error::Result<Lookup> {
            thread::sleep(self.delay);
            self.calls
                .lock()
                .expect("calls lock")
                .push(("consult".to_string(), query.to_string()));
            Ok(Lookup::Empty)
        }
    }

    const REPO_CWD: &str = "/repo/legion";

    /// A policy that exercises every arm: `rm -rf` denies, `gh issue list`
    /// rewrites, `gh pr` asks (marked), `xxd` proxies, `ls` allows with a
    /// note, `git push` needs recall and consult.
    const POLICY: &str = r#"{
        "route": {"deadline_ms": 2000},
        "tools": {"Bash": {"families": {
            "rm": {"rules": [
                {"id": "rm-rf", "predicates": [{"kind": "arg-present", "arg": "-rf"}],
                 "outcome": {"kind": "deny", "reason": "unrecoverable", "instead": "trash it"}}
            ]},
            "gh issue": {"rules": [
                {"id": "gh-issue-list", "predicates": [{"kind": "arg-present", "arg": "list"}],
                 "outcome": {"kind": "rewrite", "target": "legion issue list", "reason": "legion tracks issues"}}
            ]},
            "gh pr": {"rules": [
                {"id": "gh-pr", "outcome": {"kind": "ask", "question": "touch the PR?",
                 "reason": "PRs are the orchestrator's", "needs_operator": true}}
            ]},
            "xxd": {"rules": [{"id": "xxd", "outcome": {"kind": "proxy", "reason": "binary"}}]},
            "ls": {"rules": [{"id": "ls", "outcome": {"kind": "allow", "note": "prefer legion sym tree"}}]},
            "git push": {"rules": [
                {"id": "git-push", "requires_recall": true, "requires_consult": true,
                 "outcome": {"kind": "deny", "reason": "push through legion", "instead": "legion push"}}
            ]}
        }}}
    }"#;

    fn payload(command: &str) -> String {
        json!({
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "session_id": "s1",
            "tool_use_id": "t1",
            "cwd": REPO_CWD
        })
        .to_string()
    }

    fn respond_stub(input: &str, policy: &str) -> Value {
        respond_with(
            input,
            Ok(policy.to_string()),
            Arc::new(StubLookups(Lookup::Empty)),
            None,
        )
    }

    fn output(response: &Value) -> &Value {
        response
            .get("hookSpecificOutput")
            .expect("every response carries hookSpecificOutput")
    }

    fn reason(response: &Value) -> &str {
        output(response)["permissionDecisionReason"]
            .as_str()
            .expect("a reason string")
    }

    fn assert_denied(response: &Value) {
        assert_eq!(output(response)["hookEventName"], "PreToolUse");
        assert_eq!(output(response)["permissionDecision"], "deny");
        assert!(output(response).get("updatedInput").is_none());
    }

    fn parsed_payload(command: &str) -> HookPayload {
        serde_json::from_str(&payload(command)).expect("valid payload")
    }

    // -- run_hook: one response, exit 0, never nothing ----------------------

    struct BrokenStdin;

    impl Read for BrokenStdin {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("stdin closed"))
        }
    }

    #[test]
    fn run_hook_writes_one_json_line_and_succeeds() {
        let mut out: Vec<u8> = Vec::new();
        let code: ExitCode = run_hook("not json".as_bytes(), &mut out);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
        let text = String::from_utf8(out).expect("utf-8");
        assert_eq!(text.lines().count(), 1);
        let response: Value = serde_json::from_str(text.trim()).expect("one JSON object");
        assert_denied(&response);
        assert!(reason(&response).contains("payload"));
    }

    #[test]
    fn an_unreadable_stdin_denies_naming_the_payload() {
        let mut out: Vec<u8> = Vec::new();
        let _code: ExitCode = run_hook(BrokenStdin, &mut out);
        let response: Value = serde_json::from_slice(&out).expect("JSON");
        assert_denied(&response);
        assert!(reason(&response).contains("payload: stdin closed"));
    }

    #[test]
    fn a_panic_anywhere_in_the_adapter_is_a_deny_naming_the_panic() {
        let response = guarded(|| panic!("simulated adapter bug"));
        assert_denied(&response);
        assert!(reason(&response).contains("panic: simulated adapter bug"));
    }

    // -- internal errors fail closed (FR-CMD-009) -----------------------------

    #[test]
    fn a_malformed_payload_denies() {
        let response = respond_stub("{\"tool_name\": ", POLICY);
        assert_denied(&response);
        assert!(reason(&response).contains("payload:"));
        assert!(reason(&response).contains("legion cmd-check -- <command>"));
    }

    #[test]
    fn an_unreadable_policy_denies_and_names_the_command_to_check() {
        let response = respond_with(
            &payload("echo hi"),
            Err(AdapterError::PolicyRead(
                "/nowhere/policy.json: missing".to_string(),
            )),
            Arc::new(StubLookups(Lookup::Empty)),
            None,
        );
        assert_denied(&response);
        assert!(reason(&response).contains("policy: /nowhere/policy.json: missing"));
        assert!(reason(&response).ends_with("instead: legion cmd-check -- 'echo hi'"));
    }

    #[test]
    fn a_policy_that_does_not_parse_denies() {
        let response = respond_stub(&payload("echo hi"), "{ not json");
        assert_denied(&response);
        assert!(reason(&response).contains("policy:"));
    }

    #[test]
    fn a_bad_route_setting_denies_rather_than_defaulting() {
        let response = respond_stub(
            &payload("echo hi"),
            r#"{"route": {"deadline_ms": "soon"}, "tools": {"Bash": {"families": {
                "rm": {"rules": [{"id": "r", "outcome": {"kind": "allow"}}]}}}}}"#,
        );
        assert_denied(&response);
        assert!(reason(&response).contains("route.deadline_ms"));
    }

    #[test]
    fn an_empty_policy_denies_every_command() {
        // FR-CMD-016: nothing runs until the policy is populated.
        let response = respond_stub(&payload("echo hi"), "{}");
        assert_denied(&response);
        assert!(reason(&response).contains("policy is empty"));
    }

    #[test]
    fn a_missing_policy_path_is_a_policy_read_error() {
        // With neither env var set the adapter has nowhere to look. The test
        // only exercises the message; the env itself is not mutated.
        let err = AdapterError::PolicyRead(format!(
            "neither {POLICY_PATH_ENV} nor {PLUGIN_ROOT_ENV} is set; no policy file to read"
        ));
        assert!(err.to_string().contains("LEGION_CMD_POLICY"));
    }

    // -- each Decision applied (FR-CMD-017) -----------------------------------

    #[test]
    fn an_unmanaged_command_passes_through_with_no_permission_decision() {
        // FR-CMD-002 / FR-CMD-016: the harness's own rules decide.
        let response = respond_stub(&payload("echo hi"), POLICY);
        let out = output(&response);
        assert_eq!(out["hookEventName"], "PreToolUse");
        assert!(out.get("permissionDecision").is_none());
        assert!(out.get("updatedInput").is_none());
    }

    #[test]
    fn an_allow_note_reaches_the_agent_as_additional_context() {
        let response = respond_stub(&payload("ls -la"), POLICY);
        let out = output(&response);
        assert!(out.get("permissionDecision").is_none());
        assert_eq!(out["additionalContext"], "prefer legion sym tree");
    }

    #[test]
    fn a_deny_carries_the_reason_and_the_command_to_run_instead() {
        let response = respond_stub(&payload("rm -rf build"), POLICY);
        assert_denied(&response);
        assert_eq!(reason(&response), "unrecoverable -- instead: trash it");
    }

    #[test]
    fn a_rewrite_patches_updated_input_and_tells_the_agent() {
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "gh issue list", "description": "list", "timeout": 9000,
                           "run_in_background": true},
            "cwd": REPO_CWD
        })
        .to_string();
        let response = respond_stub(&input, POLICY);
        let out = output(&response);
        assert_eq!(out["permissionDecision"], "allow");
        assert_eq!(
            out["updatedInput"],
            json!({"command": "legion issue list", "description": "list", "timeout": 9000,
                   "run_in_background": true})
        );
        let context = out["additionalContext"].as_str().expect("context");
        assert!(context.contains("`gh issue list`"));
        assert!(context.contains("`legion issue list`"));
        assert!(context.contains("legion tracks issues"));
    }

    #[test]
    fn a_rewrite_that_would_drop_an_operand_denies_instead() {
        // `gh issue list src/` carries a path fact the target cannot express;
        // the replacement refuses and the agent sees a deny, never a
        // narrower command that ran.
        let response = respond_stub(&payload("gh issue list src/"), POLICY);
        assert_denied(&response);
        assert!(reason(&response).contains("replacement:"));
        assert!(reason(&response).contains("path operand"));
    }

    #[test]
    fn a_proxy_runs_unchanged() {
        let response = respond_stub(&payload("xxd file.bin"), POLICY);
        let out = output(&response);
        assert!(out.get("permissionDecision").is_none());
        assert!(out.get("updatedInput").is_none());
    }

    #[test]
    fn an_ask_from_route_is_refused_with_question_reason_and_confirm_hint() {
        // route never sets the operator mark in this slice (#1227), so every
        // ask it returns takes the refusal path, whatever the rule says.
        let response = respond_stub(&payload("gh pr merge 7"), POLICY);
        assert_denied(&response);
        let text = reason(&response);
        assert!(text.contains("touch the PR?"));
        assert!(text.contains("PRs are the orchestrator's"));
        assert!(text.contains("legion cmd confirm --reason <why> -- 'gh pr merge 7'"));
    }

    #[test]
    fn a_parse_error_ask_is_refused_the_same_way() {
        let response = respond_stub(&payload("gh pr 'unterminated"), POLICY);
        assert_denied(&response);
        assert!(reason(&response).contains("could not be parsed"));
        assert!(reason(&response).contains("legion cmd confirm"));
    }

    fn ask_routed(needs_operator: bool) -> Routed {
        Routed {
            decision: Decision::ask("merge it?", "the agent said: hotfix").expect("valid ask"),
            facts: Facts::default(),
            deciding: Deciding::Rule {
                id: "gh-pr".to_string(),
                needs_operator,
            },
        }
    }

    #[test]
    fn an_unmarked_ask_never_prompts_the_operator() {
        let response = apply(&ask_routed(false), &parsed_payload("gh pr merge 7"));
        assert_denied(&response);
        assert!(reason(&response).contains("merge it?"));
    }

    #[test]
    fn a_marked_ask_prompts_the_operator_through_the_harness_with_the_reason() {
        // The only path that prompts the operator: the harness's own
        // permission prompt, carrying the reason.
        let response = apply(&ask_routed(true), &parsed_payload("gh pr merge 7"));
        let out = output(&response);
        assert_eq!(out["permissionDecision"], "ask");
        assert_eq!(out["permissionDecisionReason"], "the agent said: hotfix");
        assert!(out.get("updatedInput").is_none());
    }

    #[test]
    fn a_no_go_deny_keeps_its_fixed_instead_text() {
        let routed = Routed {
            decision: Decision::Deny(DenyDetails::no_go("never").expect("valid")),
            facts: Facts::default(),
            deciding: Deciding::Default,
        };
        let response = apply(&routed, &parsed_payload("forbidden"));
        assert_denied(&response);
        assert!(reason(&response).contains(legion_cmd::NO_GO_INSTEAD));
    }

    // -- lookups (FR-CMD-016) --------------------------------------------------

    #[test]
    fn a_failing_required_lookup_denies_naming_the_lookup() {
        let response = respond_with(
            &payload("git push origin main"),
            Ok(POLICY.to_string()),
            Arc::new(FailingLookups),
            None,
        );
        assert_denied(&response);
        assert!(reason(&response).contains("lookup:"));
        assert!(reason(&response).contains("db unavailable"));
    }

    #[test]
    fn a_fetched_lookup_reaches_route_and_the_rule_decides() {
        // With both lookups fetched, the rule's own deny fires -- proof the
        // results are wired into `Context`, not fetched and dropped.
        let response = respond_stub(&payload("git push origin main"), POLICY);
        assert_denied(&response);
        assert_eq!(
            reason(&response),
            "push through legion -- instead: legion push"
        );
    }

    #[test]
    fn a_required_recall_with_no_derivable_repo_denies() {
        let input = json!({"tool_name": "Bash", "tool_input": {"command": "git push"}}).to_string();
        let response = respond_stub(&input, POLICY);
        assert_denied(&response);
        assert!(reason(&response).contains("lookup:"));
        assert!(reason(&response).contains("none could be derived"));
    }

    #[test]
    fn the_lookups_are_scoped_to_the_validated_repo_and_queried_with_the_scan_text() {
        let lookups = RecordingLookups::new(Duration::ZERO);
        let response = respond_with(
            &payload("git push origin main"),
            Ok(POLICY.to_string()),
            lookups.clone(),
            None,
        );
        assert_denied(&response);
        assert_eq!(
            lookups.calls(),
            vec![
                (
                    "recall:legion".to_string(),
                    "git push origin main".to_string()
                ),
                ("consult".to_string(), "git push origin main".to_string()),
            ]
        );
    }

    #[test]
    fn legion_repo_scopes_the_recall_over_cwd() {
        let lookups = RecordingLookups::new(Duration::ZERO);
        let _response = respond_with(
            &payload("git push"),
            Ok(POLICY.to_string()),
            lookups.clone(),
            Some("other-repo".to_string()),
        );
        assert_eq!(lookups.calls()[0].0, "recall:other-repo");
    }

    // -- the deadline (FR-CMD-009) ---------------------------------------------

    #[test]
    fn an_overrun_denies_and_names_the_deadline() {
        let outcome: Result<Routed, AdapterError> = decide(Duration::from_millis(10), || {
            thread::sleep(Duration::from_millis(300));
            Ok(ask_routed(false))
        });
        let err = outcome.expect_err("10 ms against a 300 ms worker must overrun");
        assert!(matches!(
            err,
            AdapterError::DeadlineExceeded { deadline_ms: 10 }
        ));
        let response = deny_for_error(&err, Some(&parsed_payload("slow")));
        assert_denied(&response);
        assert!(reason(&response).contains("deadline exceeded: no decision within 10 ms"));
    }

    #[test]
    fn a_slow_lookup_overruns_the_configured_deadline_end_to_end() {
        let slow = RecordingLookups::new(Duration::from_millis(400));
        let policy = POLICY.replacen("\"deadline_ms\": 2000", "\"deadline_ms\": 20", 1);
        let response = respond_with(&payload("git push"), Ok(policy), slow, None);
        assert_denied(&response);
        assert!(reason(&response).contains("no decision within 20 ms"));
    }

    #[test]
    fn a_panic_in_the_worker_denies_naming_the_panic() {
        let outcome: Result<Routed, AdapterError> =
            decide(Duration::from_secs(5), || panic!("simulated route panic"));
        let err = outcome.expect_err("a panicked worker must not succeed");
        assert!(matches!(err, AdapterError::Panic(_)));
        let response = deny_for_error(&err, None);
        assert_denied(&response);
        assert!(reason(&response).contains("panic: simulated route panic"));
    }

    // -- the repo is derived and validated once --------------------------------

    #[test]
    fn legion_repo_wins_over_cwd() {
        assert_eq!(
            repo_for(Some("legion"), Some("/elsewhere/other")),
            Some("legion".to_string())
        );
    }

    #[test]
    fn an_unsafe_legion_repo_is_none_not_a_fall_through_to_cwd() {
        assert_eq!(repo_for(Some("bad;name"), Some(REPO_CWD)), None);
    }

    #[test]
    fn a_cwd_outside_git_falls_back_to_its_basename() {
        assert_eq!(repo_for(None, Some(REPO_CWD)), Some("legion".to_string()));
        assert_eq!(repo_for(None, None), None);
    }

    #[test]
    fn an_unsafe_cwd_basename_is_none() {
        for cwd in [
            "/tmp/legion; touch pwned",
            "/tmp/legion pwned",
            "/tmp/legion$(touch pwned)",
            "/tmp/legion`touch pwned`",
            "/tmp/.hidden",
            "/tmp/..",
        ] {
            assert_eq!(
                repo_for(None, Some(cwd)),
                None,
                "cwd {cwd:?} must not name a repo"
            );
        }
    }

    #[test]
    fn a_malicious_cwd_never_reaches_a_lookup_or_an_allow() {
        // End to end: the cwd from the review of the earlier build. Its
        // basename fails validation, so the required recall has no repo and
        // the command is denied; the recorder is never called and nothing
        // is allowed or rewritten.
        let lookups = RecordingLookups::new(Duration::ZERO);
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "git push"},
            "cwd": "/tmp/legion; touch pwned"
        })
        .to_string();
        let response = respond_with(&input, Ok(POLICY.to_string()), lookups.clone(), None);
        assert_denied(&response);
        assert!(lookups.calls().is_empty());
    }

    #[test]
    fn a_worktree_cwd_names_the_main_checkout_not_the_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_dir = dir.path().join("main-repo");
        std::fs::create_dir_all(&repo_dir).expect("mkdir");
        let config = dir.path().join("empty.gitconfig");
        std::fs::write(&config, "").expect("write config");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(&repo_dir)
                .env("GIT_CONFIG_GLOBAL", &config)
                .env("GIT_CONFIG_SYSTEM", &config)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .expect("git spawns");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["commit", "--allow-empty", "-q", "-m", "initial"]);
        git(&["worktree", "add", "-q", "wt", "-b", "feature"]);
        let worktree = repo_dir.join("wt");
        assert_eq!(
            repo_for(None, worktree.to_str()),
            Some("main-repo".to_string())
        );
    }

    // -- quoting -----------------------------------------------------------------

    #[test]
    fn the_command_is_quoted_as_one_shell_word_in_every_hint() {
        assert_eq!(shell_single_quote("it's; rm -rf /"), r"'it'\''s; rm -rf /'");
        let response = deny_for_error(
            &AdapterError::Lookup("x".to_string()),
            Some(&parsed_payload("echo 'a'; touch pwned")),
        );
        assert!(reason(&response).ends_with(r"legion cmd-check -- 'echo '\''a'\''; touch pwned'"));
    }
}
