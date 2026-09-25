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
//! The operator mode (`legion cmd-check -- <command>`, #1230,
//! `crate::cli::cmd_check`) runs the same core: [`read_policy_text`] and
//! [`route_call`] (policy loading, the settings, the repo, the lookup
//! pre-pass, route, all under the deadline), [`replacement_for`], and the
//! error deny from [`error_deny_text`]. Only the rendering differs: this
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
//!   an allow never grants a permission the harness would not (FR-CMD-002);
//!   route's note, if any, rides along as `additionalContext`. The FR-CMD-016
//!   no-match default carries no note, so the adapter supplies one: a default
//!   is always visible to the agent.
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
//! # The rewrite prediction (#1272, FR-CMD-015)
//!
//! A rewrite the adapter applies is a prediction that the constructed
//! command will work: the adapter emits one `legion.cmd` prediction for it,
//! keyed by the call's `tool_use_id` (`crate::cmd::prediction`), after the
//! response is written and flushed (#1288), so a store write lock never
//! holds the response. Each run also starts, before its own work, a pass
//! that witnesses this session's earlier rewrites whose `tool_result` is now
//! in the session transcript. The pass runs on a worker thread beside the
//! decision: the decision never waits on it, and after the response it gets
//! only the rest of one `route.deadline_ms`, then is abandoned. Both run
//! outside the decision, which reads neither: a failure in either is
//! reported on stderr and never changes the response. The store both use is
//! opened before the decision, and that open waits at most one
//! `route.deadline_ms`; an open that has not finished by then skips both
//! for the call.
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
use std::time::{Duration, Instant};

use legion_cmd::{
    AskDetails, Context, Deciding, Decision, Lookup, ManagedTarget, Policy, RewriteSpec, Routed,
    Rule, RuleOutcome, ToolCall, ToolRules, parse_policy, required_lookups, route,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::cmd::config::RouteSettings;
use crate::cmd::prediction::{self, AppliedRewrite};
use crate::cmd::replacement::{build_replacement, rewritable_field};
use crate::db::Database;
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
pub(crate) const LEGION_REPO_ENV: &str = "LEGION_REPO";

const HOOK_EVENT: &str = "PreToolUse";

/// How many reflections a required recall or consult lookup fetches into
/// `Context`. Small on purpose: the lookup runs inside the decision deadline.
const LOOKUP_LIMIT: usize = 5;

/// The response written when the adapter cannot serialize its own response.
/// A fixed string, not built with `serde_json`, so it cannot itself fail.
const FALLBACK_DENY_JSON: &str = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"legion-cmd could not serialize its response -- instead: legion cmd-check -- <command>"}}"#;

/// What the agent reads when no policy rule governed its command and the
/// FR-CMD-016 default allowed it. Without it the default allow would be
/// byte-identical to the hook not running, and the default must be visible.
const DEFAULT_ALLOW_NOTE: &str =
    "legion-cmd: no policy rule governs this command; it runs under the default allow";

/// The PreToolUse payload fields the adapter acts on. Unknown fields are
/// ignored, not rejected: the harness may add fields. `tool_use_id`,
/// `session_id` and `transcript_path` serve only the rewrite prediction and
/// its witness (#1272); no decision reads them.
#[derive(Debug, Deserialize)]
struct HookPayload {
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
    #[serde(default)]
    cwd: Option<String>,
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
        Ok(_) => guarded(|| respond(&input, &mut after)),
        Err(e) => deny_for_error(&AdapterError::Payload(e.to_string()), None),
    };
    let body: String =
        serde_json::to_string(&response).unwrap_or_else(|_| FALLBACK_DENY_JSON.to_string());
    let _ = writeln!(stdout, "{body}");
    let _ = stdout.flush();
    after.finish();
    ExitCode::SUCCESS
}

/// The work a run leaves for after its response is out: the prediction for
/// the rewrite it applied, with the store it was opened against, and the
/// witness pass still running.
#[derive(Default)]
struct AfterResponse {
    prediction: Option<(AppliedRewrite, Database)>,
    witness: Option<PendingWitness>,
}

