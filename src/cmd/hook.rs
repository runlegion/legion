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
//! ([`ROUTE_DEADLINE`]) on a worker thread and turns an
//! overrun, a lookup failure, a replacement failure, an unreadable payload or
//! policy, and a panic anywhere into a deny with a reason. Nothing here
//! falls through to running the raw command, except a harness Grep, Glob or
//! Read call, which changes nothing on disk and runs as sent (#1338).
//!
//! The operator mode (`legion cmd-check -- <command>`, #1230,
//! `crate::cli::cmd_check`) runs the same core: [`read_policy_text`] and
//! [`route_call`] (policy loading, the settings, the repo, the lookup
//! pre-pass, route, all under the deadline), [`replacement_for`], and the
//! error deny from [`error_deny_reason`]. Only the rendering differs: this
//! module writes a hook response, the operator mode a report.
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
//!   an allow never grants a permission the harness would not (FR-CMD-002).
//!   A Bash command that matched no rule runs byte for byte as typed: the
//!   response carries no decision, no `additionalContext` and no note
//!   (#1337). A non-Bash rule's note, if any, rides along as
//!   `additionalContext`.
//! - a Bash insertion (#1337): `updatedInput` with `legion ` before each
//!   proxied name, and no `permissionDecision`, so the harness's own
//!   permission flow judges the command that will run and the insertion
//!   grants nothing the operator's rules would not ([`insertion_response`]).
//! - a non-Bash rewrite (Agent, Task): `allow` plus `updatedInput` built by
//!   `crate::cmd::replacement`, with what the call became and why in
//!   `additionalContext` (FR-CMD-003).
//! - an answered search (#1338): a harness Grep or Glob call the router
//!   let run, and legion can answer fully (`crate::cmd::answer`), runs with
//!   no `permissionDecision`, legion's answer as `additionalContext`, and,
//!   for Grep, `updatedInput` with `head_limit: 1` so the tool's own result
//!   is next to nothing. Anything legion cannot answer, or an answer that
//!   fails or runs out of time, runs the tool as sent.
//! - a harness Grep, Glob or Read call is never refused over an adapter
//!   failure (#1338): an unreadable policy, a store that cannot be opened,
//!   the deadline, or a panic lets it run as sent, with nothing added.
//! - deny: `deny` with the reason (FR-CMD-005). A Bash refusal names no
//!   command to run instead.
//! - ask without the operator mark: `deny` carrying route's question and
//!   reason and how to confirm (FR-CMD-006, FR-CMD-026). `deny` rather than
//!   `ask` because only a deny's reason reaches the agent, and the agent is
//!   who must answer the question. The operator is not prompted.
//! - ask with the operator mark set on `Routed` (#1227; route sets it only
//!   when the agent's confirmation is in `Context`, #1237): `ask`, the
//!   harness's own permission prompt, carrying the reason, and the inserted
//!   command as `updatedInput` when route rewrote it. This is the only path
//!   that prompts the operator. The adapter holds no routing branch for
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
//! # The rewrite prediction (#1272, FR-CMD-015)
//!
//! A rewrite the adapter applies -- a Bash insertion (#1337) or a non-Bash
//! rule's rewrite -- is a prediction that the constructed command will work: the adapter emits one `legion.cmd` prediction for it,
//! keyed by the call's `tool_use_id` (`crate::cmd::prediction`), after the
//! response is written and flushed (#1288), so a store write lock never
//! holds the response. Each run also starts, before its own work, a pass
//! that witnesses this session's earlier rewrites whose `tool_result` is now
//! in the session transcript. The pass runs on a worker thread beside the
//! decision: the decision never waits on it, and after the response it gets
//! only the rest of one [`ROUTE_DEADLINE`], then is abandoned. Both run
//! outside the decision, which reads neither: a failure in either is
//! reported on stderr and never changes the response. The store both use is
//! opened once, before the decision, and that open waits at most one
//! [`ROUTE_DEADLINE`]; the decision's confirmation and incident work
//! (#1237) shares the same handle. An open that fails or has not finished
//! by then skips both for the call, and the decision, which cannot read
//! confirmations without the store, is a deny naming why (FR-CMD-009).
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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use legion_cmd::{
    AskDetails, CommandKey, Context, Deciding, Decision, Lookup, Policy, Routed, ToolCall,
    parse_policy, required_lookups, route,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::cmd::answer::{self, Answer, Answerer, LocalAnswers, SearchCall};
use crate::cmd::confirm::{live_confirmations, use_confirmation, was_used};
use crate::cmd::incident::{IncidentLog, Origin};
use crate::cmd::prediction::{self, AppliedRewrite};
use crate::cmd::replacement::{build_replacement, rewritable_field, shell_single_quote};
use crate::db::Database;
use crate::error;
use crate::recall::{ArchiveMode, RecallResult, consult_bm25, recall_bm25};
use crate::telemetry::CmdIncidentRecord;
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
pub(crate) const LEGION_REPO_ENV: &str = "LEGION_REPO";

const HOOK_EVENT: &str = "PreToolUse";

/// The harness tools a legion failure never refuses (#1338): a failure
/// lets the call run as sent. Read, Grep and Glob change nothing on disk.
const RUNS_ON_FAILURE: [&str; 3] = ["Grep", "Glob", "Read"];

/// The harness search tools legion answers in the same response (#1338).
const ANSWERED_TOOLS: [&str; 2] = ["Grep", "Glob"];

/// How many reflections a required recall or consult lookup fetches into
/// `Context`. Small on purpose: the lookup runs inside the decision deadline.
const LOOKUP_LIMIT: usize = 5;

/// The response written when the adapter cannot serialize its own response.
/// A fixed string, not built with `serde_json`, so it cannot itself fail.
const FALLBACK_DENY_JSON: &str = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"legion-cmd could not serialize its response"}}"#;

/// How long the decision may take before the adapter denies (FR-CMD-009):
/// well under the harness's own hook timeout, so the adapter's deny reaches
/// the harness before the harness could time the hook out and fail open.
/// It bounds the pre-decision store open and the witness pass too.
pub(crate) const ROUTE_DEADLINE: Duration = Duration::from_millis(7000);

/// The PreToolUse payload fields the adapter acts on. Unknown fields are
/// ignored, not rejected: the harness may add fields. `session_id` binds the
/// confirmations the adapter reads and the incident records it writes to the
/// session (#1237). `tool_use_id`, `transcript_path` and `session_id` also
/// serve the rewrite prediction and its witness (#1272), which no decision
/// reads. `agent_id`, which the harness sends only from a subagent, and
/// `cwd` together tell a worktree-isolated agent (#1358).
#[derive(Debug, Deserialize)]
struct HookPayload {
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
}

impl HookPayload {
    /// The Bash command this payload carries, if any. Every message that
    /// names the command back to the agent goes through here.
    fn command(&self) -> Option<&str> {
        command_in(&self.tool_input)
    }

    /// True when the call comes from a subagent Claude Code isolated in its
    /// own worktree (#1358): the payload names a subagent (`agent_id`) and
    /// its `cwd` lies inside one of the harness's agent worktrees,
    /// `.claude/worktrees/agent-<id>`. A nested subagent carries its own
    /// `agent_id` but runs in its parent's worktree, so any agent worktree
    /// counts, not only one named after this agent. The harness's worktree
    /// guard refuses `legion git ...` in such an agent.
    fn worktree_isolated(&self) -> bool {
        let is_subagent: bool = self.agent_id.as_deref().is_some_and(|id| !id.is_empty());
        is_subagent && self.cwd.as_deref().is_some_and(in_agent_worktree)
    }

    /// The value a rewrite replaces -- the Bash command, or an Agent/Task
    /// spawn's `subagent_type` -- for the message that tells the agent what
    /// its call became. Reads the field `rewritable_field` names, so the
    /// message and the patch cannot name different fields.
    fn rewritten_value(&self) -> Option<&str> {
        let field: &str = rewritable_field(&self.tool_input)?;
        self.tool_input
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    }
}

/// The Bash command a `tool_input` carries, if any, for a message that names
/// the command back to its reader. Reads the one field; never scans it.
pub(crate) fn command_in(tool_input: &Value) -> Option<&str> {
    tool_input
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.is_empty())
}

/// True when `cwd` is inside a Claude Code agent worktree: a path holding
/// `.claude/worktrees/agent-<id>`, the folder the harness creates for an
/// agent spawned with worktree isolation.
fn in_agent_worktree(cwd: &str) -> bool {
    // Both separators: the harness reports a Windows cwd with `\`.
    let parts: Vec<&str> = cwd.split(['/', '\\']).collect();
    parts.windows(3).any(|window| {
        window[0] == ".claude"
            && window[1] == "worktrees"
            && window[2].len() > AGENT_WORKTREE_PREFIX.len()
            && window[2].starts_with(AGENT_WORKTREE_PREFIX)
    })
}

/// The prefix of the folder name the harness gives an isolated agent's
/// worktree.
const AGENT_WORKTREE_PREFIX: &str = "agent-";

/// Every way the adapter itself fails, each closed (FR-CMD-009). The deny
/// the agent sees names the variant and its detail.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AdapterError {
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
    /// The confirmation store could not be read (FR-CMD-026), including when
    /// the hook call's one store open, before the decision, failed or did not
    /// finish within the route deadline (#1288): a deny naming why, never a
    /// second wait on the store (FR-CMD-009).
    #[error("confirmations: {0}")]
    Confirmations(String),
    /// An incident record, or the use of a confirmation, could not be written;
    /// the command is refused even if it was confirmed (FR-CMD-027).
    #[error("incident record: {0}")]
    Record(String),
}

/// `legion cmd-check --hook`: reads one PreToolUse payload (JSON) on stdin and
/// writes one hook response (JSON) on stdout. Always exits 0 with a response.
///
/// The response, not the exit code, is the fail-closed signal: an unreadable
/// stdin, a panic anywhere in the adapter, and every failure inside it become
/// a deny body. A failed write to stdout has no recovery in-process;
/// `plugin/hooks/legion-cmd.sh` treats empty output as a broken adapter and
/// prints its own static deny.
///
/// The rewrite prediction and the witness pass's remaining budget come only
/// after the response is written and flushed (#1288): both can wait on the
/// store's write lock, and the response must never wait with them.
pub fn run_hook(mut stdin: impl Read, mut stdout: impl Write) -> ExitCode {
    let mut input = String::new();
    let mut after: AfterResponse = AfterResponse::default();
    let response: Value = match stdin.read_to_string(&mut input) {
        Ok(_) => guarded(&input, || respond(&input, &mut after)),
        Err(e) => deny_for_error(&AdapterError::Payload(e.to_string())),
    };
    let body: String =
        serde_json::to_string(&response).unwrap_or_else(|_| FALLBACK_DENY_JSON.to_string());
    let _ = writeln!(stdout, "{body}");
    let _ = stdout.flush();
    after.finish();
    ExitCode::SUCCESS
}

/// The one store connection a hook call opens (#1288): the decision's
/// confirmation and incident work (#1237) holds it first, then the rewrite
/// prediction after the response. Only one of them runs at a time, so the
/// lock is never contended; it exists because the decision runs on its own
/// worker thread.
type StoreHandle = Arc<Mutex<Database>>;

/// The work a run leaves for after its response is out: the prediction for
/// the rewrite it applied, with the store it was opened against, and the
/// witness pass still running.
#[derive(Default)]
struct AfterResponse {
    prediction: Option<(AppliedRewrite, StoreHandle)>,
    witness: Option<PendingWitness>,
}

impl AfterResponse {
    /// Emits the prediction, then gives the witness pass whatever is left
    /// of the one deadline it started under; the process exits after and
    /// takes an unfinished pass along. Neither can change the response,
    /// which is already written.
    fn finish(self) {
        if let Some((rewrite, store)) = self.prediction {
            best_effort("rewrite prediction", || {
                // A decision that panicked while holding the store denied;
                // a denied call has no rewrite, so poison is never read here.
                let db = store.lock().unwrap_or_else(PoisonError::into_inner);
                prediction::emit_rewrite_prediction(&db, &rewrite)?;
                Ok(())
            });
        }
        if let Some(witness) = self.witness {
            witness.wait();
        }
    }
}

