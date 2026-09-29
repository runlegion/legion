//! `legion cmd-check`: the CLI surface over the legion-cmd router.
//!
//! Two modes. `--hook` (#1229) reads one PreToolUse payload on stdin and
//! writes one hook response; `--deny-patterns` (#1237) beside it prints the
//! no-go permissions mirror. The operator and scripting mode (#1230) shows
//! what route decides for one tool call, and why, without running it:
//!
//! ```text
//! legion cmd-check [--repo <REPO>] [--tool <TOOL>] [--json] [--policy <PATH>] -- '<COMMAND>'
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

use legion_cmd::{Decision, Facts, ToolCall, ToolKind};
use serde::Serialize;
use serde_json::Value;

use crate::cmd::hook::{
    AdapterError, LEGION_REPO_ENV, LookupRunner, StoreLookups, error_deny_reason, panic_message,
    read_policy_text, replacement_for, route_call, run_hook,
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
        // No session work: a dry run never records, notifies, or uses up a
        // confirmation (#1237).
        // An operator's dry run comes from no agent, so no worktree isolation.
        let routed = route_call(policy_text, call, lookups, legion_repo, cwd, false, None)?;
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
            decision: error_decision(&e),
            facts: Facts::default(),
            replacement: None,
            elapsed,
        },
    }
}

/// The deny the hook sends for `err`, as a Decision (FR-CMD-009). It names
/// no command to run instead (#1337).
fn error_decision(err: &AdapterError) -> Decision {
    Decision::refuse(error_deny_reason(err))
}

/// Dispatches `legion cmd-check`. The hook mode always exits 0 with a
/// response. The operator mode exits 0 with a report for every decision,
/// deny included; only a usage error (no command, more than one word after
/// `--`, invalid `--input` JSON, an unknown `--tool`) prints a `[legion]`
/// error and exits 2.
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

/// `legion cmd-check --deny-patterns` (#1237): prints the no-go
/// permissions mirror and exits 0. Clap refuses it beside any other option.
pub(crate) fn handle_deny_patterns() -> error::Result<()> {
    println!("{}", deny_patterns_json()?);
    Ok(())
}

/// The permissions.deny patterns mirroring every built-in no-go entry
/// (FR-CMD-025), as one JSON array in entry order, for plugin setup
/// (`plugin/hooks/lib/deny-mirror.sh`) to merge into the harness settings.
fn deny_patterns_json() -> error::Result<String> {
    let patterns: Vec<&str> = legion_cmd::BUILTIN_DENY_PATTERNS
        .iter()
        .flat_map(|(_, patterns)| patterns.iter().copied())
        .collect();
    Ok(serde_json::to_string(&patterns)?)
}

/// Builds the tool call from the flags: the one positional argument, used
/// verbatim, as a Bash `tool_input.command` (or under `--tool`), else
/// `--input` as the `tool_input`. A usage error names what is wrong.
///
/// The positional form takes exactly one argument, the command as typed. With
/// several words the invoking shell has already removed their quoting, and
/// shell text rebuilt from them is not the command the operator typed: a
/// quoted `;`, a subscripted assignment prefix, or some other form would be
/// read differently. So more than one word is refused, never rebuilt.
fn tool_call(
    tool: Option<String>,
    input: Option<String>,
    command: Vec<String>,
) -> Result<ToolCall, String> {
    let tool: String = tool.unwrap_or_else(|| DEFAULT_TOOL.to_string());
    if tool != DEFAULT_TOOL && ToolKind::parse(&tool).is_none() {
        let known: Vec<&str> = std::iter::once(DEFAULT_TOOL)
            .chain(ToolKind::ALL.into_iter().map(ToolKind::as_str))
            .collect();
        return Err(format!(
            "unknown --tool '{tool}'; expected one of: {}",
            known.join(", ")
        ));
    }
    let input: Value = match input {
        Some(raw) => {
            serde_json::from_str(&raw).map_err(|e| format!("--input is not valid JSON: {e}"))?
        }
        None => match command.as_slice() {
            [] => {
                return Err(
                    "no command to check: pass it after `--`, or pass --tool with --input"
                        .to_string(),
                );
            }
            [only] => serde_json::json!({ "command": only }),
            _ => return Err(MULTI_WORD_USAGE.to_string()),
        },
    };
    Ok(ToolCall { tool, input })
}

