//! `legion cmd-check`: the CLI surface over the legion-cmd router.
//!
//! Two modes. `--hook` (#1229) reads one PreToolUse payload on stdin and
//! writes one hook response. The operator and scripting mode (#1230) shows
//! what route decides for one tool call, and why, without running it:
//!
//! ```text
//! legion cmd-check [--repo <REPO>] [--tool <TOOL>] [--json] [--policy <PATH>] -- <COMMAND>
//! legion cmd-check [--repo <REPO>] --tool <TOOL> --input <JSON> [--json] [--policy <PATH>]
//! ```
//!
//! [`check`] runs the hook mode's own core (`crate::cmd::hook`): the same
//! policy loading, repo derivation, lookup pre-pass, deadline, and
//! replacement builder. It holds no routing branch and never scans the
//! command (FR-CMD-011, FR-CMD-017); it only renders what the core returns.
//! A failure anywhere in the core -- an unreadable or unparsable policy, a
//! failed lookup, an overrun, a replacement that cannot be built, a panic --
//! is reported as the deny the hook would send, naming the failure
//! (FR-CMD-009, FR-CMD-016). Nothing here spawns the command.

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use legion_cmd::{Decision, Facts, ProxyReason, ToolCall, ToolKind};
use serde::Serialize;
use serde_json::Value;

use crate::cmd::hook::{
    AdapterError, LEGION_REPO_ENV, LookupRunner, StoreLookups, command_in, error_deny_text,
    panic_message, read_policy_text, replacement_for, route_call, run_hook,
};
use crate::cmd::replacement::rewritable_field;
use crate::error;

/// The tool a positional command is checked as when `--tool` is not given.
const DEFAULT_TOOL: &str = "Bash";

/// What the operator mode reads beyond the tool call itself.
#[derive(Debug, Default)]
pub struct CheckOpts {
    /// Scopes a required recall lookup, like `LEGION_REPO` in the hook mode.
    /// `None` falls back to `LEGION_REPO`, then to the current directory.
    pub repo: Option<String>,
    /// A policy file other than the shipped default. `None` resolves the
    /// file the way the hook mode does.
    pub policy: Option<PathBuf>,
}

/// What route decided for one tool call, as the operator mode reports it.
#[derive(Debug, Serialize)]
pub struct CheckReport {
    pub decision: Decision,
    pub facts: Facts,
    /// The built `tool_input` for a rewrite; `None` for every other arm.
    pub replacement: Option<Value>,
    pub elapsed: Duration,
}

/// Shared with the hook mode: same policy loading, same Context building,
/// same lookup pre-pass, same deadline, same replacement builder.
pub fn check(call: ToolCall, opts: CheckOpts) -> CheckReport {
    let legion_repo: Option<String> = opts.repo.or_else(|| std::env::var(LEGION_REPO_ENV).ok());
    let cwd: Option<String> = std::env::current_dir()
        .ok()
        .and_then(|dir| dir.to_str().map(str::to_string));
    let policy_text = read_policy_text(opts.policy.as_deref());
    check_with(call, policy_text, Arc::new(StoreLookups), legion_repo, cwd)
}

/// [`check`] over injected sources, so a test drives every branch without
/// the environment or the store. A panic anywhere in the core is caught and
/// reported as a deny naming it, as the hook mode does.
fn check_with(
    call: ToolCall,
    policy_text: Result<String, AdapterError>,
    lookups: Arc<dyn LookupRunner>,
    legion_repo: Option<String>,
    cwd: Option<String>,
) -> CheckReport {
    let started = Instant::now();
    let original: Value = call.input.clone();
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        let routed = route_call(policy_text, call, lookups, legion_repo, cwd)?;
        let replacement = replacement_for(&routed, &original)?;
        Ok((routed, replacement))
    }))
    .unwrap_or_else(|payload| Err(AdapterError::Panic(panic_message(&payload))));
    let elapsed = started.elapsed();
    match outcome {
        Ok((routed, replacement)) => CheckReport {
            decision: routed.decision,
            facts: routed.facts,
            replacement,
            elapsed,
        },
        Err(e) => CheckReport {
            decision: error_decision(&e, command_in(&original)),
            facts: Facts::default(),
            replacement: None,
            elapsed,
        },
    }
}