/// Runs `build` and turns a panic anywhere inside it into a deny, so the
/// process never exits without a response over an adapter bug -- or, for a
/// Grep, Glob or Read payload, into running the call as sent (#1338).
fn guarded(input: &str, build: impl FnOnce() -> Value) -> Value {
    match panic::catch_unwind(AssertUnwindSafe(build)) {
        Ok(response) => response,
        Err(payload) => failure_response(
            tool_named_in(input).as_deref(),
            &AdapterError::Panic(panic_message(&payload)),
        ),
    }
}

/// The `tool_name` of a payload, when it parses that far.
fn tool_named_in(input: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct ToolName {
        tool_name: String,
    }
    serde_json::from_str::<ToolName>(input)
        .ok()
        .map(|payload| payload.tool_name)
}

/// The response to an adapter failure: a deny naming it (FR-CMD-009), or,
/// for a Grep, Glob or Read call, the call as sent with nothing added --
/// a legion failure never refuses those (#1338).
fn failure_response(tool: Option<&str>, err: &AdapterError) -> Value {
    if tool.is_some_and(|tool| RUNS_ON_FAILURE.contains(&tool)) {
        eprintln!("[legion cmd-check] {err}; the call runs as sent");
        return pass_through(None);
    }
    deny_for_error(err)
}