/// The usage error for more than one word after `--`.
const MULTI_WORD_USAGE: &str = "pass the command as one quoted argument, or use --tool/--input";

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
    if let Some(replacement) = &report.replacement {
        lines.push(format!("replacement: {}", replaced_value(replacement)));
    }
    lines.push(format!("elapsed:  {:?}", report.elapsed));
    let mut text = lines.join("\n");
    text.push('\n');
    text
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

    /// One entry per outcome: `git` and `gh` get `legion ` inserted, `rm -rf`
    /// never runs, `curl` is asked, a Read allows with a note, a WebFetch
    /// needs recall and consult, and an Explore spawn rewrites its
    /// `subagent_type`.
    const POLICY: &str = r#"{
        "proxy": ["git", "gh"],
        "never_run": [
            {"id": "rm-rf", "names": ["rm"], "reason": "unrecoverable",
             "predicates": [{"kind": "flag", "short": ["r"]}, {"kind": "flag", "short": ["f"]}]}
        ],
        "ask": [{"id": "curl-network", "names": ["curl"], "reason": "curl reaches the network"}],
        "tools": {
            "Read": {"rules": [
                {"id": "read-note", "outcome": {"kind": "allow", "note": "prefer legion sym tree"}}
            ]},
            "WebFetch": {"rules": [
                {"id": "fetch-lookups", "requires_recall": true, "requires_consult": true,
                 "outcome": {"kind": "deny", "reason": "fetch through recall", "instead": "legion recall"}}
            ]},
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

    fn fetch() -> ToolCall {
        ToolCall {
            tool: "WebFetch".to_string(),
            input: serde_json::json!({ "url": "https://example.com" }),
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

    // -- each outcome reported (FR-CMD-017) -----------------------------------

    #[test]
    fn a_never_run_deny_reports_its_reason_and_names_no_command() {
        let report = check_policy(bash("rm -rf build"), POLICY);
        let Decision::Deny(details) = &report.decision else {
            panic!("expected a deny, got {:?}", report.decision);
        };
        assert_eq!(details.reason(), "unrecoverable");
        assert_eq!(details.instead(), legion_cmd::NO_GO_INSTEAD);
        assert!(report.replacement.is_none());
        let text = render_text(&report);
        assert!(text.contains("decision: deny"), "{text}");
        assert!(text.contains("reason:   unrecoverable"), "{text}");
        assert!(text.contains("elapsed:"), "{text}");
    }

    #[test]
    fn an_insertion_reports_the_rewritten_command() {
        // The replacement comes from the shared builder, which patches the
        // command and keeps every sibling field.
        let call = ToolCall {
            tool: "Bash".to_string(),
            input: serde_json::json!({"command": "cd a && git log", "timeout": 9000}),
        };
        let report = check_policy(call, POLICY);
        assert!(matches!(report.decision, Decision::Rewrite { .. }));
        assert_eq!(
            report.facts.rewritten.as_deref(),
            Some("cd a && legion git log")
        );
        assert_eq!(
            report.replacement,
            Some(serde_json::json!({"command": "cd a && legion git log", "timeout": 9000}))
        );
        let text = render_text(&report);
        assert!(text.contains("decision: rewrite"), "{text}");
        assert!(
            text.contains("replacement: cd a && legion git log"),
            "{text}"
        );
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
    fn an_allow_reports_its_note_and_an_untouched_command_has_none() {
        let call = ToolCall {
            tool: "Read".to_string(),
            input: serde_json::json!({"file_path": "a.md"}),
        };
        let report = check_policy(call, POLICY);
        assert!(render_text(&report).contains("note:     prefer legion sym tree"));
        let report = check_policy(bash("echo hi"), POLICY);
        assert_eq!(report.decision, Decision::Allow { note: None });
        assert!(render_text(&report).contains("note:     (none)"));
        assert!(report.replacement.is_none());
    }

    #[test]
    fn an_ask_reports_its_question_and_reason() {
        let report = check_policy(bash("curl example.com"), POLICY);
        let text = render_text(&report);
        assert!(text.contains("decision: ask"), "{text}");
        assert!(
            text.contains("question: this command needs the operator's approval"),
            "{text}"
        );
        assert!(
            text.contains("reason:   curl reaches the network"),
            "{text}"
        );
    }

    #[test]
    fn the_json_report_carries_every_field() {
        let report = check_policy(bash("git status"), POLICY);
        let value: Value = serde_json::to_value(&report).expect("serializes");
        assert_eq!(value["decision"]["kind"], "rewrite");
        assert_eq!(value["decision"]["target"], "legion");
        assert_eq!(value["facts"]["rewritten"], "legion git status");
        assert_eq!(value["replacement"]["command"], "legion git status");
        assert!(value["elapsed"].is_object());
    }

    // -- the shared core: lookups and the repo ----------------------------------

    #[test]
    fn the_lookup_pre_pass_runs_scoped_to_the_given_repo() {
        let lookups = Lookups::new(Duration::ZERO);
        let report = check_with(
            fetch(),
            Ok(POLICY.to_string()),
            lookups.clone(),
            Some("other-repo".to_string()),
            None,
        );
        assert_eq!(
            lookups.calls.lock().expect("calls lock").clone(),
            vec!["recall:other-repo".to_string(), "consult".to_string()]
        );
        assert_eq!(deny_reason(&report), "fetch through recall");
    }

    // -- fail closed (FR-CMD-009, FR-CMD-016) -----------------------------------

    #[test]
    fn a_failed_lookup_reports_a_deny_naming_the_error() {
        let report = check_with(
            fetch(),
            Ok(POLICY.to_string()),
            Arc::new(FailingLookups),
            Some("legion".to_string()),
            None,
        );
        assert!(deny_reason(&report).contains("lookup: search index error: db unavailable"));
        assert_eq!(report.facts, Facts::default());
        assert!(report.replacement.is_none());
    }

    #[test]
    fn a_panic_in_the_core_reports_a_deny_naming_the_panic() {
        let report = check_with(
            fetch(),
            Ok(POLICY.to_string()),
            Arc::new(PanickingLookups),
            Some("legion".to_string()),
            None,
        );
        assert!(deny_reason(&report).contains("simulated lookup bug"));
    }

    #[test]
    fn an_unreadable_policy_reports_a_deny_with_the_read_error_and_no_command() {
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
        assert_eq!(details.instead(), legion_cmd::NO_GO_INSTEAD);
    }

    #[test]
    fn an_unparsable_policy_reports_a_deny_with_the_parse_error() {
        let report = check_policy(bash("echo hi"), "{ not json");
        assert!(deny_reason(&report).contains("policy: policy is not valid JSON"));
        let report = check_policy(bash("echo hi"), r#"{"route": {}}"#);
        assert!(deny_reason(&report).contains("unknown field 'route'"));
    }

    #[test]
    fn an_empty_policy_runs_an_unmatched_command_untouched() {
        let report = check_policy(bash("echo hi"), "{}");
        assert_eq!(report.decision, Decision::Allow { note: None });
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
    fn the_one_positional_argument_is_the_bash_command_verbatim() {
        let command = "git commit -m \"a; rm -rf x\"";
        let call = tool_call(None, None, vec![command.to_string()]).expect("a valid call");
        assert_eq!(call.tool, "Bash");
        assert_eq!(call.input, serde_json::json!({ "command": command }));
    }

    #[test]
    fn one_quoted_argument_decides_the_same_as_the_string_via_input() {
        // The command passed as one argument is the typed string, so it
        // decides exactly as the same string given with --input.
        for command in [
            "git commit -m \"a; rm -rf x\"",
            "FOO+='a b' rm -rf build",
            "arr[i[0]]=x rm -rf build",
        ] {
            let positional = check_policy(
                tool_call(None, None, vec![command.to_string()]).expect("a valid call"),
                POLICY,
            );
            let input = serde_json::json!({ "command": command }).to_string();
            let typed = check_policy(
                tool_call(Some("Bash".to_string()), Some(input), Vec::new()).expect("a valid call"),
                POLICY,
            );
            assert_eq!(positional.decision, typed.decision, "{command}");
            assert_eq!(positional.facts, typed.facts, "{command}");
        }
        assert_eq!(
            check_policy(bash("FOO+='a b' rm -rf build"), POLICY).decision,
            Decision::no_go("unrecoverable").expect("valid deny")
        );
    }

    #[test]
    fn more_than_one_positional_word_is_a_usage_error() {
        for words in [
            vec!["rm", "-rf", "build"],
            vec!["git", "commit", "-m", "a; rm -rf x"],
            vec!["arr[i[0]]=x", "rm", "-rf", "build"],
        ] {
            let err = tool_call(None, None, words.iter().map(|w| w.to_string()).collect())
                .expect_err("several words are refused, never rebuilt");
            assert_eq!(err, MULTI_WORD_USAGE, "{words:?}");
        }
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
        assert!(err.contains("Bash"), "{err}");
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
    fn the_deny_patterns_are_one_json_array_covering_every_builtin_entry() {
        let text = deny_patterns_json().expect("serializes");
        let patterns: Vec<String> = serde_json::from_str(&text).expect("a JSON string array");
        for (id, entry_patterns) in legion_cmd::BUILTIN_DENY_PATTERNS {
            assert!(
                entry_patterns
                    .iter()
                    .all(|p| patterns.contains(&p.to_string())),
                "{id} missing"
            );
        }
        assert_eq!(
            legion_cmd::BUILTIN_DENY_PATTERNS.len(),
            legion_cmd::builtin_no_go().len()
        );
    }

    #[test]
    fn no_command_and_no_input_is_a_usage_error() {
        let err = tool_call(None, None, Vec::new()).expect_err("nothing to check");
        assert!(err.contains("no command to check"), "{err}");
    }
}