/// The deny the hook sends for `err`, as a Decision (FR-CMD-009).
fn error_decision(err: &AdapterError, command: Option<&str>) -> Decision {
    let (reason, instead) = error_deny_text(err, command);
    // Both strings carry fixed text, so the non-empty check cannot fail; the
    // fallback mirrors legion_cmd's own `deny` helper.
    Decision::deny(reason, instead).unwrap_or(Decision::Proxy {
        reason: ProxyReason::Opaque,
    })
}

/// Dispatches `legion cmd-check`. The hook mode always exits 0 with a
/// response. The operator mode exits 0 with a report for every decision,
/// deny included; only a usage error (no command, invalid `--input` JSON, an
/// unknown `--tool`) prints a `[legion]` error and exits non-zero.
pub(crate) fn handle_cmd_check(
    hook: bool,
    repo: Option<String>,
    tool: Option<String>,
    input: Option<String>,
    json: bool,
    policy: Option<PathBuf>,
    command: Vec<String>,
) -> error::Result<()> {
    if hook {
        // `run_hook` answers every payload with a response and exits 0
        // (FR-CMD-009); the code it returns is always success, so there is
        // nothing to map onto `LegionError::ExitWith` here.
        let _always_success: ExitCode = run_hook(std::io::stdin().lock(), std::io::stdout().lock());
        return Ok(());
    }
    let call: ToolCall = match tool_call(tool, input, command) {
        Ok(call) => call,
        Err(message) => {
            eprintln!("[legion] error: {message}");
            return Err(error::LegionError::ExitWith(2));
        }
    };
    let report = check(call, CheckOpts { repo, policy });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render_text(&report));
    }
    Ok(())
}

/// Builds the tool call from the flags: the positional command as a Bash
/// `tool_input.command` (or under `--tool`), else `--input` as the
/// `tool_input`. A usage error names what is wrong.
fn tool_call(
    tool: Option<String>,
    input: Option<String>,
    command: Vec<String>,
) -> Result<ToolCall, String> {
    let tool: String = tool.unwrap_or_else(|| DEFAULT_TOOL.to_string());
    if !ToolKind::ALL.into_iter().any(|kind| kind.as_str() == tool) {
        let known: Vec<&str> = ToolKind::ALL.into_iter().map(ToolKind::as_str).collect();
        return Err(format!(
            "unknown --tool '{tool}'; expected one of: {}",
            known.join(", ")
        ));
    }
    let input: Value = match input {
        Some(raw) => {
            serde_json::from_str(&raw).map_err(|e| format!("--input is not valid JSON: {e}"))?
        }
        None if command.is_empty() => {
            return Err(
                "no command to check: pass it after `--`, or pass --tool with --input".to_string(),
            );
        }
        None => serde_json::json!({ "command": command.join(" ") }),
    };
    Ok(ToolCall { tool, input })
}