impl AfterResponse {
    /// Emits the prediction, then gives the witness pass whatever is left
    /// of the one deadline it started under; the process exits after and
    /// takes an unfinished pass along. Neither can change the response,
    /// which is already written.
    fn finish(self) {
        if let Some((rewrite, db)) = self.prediction {
            best_effort("rewrite prediction", || {
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
/// process never exits without a response over an adapter bug.
fn guarded(build: impl FnOnce() -> Value) -> Value {
    match panic::catch_unwind(AssertUnwindSafe(build)) {
        Ok(response) => response,
        Err(payload) => deny_for_error(&AdapterError::Panic(panic_message(&payload)), None),
    }
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
/// once collide. The emit uses this handle; the pass and the decision's
/// lookups open their own connections only after it, when migration is done.
/// The open waits at most one route deadline ([`open_store_within`]); a
/// store that cannot be opened in that time skips the pass and the emit.
fn respond(input: &str, after: &mut AfterResponse) -> Value {
    let policy_text: Result<String, AdapterError> = read_policy_text(None);
    let deadline: Duration = deadline_for(&policy_text);
    let store: Option<Database> = open_store_within(deadline, crate::cli::util::open_db);
    let legion_repo: Option<String> = std::env::var(LEGION_REPO_ENV).ok();
    let session_input: Option<String> = store.as_ref().map(|_| input.to_string());
    let (applied, pending) = respond_beside_witness(
        input,
        deadline,
        policy_text,
        Arc::new(StoreLookups),
        legion_repo,
        move || match session_input {
            Some(session_input) => witness_session(&session_input),
            None => Ok(()),
        },
    );
    after.witness = pending;
    after.prediction = applied.rewrite.zip(store);
    applied.response
}

/// The route deadline the policy sets: the default when the policy cannot be
/// read or its settings do not parse, which the decision reports itself.
/// Bounds the pre-decision store open and the witness pass's budget.
fn deadline_for(policy_text: &Result<String, AdapterError>) -> Duration {
    policy_text
        .as_ref()
        .ok()
        .and_then(|text| RouteSettings::from_policy_text(text).ok())
        .unwrap_or_default()
        .deadline
}

/// Runs `open` on its own worker thread and waits at most `deadline` for the
/// store (#1288). An open that fails, panics, or has not finished in time is
/// `None`, reported on stderr, and the decision goes ahead without a store.
/// An open still running at the deadline is left to finish or not: the
/// process is one-shot, and it ends the thread.
fn open_store_within(
    deadline: Duration,
    open: impl FnOnce() -> error::Result<Database> + Send + 'static,
) -> Option<Database> {
    let (tx, rx) = mpsc::channel::<Option<Database>>();
    let spawned = thread::Builder::new()
        .name("legion-cmd-store-open".to_string())
        .spawn(move || {
            let mut store: Option<Database> = None;
            best_effort("store open", || {
                store = Some(open()?);
                Ok(())
            });
            let _ = tx.send(store);
        });
    if let Err(e) = spawned {
        eprintln!("[legion cmd-check] store open not started: {e}");
        return None;
    }
    match rx.recv_timeout(deadline) {
        Ok(store) => store,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!(
                "[legion cmd-check] store open not finished within {deadline:?}; \
                 the prediction and the witness pass are skipped"
            );
            None
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => None,
    }
}

/// Starts the witness pass on its own worker thread, then runs the decision
/// as it always runs, under its own deadline. The decision never waits on the
/// pass. The pass's budget is one `deadline` from its start, so the call as
/// a whole stays within one deadline plus the decision's overhead.
fn respond_beside_witness(
    input: &str,
    deadline: Duration,
    policy_text: Result<String, AdapterError>,
    lookups: Arc<dyn LookupRunner>,
    legion_repo: Option<String>,
    witness: impl FnOnce() -> Result<(), Box<dyn std::error::Error>> + Send + 'static,
) -> (Applied, Option<PendingWitness>) {
    let pending: Option<PendingWitness> = PendingWitness::start(deadline, witness);
    let applied: Applied = respond_with(input, policy_text, lookups, legion_repo);
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

/// The adapter over injected sources, so a test drives every branch without
/// touching the environment or the store.
fn respond_with(
    input: &str,
    policy_text: Result<String, AdapterError>,
    lookups: Arc<dyn LookupRunner>,
    legion_repo: Option<String>,
) -> Applied {
    let payload: HookPayload = match serde_json::from_str(input) {
        Ok(payload) => payload,
        Err(e) => return deny_for_error(&AdapterError::Payload(e.to_string()), None).into(),
    };
    let call = ToolCall {
        tool: payload.tool_name.clone(),
        input: payload.tool_input.clone(),
    };
    match route_call(policy_text, call, lookups, legion_repo, payload.cwd.clone()) {
        Ok((routed, policy)) => apply(&routed, &payload, &policy),
        Err(e) => deny_for_error(&e, Some(&payload)).into(),
    }
}

/// The decision core both modes run (FR-CMD-017). Parses the policy and its
/// settings first (the text is a local file, read outside the deadline), then
/// runs repo derivation, the lookup pre-pass, and route on a worker thread
/// under `route.deadline_ms`. Returns route's result with the policy it
/// decided under, which [`replacement_for`] reads to find a rewrite's rule.
/// Every failure is an [`AdapterError`], which each mode turns into a deny
/// (FR-CMD-009, FR-CMD-016).
pub(crate) fn route_call(
    policy_text: Result<String, AdapterError>,
    call: ToolCall,
    lookups: Arc<dyn LookupRunner>,
    legion_repo: Option<String>,
    cwd: Option<String>,
) -> Result<(Routed, Arc<Policy>), AdapterError> {
    let policy_text: String = policy_text?;
    let settings: RouteSettings = RouteSettings::from_policy_text(&policy_text)
        .map_err(|e| AdapterError::PolicyRead(e.to_string()))?;
    let policy: Arc<Policy> =
        Arc::new(parse_policy(&policy_text).map_err(|e| AdapterError::PolicyRead(e.to_string()))?);
    let worker_policy: Arc<Policy> = Arc::clone(&policy);
    let routed: Routed = decide(settings.deadline, move || {
        let repo: Option<String> = repo_for(legion_repo.as_deref(), cwd.as_deref());
        let ctx: Context = fetch_context(&worker_policy, &call, repo, lookups.as_ref())?;
        Ok(route(&worker_policy, &call, &ctx))
    })?;
    Ok((routed, policy))
}

/// The replacement `tool_input` for a rewrite, built from route's facts
/// (FR-CMD-003); `None` for every other arm. A replacement that cannot be
/// built -- including a rewrite whose rule declares translatable arguments
/// (#1267) -- is a [`AdapterError::Replacement`], a deny in both modes.
pub(crate) fn replacement_for(
    routed: &Routed,
    policy: &Policy,
    original: &Value,
) -> Result<Option<Value>, AdapterError> {
    match &routed.decision {
        Decision::Rewrite { target, .. } => {
            rewritten_input(policy, routed, target, original).map(Some)
        }
        _ => Ok(None),
    }
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

/// Finds the rewrite rule that decided `routed` and builds the replacement
/// under it. A rewrite whose deciding entry names no rewrite rule in `policy`
/// is refused rather than built.
fn rewritten_input(
    policy: &Policy,
    routed: &Routed,
    target: &ManagedTarget,
    original: &Value,
) -> Result<Value, AdapterError> {
    let (rule_id, spec) = rewrite_rule(policy, &routed.deciding).ok_or_else(|| {
        AdapterError::Replacement("the rewrite names no rewrite rule in the policy".to_string())
    })?;
    build_replacement(target, rule_id, &spec.translatable, &routed.facts, original)
        .map_err(|e| AdapterError::Replacement(e.to_string()))
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

/// Applies route's Decision to the hook response (FR-CMD-017). `policy` is
/// the policy route decided under, read only to find a rewrite's rule. A
/// rewrite whose replacement was built is also returned as the
/// [`AppliedRewrite`] its prediction records (#1272); a refused rewrite is a
/// deny and carries none.
fn apply(routed: &Routed, payload: &HookPayload, policy: &Policy) -> Applied {
    let response: Value = match &routed.decision {
        Decision::Allow { note } => match (&routed.deciding, note.as_deref()) {
            (Deciding::Default, None | Some("")) => pass_through(Some(DEFAULT_ALLOW_NOTE)),
            (_, note) => pass_through(note),
        },
        Decision::Proxy { .. } => pass_through(None),
        Decision::Rewrite { target, reason } => {
            return match rewritten_input(policy, routed, target, &payload.tool_input) {
                Ok(updated) => Applied {
                    response: rewrite_response(payload.rewritten_value(), target, reason, updated),
                    rewrite: Some(applied_rewrite(payload, target)),
                },
                Err(e) => deny_for_error(&e, Some(payload)).into(),
            };
        }
        Decision::Deny(details) => deny_response(&format!(
            "{} -- instead: {}",
            details.reason(),
            details.instead()
        )),
        Decision::Ask(details) => ask_response(details, &routed.deciding, payload),
    };
    response.into()
}

/// The rewrite as its prediction records it: the call's ids, the value as
/// issued and as constructed, and whether the call runs in the background
/// (whose transcript result records only that it started).
fn applied_rewrite(payload: &HookPayload, target: &ManagedTarget) -> AppliedRewrite {
    AppliedRewrite {
        tool_use_id: payload.tool_use_id.clone(),
        session_id: payload.session_id.clone(),
        tool_name: payload.tool_name.clone(),
        issued: payload.rewritten_value().map(str::to_string),
        constructed: target.as_str().to_string(),
        background: payload
            .tool_input
            .get("run_in_background")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// The rewrite rule that decided a rewrite: its id and its [`RewriteSpec`].
/// Rule ids are unique across the whole policy, so the id `Deciding` names
/// finds exactly one rule, under a Bash family or a Fields tool. `None` when
/// the deciding entry is not a rule, or names no rewrite rule in `policy`;
/// the adapter then refuses rather than build a replacement it cannot judge.
fn rewrite_rule<'p>(policy: &'p Policy, deciding: &Deciding) -> Option<(&'p str, &'p RewriteSpec)> {
    let Deciding::Rule { id, .. } = deciding else {
        return None;
    };
    let named = |rule: &&Rule| rule.id == *id;
    let rule: &Rule = policy.tools.values().find_map(|rules| match rules {
        ToolRules::Bash { families } => families
            .values()
            .flat_map(|family| family.rules.iter())
            .find(named),
        ToolRules::Fields { rules } => rules.iter().find(named),
    })?;
    match &rule.outcome {
        RuleOutcome::Rewrite { spec, .. } => Some((rule.id.as_str(), spec)),
        _ => None,
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
    let (reason, instead) = error_deny_text(err, payload.and_then(HookPayload::command));
    deny_response(&format!("{reason} -- instead: {instead}"))
}

/// The reason and the command to run instead for a deny over an adapter
/// failure, shared by both modes so the operator mode reports the deny the
/// hook would send.
pub(crate) fn error_deny_text(err: &AdapterError, command: Option<&str>) -> (String, String) {
    (
        format!("legion-cmd could not decide this command ({err})"),
        format!("legion cmd-check -- {}", quoted_command(command)),
    )
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
    /// rewrites, `gh issue view` rewrites under a rule declaring translatable
    /// arguments, `gh pr` asks (marked), `xxd` proxies, `ls` allows with a
    /// note, `git push` needs recall and consult. The Agent and Edit rewrite
    /// rules back the hand-built `Routed` values that name them.
    const POLICY: &str = r#"{
        "route": {"deadline_ms": 2000},
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
          "Bash": {"families": {
            "gh issue view": {"rules": [
                {"id": "gh-issue-view",
                 "outcome": {"kind": "rewrite", "target": "legion issue view", "reason": "legion tracks issues",
                             "translatable": {"flags": ["--web"], "operands": ["integer"]}}}
            ]},
            "rm": {"rules": [
                {"id": "rm-rf", "predicates": [{"kind": "arg-present", "arg": "-rf"}],
                 "outcome": {"kind": "deny", "reason": "unrecoverable", "instead": "trash it"}}
            ]},
            "gh issue list": {"rules": [
                {"id": "gh-issue-list",
                 "outcome": {"kind": "rewrite", "target": "legion issue list", "reason": "legion tracks issues",
                             "translatable": {}}}
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
          }}
        }
    }"#;

    /// [`POLICY`] parsed, for the tests that call [`apply`] on a hand-built
    /// `Routed`.
    fn policy() -> Policy {
        parse_policy(POLICY).expect("the test policy parses")
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
            None,
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
        )
        .response;
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
    fn an_unmanaged_command_passes_through_and_the_default_is_surfaced() {
        // FR-CMD-002: the harness's own rules decide the permission.
        // FR-CMD-016: the default allow still reaches the agent, so it is
        // never indistinguishable from the hook not running.
        let response = respond_stub(&payload("echo hi"), POLICY);
        let out = output(&response);
        assert_eq!(out["hookEventName"], "PreToolUse");
        assert!(out.get("permissionDecision").is_none());
        assert!(out.get("updatedInput").is_none());
        assert_eq!(out["additionalContext"], DEFAULT_ALLOW_NOTE);
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
        };
        let response = apply(&routed, &parsed_payload("ls"), &policy()).response;
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
        };
        let payload: HookPayload = serde_json::from_value(json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": "a.rs", "old_string": "x", "new_string": "y"},
            "cwd": REPO_CWD
        }))
        .expect("valid payload");
        let response = apply(&routed, &payload, &policy()).response;
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
        };
        let payload: HookPayload = serde_json::from_value(json!({
            "tool_name": "Agent",
            "tool_input": {"subagent_type": "Explore", "prompt": "map it"},
            "cwd": REPO_CWD
        }))
        .expect("valid payload");
        let out = apply(&routed, &payload, &policy()).response["hookSpecificOutput"].clone();
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
        // A rewrite whose facts carry a path the target cannot express: the
        // replacement refuses and the agent sees a deny, never a narrower
        // command that ran. Built by hand, because route itself no longer
        // rewrites `gh issue list src/` (#1228: `src/` is not translatable).
        let routed = Routed {
            decision: Decision::Rewrite {
                target: ManagedTarget::new("legion issue list"),
                reason: "legion tracks issues".to_string(),
            },
            facts: Facts {
                paths: vec!["src/".to_string()],
                ..Facts::default()
            },
            deciding: Deciding::Rule {
                id: "gh-issue-list".to_string(),
                needs_operator: false,
            },
        };
        let response = apply(&routed, &parsed_payload("gh issue list src/"), &policy()).response;
        assert_denied(&response);
        assert!(reason(&response).contains("replacement:"));
        assert!(reason(&response).contains("path operand"));
    }

    #[test]
    fn a_rewrite_under_a_rule_declaring_translatable_arguments_is_refused_naming_the_rule() {
        // #1267: route judges `--web` and `7` translatable and returns
        // Rewrite, but the replacement carries no argument forward, so the
        // adapter denies naming the rule instead of running the bare target.
        let command = "gh issue view 7 --web";
        let routed = route(
            &policy(),
            &ToolCall {
                tool: "Bash".to_string(),
                input: json!({"command": command}),
            },
            &Context::default(),
        );
        assert!(
            matches!(routed.decision, Decision::Rewrite { .. }),
            "route must judge the covered arguments translatable: {:?}",
            routed.decision
        );

        let response = respond_stub(&payload(command), POLICY);
        assert_denied(&response);
        let text = reason(&response);
        assert!(text.contains("replacement:"), "got: {text}");
        assert!(text.contains("rewrite rule 'gh-issue-view'"), "got: {text}");
        assert!(!text.contains("instead: legion issue view"), "got: {text}");
    }

    #[test]
    fn a_rewrite_whose_rule_is_not_in_the_policy_is_refused() {
        let routed = Routed {
            decision: Decision::Rewrite {
                target: ManagedTarget::new("legion issue list"),
                reason: "legion tracks issues".to_string(),
            },
            facts: Facts::default(),
            deciding: Deciding::Rule {
                id: "no-such-rule".to_string(),
                needs_operator: false,
            },
        };
        let applied = apply(&routed, &parsed_payload("gh issue list"), &policy());
        assert!(
            applied.rewrite.is_none(),
            "a refused rewrite yields no prediction"
        );
        let response = applied.response;
        assert_denied(&response);
        assert!(reason(&response).contains("names no rewrite rule"));
    }

    #[test]
    fn route_denies_a_rewrite_whose_argument_does_not_translate() {
        // #1228: the rule declares no translatable operand, so route denies
        // before the adapter builds anything, naming the argument and the
        // managed command to run instead.
        let response = respond_stub(&payload("gh issue list src/"), POLICY);
        assert_denied(&response);
        assert!(
            reason(&response).contains("`src/`"),
            "got: {}",
            reason(&response)
        );
        assert!(reason(&response).contains("instead: legion issue list"));
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
        let response = apply(
            &ask_routed(false),
            &parsed_payload("gh pr merge 7"),
            &policy(),
        )
        .response;
        assert_denied(&response);
        assert!(reason(&response).contains("merge it?"));
    }

    #[test]
    fn a_marked_ask_prompts_the_operator_through_the_harness_with_the_reason() {
        // The only path that prompts the operator: the harness's own
        // permission prompt, carrying the reason.
        let response = apply(
            &ask_routed(true),
            &parsed_payload("gh pr merge 7"),
            &policy(),
        )
        .response;
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
        let response = apply(&routed, &parsed_payload("forbidden"), &policy()).response;
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
        )
        .response;
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
        let response = deny_for_error(&err, Some(&parsed_payload("slow")));
        assert_denied(&response);
        assert!(reason(&response).contains("deadline exceeded: no decision within 10 ms"));
    }

    #[test]
    fn a_slow_lookup_overruns_the_configured_deadline_end_to_end() {
        let slow = RecordingLookups::new(Duration::from_millis(400));
        let policy = POLICY.replacen("\"deadline_ms\": 2000", "\"deadline_ms\": 20", 1);
        let response = respond_with(&payload("git push"), Ok(policy), slow, None).response;
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
        let response = respond_with(&input, Ok(POLICY.to_string()), lookups.clone(), None).response;
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

    #[test]
    fn an_applied_rewrite_is_returned_once_with_issued_and_constructed() {
        // #1272: the prediction is emitted from this, so it must carry the
        // call's ids, the command as issued and the command constructed.
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "gh issue list"},
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
                issued: Some("gh issue list".to_string()),
                constructed: "legion issue list".to_string(),
                background: false,
            })
        );
        assert_eq!(output(&applied.response)["permissionDecision"], "allow");
    }

    #[test]
    fn a_backgrounded_rewrite_is_marked_background() {
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "gh issue list", "run_in_background": true},
            "tool_use_id": "toolu_bg",
            "cwd": REPO_CWD
        })
        .to_string();
        let rewrite = respond_applied(&input, POLICY).rewrite.expect("a rewrite");
        assert!(rewrite.background);
    }

    #[test]
    fn a_rewrite_refused_for_translatable_arguments_emits_no_prediction() {
        // #1267/#1274: route returns Rewrite, but the rule declares
        // translatable arguments the replacement cannot carry, so the adapter
        // denies. Nothing was constructed, so nothing is predicted.
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "gh issue view 7 --web"},
            "session_id": "s1",
            "tool_use_id": "toolu_refused",
            "cwd": REPO_CWD
        })
        .to_string();
        let applied = respond_applied(&input, POLICY);
        assert_denied(&applied.response);
        assert!(reason(&applied.response).contains("rewrite rule 'gh-issue-view'"));
        assert!(applied.rewrite.is_none());
    }

    #[test]
    fn no_decision_but_an_applied_rewrite_yields_a_prediction() {
        // Allow, deny, proxy, ask, and a route-level refusal of a rewrite.
        for command in [
            "ls -la",
            "rm -rf build",
            "xxd file.bin",
            "gh pr merge 7",
            "gh issue list src/",
        ] {
            let applied = respond_applied(&payload(command), POLICY);
            assert!(applied.rewrite.is_none(), "{command} yielded a rewrite");
        }
    }

    #[test]
    fn a_rewrite_whose_replacement_fails_yields_no_prediction() {
        let routed = Routed {
            decision: Decision::Rewrite {
                target: ManagedTarget::new("legion issue list"),
                reason: "legion tracks issues".to_string(),
            },
            facts: Facts {
                paths: vec!["src/".to_string()],
                ..Facts::default()
            },
            deciding: Deciding::Rule {
                id: "gh-issue-list".to_string(),
                needs_operator: false,
            },
        };
        let applied = apply(&routed, &parsed_payload("gh issue list src/"), &policy());
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
                issued: Some("gh issue list".to_string()),
                constructed: "legion issue list".to_string(),
                background: false,
            },
        )
        .expect("emits");

        let deadline = Duration::from_millis(400);
        let policy: String = POLICY.replace(r#""deadline_ms": 2000"#, r#""deadline_ms": 400"#);
        assert_ne!(
            policy, POLICY,
            "the test policy must carry the short deadline"
        );
        let transcript: PathBuf = fifo.clone();
        let started = Instant::now();
        let (applied, pending) = respond_beside_witness(
            &payload("gh issue list"),
            deadline,
            Ok(policy),
            Arc::new(StubLookups(Lookup::Empty)),
            None,
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
        assert_eq!(output(&applied.response)["permissionDecision"], "allow");
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
    fn an_extreme_configured_deadline_yields_the_routing_decision() {
        // #1288: the largest deadline the policy accepts reaches the
        // decision, not a panic deny.
        let policy: String = POLICY.replace(
            r#""deadline_ms": 2000"#,
            &format!(r#""deadline_ms": {}"#, u64::MAX),
        );
        assert_ne!(policy, POLICY, "the test policy must carry the deadline");
        let policy_text: Result<String, AdapterError> = Ok(policy);
        let deadline: Duration = deadline_for(&policy_text);
        assert_eq!(deadline, Duration::from_millis(u64::MAX));
        let response: Value = guarded(|| {
            respond_beside_witness(
                &payload("gh issue list"),
                deadline,
                policy_text,
                Arc::new(StubLookups(Lookup::Empty)),
                None,
                || Ok(()),
            )
            .0
            .response
        });
        assert_eq!(
            output(&response)["permissionDecision"],
            "allow",
            "got {response}"
        );
        assert!(output(&response).get("updatedInput").is_some());
    }

    #[test]
    fn a_store_open_past_the_deadline_is_skipped_at_the_deadline() {
        // #1288: an open that never finishes costs the decision one
        // deadline, then the call goes ahead without a store.
        let (_hold, never) = mpsc::channel::<()>();
        let deadline = Duration::from_millis(100);
        let started = Instant::now();
        let store: Option<Database> = open_store_within(deadline, move || {
            let _ = never.recv();
            Err(error::LegionError::Search(
                "released at the test's end".to_string(),
            ))
        });
        let waited: Duration = started.elapsed();
        assert!(store.is_none(), "an unfinished open yielded a store");
        assert!(waited >= deadline, "gave up early, after {waited:?}");
        assert!(
            waited < deadline + Duration::from_millis(300),
            "the open held the call for {waited:?}"
        );
    }

    #[test]
    fn a_store_open_that_fails_or_panics_is_skipped() {
        let failed = open_store_within(Duration::from_secs(5), || {
            Err(error::LegionError::Search("unopenable".to_string()))
        });
        assert!(failed.is_none());
        let panicked = open_store_within(Duration::from_secs(5), || panic!("open panicked"));
        assert!(panicked.is_none());
    }

    #[test]
    fn a_store_open_in_time_yields_the_store() {
        let store = open_store_within(
            Duration::from_secs(5),
            || Ok(crate::db::testutil::test_db()),
        );
        assert!(store.is_some());
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
}