pub(crate) fn panic_message(payload: &Box<dyn Any + Send>) -> String {
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
///
/// Beside and after the decision, and never able to change it (#1272): the
/// pending-witness pass for this session runs concurrently with the decision
/// ([`respond_beside_witness`]), and the prediction for a rewrite the
/// decision applied is left, with the store to write it to, in `after` for
/// [`run_hook`] to finish once the response is written (#1288). Each runs
/// under [`best_effort`], so a failure or a panic in either is a line on
/// stderr, not a deny.
///
/// The store is opened once, here, before the pass starts: `Database::open`
/// runs the migration chain, and two connections migrating a fresh store at
/// once collide. The decision's confirmation and incident work and the emit
/// share this one handle; the pass and the decision's lookups open their own
/// connections only after it, when migration is done. The open waits at
/// most one route deadline ([`open_store_within`]). A store that cannot be
/// opened in that time skips the pass and the emit, and the decision, which
/// needs the store for its confirmations, is a deny naming why
/// (FR-CMD-009) rather than a second wait on the store.
fn respond(input: &str, after: &mut AfterResponse) -> Value {
    let policy_text: Result<String, AdapterError> = read_policy_text(None);
    let deadline: Duration = ROUTE_DEADLINE;
    let store: Result<StoreHandle, String> =
        open_store_within(deadline, crate::cli::util::open_db).map(|db| Arc::new(Mutex::new(db)));
    let legion_repo: Option<String> = std::env::var(LEGION_REPO_ENV).ok();
    let session_input: Option<String> = store.is_ok().then(|| input.to_string());
    let decision_store: Arc<dyn CmdStore> = Arc::new(LocalCmdStore {
        store: store.clone(),
    });
    let (applied, pending) = respond_beside_witness(
        deadline,
        || {
            respond_with(
                input,
                policy_text,
                Arc::new(StoreLookups),
                decision_store,
                legion_repo,
                Arc::new(LocalAnswers),
            )
        },
        move || match session_input {
            Some(session_input) => witness_session(&session_input),
            None => Ok(()),
        },
    );
    after.witness = pending;
    after.prediction = applied.rewrite.zip(store.ok());
    applied.response
}

/// Runs `open` on its own worker thread and waits at most `deadline` for the
/// store (#1288). An open that fails, panics, has not finished in time, or
/// whose thread ends without a result is an `Err` naming the cause, reported
/// on stderr here; the caller skips the prediction and the witness pass and
/// hands the cause to the decision. An open still running at the deadline is
/// left to finish or not: the process is one-shot, and it ends the thread.
fn open_store_within(
    deadline: Duration,
    open: impl FnOnce() -> error::Result<Database> + Send + 'static,
) -> Result<Database, String> {
    let (tx, rx) = mpsc::channel::<Result<Database, String>>();
    let spawned = thread::Builder::new()
        .name("legion-cmd-store-open".to_string())
        .spawn(move || {
            let opened: Result<Database, String> = match panic::catch_unwind(AssertUnwindSafe(open))
            {
                Ok(result) => result.map_err(|e| e.to_string()),
                Err(payload) => Err(format!("panic: {}", panic_message(&payload))),
            };
            let _ = tx.send(opened);
        });
    let opened: Result<Database, String> = match spawned {
        Err(e) => Err(format!("the store open could not start: {e}")),
        Ok(_) => match rx.recv_timeout(deadline) {
            Ok(opened) => opened,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(format!("the store could not be opened within {deadline:?}"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("the store open thread ended without a result".to_string())
            }
        },
    };
    if let Err(cause) = &opened {
        eprintln!(
            "[legion cmd-check] store open failed: {cause}; \
             the prediction and the witness pass are skipped"
        );
    }
    opened
}

/// Starts the witness pass on its own worker thread, then runs the decision
/// (`respond`, the adapter over its sources) as it always runs, under its own
/// deadline. The decision never waits on the pass. The pass's budget is one
/// `deadline` from its start, so from here the call stays within one
/// deadline plus the decision's overhead (the store open before it waits at
/// most one more).
fn respond_beside_witness(
    deadline: Duration,
    respond: impl FnOnce() -> Applied,
    witness: impl FnOnce() -> Result<(), Box<dyn std::error::Error>> + Send + 'static,
) -> (Applied, Option<PendingWitness>) {
    let pending: Option<PendingWitness> = PendingWitness::start(deadline, witness);
    let applied: Applied = respond();
    (applied, pending)
}

/// A witness pass running on its worker thread, with the instant its budget
/// ends. Dropping it, or [`PendingWitness::wait`] running out, abandons the
/// pass: the worker ends with the one-shot process, and the predictions it
/// did not reach stay emitted for a later run.
#[derive(Debug)]
struct PendingWitness {
    finished: mpsc::Receiver<()>,
    budget_ends: Instant,
}

impl PendingWitness {
    /// Spawns `pass` under [`best_effort`]. `None` when the thread cannot be
    /// started, or when `budget` ends past what the clock can represent
    /// (#1288): either only skips this run's pass.
    fn start(
        budget: Duration,
        pass: impl FnOnce() -> Result<(), Box<dyn std::error::Error>> + Send + 'static,
    ) -> Option<Self> {
        let Some(budget_ends) = Instant::now().checked_add(budget) else {
            eprintln!(
                "[legion cmd-check] witness pass skipped: a {budget:?} budget is out of range"
            );
            return None;
        };
        let (tx, finished) = mpsc::channel::<()>();
        let spawned = thread::Builder::new()
            .name("legion-cmd-witness".to_string())
            .spawn(move || {
                best_effort("witness pass", pass);
                let _ = tx.send(());
            });
        match spawned {
            Ok(_) => Some(Self {
                finished,
                budget_ends,
            }),
            Err(e) => {
                eprintln!("[legion cmd-check] witness pass not started: {e}");
                None
            }
        }
    }

    /// Waits for the pass until its budget ends, and no longer.
    fn wait(self) {
        let left: Duration = self.budget_ends.saturating_duration_since(Instant::now());
        if let Err(mpsc::RecvTimeoutError::Timeout) = self.finished.recv_timeout(left) {
            eprintln!("[legion cmd-check] witness pass abandoned at the deadline");
        }
    }
}

/// Witnesses this session's rewrites whose results are now in its
/// transcript. A payload with no session or transcript has nothing to
/// witness; the decision's own parse reports a malformed payload.
fn witness_session(input: &str) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(payload) = serde_json::from_str::<HookPayload>(input) else {
        return Ok(());
    };
    let (Some(session_id), Some(transcript)) = (payload.session_id, payload.transcript_path) else {
        return Ok(());
    };
    let db = crate::cli::util::open_db()?;
    prediction::witness_pending(&db, &session_id, Path::new(&transcript))?;
    Ok(())
}

/// Runs work that must never touch the decision: an error or a panic is
/// reported on stderr (never stdout, which carries the response) and
/// swallowed.
fn best_effort(what: &str, work: impl FnOnce() -> Result<(), Box<dyn std::error::Error>>) {
    let message: Option<String> = match panic::catch_unwind(AssertUnwindSafe(work)) {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(payload) => Some(format!("panic: {}", panic_message(&payload))),
    };
    if let Some(message) = message {
        eprintln!("[legion cmd-check] {what} failed: {message}");
    }
}

/// Where the adapter reads confirmations and writes incident records. A seam
/// so tests drive the adapter against a temporary store and log.
trait CmdStore: Send + Sync {
    /// The local legion store holding the confirmations. An `Err` is the
    /// deny the decision returns (FR-CMD-009): it names why there is no store.
    fn open(&self) -> Result<StoreHandle, AdapterError>;
    /// The incident log in legion's local telemetry.
    fn log(&self) -> IncidentLog;
    /// The agent a repo's commands are recorded under.
    fn agent_for(&self, repo: &str) -> String;
    /// Sends a first no-go hit's operator notice (FR-CMD-027).
    fn notify(&self, record: &CmdIncidentRecord) -> error::Result<()>;
}

/// The production store: the node's legion database, as the hook call's one
/// open left it (#1288), and its telemetry log. Never opens a second
/// connection: a failed or timed-out open is already the answer.
struct LocalCmdStore {
    store: Result<StoreHandle, String>,
}

impl CmdStore for LocalCmdStore {
    fn open(&self) -> Result<StoreHandle, AdapterError> {
        self.store.clone().map_err(AdapterError::Confirmations)
    }
    fn log(&self) -> IncidentLog {
        IncidentLog::production()
    }
    fn agent_for(&self, repo: &str) -> String {
        crate::cmd::incident::agent_for(repo)
    }
    fn notify(&self, record: &CmdIncidentRecord) -> error::Result<()> {
        crate::cmd::incident::send_notice(record)
    }
}

/// The hook mode's session work around route (#1237): pending drops before
/// the lookups, live confirmations into the Context, and the incident record
/// and confirmation use after route. The operator mode passes none to
/// [`route_call`], so a dry run never records, notifies, or uses up a
/// confirmation.
pub(crate) struct SessionWork {
    store: Arc<dyn CmdStore>,
    /// The payload's `session_id`, when it names one.
    session: Option<String>,
    /// The command as the agent issued it, for the incident record.
    issued: String,
}

/// The adapter over injected sources, so a test drives every branch without
/// touching the environment or the store. A Grep or Glob call the router
/// lets run with nothing added is offered to `answers` after the decision
/// (#1338).
fn respond_with(
    input: &str,
    policy_text: Result<String, AdapterError>,
    lookups: Arc<dyn LookupRunner>,
    store: Arc<dyn CmdStore>,
    legion_repo: Option<String>,
    answers: Arc<dyn Answerer>,
) -> Applied {
    let payload: HookPayload = match serde_json::from_str(input) {
        Ok(payload) => payload,
        Err(e) => {
            return failure_response(
                tool_named_in(input).as_deref(),
                &AdapterError::Payload(e.to_string()),
            )
            .into();
        }
    };
    let call = ToolCall {
        tool: payload.tool_name.clone(),
        input: payload.tool_input.clone(),
    };
    let session = SessionWork {
        store,
        session: payload.session_id.clone().filter(|s| !s.is_empty()),
        issued: match payload.command() {
            Some(command) => command.to_string(),
            None => format!("{} {}", payload.tool_name, payload.tool_input),
        },
    };
    match route_call(
        policy_text,
        call,
        lookups,
        legion_repo.clone(),
        payload.cwd.clone(),
        payload.worktree_isolated(),
        Some(session),
    ) {
        Ok(routed) => match answered(&routed, &payload, legion_repo, answers) {
            Some(answer) => answer_response(answer).into(),
            None => apply(&routed, &payload),
        },
        Err(e) => failure_response(Some(&payload.tool_name), &e).into(),
    }
}

/// Legion's answer to a harness Grep or Glob call the router let run with
/// nothing added, within [`answer::ANSWER_DEADLINE`]. `None` -- declined,
/// failed, too slow, or any other call -- leaves the decision as it was.
fn answered(
    routed: &Routed,
    payload: &HookPayload,
    legion_repo: Option<String>,
    answers: Arc<dyn Answerer>,
) -> Option<Answer> {
    if !ANSWERED_TOOLS.contains(&payload.tool_name.as_str())
        || routed.decision != (Decision::Allow { note: None })
    {
        return None;
    }
    let call = SearchCall {
        tool: payload.tool_name.clone(),
        input: payload.tool_input.clone(),
        cwd: payload.cwd.clone(),
        legion_repo,
    };
    answer::answer_within(answers, call, answer::ANSWER_DEADLINE)
}

/// An answered search (#1338): no `permissionDecision`, so the harness's
/// own rules decide and nothing is granted; the answer as
/// `additionalContext`, which reaches the agent in the same response; and
/// the input that keeps the tool's own result small, when there is one.
fn answer_response(answer: Answer) -> Value {
    let mut fields = Map::new();
    fields.insert("additionalContext".to_string(), Value::String(answer.text));
    if let Some(updated) = answer.updated_input {
        fields.insert("updatedInput".to_string(), updated);
    }
    hook_output(fields)
}

/// The decision core both modes run (FR-CMD-017). Parses the policy first
/// (the text is a local file, read outside the deadline), then runs repo
/// derivation, the lookup pre-pass, and route on a worker thread under
/// [`ROUTE_DEADLINE`]. Returns route's result. Every failure is an [`AdapterError`], which each mode turns into a deny
/// (FR-CMD-009, FR-CMD-016). `worktree_isolated` is whether the call comes
/// from a worktree-isolated agent (#1358), which only the hook payload can
/// say; the operator mode passes `false`. `session` is the hook mode's
/// [`SessionWork`], run inside the same deadline; the operator mode passes
/// `None`.
pub(crate) fn route_call(
    policy_text: Result<String, AdapterError>,
    call: ToolCall,
    lookups: Arc<dyn LookupRunner>,
    legion_repo: Option<String>,
    cwd: Option<String>,
    worktree_isolated: bool,
    session: Option<SessionWork>,
) -> Result<Routed, AdapterError> {
    // A policy file that cannot be read still leaves the built-in no-go list
    // in force (FR-CMD-025): route runs over an empty policy, so a no-go
    // match is refused (and, in the hook mode, recorded and notified), and
    // every other command is denied with the read error exactly as before. A
    // file that reads but does not parse is a different failure and denies
    // outright.
    let (policy, unread): (Arc<Policy>, Option<AdapterError>) = match policy_text {
        Err(e) => (Arc::new(Policy::default()), Some(e)),
        Ok(text) => {
            let policy: Policy =
                parse_policy(&text).map_err(|e| AdapterError::PolicyRead(e.to_string()))?;
            (Arc::new(policy), None)
        }
    };
    let worker_policy: Arc<Policy> = Arc::clone(&policy);
    let routed: Routed = decide(ROUTE_DEADLINE, move || {
        let repo: Option<String> = repo_for(legion_repo.as_deref(), cwd.as_deref());
        let now: DateTime<Utc> = Utc::now();
        // The hook's confirmation and incident-record work (#1237). Pending
        // drops are written before the adapter's own work (FR-CMD-027).
        let store: Option<(StoreHandle, IncidentLog)> = match &session {
            None => None,
            Some(work) => Some((work.store.open()?, work.store.log())),
        };
        // Held for the rest of the decision; released before the rewrite
        // prediction takes the same handle after the response.
        let db: Option<MutexGuard<'_, Database>> = store
            .as_ref()
            .map(|(handle, _)| handle.lock().unwrap_or_else(PoisonError::into_inner));
        if let (Some((_, log)), Some(db)) = (&store, &db) {
            log.record_pending_drops(&|id: &str| was_used(db, id), now)
                .map_err(|e| AdapterError::Record(e.to_string()))?;
        }

        let mut ctx: Context =
            fetch_context(&worker_policy, &call, repo.clone(), lookups.as_ref())?;
        ctx.worktree_isolated = worktree_isolated;
        if let (Some(work), Some(db)) = (&session, &db)
            && let Some(session) = &work.session
        {
            ctx.confirmations = live_confirmations(db, session, now)
                .map_err(|e| AdapterError::Confirmations(e.to_string()))?;
        }
        let routed: Routed = route(&worker_policy, &call, &ctx);

        if let (Some(work), Some((_, log)), Some(db)) = (session, &store, &db) {
            let repo: String = repo.clone().unwrap_or_default();
            let origin = Origin {
                command: work.issued,
                agent: work.store.agent_for(&repo),
                repo,
                session_id: work.session.unwrap_or_default(),
                cwd: cwd.unwrap_or_default(),
            };
            record_outcome(&routed, &origin, db, log, now, &|record| {
                work.store.notify(record)
            })?;
        }
        Ok(routed)
    })?;
    match unread {
        Some(read_error) if !matches!(routed.deciding, Deciding::NoGo { .. }) => Err(read_error),
        _ => Ok(routed),
    }
}

/// The replacement `tool_input` for a changed call (FR-CMD-003): a Bash
/// command with `legion ` inserted, on a rewrite or an operator ask, or a
/// spawn's new `subagent_type` on a non-Bash rewrite; `None` for every other
/// outcome. A replacement that cannot be built is an
/// [`AdapterError::Replacement`], a deny in both modes.
pub(crate) fn replacement_for(
    routed: &Routed,
    original: &Value,
) -> Result<Option<Value>, AdapterError> {
    let value: Option<&str> = match (&routed.decision, routed.facts.rewritten.as_deref()) {
        (Decision::Rewrite { .. } | Decision::Ask(_), Some(rewritten)) => Some(rewritten),
        // A Bash command changes only by insertion: a rewrite with nothing
        // inserted has nothing to run, and the target is not a command.
        (Decision::Rewrite { .. }, None) if rewritable_field(original) == Some("command") => {
            return Err(AdapterError::Replacement(
                "the rewrite carries no rewritten command".to_string(),
            ));
        }
        (Decision::Rewrite { target, .. }, None) => Some(target.as_str()),
        _ => None,
    };
    value
        .map(|value| {
            build_replacement(original, value).map_err(|e| AdapterError::Replacement(e.to_string()))
        })
        .transpose()
}

/// One hook response, and the rewrite it applied if it applied one -- the
/// only thing the rewrite prediction is emitted from, so a rewrite that
/// became a deny (a replacement that could not be built) emits nothing.
#[derive(Debug)]
struct Applied {
    response: Value,
    rewrite: Option<AppliedRewrite>,
}

impl From<Value> for Applied {
    fn from(response: Value) -> Self {
        Self {
            response,
            rewrite: None,
        }
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
pub(crate) trait LookupRunner: Send + Sync {
    fn recall(&self, repo: &str, query: &str) -> error::Result<Lookup>;
    fn consult(&self, query: &str) -> error::Result<Lookup>;
}

/// The production runner: BM25 over the local store. BM25 only, no embedding
/// model -- loading the model per hook invocation would spend the decision
/// deadline on setup, which is the failure the deadline exists to catch.
pub(crate) struct StoreLookups;

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

/// Reads the policy file: `explicit` when given (the operator mode's
/// `--policy`), else `LEGION_CMD_POLICY`, else
/// `${CLAUDE_PLUGIN_ROOT}/legion-cmd/policy.json`. None of them is a
/// `PolicyRead` failure, which denies: no policy is not an empty policy, but
/// it is refused the same way (FR-CMD-016).
pub(crate) fn read_policy_text(explicit: Option<&Path>) -> Result<String, AdapterError> {
    let path: PathBuf = match explicit {
        Some(path) => path.to_path_buf(),
        None => policy_path()?,
    };
    std::fs::read_to_string(&path)
        .map_err(|e| AdapterError::PolicyRead(format!("{}: {e}", path.display())))
}

fn policy_path() -> Result<PathBuf, AdapterError> {
    configured_policy_path().ok_or_else(|| {
        AdapterError::PolicyRead(format!(
            "neither {POLICY_PATH_ENV} nor {PLUGIN_ROOT_ENV} is set; no policy file to read"
        ))
    })
}

/// The policy file the environment names: `LEGION_CMD_POLICY`, else
/// `${CLAUDE_PLUGIN_ROOT}/legion-cmd/policy.json`, else `None`. Shared with
/// `legion cmd confirm`, which reads the same file for its no-go entries.
pub(crate) fn configured_policy_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var(POLICY_PATH_ENV) {
        return Some(PathBuf::from(path));
    }
    std::env::var(PLUGIN_ROOT_ENV)
        .ok()
        .map(|root| PathBuf::from(root).join("legion-cmd").join("policy.json"))
}

/// The repo a recall lookup is scoped to (see the module doc). `LEGION_REPO`
/// when set and non-empty, else the name derived from `cwd`; `None` when
/// neither yields a name that passes [`is_safe_repo_name`]. An unsafe
/// `LEGION_REPO` is `None`, not a fall-through to `cwd`: an explicit override
/// that fails validation is a misconfiguration, not a hint.
pub(crate) fn repo_for(legion_repo: Option<&str>, cwd: Option<&str>) -> Option<String> {
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

/// What the agent reads after a refusal whose incident record was written
/// (FR-CMD-027). Every path that reaches [`apply`] with a no-go hit or an ask
/// to the agent has written its record first; a failed write is a deny from
/// [`deny_for_error`] instead, which never claims a record.
const RECORDED_NOTE: &str = "This attempt was recorded.";

/// Writes the incident record route's outcome calls for (FR-CMD-027) and uses
/// up the confirmation route reports it used (FR-CMD-026). A no-go hit and an
/// ask are recorded, the ask at both stages: put to the agent, and at the
/// operator prompt, the second stage of an ask the agent already confirmed,
/// which carries the agent's reason. Any failure refuses the command, even a
/// confirmed one.
fn record_outcome(
    routed: &Routed,
    origin: &Origin,
    db: &Database,
    log: &IncidentLog,
    now: DateTime<Utc>,
    notify: &dyn Fn(&CmdIncidentRecord) -> error::Result<()>,
) -> Result<(), AdapterError> {
    let key: Option<&CommandKey> = routed.facts.command_key.as_ref();
    let failed = |e: error::LegionError| AdapterError::Record(e.to_string());
    match (&routed.decision, &routed.deciding) {
        (Decision::Deny(_), Deciding::NoGo { id }) => {
            log.record_no_go(origin, id, key.map(CommandKey::as_str), now, notify)
                .map_err(failed)?;
        }
        // The operator prompt: the second stage of an ask the agent already
        // confirmed, recorded with the agent's reason it carries.
        (
            Decision::Ask(details),
            Deciding::Rule {
                id,
                needs_operator: true,
            },
        ) => {
            log.record_ask(
                origin,
                Some(id.as_str()),
                key.map(CommandKey::as_str),
                Some(details.reason()),
                now,
            )
            .map_err(failed)?;
        }
        (Decision::Ask(_), deciding) => {
            let entry: Option<&str> = match deciding {
                Deciding::Rule { id, .. } | Deciding::NoGo { id } => Some(id.as_str()),
                Deciding::ParseError | Deciding::Default => None,
            };
            log.record_ask(origin, entry, key.map(CommandKey::as_str), None, now)
                .map_err(failed)?;
        }
        _ => {}
    }
    if routed.confirmed {
        let key: &CommandKey = key.ok_or_else(|| {
            AdapterError::Record("route used a confirmation for a command with no key".to_string())
        })?;
        if !use_confirmation(db, &origin.session_id, key, now).map_err(failed)? {
            return Err(AdapterError::Record(
                "the confirmation route used is no longer live".to_string(),
            ));
        }
    }
    Ok(())
}

/// Applies route's Decision to the hook response (FR-CMD-017). A rewrite whose replacement was built is also returned as the
/// [`AppliedRewrite`] its prediction records (#1272); a refused rewrite is a
/// deny and carries none.
fn apply(routed: &Routed, payload: &HookPayload) -> Applied {
    let replacement: Option<Value> = match replacement_for(routed, &payload.tool_input) {
        Ok(replacement) => replacement,
        Err(e) => return deny_for_error(&e).into(),
    };
    let response: Value = match (&routed.decision, replacement) {
        (Decision::Allow { note }, _) => pass_through(note.as_deref()),
        (Decision::Rewrite { reason, .. }, Some(updated)) => {
            let constructed: String = rewritten_value_in(&updated).unwrap_or_default().to_string();
            let response: Value = if routed.facts.rewritten.is_some() {
                insertion_response(updated)
            } else {
                rewrite_response(payload.rewritten_value(), &constructed, reason, updated)
            };
            return Applied {
                response,
                rewrite: Some(applied_rewrite(payload, constructed)),
            };
        }
        (Decision::Rewrite { .. }, None) => deny_for_error(&AdapterError::Replacement(
            "the rewrite carries nothing to run".to_string(),
        )),
        (Decision::Deny(details), _) => {
            let mut reason: String = details.reason().to_string();
            // A Bash refusal names no replacement; a non-Bash rule's deny
            // still says what to do instead.
            if details.instead() != legion_cmd::NO_GO_INSTEAD {
                reason = format!("{reason} -- instead: {}", details.instead());
            }
            if matches!(routed.deciding, Deciding::NoGo { .. }) {
                reason.push_str(". ");
                reason.push_str(RECORDED_NOTE);
            }
            deny_response(&reason)
        }
        (Decision::Ask(details), updated) => {
            ask_response(details, &routed.deciding, payload, updated)
        }
    };
    response.into()
}

/// The value a built replacement put in place of the call's -- the command,
/// or the `subagent_type` -- read from the field `rewritable_field` names, so
/// the message, the prediction and the patch cannot name different values.
fn rewritten_value_in(updated: &Value) -> Option<&str> {
    let field: &str = rewritable_field(updated)?;
    updated.get(field).and_then(Value::as_str)
}

/// The rewrite as its prediction records it: the call's ids, the value as
/// issued and as constructed, and whether the call runs in the background
/// (whose transcript result records only that it started).
fn applied_rewrite(payload: &HookPayload, constructed: String) -> AppliedRewrite {
    AppliedRewrite {
        tool_use_id: payload.tool_use_id.clone(),
        session_id: payload.session_id.clone(),
        tool_name: payload.tool_name.clone(),
        issued: payload.rewritten_value().map(str::to_string),
        constructed,
        background: payload
            .tool_input
            .get("run_in_background")
            .and_then(Value::as_bool)
            .unwrap_or(false),
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

/// A Bash insertion (#1337): the command with `legion ` before each proxied
/// name as `updatedInput`, and no `permissionDecision`, so the harness's own
/// permission flow judges the command that will run: the insertion grants
/// nothing the operator's rules would not. The one place the insertion's
/// response shape is decided.
fn insertion_response(updated: Value) -> Value {
    let mut fields = Map::new();
    fields.insert("updatedInput".to_string(), updated);
    hook_output(fields)
}

/// A non-Bash rewrite (FR-CMD-003): `allow` so the replacement runs, the
/// patched `tool_input` as `updatedInput`, and what the command became and
/// why as `additionalContext`, so the agent is never left believing its
/// original command ran (FR-CMD-009).
fn rewrite_response(
    original: Option<&str>,
    constructed: &str,
    reason: &str,
    updated: Value,
) -> Value {
    let message: String = format!(
        "legion-cmd rewrote `{}` to `{constructed}`: {reason}",
        original.unwrap_or("<command>"),
    );
    let mut fields = decision_fields("allow", &message);
    fields.insert("updatedInput".to_string(), updated);
    fields.insert("additionalContext".to_string(), Value::String(message));
    hook_output(fields)
}

/// The ask (FR-CMD-006). With the operator mark set on `Routed`, the
/// harness's own permission prompt carrying the reason, and the inserted
/// command as `updatedInput` when route rewrote one. Without it, a
/// refusal the agent reads: the question, the reason, and how to confirm
/// (`legion cmd confirm`, #1237). The confirm hint does not pre-fill the
/// reason: the agent must state its own, not copy the policy's.
fn ask_response(
    details: &AskDetails,
    deciding: &Deciding,
    payload: &HookPayload,
    updated: Option<Value>,
) -> Value {
    if matches!(
        deciding,
        Deciding::Rule {
            needs_operator: true,
            ..
        }
    ) {
        let mut fields = decision_fields("ask", details.reason());
        if let Some(updated) = updated {
            fields.insert("updatedInput".to_string(), updated);
        }
        return hook_output(fields);
    }
    deny_response(&format!(
        "{} -- {}. {RECORDED_NOTE} To confirm: legion cmd confirm --reason <why> -- {}",
        details.question(),
        details.reason(),
        quoted_command(payload.command())
    ))
}

/// The deny for an adapter failure (FR-CMD-005, FR-CMD-009): the reason
/// names the failure. It names no command to run instead (#1337).
fn deny_for_error(err: &AdapterError) -> Value {
    deny_response(&error_deny_reason(err))
}

/// The reason for a deny over an adapter failure, shared by both modes so the
/// operator mode reports the deny the hook would send.
pub(crate) fn error_deny_reason(err: &AdapterError) -> String {
    format!("legion-cmd could not decide this command ({err})")
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

#[cfg(test)]
mod tests {
    use super::*;
    use legion_cmd::{DenyDetails, Facts, ManagedTarget};
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

    /// Answers every search with the same result, and records the calls.
    struct StubAnswers {
        answer: Option<Answer>,
        calls: Mutex<Vec<SearchCall>>,
    }

    impl Answerer for StubAnswers {
        fn answer(&self, call: &SearchCall) -> Option<Answer> {
            self.calls.lock().expect("calls lock").push(call.clone());
            self.answer.clone()
        }
    }

    fn stub_answers(answer: Option<Answer>) -> Arc<StubAnswers> {
        Arc::new(StubAnswers {
            answer,
            calls: Mutex::new(Vec::new()),
        })
    }

    /// Declines every search: the tool runs as sent.
    fn no_answers() -> Arc<dyn Answerer> {
        stub_answers(None)
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

    /// A confirmation store and incident log in a temporary directory.
    struct TempStore {
        dir: tempfile::TempDir,
        /// The entry of every operator notice sent, in order.
        notices: Mutex<Vec<String>>,
        /// When set, every notice fails with this text.
        notice_failure: Option<String>,
    }

    impl TempStore {
        fn db_path(&self) -> PathBuf {
            self.dir.path().join("legion.db")
        }
        fn log_path(&self) -> PathBuf {
            self.dir.path().join("cmd-incidents.jsonl")
        }
        /// A connection of the test's own, to seed or inspect the store.
        fn db(&self) -> Database {
            Database::open(&self.db_path()).expect("db")
        }
    }

    impl CmdStore for TempStore {
        fn open(&self) -> Result<StoreHandle, AdapterError> {
            Database::open(&self.db_path())
                .map(|db| Arc::new(Mutex::new(db)))
                .map_err(|e| AdapterError::Confirmations(e.to_string()))
        }
        fn log(&self) -> IncidentLog {
            IncidentLog::at(self.log_path())
        }
        fn agent_for(&self, repo: &str) -> String {
            format!("agent-of-{repo}")
        }
        fn notify(&self, record: &CmdIncidentRecord) -> error::Result<()> {
            if let Some(failure) = &self.notice_failure {
                return Err(error::LegionError::Telemetry(failure.clone()));
            }
            self.notices
                .lock()
                .expect("notices lock")
                .push(record.entry.clone().unwrap_or_default());
            Ok(())
        }
    }

    fn temp_store() -> Arc<TempStore> {
        Arc::new(TempStore {
            dir: tempfile::tempdir().expect("tempdir"),
            notices: Mutex::new(Vec::new()),
            notice_failure: None,
        })
    }

    const REPO_CWD: &str = "/repo/legion";

    /// A policy that exercises every arm: `git`, `gh`, `grep` and `rg` get
    /// `legion ` inserted, `rm -rf` never runs, `curl` is asked, a forced
    /// `legion ... push` is a power switch, a Read allows with a note, a
    /// WebFetch needs recall and consult. The Agent and Edit rewrite rules
    /// back the hand-built `Routed` values that name them.
    const POLICY: &str = r#"{
        "proxy": ["git", "gh", "grep", "rg"],
        "never_run": [
            {"id": "rm-rf", "names": ["rm"], "reason": "unrecoverable",
             "predicates": [{"kind": "flag", "short": ["r"]}, {"kind": "flag", "short": ["f"]}]}
        ],
        "ask": [
            {"id": "curl-network", "names": ["curl"], "reason": "curl reaches the network"}
        ],
        "power_switches": [
            {"id": "push-force", "names": ["legion"], "reason": "a forced push",
             "predicates": [{"kind": "operand", "equals": ["push"]},
                            {"kind": "flag", "short": ["f"], "long": ["force"]}]}
        ],
        "tools": {
          "Agent": {"rules": [
              {"id": "agent-explore-to-legion",
               "predicates": [{"kind": "field-equals", "field": "subagent_type", "any_of": ["Explore"]}],
               "outcome": {"kind": "rewrite", "target": "legion:legion-explore",
                           "reason": "use the legion explorer"}}
          ]},
          "Edit": {"rules": [
              {"id": "edit-rewrite",
               "predicates": [{"kind": "field-equals", "field": "file_path", "any_of": ["a.rs"]}],
               "outcome": {"kind": "rewrite", "target": "legion issue list",
                           "reason": "a hand-built rewrite on an Edit"}}
          ]},
          "Read": {"rules": [
              {"id": "read-note", "outcome": {"kind": "allow", "note": "prefer legion sym tree"}}
          ]},
          "WebFetch": {"rules": [
              {"id": "fetch-lookups", "requires_recall": true, "requires_consult": true,
               "outcome": {"kind": "deny", "reason": "fetch through recall", "instead": "legion recall"}}
          ]}
        }
    }"#;

    /// A WebFetch payload: the tool whose test rule needs both lookups.
    fn fetch_payload(url: &str, cwd: Option<&str>) -> String {
        let mut value = json!({
            "tool_name": "WebFetch",
            "tool_input": {"url": url},
            "session_id": "s1",
            "tool_use_id": "t1"
        });
        if let Some(cwd) = cwd {
            value["cwd"] = json!(cwd);
        }
        value.to_string()
    }

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

    fn respond_applied(input: &str, policy: &str) -> Applied {
        respond_with(
            input,
            Ok(policy.to_string()),
            Arc::new(StubLookups(Lookup::Empty)),
            temp_store(),
            None,
            no_answers(),
        )
    }

    fn respond_stub(input: &str, policy: &str) -> Value {
        respond_applied(input, policy).response
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
        let response = guarded(&payload("ls"), || panic!("simulated adapter bug"));
        assert_denied(&response);
        assert!(reason(&response).contains("panic: simulated adapter bug"));
    }

    // -- internal errors fail closed (FR-CMD-009) -----------------------------

    #[test]
    fn a_malformed_payload_denies() {
        let response = respond_stub("{\"tool_name\": ", POLICY);
        assert_denied(&response);
        assert!(reason(&response).contains("payload:"));
        assert!(!reason(&response).contains("instead"));
    }

    #[test]
    fn an_unreadable_policy_denies_naming_the_failure_and_no_command() {
        let response = respond_with(
            &payload("echo hi"),
            Err(AdapterError::PolicyRead(
                "/nowhere/policy.json: missing".to_string(),
            )),
            Arc::new(StubLookups(Lookup::Empty)),
            temp_store(),
            None,
            no_answers(),
        )
        .response;
        assert_denied(&response);
        assert!(reason(&response).contains("policy: /nowhere/policy.json: missing"));
        assert!(!reason(&response).contains("instead"));
    }

    #[test]
    fn a_policy_that_does_not_parse_denies() {
        let response = respond_stub(&payload("echo hi"), "{ not json");
        assert_denied(&response);
        assert!(reason(&response).contains("policy:"));
    }

    #[test]
    fn a_policy_with_a_key_outside_the_four_lists_denies() {
        // #1337: the deadline no longer lives in the policy file, so a
        // `route` key is an unknown field and the file fails to parse.
        let response = respond_stub(&payload("echo hi"), r#"{"route": {"deadline_ms": 7000}}"#);
        assert_denied(&response);
        assert!(reason(&response).contains("policy:"));
        assert!(reason(&response).contains("route"));
    }

    #[test]
    fn an_empty_policy_still_refuses_the_builtins_and_runs_the_rest() {
        let refused = respond_stub(&payload("rm -rf /"), "{}");
        assert_denied(&refused);
        let untouched = respond_stub(&payload("git status"), "{}");
        let out = output(&untouched);
        assert!(out.get("permissionDecision").is_none());
        assert!(out.get("updatedInput").is_none());
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
    fn a_command_matching_no_rule_runs_as_typed_with_nothing_added() {
        // #1337: no decision, no additionalContext, no note, no updatedInput.
        for command in ["echo hi", "ls -la", "echo git", "sh -c 'git status'"] {
            let response = respond_stub(&payload(command), POLICY);
            assert_eq!(
                response,
                json!({"hookSpecificOutput": {"hookEventName": "PreToolUse"}}),
                "{command}"
            );
        }
    }

    #[test]
    fn a_rule_allow_without_a_note_adds_no_default_note() {
        // Only the FR-CMD-016 default is labelled as one; a matched rule's
        // allow is the rule's decision, not a default.
        let routed = Routed {
            decision: Decision::Allow { note: None },
            facts: Facts::default(),
            deciding: Deciding::Rule {
                id: "ls".to_string(),
                needs_operator: false,
            },
            confirmed: false,
        };
        let response = apply(&routed, &parsed_payload("ls")).response;
        assert!(output(&response).get("additionalContext").is_none());
    }

    #[test]
    fn a_rewrite_of_a_tool_with_no_rewritable_field_denies() {
        // An Edit/Write rewrite has no `command` or `subagent_type` to
        // replace. Patching one in would explicitly allow the untouched
        // original call, a permission the harness would not grant; it denies.
        let routed = Routed {
            decision: Decision::Rewrite {
                target: ManagedTarget::new("legion issue list"),
                reason: "a hand-built rewrite on an Edit".to_string(),
            },
            facts: Facts::default(),
            deciding: Deciding::Rule {
                id: "edit-rewrite".to_string(),
                needs_operator: false,
            },
            confirmed: false,
        };
        let payload: HookPayload = serde_json::from_value(json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": "a.rs", "old_string": "x", "new_string": "y"},
            "cwd": REPO_CWD
        }))
        .expect("valid payload");
        let response = apply(&routed, &payload).response;
        assert_denied(&response);
        assert!(reason(&response).contains("replacement:"));
    }

    #[test]
    fn an_explore_spawn_is_rewritten_to_the_legion_explorer() {
        // #1233: the no-harness-explore.sh case through the adapter -- allow,
        // updatedInput with only subagent_type changed, and a message naming
        // what the spawn was and what it became.
        let routed = Routed {
            decision: Decision::Rewrite {
                target: ManagedTarget::new("legion:legion-explore"),
                reason: "use the legion explorer".to_string(),
            },
            facts: Facts::default(),
            deciding: Deciding::Rule {
                id: "agent-explore-to-legion".to_string(),
                needs_operator: false,
            },
            confirmed: false,
        };
        let payload: HookPayload = serde_json::from_value(json!({
            "tool_name": "Agent",
            "tool_input": {"subagent_type": "Explore", "prompt": "map it"},
            "cwd": REPO_CWD
        }))
        .expect("valid payload");
        let out = apply(&routed, &payload).response["hookSpecificOutput"].clone();
        assert_eq!(out["permissionDecision"], "allow");
        assert_eq!(
            out["updatedInput"],
            json!({"subagent_type": "legion:legion-explore", "prompt": "map it"})
        );
        let context = out["additionalContext"].as_str().expect("context");
        assert!(context.contains("`Explore`"), "got: {context}");
        assert!(
            context.contains("`legion:legion-explore`"),
            "got: {context}"
        );
    }

    #[test]
    fn a_tool_rule_allow_note_reaches_the_agent_as_additional_context() {
        let input = json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "a.md"},
            "cwd": REPO_CWD
        })
        .to_string();
        let response = respond_stub(&input, POLICY);
        let out = output(&response);
        assert!(out.get("permissionDecision").is_none());
        assert_eq!(out["additionalContext"], "prefer legion sym tree");
    }

    #[test]
    fn a_never_run_deny_carries_its_reason_and_names_no_command() {
        let response = respond_stub(&payload("rm -rf build"), POLICY);
        assert_denied(&response);
        assert_eq!(reason(&response), format!("unrecoverable. {RECORDED_NOTE}"));
    }

    #[test]
    fn an_insertion_patches_updated_input_and_grants_nothing() {
        // #1337: `updatedInput` carries the inserted command with every
        // sibling field kept, and no `permissionDecision`: the harness's own
        // permission flow judges the command that runs.
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "cd a && git log | grep x", "description": "log",
                           "timeout": 9000, "run_in_background": true},
            "cwd": REPO_CWD
        })
        .to_string();
        let response = respond_stub(&input, POLICY);
        assert_eq!(
            response,
            json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": {"command": "cd a && legion git log | legion grep x",
                                 "description": "log", "timeout": 9000,
                                 "run_in_background": true}
            }})
        );
    }

    #[test]
    fn a_bash_rewrite_with_nothing_inserted_is_refused() {
        let routed = Routed {
            decision: Decision::Rewrite {
                target: ManagedTarget::new("legion"),
                reason: "a hand-built rewrite".to_string(),
            },
            facts: Facts::default(),
            deciding: Deciding::Rule {
                id: legion_cmd::PROXY_ID.to_string(),
                needs_operator: false,
            },
            confirmed: false,
        };
        let applied = apply(&routed, &parsed_payload("git status"));
        assert!(
            applied.rewrite.is_none(),
            "a refused rewrite yields no prediction"
        );
        assert_denied(&applied.response);
        assert!(reason(&applied.response).contains("no rewritten command"));
    }

    #[test]
    fn an_ask_from_route_is_refused_with_question_reason_and_confirm_hint() {
        // Without a confirmation route leaves the operator mark unset, so the
        // ask takes the refusal path (#1237).
        let response = respond_stub(&payload("curl example.com"), POLICY);
        assert_denied(&response);
        let text = reason(&response);
        assert!(text.contains("needs the operator's approval"));
        assert!(text.contains("curl reaches the network"));
        assert!(text.contains("legion cmd confirm --reason <why> -- 'curl example.com'"));
    }

    #[test]
    fn a_parse_error_is_denied_naming_the_parser() {
        let response = respond_stub(&payload("gh pr 'unterminated"), POLICY);
        assert_denied(&response);
        assert!(reason(&response).contains("shell parser"));
        assert!(!reason(&response).contains("instead"));
    }

    fn ask_routed(needs_operator: bool) -> Routed {
        Routed {
            decision: Decision::ask("merge it?", "the agent said: hotfix").expect("valid ask"),
            facts: Facts::default(),
            deciding: Deciding::Rule {
                id: "gh-pr".to_string(),
                needs_operator,
            },
            confirmed: false,
        }
    }

    #[test]
    fn an_unmarked_ask_never_prompts_the_operator() {
        let response = apply(&ask_routed(false), &parsed_payload("gh pr merge 7")).response;
        assert_denied(&response);
        assert!(reason(&response).contains("merge it?"));
    }

    #[test]
    fn a_marked_ask_prompts_the_operator_through_the_harness_with_the_reason() {
        // The only path that prompts the operator: the harness's own
        // permission prompt, carrying the reason.
        let response = apply(&ask_routed(true), &parsed_payload("gh pr merge 7")).response;
        let out = output(&response);
        assert_eq!(out["permissionDecision"], "ask");
        assert_eq!(out["permissionDecisionReason"], "the agent said: hotfix");
        assert!(out.get("updatedInput").is_none());

        // With a rewritten command, the operator's ask carries it.
        let mut rewritten = ask_routed(true);
        rewritten.facts.rewritten = Some("legion gh pr merge 7".to_string());
        let response = apply(&rewritten, &parsed_payload("gh pr merge 7")).response;
        let out = output(&response);
        assert_eq!(out["permissionDecision"], "ask");
        assert_eq!(out["updatedInput"]["command"], "legion gh pr merge 7");
    }

    #[test]
    fn a_no_go_deny_names_no_command_and_says_it_was_recorded() {
        let routed = Routed {
            decision: Decision::Deny(DenyDetails::no_go("never").expect("valid")),
            facts: Facts::default(),
            deciding: Deciding::NoGo {
                id: "fork-bomb".to_string(),
            },
            confirmed: false,
        };
        let response = apply(&routed, &parsed_payload("forbidden")).response;
        assert_denied(&response);
        assert!(!reason(&response).contains(legion_cmd::NO_GO_INSTEAD));
        assert_eq!(reason(&response), format!("never. {RECORDED_NOTE}"));
    }

    // -- no-go, confirmations, and incident records (#1237) --------------------

    /// The policy the confirmation tests run under.
    fn confirm_policy() -> String {
        POLICY.to_string()
    }

    fn session_payload(command: &str, session: &str) -> String {
        json!({
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "session_id": session,
            "cwd": REPO_CWD
        })
        .to_string()
    }

    fn run(store: &Arc<TempStore>, command: &str, session: &str) -> Value {
        respond_with(
            &session_payload(command, session),
            Ok(confirm_policy()),
            Arc::new(StubLookups(Lookup::Empty)),
            store.clone(),
            None,
            no_answers(),
        )
        .response
    }

    fn records(store: &TempStore) -> Vec<crate::telemetry::CmdIncidentRecord> {
        store.log().records().expect("read log")
    }

    fn agent_confirms(store: &TempStore, command: &str, session: &str, at: DateTime<Utc>) {
        let db = store.db();
        let request = crate::cmd::confirm::ConfirmRequest {
            origin: Origin {
                command: command.to_string(),
                agent: "agent-of-legion".to_string(),
                repo: "legion".to_string(),
                session_id: session.to_string(),
                cwd: REPO_CWD.to_string(),
            },
            reason: Some("the agent's reason".to_string()),
        };
        let policy = parse_policy(&confirm_policy()).expect("policy");
        crate::cmd::confirm::confirm(&request, &policy, &db, &store.log(), at, &|_| Ok(()))
            .expect("confirmed");
    }

    #[test]
    fn a_no_go_hit_is_refused_recorded_in_full_and_counted_per_session_and_entry() {
        let store = temp_store();
        let response = run(&store, "rm -rf /", "s1");
        assert_denied(&response);
        assert!(!reason(&response).contains(legion_cmd::NO_GO_INSTEAD));
        assert!(reason(&response).contains(RECORDED_NOTE));

        let first = &records(&store)[0];
        assert_eq!(first.kind, crate::telemetry::CmdIncidentKind::NoGo);
        assert_eq!(first.command, "rm -rf /");
        assert_eq!(first.agent, "agent-of-legion");
        assert_eq!(first.repo, "legion");
        assert_eq!(first.session_id, "s1");
        assert_eq!(first.cwd, REPO_CWD);
        assert_eq!(first.entry.as_deref(), Some("rm-recursive-force-root"));
        assert_eq!(first.hit_count, Some(1));

        run(&store, "rm -fr /", "s1");
        run(&store, "rm -rf /", "s2");
        let counts: Vec<Option<u64>> = records(&store).iter().map(|r| r.hit_count).collect();
        assert_eq!(counts, vec![Some(1), Some(2), Some(1)]);
    }

    #[test]
    fn the_first_no_go_hit_in_a_session_notifies_the_operator_once_per_entry() {
        let store = temp_store();
        run(&store, "rm -rf /", "s1");
        run(&store, "rm -fr /", "s1");
        run(&store, ":(){ :|:& };:", "s1");
        run(&store, "rm -rf /", "s2");
        assert_eq!(
            store.notices.lock().expect("notices lock").clone(),
            vec![
                "rm-recursive-force-root".to_string(),
                "fork-bomb".to_string(),
                "rm-recursive-force-root".to_string(),
            ]
        );
    }

    #[test]
    fn a_notice_that_fails_to_send_is_recorded_and_the_command_stays_refused() {
        let store = Arc::new(TempStore {
            dir: tempfile::tempdir().expect("tempdir"),
            notices: Mutex::new(Vec::new()),
            notice_failure: Some("inbox unreachable".to_string()),
        });
        let response = run(&store, "rm -rf /", "s1");
        assert_denied(&response);
        assert!(!reason(&response).contains(legion_cmd::NO_GO_INSTEAD));
        assert!(
            records(&store)[0]
                .notice_error
                .as_deref()
                .is_some_and(|e| e.contains("inbox unreachable"))
        );
    }

    #[test]
    fn an_unreadable_policy_still_refuses_records_and_notifies_a_no_go_hit() {
        // FR-CMD-025: the built-ins apply when the policy file is absent.
        let store = temp_store();
        let unread = || {
            Err(AdapterError::PolicyRead(
                "/plugin/legion-cmd/policy.json: No such file".to_string(),
            ))
        };
        let response = respond_with(
            &session_payload("rm -rf /", "s1"),
            unread(),
            Arc::new(StubLookups(Lookup::Empty)),
            store.clone(),
            None,
            no_answers(),
        )
        .response;
        assert_denied(&response);
        assert!(!reason(&response).contains(legion_cmd::NO_GO_INSTEAD));
        assert!(reason(&response).contains(RECORDED_NOTE));
        let rows = records(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, crate::telemetry::CmdIncidentKind::NoGo);
        assert_eq!(
            store.notices.lock().expect("notices lock").clone(),
            vec!["rm-recursive-force-root".to_string()]
        );

        // Every other command is still denied with the read error, as before.
        let response = respond_with(
            &session_payload("echo hi", "s1"),
            unread(),
            Arc::new(StubLookups(Lookup::Empty)),
            store.clone(),
            None,
            no_answers(),
        )
        .response;
        assert_denied(&response);
        assert!(reason(&response).contains("policy: /plugin/legion-cmd/policy.json"));
        assert_eq!(records(&store).len(), 1);
    }

    #[test]
    fn an_unreadable_policy_refuses_records_and_notifies_wrapped_no_go_hits() {
        // The wrapped forms, not just the bare one: with no policy file the
        // built-in no-go check resolves the wrapper itself (FR-CMD-025), and
        // each hit is refused, recorded once, and notified once.
        for (command, entry) in [
            ("sudo rm -rf /", "rm-recursive-force-root"),
            ("env FOO=bar mkfs.ext4 /dev/sda1", "mkfs-device"),
            ("sh -c 'rm -rf /'", "rm-recursive-force-root"),
            ("timeout 5 mkfs.ext4 /dev/sda1", "mkfs-device"),
        ] {
            let store = temp_store();
            let response = respond_with(
                &session_payload(command, "s1"),
                Err(AdapterError::PolicyRead(
                    "/plugin/legion-cmd/policy.json: No such file".to_string(),
                )),
                Arc::new(StubLookups(Lookup::Empty)),
                store.clone(),
                None,
                no_answers(),
            )
            .response;
            assert_denied(&response);
            assert!(
                !reason(&response).contains(legion_cmd::NO_GO_INSTEAD),
                "{command}"
            );
            assert!(reason(&response).contains(RECORDED_NOTE), "{command}");
            let rows = records(&store);
            assert_eq!(rows.len(), 1, "{command}");
            assert_eq!(rows[0].kind, crate::telemetry::CmdIncidentKind::NoGo);
            assert_eq!(rows[0].command, command);
            assert_eq!(rows[0].entry.as_deref(), Some(entry), "{command}");
            assert_eq!(
                store.notices.lock().expect("notices lock").clone(),
                vec![entry.to_string()],
                "{command}"
            );
        }
    }

    #[test]
    fn an_ask_is_recorded_and_the_refusal_says_so() {
        let store = temp_store();
        let response = run(&store, "curl example.com", "s1");
        assert_denied(&response);
        assert!(reason(&response).contains(RECORDED_NOTE));
        let rows = records(&store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, crate::telemetry::CmdIncidentKind::Ask);
        assert_eq!(rows[0].entry.as_deref(), Some("curl-network"));
    }

    #[test]
    fn a_confirmed_command_goes_to_the_operator_once_and_a_second_run_is_asked_again() {
        let store = temp_store();
        agent_confirms(&store, "curl example.com", "s1", Utc::now());
        let response = run(&store, "curl   'example.com'", "s1");
        assert_eq!(output(&response)["permissionDecision"], "ask");
        let again = run(&store, "curl example.com", "s1");
        assert_denied(&again);
        assert!(reason(&again).contains("needs the operator's approval"));
    }

    #[test]
    fn a_confirmation_does_not_cross_sessions_commands_or_its_ten_minutes() {
        let store = temp_store();
        agent_confirms(&store, "curl example.com", "s1", Utc::now());
        assert_denied(&run(&store, "curl example.com", "s2"));
        assert_denied(&run(&store, "curl example.org", "s1"));

        let stale = temp_store();
        agent_confirms(
            &stale,
            "curl example.com",
            "s1",
            Utc::now() - chrono::Duration::minutes(11),
        );
        assert_denied(&run(&stale, "curl example.com", "s1"));
    }

    #[test]
    fn a_confirmed_power_switch_prompts_with_the_agents_reason_and_the_inserted_command() {
        let store = temp_store();
        agent_confirms(&store, "git push --force", "s1", Utc::now());
        let response = run(&store, "git push --force", "s1");
        let out = output(&response);
        assert_eq!(out["permissionDecision"], "ask");
        assert_eq!(out["permissionDecisionReason"], "the agent's reason");
        assert_eq!(out["updatedInput"]["command"], "legion git push --force");
        // The confirmation was used by that prompt: an operator refusal leaves
        // nothing to reuse, and the next attempt is asked again.
        let again = run(&store, "git push --force", "s1");
        assert_denied(&again);
        assert!(reason(&again).contains("a forced push"));
    }

    #[test]
    fn the_operator_prompt_stage_of_a_confirmed_ask_writes_an_ask_record() {
        // FR-CMD-027: an ask is recorded at both stages. The operator stage
        // carries the agent's reason, which is what tells it from the first
        // ask, and it is never later read as a drop: its confirmation came
        // before it.
        let store = temp_store();
        let now = Utc::now();
        agent_confirms(&store, "curl example.com", "s1", now);
        let response = run(&store, "curl example.com", "s1");
        assert_eq!(output(&response)["permissionDecision"], "ask");
        let asks: Vec<crate::telemetry::CmdIncidentRecord> = records(&store)
            .into_iter()
            .filter(|r| r.kind == crate::telemetry::CmdIncidentKind::Ask)
            .collect();
        assert_eq!(asks.len(), 1, "{asks:?}");
        assert_eq!(asks[0].entry.as_deref(), Some("curl-network"));
        assert_eq!(asks[0].command, "curl example.com");
        assert_eq!(asks[0].reason.as_deref(), Some("the agent's reason"));
        let db = store.db();
        let drops = store
            .log()
            .record_pending_drops(
                &|id: &str| was_used(&db, id),
                now + chrono::Duration::minutes(11),
            )
            .expect("drops");
        assert_eq!(drops, 0, "the operator-stage ask is not a drop");
    }

    #[test]
    fn an_ask_whose_record_cannot_be_written_is_refused_naming_the_failure() {
        let store = temp_store();
        std::fs::write(store.log_path(), "").expect("create log");
        let mut perms = std::fs::metadata(store.log_path())
            .expect("log exists")
            .permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(store.log_path(), perms).expect("chmod");
        let response = run(&store, "curl example.com", "s1");
        assert_denied(&response);
        assert!(reason(&response).contains("incident record:"));
        assert!(!reason(&response).contains(RECORDED_NOTE));
    }

    #[test]
    fn a_confirmed_command_whose_confirmation_cannot_be_used_up_is_refused() {
        // A confirmed run writes one thing before it proceeds: the use of its
        // confirmation in the store. When that write fails the command is
        // refused, even though it was confirmed.
        let store = temp_store();
        agent_confirms(&store, "curl example.com", "s1", Utc::now());
        {
            let db = store.db();
            db.conn
                .execute_batch(
                    "CREATE TRIGGER refuse_use BEFORE UPDATE ON cmd_confirmations \
                     BEGIN SELECT RAISE(ABORT, 'store is read-only'); END;",
                )
                .expect("trigger");
        }
        let response = run(&store, "curl example.com", "s1");
        assert_denied(&response);
        assert!(reason(&response).contains("incident record:"));
        assert!(reason(&response).contains("store is read-only"));
    }

    #[test]
    fn a_confirmation_store_that_cannot_be_read_denies_naming_it() {
        struct BrokenStore(TempStore);
        impl CmdStore for BrokenStore {
            fn open(&self) -> Result<StoreHandle, AdapterError> {
                Err(AdapterError::Confirmations("db locked".to_string()))
            }
            fn log(&self) -> IncidentLog {
                self.0.log()
            }
            fn agent_for(&self, repo: &str) -> String {
                repo.to_string()
            }
            fn notify(&self, _record: &CmdIncidentRecord) -> error::Result<()> {
                Ok(())
            }
        }
        let response = respond_with(
            &session_payload("curl example.com", "s1"),
            Ok(confirm_policy()),
            Arc::new(StubLookups(Lookup::Empty)),
            Arc::new(BrokenStore(TempStore {
                dir: tempfile::tempdir().expect("tempdir"),
                notices: Mutex::new(Vec::new()),
                notice_failure: None,
            })),
            None,
            no_answers(),
        )
        .response;
        assert_denied(&response);
        assert!(reason(&response).contains("confirmations: "));
        assert!(reason(&response).contains("db locked"));
    }

    #[test]
    fn the_adapter_writes_pending_drops_before_its_own_work() {
        let store = temp_store();
        let origin = Origin {
            command: "curl example.com".to_string(),
            agent: "a".to_string(),
            repo: "legion".to_string(),
            session_id: "s1".to_string(),
            cwd: REPO_CWD.to_string(),
        };
        store
            .log()
            .record_ask(
                &origin,
                Some("curl-network"),
                None,
                None,
                Utc::now() - chrono::Duration::minutes(15),
            )
            .expect("record");
        run(&store, "echo hi", "s1");
        let drops = records(&store)
            .into_iter()
            .filter(|r| r.kind == crate::telemetry::CmdIncidentKind::Drop)
            .count();
        assert_eq!(drops, 1);
    }

    // -- lookups (FR-CMD-016) --------------------------------------------------

    #[test]
    fn a_failing_required_lookup_denies_naming_the_lookup() {
        let response = respond_with(
            &fetch_payload("https://example.com", Some(REPO_CWD)),
            Ok(POLICY.to_string()),
            Arc::new(FailingLookups),
            temp_store(),
            None,
            no_answers(),
        )
        .response;
        assert_denied(&response);
        assert!(reason(&response).contains("lookup:"));
        assert!(reason(&response).contains("db unavailable"));
    }

    #[test]
    fn a_fetched_lookup_reaches_route_and_the_rule_decides() {
        // With both lookups fetched, the rule's own deny fires -- proof the
        // results are wired into `Context`, not fetched and dropped.
        let response = respond_stub(
            &fetch_payload("https://example.com", Some(REPO_CWD)),
            POLICY,
        );
        assert_denied(&response);
        assert_eq!(
            reason(&response),
            "fetch through recall -- instead: legion recall"
        );
    }

    #[test]
    fn a_required_recall_with_no_derivable_repo_denies() {
        let response = respond_stub(&fetch_payload("https://example.com", None), POLICY);
        assert_denied(&response);
        assert!(reason(&response).contains("lookup:"));
        assert!(reason(&response).contains("none could be derived"));
    }

    #[test]
    fn the_lookups_are_scoped_to_the_validated_repo_and_queried_with_the_input_text() {
        let lookups = RecordingLookups::new(Duration::ZERO);
        let response = respond_with(
            &fetch_payload("https://example.com", Some(REPO_CWD)),
            Ok(POLICY.to_string()),
            lookups.clone(),
            temp_store(),
            None,
            no_answers(),
        )
        .response;
        assert_denied(&response);
        assert_eq!(
            lookups.calls(),
            vec![
                (
                    "recall:legion".to_string(),
                    "https://example.com".to_string()
                ),
                ("consult".to_string(), "https://example.com".to_string()),
            ]
        );
    }

    #[test]
    fn legion_repo_scopes_the_recall_over_cwd() {
        let lookups = RecordingLookups::new(Duration::ZERO);
        let _response = respond_with(
            &fetch_payload("https://example.com", Some(REPO_CWD)),
            Ok(POLICY.to_string()),
            lookups.clone(),
            temp_store(),
            Some("other-repo".to_string()),
            no_answers(),
        )
        .response;
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
        let response = deny_for_error(&err);
        assert_denied(&response);
        assert!(reason(&response).contains("deadline exceeded: no decision within 10 ms"));
    }

    #[test]
    fn a_panic_in_the_worker_denies_naming_the_panic() {
        let outcome: Result<Routed, AdapterError> =
            decide(Duration::from_secs(5), || panic!("simulated route panic"));
        let err = outcome.expect_err("a panicked worker must not succeed");
        assert!(matches!(err, AdapterError::Panic(_)));
        let response = deny_for_error(&err);
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
        let response = respond_with(
            &fetch_payload("https://example.com", Some("/tmp/legion; touch pwned")),
            Ok(POLICY.to_string()),
            lookups.clone(),
            temp_store(),
            None,
            no_answers(),
        )
        .response;
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
    fn the_command_is_quoted_as_one_shell_word_in_the_confirm_hint() {
        assert_eq!(shell_single_quote("it's; rm -rf /"), r"'it'\''s; rm -rf /'");
        let response = respond_stub(&payload("curl 'a'; touch pwned"), POLICY);
        assert_denied(&response);
        assert!(
            reason(&response)
                .ends_with(r"legion cmd confirm --reason <why> -- 'curl '\''a'\''; touch pwned'"),
            "{}",
            reason(&response)
        );
    }

    #[test]
    fn an_applied_rewrite_is_returned_once_with_issued_and_constructed() {
        // #1272: the prediction is emitted from this, so it must carry the
        // call's ids, the command as issued and the command constructed.
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "git status"},
            "session_id": "s1",
            "tool_use_id": "toolu_1",
            "transcript_path": "/tmp/s1.jsonl",
            "cwd": REPO_CWD
        })
        .to_string();
        let applied = respond_applied(&input, POLICY);
        assert_eq!(
            applied.rewrite,
            Some(AppliedRewrite {
                tool_use_id: Some("toolu_1".to_string()),
                session_id: Some("s1".to_string()),
                tool_name: "Bash".to_string(),
                issued: Some("git status".to_string()),
                constructed: "legion git status".to_string(),
                background: false,
            })
        );
        assert!(
            output(&applied.response)
                .get("permissionDecision")
                .is_none()
        );
    }

    #[test]
    fn a_backgrounded_rewrite_is_marked_background() {
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "git status", "run_in_background": true},
            "tool_use_id": "toolu_bg",
            "cwd": REPO_CWD
        })
        .to_string();
        let rewrite = respond_applied(&input, POLICY).rewrite.expect("a rewrite");
        assert!(rewrite.background);
    }

    #[test]
    fn no_decision_but_an_applied_rewrite_yields_a_prediction() {
        // Untouched, never-run, ask, and a parse error.
        for command in [
            "ls -la",
            "echo git",
            "rm -rf build",
            "curl example.com",
            "git 'unterminated",
        ] {
            let applied = respond_applied(&payload(command), POLICY);
            assert!(applied.rewrite.is_none(), "{command} yielded a rewrite");
        }
    }

    #[test]
    fn a_rewrite_whose_replacement_fails_yields_no_prediction() {
        let routed = Routed {
            decision: Decision::Rewrite {
                target: ManagedTarget::new("legion"),
                reason: "a hand-built rewrite".to_string(),
            },
            facts: Facts {
                rewritten: Some("  ".to_string()),
                ..Facts::default()
            },
            deciding: Deciding::Rule {
                id: legion_cmd::PROXY_ID.to_string(),
                needs_operator: false,
            },
            confirmed: false,
        };
        let applied = apply(&routed, &parsed_payload("git status"));
        assert_denied(&applied.response);
        assert!(applied.rewrite.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_stuck_transcript_read_never_holds_the_decision_past_the_deadline() {
        // The transcript is a FIFO nobody writes: opening it blocks forever,
        // the slowest transcript read there is. The pass runs beside the
        // decision, which never waits on it; after the response the pass gets
        // only the rest of the one deadline, then is abandoned with the
        // pending prediction still emitted.
        let dir = tempfile::tempdir().expect("tempdir");
        let fifo: PathBuf = dir.path().join("session.jsonl");
        // Shells out rather than calling libc::mkfifo: the binary is no-unsafe.
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "mkfifo failed");

        let db = crate::db::testutil::test_db();
        prediction::emit_rewrite_prediction(
            &db,
            &AppliedRewrite {
                tool_use_id: Some("toolu_pending".to_string()),
                session_id: Some("s1".to_string()),
                tool_name: "Bash".to_string(),
                issued: Some("git status".to_string()),
                constructed: "legion git status".to_string(),
                background: false,
            },
        )
        .expect("emits");

        let deadline = Duration::from_millis(400);
        let transcript: PathBuf = fifo.clone();
        // The decision opens the confirmation store (#1237); migrate it before
        // the clock starts so the timing measures the decision, not setup.
        let store = temp_store();
        drop(store.db());
        let started = Instant::now();
        let (applied, pending) = respond_beside_witness(
            deadline,
            || {
                respond_with(
                    &payload("git status"),
                    Ok(POLICY.to_string()),
                    Arc::new(StubLookups(Lookup::Empty)),
                    store,
                    None,
                    no_answers(),
                )
            },
            move || {
                prediction::witness_pending(&db, "s1", &transcript)?;
                Ok(())
            },
        );
        let decided: Duration = started.elapsed();
        pending.expect("the pass started").wait();
        let total: Duration = started.elapsed();

        // The decision never waited on the stuck pass.
        assert!(
            decided < deadline,
            "the decision waited {decided:?} on a stuck witness pass"
        );
        assert!(output(&applied.response).get("updatedInput").is_some());
        assert!(
            applied.rewrite.is_some(),
            "the decision itself is unchanged"
        );
        // The whole call fits one deadline, not two. At least the deadline:
        // the pass really was stuck, not ended early by an error.
        assert!(
            total >= deadline,
            "the witness pass ended early ({total:?}); the FIFO did not block"
        );
        assert!(
            total < deadline + Duration::from_millis(300),
            "the call took {total:?}, past one deadline"
        );
        // The abandoned worker still holds the FIFO path open for reading.
        std::mem::forget(dir);
    }

    #[test]
    fn a_witness_budget_past_the_clock_skips_the_pass_instead_of_panicking() {
        // #1288: `Instant::now() + Duration::MAX` overflows. The pass is
        // skipped, and the closure never runs.
        let ran = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&ran);
        let pending = PendingWitness::start(Duration::MAX, move || {
            *flag.lock().expect("flag lock") = true;
            Ok(())
        });
        assert!(pending.is_none(), "an out-of-range budget started the pass");
        assert!(!*ran.lock().expect("flag lock"), "the skipped pass ran");
    }

    #[test]
    fn an_extreme_witness_deadline_yields_the_routing_decision() {
        // #1288: the largest deadline reaches the decision, not a panic deny.
        let deadline = Duration::from_millis(u64::MAX);
        let response: Value = guarded(&payload("git status"), || {
            respond_beside_witness(
                deadline,
                || {
                    respond_with(
                        &payload("git status"),
                        Ok(POLICY.to_string()),
                        Arc::new(StubLookups(Lookup::Empty)),
                        temp_store(),
                        None,
                        no_answers(),
                    )
                },
                || Ok(()),
            )
            .0
            .response
        });
        assert_eq!(
            output(&response)["updatedInput"]["command"],
            "legion git status",
            "got {response}"
        );
    }

    #[test]
    fn a_store_open_past_the_deadline_gives_up_at_the_deadline_naming_it() {
        // #1288: an open that never finishes costs the call one deadline,
        // then the call goes on without a store, knowing why.
        let (_hold, never) = mpsc::channel::<()>();
        let deadline = Duration::from_millis(100);
        let started = Instant::now();
        let store: Result<Database, String> = open_store_within(deadline, move || {
            let _ = never.recv();
            Err(error::LegionError::Search(
                "released at the test's end".to_string(),
            ))
        });
        let waited: Duration = started.elapsed();
        let cause: String = store.err().expect("an unfinished open yielded a store");
        assert!(cause.contains("could not be opened within"), "{cause}");
        assert!(waited >= deadline, "gave up early, after {waited:?}");
        assert!(
            waited < deadline + Duration::from_millis(300),
            "the open held the call for {waited:?}"
        );
    }

    #[test]
    fn a_store_open_that_fails_or_panics_names_the_cause() {
        let failed = open_store_within(Duration::from_secs(5), || {
            Err(error::LegionError::Search("unopenable".to_string()))
        });
        assert!(failed.err().expect("a failed open").contains("unopenable"));
        let panicked = open_store_within(Duration::from_secs(5), || panic!("open panicked"));
        assert!(
            panicked
                .err()
                .expect("a panicked open")
                .contains("panic: open panicked")
        );
    }

    #[test]
    fn a_store_open_in_time_yields_the_store() {
        let store = open_store_within(
            Duration::from_secs(5),
            || Ok(crate::db::testutil::test_db()),
        );
        assert!(store.is_ok());
    }

    #[test]
    fn best_effort_swallows_errors_and_panics() {
        best_effort("error", || Err("boom".into()));
        best_effort("panic", || panic!("boom"));
        best_effort("ok", || Ok(()));
    }

    #[test]
    fn the_witness_pass_has_nothing_to_do_without_a_session_or_transcript() {
        // Neither case opens the store: there is nothing to witness.
        assert!(witness_session("not json").is_ok());
        assert!(witness_session(&payload("ls")).is_ok());
    }

    // -- the shipped policy, end to end (#1278) --------------------------------

    /// The shipped artifact, compiled in so these tests run the adapter over
    /// exactly what ships, with legion's own clap tree judging each rewrite.
    const SHIPPED_POLICY: &str = include_str!("../../plugin/legion-cmd/policy.json");

    /// The adapter's response to `command` under the shipped policy, for the
    /// repo `legion`.
    fn shipped(command: &str) -> Value {
        respond_with(
            &payload(command),
            Ok(SHIPPED_POLICY.to_string()),
            Arc::new(StubLookups(Lookup::Empty)),
            temp_store(),
            Some("legion".to_string()),
            no_answers(),
        )
        .response
    }

    /// A worktree-isolated agent's cwd, as the harness lays it out.
    const ISOLATED_CWD: &str = "/repo/legion/.claude/worktrees/agent-a6875e266df3b6908";

    /// A Bash payload from a subagent (`agent_id` set) whose cwd is `cwd`.
    fn agent_payload(command: &str, cwd: &str) -> String {
        json!({
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "session_id": "s1",
            "tool_use_id": "t1",
            "cwd": cwd,
            "agent_id": "a6875e266df3b6908",
            "agent_type": "rust"
        })
        .to_string()
    }

    /// The adapter's response to `input` under the shipped policy.
    fn shipped_input(input: &str) -> Value {
        shipped_input_with(SHIPPED_POLICY, input)
    }

    #[test]
    fn a_worktree_isolated_agent_gets_git_and_gh_as_typed() {
        // #1358: Claude Code's worktree guard refuses `legion git ...` in an
        // isolated agent, so no form with `legion` inserted is returned.
        for cwd in [
            ISOLATED_CWD,
            &format!("{ISOLATED_CWD}/crates/legion-cmd"),
            // A Windows cwd, as the harness reports it there.
            r"C:\repo\legion\.claude\worktrees\agent-a6875e266df3b6908",
        ] {
            for command in ["git status --short", "gh pr view 1"] {
                let response = shipped_input(&agent_payload(command, cwd));
                assert_eq!(
                    response,
                    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse"}}),
                    "`{command}` in {cwd}"
                );
            }
        }
        // grep and rg still reach their legion proxies there.
        let response = shipped_input(&agent_payload("git log | grep x", ISOLATED_CWD));
        assert_eq!(
            output(&response)["updatedInput"]["command"],
            "git log | legion grep x"
        );
    }

    #[test]
    fn a_worktree_isolated_agent_is_denied_and_asked_as_elsewhere() {
        let denied = shipped_input(&agent_payload("rm -rf /", ISOLATED_CWD));
        assert_denied(&denied);

        // An unconfirmed power switch asks the agent to drop or confirm it,
        // exactly as it does for any other caller.
        let asked = shipped_input(&agent_payload("git push --force", ISOLATED_CWD));
        assert_denied(&asked);
        assert!(
            reason(&asked).starts_with(
                "this command needs the operator's approval: drop it or confirm it -- \
                 a forced push discards commits"
            ),
            "the push-force entry's ask: {asked}"
        );
        assert_eq!(asked, shipped("git push --force"));

        // Once the agent confirms, the operator is asked about the command as
        // typed: nothing is inserted into it.
        let store = temp_store();
        agent_confirms(&store, "git push --force", "s1", Utc::now());
        let confirmed = respond_with(
            &agent_payload("git push --force", ISOLATED_CWD),
            Ok(SHIPPED_POLICY.to_string()),
            Arc::new(StubLookups(Lookup::Empty)),
            store,
            Some("legion".to_string()),
            no_answers(),
        )
        .response;
        let out = output(&confirmed);
        assert_eq!(out["permissionDecision"], "ask", "{confirmed}");
        assert!(out.get("updatedInput").is_none(), "{confirmed}");
    }

    #[test]
    fn outside_a_worktree_isolated_agent_git_still_gets_legion() {
        let main_session = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "git status"},
            "session_id": "s1",
            "tool_use_id": "t1",
            "cwd": ISOLATED_CWD
        })
        .to_string();
        for input in [
            // The main thread, even in an agent worktree: no `agent_id`.
            main_session,
            // A subagent in the main checkout.
            agent_payload("git status", REPO_CWD),
            // A subagent in a worktree the harness did not make for an agent.
            agent_payload("git status", "/repo/legion/.claude/worktrees/spec-writer"),
            // The operator's own payloads, no agent at all.
            payload("git status"),
        ] {
            let response = shipped_input(&input);
            assert_eq!(
                rewritten_command(&response),
                Some("legion git status"),
                "{input}"
            );
        }
    }

    #[test]
    fn a_policy_without_the_passthrough_list_still_inserts_in_an_isolated_agent() {
        // An absent list is an empty one: every proxied name is inserted, as
        // in 0.43.1.
        let mut root: Value = serde_json::from_str(SHIPPED_POLICY).expect("valid JSON");
        root.as_object_mut()
            .expect("an object")
            .remove("worktree_agent_passthrough");
        let response = shipped_input_with(
            &root.to_string(),
            &agent_payload("git status --short", ISOLATED_CWD),
        );
        assert_eq!(
            rewritten_command(&response),
            Some("legion git status --short")
        );
    }

    fn shipped_input_with(policy: &str, input: &str) -> Value {
        respond_with(
            input,
            Ok(policy.to_string()),
            Arc::new(StubLookups(Lookup::Empty)),
            temp_store(),
            Some("legion".to_string()),
            no_answers(),
        )
        .response
    }

    #[test]
    fn an_empty_agent_id_is_not_a_subagent() {
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "git status"},
            "cwd": ISOLATED_CWD,
            "agent_id": ""
        })
        .to_string();
        assert_eq!(
            rewritten_command(&shipped_input(&input)),
            Some("legion git status")
        );
    }

    #[test]
    fn an_agent_worktree_is_a_named_folder_under_claude_worktrees() {
        for cwd in [
            ISOLATED_CWD,
            "/repo/legion/.claude/worktrees/agent-a1b2c3d/src",
            r"C:\repo\legion\.claude\worktrees\agent-a1b2c3d",
            r"C:\repo\legion\.claude\worktrees\agent-a1b2c3d\src",
        ] {
            assert!(in_agent_worktree(cwd), "{cwd}");
        }
        for cwd in [
            "/repo/legion",
            "/repo/legion/.claude/worktrees/spec-writer",
            "/repo/legion/.claude/worktrees/agent-",
            "/repo/legion/worktrees/agent-a1b2c3d",
            "/repo/legion/.claude/agent-a1b2c3d",
            r"C:\repo\legion",
            r"C:\repo\legion\.claude\worktrees\spec-writer",
        ] {
            assert!(!in_agent_worktree(cwd), "{cwd}");
        }
    }

    /// The command an insertion response runs in place of the agent's.
    fn rewritten_command(response: &Value) -> Option<&str> {
        let out = output(response);
        out.get("permissionDecision")
            .is_none()
            .then(|| out["updatedInput"]["command"].as_str())
            .flatten()
    }

    #[test]
    fn the_shipped_policy_inserts_legion_and_keeps_every_other_byte() {
        for (command, replacement) in [
            ("git commit -m \"a b\"", "legion git commit -m \"a b\""),
            ("git push", "legion git push"),
            ("git push -h", "legion git push -h"),
            (
                "gh pr view 42 --json title",
                "legion gh pr view 42 --json title",
            ),
            ("git -C /tmp/x push", "legion git -C /tmp/x push"),
            ("grep -rn foo src | head", "legion grep -rn foo src | head"),
            ("cd x && rg foo", "cd x && legion rg foo"),
        ] {
            let response = shipped(command);
            assert_eq!(
                rewritten_command(&response),
                Some(replacement),
                "`{command}`: {response}"
            );
        }
    }

    #[test]
    fn no_shipped_response_names_a_command_to_run_instead() {
        for command in [
            "rm -rf /",
            "rm -rf build",
            "sqlite3 legion.db",
            "git push --force origin main",
            "grep 'unterminated",
        ] {
            let response = shipped(command);
            assert_denied(&response);
            assert!(
                !reason(&response).contains("instead"),
                "`{command}`: {}",
                reason(&response)
            );
        }
    }

    #[test]
    fn the_command_tree_is_well_formed_with_the_git_short_spellings() {
        // `-m` and `-F` on `legion commit` (#1278) must not collide with any
        // other short on the path, the global `-v` included.
        use clap::CommandFactory;
        crate::cli::Cli::command().debug_assert();
    }

    // -- harness Grep, Glob and Read (#1338) --------------------------------

    fn tool_payload(tool: &str, input: Value) -> String {
        json!({
            "tool_name": tool,
            "tool_input": input,
            "session_id": "s1",
            "tool_use_id": "t1",
            "cwd": REPO_CWD
        })
        .to_string()
    }

    fn respond_answering(input: &str, policy: &str, answers: Arc<dyn Answerer>) -> Value {
        respond_with(
            input,
            Ok(policy.to_string()),
            Arc::new(StubLookups(Lookup::Empty)),
            temp_store(),
            None,
            answers,
        )
        .response
    }

    fn grep_answer_stub() -> Answer {
        Answer {
            text: "legion answered this search in full: /repo/legion/src/a.rs:1:fn main() {}"
                .to_string(),
            updated_input: Some(json!({"pattern": "fn main", "head_limit": 1})),
        }
    }

    #[test]
    fn an_answered_grep_carries_the_answer_and_the_limited_input_and_no_decision() {
        let answers = stub_answers(Some(grep_answer_stub()));
        let response = respond_answering(
            &tool_payload("Grep", json!({"pattern": "fn main"})),
            POLICY,
            answers.clone(),
        );
        let out = output(&response);
        assert!(out.get("permissionDecision").is_none());
        assert!(out.get("permissionDecisionReason").is_none());
        assert_eq!(
            out["additionalContext"],
            "legion answered this search in full: /repo/legion/src/a.rs:1:fn main() {}"
        );
        assert_eq!(out["updatedInput"]["head_limit"], 1);
        assert_eq!(out["updatedInput"]["pattern"], "fn main");
        let calls = answers.calls.lock().expect("calls lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "Grep");
        assert_eq!(calls[0].cwd.as_deref(), Some(REPO_CWD));
    }

    #[test]
    fn an_answered_glob_carries_the_answer_and_leaves_the_input_alone() {
        let answers = stub_answers(Some(Answer {
            text: "legion answered this search in full from its file inventory".to_string(),
            updated_input: None,
        }));
        let response = respond_answering(
            &tool_payload("Glob", json!({"pattern": "src/**/*.rs"})),
            POLICY,
            answers,
        );
        let out = output(&response);
        assert!(out.get("permissionDecision").is_none());
        assert!(out.get("updatedInput").is_none());
        assert!(
            out["additionalContext"]
                .as_str()
                .expect("context")
                .starts_with("legion answered this search")
        );
    }

    #[test]
    fn a_declined_search_runs_the_tool_as_sent_with_nothing_added() {
        for tool in ["Grep", "Glob"] {
            let response = respond_answering(
                &tool_payload(tool, json!({"pattern": "x", "glob": "*.rs"})),
                POLICY,
                no_answers(),
            );
            assert_eq!(
                response,
                json!({"hookSpecificOutput": {"hookEventName": "PreToolUse"}}),
                "{tool}"
            );
        }
    }

    #[test]
    fn only_grep_and_glob_are_offered_to_the_answerer() {
        let answers = stub_answers(Some(grep_answer_stub()));
        for payload in [
            payload("grep -rn x src"),
            tool_payload("Read", json!({"file_path": "/repo/legion/src/a.rs"})),
            tool_payload("WebSearch", json!({"query": "x"})),
        ] {
            let response = respond_answering(&payload, POLICY, answers.clone());
            assert_ne!(
                output(&response)["additionalContext"],
                grep_answer_stub().text,
                "{payload}"
            );
        }
        assert!(answers.calls.lock().expect("calls lock").is_empty());
    }

    #[test]
    fn an_unbounded_read_and_a_read_over_500_lines_of_a_source_file_both_run() {
        let policy: String =
            std::fs::read_to_string("plugin/legion-cmd/policy.json").expect("shipped policy");
        for input in [
            json!({"file_path": "/repo/legion/src/cmd/hook.rs"}),
            json!({"file_path": "/repo/legion/src/cmd/hook.rs", "limit": 600}),
        ] {
            let response =
                respond_answering(&tool_payload("Read", input.clone()), &policy, no_answers());
            let out = output(&response);
            assert!(out.get("permissionDecision").is_none(), "{input}");
            assert!(out.get("updatedInput").is_none(), "{input}");
        }
    }

    #[test]
    fn a_legion_failure_never_refuses_a_grep_glob_or_read() {
        struct Unopenable;
        impl CmdStore for Unopenable {
            fn open(&self) -> Result<StoreHandle, AdapterError> {
                Err(AdapterError::Confirmations("store locked".to_string()))
            }
            fn log(&self) -> IncidentLog {
                IncidentLog::at(std::env::temp_dir().join("legion-unused-incidents.jsonl"))
            }
            fn agent_for(&self, repo: &str) -> String {
                repo.to_string()
            }
            fn notify(&self, _record: &CmdIncidentRecord) -> error::Result<()> {
                Ok(())
            }
        }
        for tool in ["Grep", "Glob", "Read"] {
            let input = json!({"pattern": "x", "file_path": "/repo/legion/src/a.rs"});
            let from_policy = respond_with(
                &tool_payload(tool, input.clone()),
                Err(AdapterError::PolicyRead("no such file".to_string())),
                Arc::new(StubLookups(Lookup::Empty)),
                temp_store(),
                None,
                no_answers(),
            )
            .response;
            let from_store = respond_with(
                &tool_payload(tool, input.clone()),
                Ok(POLICY.to_string()),
                Arc::new(StubLookups(Lookup::Empty)),
                Arc::new(Unopenable),
                None,
                no_answers(),
            )
            .response;
            let from_panic = guarded(&tool_payload(tool, input), || panic!("adapter bug"));
            // A payload that names its tool but is otherwise malformed.
            let malformed: String = json!({"tool_name": tool, "cwd": 7}).to_string();
            let from_payload = respond_answering(&malformed, POLICY, no_answers());
            for response in [from_policy, from_store, from_panic, from_payload] {
                assert_eq!(
                    response,
                    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse"}}),
                    "{tool}"
                );
            }
        }
        // Every other tool still fails closed.
        let bash = guarded(&payload("ls"), || panic!("adapter bug"));
        assert_denied(&bash);
    }
}