/// The report as text: the Decision arm and its details, the facts, the
/// replacement for a rewrite, and the elapsed time.
fn render_text(report: &CheckReport) -> String {
    let mut lines: Vec<String> = Vec::new();
    match &report.decision {
        Decision::Allow { note } => {
            lines.push("decision: allow".to_string());
            lines.push(format!("note:     {}", note.as_deref().unwrap_or("(none)")));
        }
        Decision::Rewrite { target, reason } => {
            lines.push("decision: rewrite".to_string());
            lines.push(format!("target:   {}", target.as_str()));
            lines.push(format!("reason:   {reason}"));
        }
        Decision::Proxy { reason } => {
            lines.push("decision: proxy".to_string());
            lines.push(format!("reason:   {}", reason.as_str()));
        }
        Decision::Deny(details) => {
            lines.push("decision: deny".to_string());
            lines.push(format!("reason:   {}", details.reason()));
            lines.push(format!("instead:  {}", details.instead()));
        }
        Decision::Ask(details) => {
            lines.push("decision: ask".to_string());
            lines.push(format!("question: {}", details.question()));
            lines.push(format!("reason:   {}", details.reason()));
        }
    }
    let facts = &report.facts;
    let issue_numbers: Vec<String> = facts.issue_numbers.iter().map(u64::to_string).collect();
    lines.push("facts:".to_string());
    lines.push(format!(
        "  verb:          {}",
        facts.verb.as_deref().unwrap_or("(none)")
    ));
    lines.push(format!("  paths:         {}", listed(&facts.paths)));
    lines.push(format!("  issue numbers: {}", listed(&issue_numbers)));
    lines.push(format!("  keywords:      {}", listed(&facts.keywords)));
    if let Some(replacement) = &report.replacement {
        lines.push(format!("replacement: {}", replaced_value(replacement)));
    }
    lines.push(format!("elapsed:  {:?}", report.elapsed));
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn listed(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(", ")
    }
}

/// The value the rewrite put in place: the replacement command for a Bash
/// call, the new `subagent_type` for a spawn. Reads the field the builder
/// patched, so the report and the patch cannot name different fields.
fn replaced_value(replacement: &Value) -> String {
    rewritable_field(replacement)
        .and_then(|field| replacement.get(field))
        .and_then(Value::as_str)
        .map_or_else(|| replacement.to_string(), str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use legion_cmd::Lookup;
    use std::path::Path;
    use std::sync::Mutex;
    use std::thread;

    /// Answers every lookup with `Lookup::Empty` after `delay`, recording
    /// each call.
    struct Lookups {
        delay: Duration,
        calls: Mutex<Vec<String>>,
    }

    impl Lookups {
        fn new(delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                delay,
                calls: Mutex::new(Vec::new()),
            })
        }
    }

    impl LookupRunner for Lookups {
        fn recall(&self, repo: &str, _query: &str) -> error::Result<Lookup> {
            thread::sleep(self.delay);
            self.calls
                .lock()
                .expect("calls lock")
                .push(format!("recall:{repo}"));
            Ok(Lookup::Empty)
        }
        fn consult(&self, _query: &str) -> error::Result<Lookup> {
            thread::sleep(self.delay);
            self.calls
                .lock()
                .expect("calls lock")
                .push("consult".to_string());
            Ok(Lookup::Empty)
        }
    }

    struct FailingLookups;

    impl LookupRunner for FailingLookups {
        fn recall(&self, _repo: &str, _query: &str) -> error::Result<Lookup> {
            Err(error::LegionError::Search("db unavailable".to_string()))
        }
        fn consult(&self, _query: &str) -> error::Result<Lookup> {
            Err(error::LegionError::Search("db unavailable".to_string()))
        }
    }

    /// Panics on every lookup, standing in for a bug inside the core.
    struct PanickingLookups;

    impl LookupRunner for PanickingLookups {
        fn recall(&self, _repo: &str, _query: &str) -> error::Result<Lookup> {
            panic!("simulated lookup bug")
        }
        fn consult(&self, _query: &str) -> error::Result<Lookup> {
            panic!("simulated lookup bug")
        }
    }

    /// One rule per arm: `rm -rf` denies, `gh issue list` rewrites, `gh pr`
    /// asks, `xxd` proxies, `ls` allows with a note, `git push` needs recall
    /// and consult, and an Explore spawn rewrites its `subagent_type`.
    const POLICY: &str = r#"{
        "route": {"deadline_ms": 2000},
        "tools": {
            "Bash": {"families": {
                "rm": {"rules": [
                    {"id": "rm-rf", "predicates": [{"kind": "arg-present", "arg": "-rf"}],
                     "outcome": {"kind": "deny", "reason": "unrecoverable", "instead": "trash it"}}
                ]},
                "gh issue list": {"rules": [
                    {"id": "gh-issue-list",
                     "outcome": {"kind": "rewrite", "target": "legion issue list",
                                 "reason": "legion tracks issues", "translatable": {}}}
                ]},
                "gh pr": {"rules": [
                    {"id": "gh-pr", "outcome": {"kind": "ask", "question": "touch the PR?",
                     "reason": "PRs are the orchestrator's"}}
                ]},
                "xxd": {"rules": [{"id": "xxd", "outcome": {"kind": "proxy", "reason": "binary"}}]},
                "ls": {"rules": [{"id": "ls", "outcome": {"kind": "allow", "note": "prefer legion sym tree"}}]},
                "git push": {"rules": [
                    {"id": "git-push", "requires_recall": true, "requires_consult": true,
                     "outcome": {"kind": "deny", "reason": "push through legion", "instead": "legion push"}}
                ]}
            }},
            "Agent": {"rules": [
                {"id": "agent-explore", "predicates": [{"kind": "field-equals", "field": "subagent_type", "any_of": ["Explore"]}],
                 "outcome": {"kind": "rewrite", "target": "legion:legion-explore", "reason": "use the legion explorer"}}
            ]}
        }
    }"#;

    fn bash(command: &str) -> ToolCall {
        ToolCall {
            tool: "Bash".to_string(),
            input: serde_json::json!({ "command": command }),
        }
    }

    fn check_policy(call: ToolCall, policy: &str) -> CheckReport {
        check_with(
            call,
            Ok(policy.to_string()),
            Lookups::new(Duration::ZERO),
            Some("legion".to_string()),
            None,
        )
    }

    fn deny_reason(report: &CheckReport) -> &str {
        match &report.decision {
            Decision::Deny(details) => details.reason(),
            other => panic!("expected a deny, got {other:?}"),
        }
    }

    // -- each arm reported, with its facts (FR-CMD-017) -----------------------

    #[test]
    fn a_deny_reports_its_reason_instead_and_facts() {
        let report = check_policy(bash("rm -rf build"), POLICY);
        let Decision::Deny(details) = &report.decision else {
            panic!("expected a deny, got {:?}", report.decision);
        };
        assert_eq!(details.reason(), "unrecoverable");
        assert_eq!(details.instead(), "trash it");
        assert_eq!(report.facts.verb.as_deref(), Some("rm"));
        assert!(report.replacement.is_none());
        let text = render_text(&report);
        assert!(text.contains("decision: deny"), "{text}");
        assert!(text.contains("reason:   unrecoverable"), "{text}");
        assert!(text.contains("instead:  trash it"), "{text}");
        assert!(text.contains("verb:          rm"), "{text}");
        assert!(text.contains("elapsed:"), "{text}");
    }

    #[test]
    fn a_rewrite_reports_the_target_and_the_built_replacement() {
        // FR-CMD-003: the replacement comes from the shared builder, which
        // patches the command and keeps every sibling field.
        let call = ToolCall {
            tool: "Bash".to_string(),
            input: serde_json::json!({"command": "gh issue list", "timeout": 9000}),
        };
        let report = check_policy(call, POLICY);
        assert!(matches!(report.decision, Decision::Rewrite { .. }));
        assert_eq!(
            report.replacement,
            Some(serde_json::json!({"command": "legion issue list", "timeout": 9000}))
        );
        let text = render_text(&report);
        assert!(text.contains("decision: rewrite"), "{text}");
        assert!(text.contains("target:   legion issue list"), "{text}");
        assert!(text.contains("reason:   legion tracks issues"), "{text}");
        assert!(text.contains("replacement: legion issue list"), "{text}");
    }

    #[test]
    fn an_agent_rewrite_reports_the_new_subagent_type() {
        let call = ToolCall {
            tool: "Agent".to_string(),
            input: serde_json::json!({"subagent_type": "Explore", "prompt": "map it"}),
        };
        let report = check_policy(call, POLICY);
        assert_eq!(
            report.replacement,
            Some(serde_json::json!({"subagent_type": "legion:legion-explore", "prompt": "map it"}))
        );
        assert!(render_text(&report).contains("replacement: legion:legion-explore"));
    }

    #[test]
    fn a_proxy_reports_its_closed_set_reason() {
        let report = check_policy(bash("xxd file.bin"), POLICY);
        assert_eq!(
            report.decision,
            Decision::Proxy {
                reason: ProxyReason::Binary
            }
        );
        let text = render_text(&report);
        assert!(text.contains("decision: proxy"), "{text}");
        assert!(text.contains("reason:   binary"), "{text}");
    }

    #[test]
    fn an_allow_reports_its_note_and_the_default_allow_has_none() {
        let report = check_policy(bash("ls -la"), POLICY);
        assert!(render_text(&report).contains("note:     prefer legion sym tree"));
        let report = check_policy(bash("echo hi"), POLICY);
        assert_eq!(report.decision, Decision::Allow { note: None });
        assert!(render_text(&report).contains("note:     (none)"));
    }

    #[test]
    fn an_ask_reports_its_question_and_reason() {
        let report = check_policy(bash("gh pr merge 7"), POLICY);
        let text = render_text(&report);
        assert!(text.contains("decision: ask"), "{text}");
        assert!(text.contains("question: touch the PR?"), "{text}");
        assert!(
            text.contains("reason:   PRs are the orchestrator's"),
            "{text}"
        );
    }

    #[test]
    fn the_json_report_carries_every_field() {
        let report = check_policy(bash("gh issue list"), POLICY);
        let value: Value = serde_json::to_value(&report).expect("serializes");
        assert_eq!(value["decision"]["kind"], "rewrite");
        assert_eq!(value["decision"]["target"], "legion issue list");
        assert_eq!(value["facts"]["verb"], "issue list");
        assert_eq!(value["replacement"]["command"], "legion issue list");
        assert!(value["elapsed"].is_object());
    }

    // -- the shared core: lookups and the repo ----------------------------------

    #[test]
    fn the_lookup_pre_pass_runs_scoped_to_the_given_repo() {
        let lookups = Lookups::new(Duration::ZERO);
        let report = check_with(
            bash("git push origin main"),
            Ok(POLICY.to_string()),
            lookups.clone(),
            Some("other-repo".to_string()),
            None,
        );
        assert_eq!(
            lookups.calls.lock().expect("calls lock").clone(),
            vec!["recall:other-repo".to_string(), "consult".to_string()]
        );
        assert_eq!(deny_reason(&report), "push through legion");
    }

    // -- fail closed (FR-CMD-009, FR-CMD-016) -----------------------------------

    #[test]
    fn an_overrun_reports_a_deny_naming_the_deadline() {
        let policy = POLICY.replacen("\"deadline_ms\": 2000", "\"deadline_ms\": 20", 1);
        let report = check_with(
            bash("git push"),
            Ok(policy),
            Lookups::new(Duration::from_millis(400)),
            Some("legion".to_string()),
            None,
        );
        assert!(
            deny_reason(&report).contains("deadline exceeded: no decision within 20 ms"),
            "{}",
            deny_reason(&report)
        );
        assert_eq!(report.facts, Facts::default());
        assert!(report.replacement.is_none());
    }

    #[test]
    fn a_failed_lookup_reports_a_deny_naming_the_error() {
        let report = check_with(
            bash("git push"),
            Ok(POLICY.to_string()),
            Arc::new(FailingLookups),
            Some("legion".to_string()),
            None,
        );
        assert!(deny_reason(&report).contains("lookup: search index error: db unavailable"));
    }

    #[test]
    fn a_panic_in_the_core_reports_a_deny_naming_the_panic() {
        let report = check_with(
            bash("git push"),
            Ok(POLICY.to_string()),
            Arc::new(PanickingLookups),
            Some("legion".to_string()),
            None,
        );
        assert!(deny_reason(&report).contains("simulated lookup bug"));
    }

    #[test]
    fn an_unreadable_policy_reports_a_deny_with_the_read_error() {
        let report = check_with(
            bash("echo hi"),
            read_policy_text(Some(Path::new("/nowhere/policy.json"))),
            Lookups::new(Duration::ZERO),
            None,
            None,
        );
        let reason = deny_reason(&report);
        assert!(reason.contains("policy: /nowhere/policy.json"), "{reason}");
        let Decision::Deny(details) = &report.decision else {
            unreachable!()
        };
        assert_eq!(details.instead(), "legion cmd-check -- 'echo hi'");
    }

    #[test]
    fn an_unparsable_policy_reports_a_deny_with_the_parse_error() {
        let report = check_policy(bash("echo hi"), "{ not json");
        assert!(deny_reason(&report).contains("policy: policy text is not valid JSON"));
    }

    #[test]
    fn an_empty_policy_denies_every_command() {
        let report = check_policy(bash("echo hi"), "{}");
        assert!(deny_reason(&report).contains("policy is empty"));
    }

    #[test]
    fn a_replacement_that_cannot_be_built_reports_a_deny() {
        // A rewrite rule on a tool whose input has no field to replace: the
        // shared builder refuses, and the report is the hook's deny.
        let policy = r#"{"tools": {"Edit": {"rules": [
            {"id": "edit-rewrite", "outcome": {"kind": "rewrite", "target": "legion x",
             "reason": "hand-built"}}
        ]}}}"#;
        let call = ToolCall {
            tool: "Edit".to_string(),
            input: serde_json::json!({"file_path": "a.rs", "old_string": "x", "new_string": "y"}),
        };
        let report = check_policy(call, policy);
        assert!(deny_reason(&report).contains("replacement:"));
        assert!(report.replacement.is_none());
    }

    // -- usage errors are not decisions ------------------------------------------

    #[test]
    fn the_positional_command_becomes_a_bash_tool_input() {
        let call = tool_call(None, None, vec!["rm".into(), "-rf".into(), "build".into()])
            .expect("a valid call");
        assert_eq!(call.tool, "Bash");
        assert_eq!(call.input, serde_json::json!({"command": "rm -rf build"}));
    }

    #[test]
    fn input_is_the_tool_input_verbatim() {
        let call = tool_call(
            Some("Agent".to_string()),
            Some(r#"{"subagent_type": "Explore"}"#.to_string()),
            Vec::new(),
        )
        .expect("a valid call");
        assert_eq!(call.tool, "Agent");
        assert_eq!(call.input, serde_json::json!({"subagent_type": "Explore"}));
    }

    #[test]
    fn an_unknown_tool_is_a_usage_error() {
        let err =
            tool_call(Some("Bsh".to_string()), None, vec!["ls".into()]).expect_err("unknown tool");
        assert!(err.contains("unknown --tool 'Bsh'"), "{err}");
    }

    #[test]
    fn invalid_input_json_is_a_usage_error() {
        let err = tool_call(
            Some("Edit".to_string()),
            Some("{ nope".to_string()),
            Vec::new(),
        )
        .expect_err("invalid JSON");
        assert!(err.contains("--input is not valid JSON"), "{err}");
    }

    #[test]
    fn no_command_and_no_input_is_a_usage_error() {
        let err = tool_call(None, None, Vec::new()).expect_err("nothing to check");
        assert!(err.contains("no command to check"), "{err}");
    }
}
