//! FR-CMD-010 over the hook case battery: every command class the current
//! hooks handle receives a Decision from route, and where route and a hook
//! disagree, route's Decision is authoritative absent an operator ruling.
//!
//! `tests/fixtures/hook_cases.json` carries one row per case class of each
//! hook, enumerated from the hook script's branches and its test suite. The
//! hooks are accounted for as cases, not transcribed as rules: a row records
//! what the hook does today (`hook_behavior`) beside what route does against
//! the SHIPPED `plugin/legion-cmd/policy.json` (`route`), and `agrees` says
//! whether the agent sees the same thing from both: the same Decision arm,
//! and on the allow arm the same presence of text (a silent hook matches only
//! a route allow with no note, an injecting hook only one with a note). This
//! test asserts the `route` column against route's actual Decision, so a row
//! can never drift from reality while claiming a decision route no longer
//! makes; and it asserts the row is internally honest: `agrees` cannot be true
//! when the arms or the allow text differ, and every disagreement carries a
//! note for the operator.
//!
//! Row shape (the Bash-hook issue's, plus optional expectations):
//!
//! - `hook`, `case` (unique), `tool`, `input` (the `tool_input` object)
//! - `hook_behavior`: `deny` | `rewrite` | `allow` | `inject` (inject-only
//!   hooks map to allow with a note)
//! - `route`: the expected Decision arm, `allow` | `rewrite` | `proxy` |
//!   `deny` | `ask`
//! - `agrees`, `note`
//! - optional `context`: `{"recall": "found" | "empty" | "not-fetched"}` (and
//!   the same for `consult`), the lookups the caller passed; default
//!   not-fetched
//! - optional `target` (a rewrite's target name), `facts` (the four Facts
//!   fields a rewrite row shows), `note_contains` / `note_excludes` (an
//!   allow's note), `instead_contains` (a deny's replacement)
//!
//! Every failing row is reported by hook and case id, not only the first. The
//! policy and the fixture are compiled in with `include_str!`, so no test here
//! opens a file at run time (NFR-CMD-001).

use std::collections::BTreeSet;

use legion_cmd::{
    Context, Decision, Facts, Lookup, PolicyError, ToolCall, ToolKind, ToolRules, parse_policy,
    route,
};
use serde::Deserialize;
use serde_json::Value;

const POLICY_JSON: &str = include_str!("../../../plugin/legion-cmd/policy.json");
const CASES_JSON: &str = include_str!("fixtures/hook_cases.json");

/// The tool-field hooks this battery accounts for (#1233), by script name as
/// registered in `plugin/hooks/hooks.json`.
const TOOL_FIELD_HOOKS: [&str; 6] = [
    "pre-grep.sh",
    "pre-read-sym.sh",
    "no-local-memory.sh",
    "pre-script-search.sh",
    "no-harness-explore.sh",
    "recall-first.sh",
];

/// The matchers those hooks are registered under, which the shipped policy
/// must carry as Fields tool kinds.
const TOOL_FIELD_KINDS: [ToolKind; 10] = [
    ToolKind::Grep,
    ToolKind::Glob,
    ToolKind::Read,
    ToolKind::Write,
    ToolKind::Edit,
    ToolKind::MultiEdit,
    ToolKind::Agent,
    ToolKind::Task,
    ToolKind::WebFetch,
    ToolKind::WebSearch,
];

const HOOK_BEHAVIORS: [&str; 4] = ["deny", "rewrite", "allow", "inject"];
const DECISION_ARMS: [&str; 5] = ["allow", "rewrite", "proxy", "deny", "ask"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookCase {
    hook: String,
    case: String,
    tool: String,
    input: Value,
    hook_behavior: String,
    route: String,
    agrees: bool,
    note: String,
    #[serde(default)]
    context: Option<CaseContext>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    facts: Option<ExpectedFacts>,
    #[serde(default)]
    note_contains: Option<String>,
    #[serde(default)]
    note_excludes: Option<String>,
    #[serde(default)]
    instead_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseContext {
    #[serde(default)]
    recall: Option<String>,
    #[serde(default)]
    consult: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedFacts {
    paths: Vec<String>,
    verb: Option<String>,
    issue_numbers: Vec<u64>,
    keywords: Vec<String>,
}

impl HookCase {
    fn id(&self) -> String {
        format!("{}/{}", self.hook, self.case)
    }

    fn call(&self) -> ToolCall {
        ToolCall {
            tool: self.tool.clone(),
            input: self.input.clone(),
        }
    }

    fn context(&self) -> Context {
        let lookup = |raw: &Option<String>| match raw.as_deref() {
            None | Some("not-fetched") => Lookup::NotFetched,
            Some("empty") => Lookup::Empty,
            Some("found") => Lookup::Found(vec!["a recall hit".to_string()]),
            Some(other) => panic!("{}: unknown lookup state {other:?}", self.id()),
        };
        match &self.context {
            None => Context::default(),
            Some(ctx) => Context {
                recall: lookup(&ctx.recall),
                consult: lookup(&ctx.consult),
                ..Context::default()
            },
        }
    }

    /// The Decision arm the hook's behavior corresponds to: an inject-only
    /// hook allows the call and adds context, which is route's allow with a
    /// note.
    fn hook_arm(&self) -> &str {
        match self.hook_behavior.as_str() {
            "inject" => "allow",
            other => other,
        }
    }
}

fn load_cases() -> Vec<HookCase> {
    serde_json::from_str(CASES_JSON).unwrap_or_else(|e| panic!("hook_cases.json is not valid: {e}"))
}

fn shipped_policy() -> legion_cmd::Policy {
    parse_policy(POLICY_JSON)
        .unwrap_or_else(|e: PolicyError| panic!("the shipped policy.json must parse: {e}"))
}

fn arm_name(decision: &Decision) -> &'static str {
    match decision {
        Decision::Allow { .. } => "allow",
        Decision::Rewrite { .. } => "rewrite",
        Decision::Proxy { .. } => "proxy",
        Decision::Deny(_) => "deny",
        Decision::Ask(_) => "ask",
    }
}

#[test]
fn hook_cases_fixture_is_well_formed_and_honest() {
    let cases = load_cases();
    assert!(!cases.is_empty(), "hook_cases.json must carry rows");

    let mut seen = BTreeSet::new();
    let mut problems = Vec::new();
    for case in &cases {
        let id = case.id();
        if !seen.insert(case.case.as_str()) {
            problems.push(format!("{id}: duplicate case id"));
        }
        if !HOOK_BEHAVIORS.contains(&case.hook_behavior.as_str()) {
            problems.push(format!(
                "{id}: hook_behavior must be one of {HOOK_BEHAVIORS:?}, got {:?}",
                case.hook_behavior
            ));
        }
        if !DECISION_ARMS.contains(&case.route.as_str()) {
            problems.push(format!(
                "{id}: route must be one of {DECISION_ARMS:?}, got {:?}",
                case.route
            ));
        }
        // `agrees` is a claim about the two arms, and it can only ever be
        // narrower than the arms: the same arm may still be recorded as a
        // disagreement (a note route cannot produce), but different arms can
        // never be recorded as agreement.
        if case.agrees && case.hook_arm() != case.route {
            problems.push(format!(
                "{id}: agrees is true but the hook's arm ({}) differs from route's ({})",
                case.hook_arm(),
                case.route
            ));
        }
        if !case.agrees && case.note.trim().is_empty() {
            problems.push(format!(
                "{id}: a disagreement must carry a note for the operator"
            ));
        }
        if case.target.is_some() && case.route != "rewrite" {
            problems.push(format!("{id}: target is only meaningful on a rewrite row"));
        }
        if (case.note_contains.is_some() || case.note_excludes.is_some()) && case.route != "allow" {
            problems.push(format!(
                "{id}: note_contains / note_excludes are only meaningful on an allow row"
            ));
        }
        if case.instead_contains.is_some() && case.route != "deny" {
            problems.push(format!(
                "{id}: instead_contains is only meaningful on a deny row"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "malformed rows:\n{}",
        problems.join("\n")
    );
}

#[test]
fn every_tool_field_hook_and_matcher_has_at_least_one_row() {
    // FR-CMD-010: "every command class the current hooks handle receives a
    // Decision from route". The mechanical floor: a row per hook, and a row
    // per matcher the hooks are registered under.
    let cases = load_cases();
    for hook in TOOL_FIELD_HOOKS {
        assert!(
            cases.iter().any(|c| c.hook == hook),
            "no hook_cases.json row names hook {hook:?}"
        );
    }
    for kind in TOOL_FIELD_KINDS {
        assert!(
            cases.iter().any(|c| c.tool == kind.as_str()),
            "no hook_cases.json row exercises the {} tool",
            kind.as_str()
        );
    }
}

#[test]
fn shipped_policy_carries_a_fields_entry_for_every_tool_field_kind() {
    let policy = shipped_policy();
    for kind in TOOL_FIELD_KINDS {
        match policy.tools.get(&kind) {
            Some(ToolRules::Fields { rules }) => assert!(
                !rules.is_empty(),
                "the shipped policy's {} entry must carry at least one rule",
                kind.as_str()
            ),
            other => panic!(
                "the shipped policy must carry a Fields entry for {}, got {other:?}",
                kind.as_str()
            ),
        }
    }
}

#[test]
fn every_hook_case_matches_routes_actual_decision() {
    let policy = shipped_policy();
    let cases = load_cases();
    let mut failures: Vec<String> = Vec::new();
    let mut disagreements: Vec<String> = Vec::new();

    for case in &cases {
        let id = case.id();
        let routed = route(&policy, &case.call(), &case.context());
        let actual = arm_name(&routed.decision);
        if actual != case.route {
            failures.push(format!(
                "{id}: expected route={:?}, got {actual:?} ({:?})",
                case.route, routed.decision
            ));
            continue;
        }

        match (&routed.decision, case) {
            (
                Decision::Rewrite { target, .. },
                HookCase {
                    target: Some(want), ..
                },
            ) => {
                if target.as_str() != want {
                    failures.push(format!(
                        "{id}: expected rewrite target {want:?}, got {:?}",
                        target.as_str()
                    ));
                }
            }
            (
                Decision::Allow { note },
                HookCase {
                    note_contains,
                    note_excludes,
                    ..
                },
            ) => {
                let note = note.as_deref().unwrap_or("");
                // On the allow arm, what the agent reads is part of the
                // decision: a silent hook agrees only with a silent route,
                // and an injecting hook only with a route that says
                // something. Anything else is a note-content disagreement
                // and must be listed for the operator.
                let route_speaks = !note.is_empty();
                let hook_speaks = case.hook_behavior == "inject";
                if case.agrees && route_speaks != hook_speaks {
                    failures.push(format!(
                        "{id}: agrees is true, but the hook is {} and route's allow {} -- \
                         mark it a disagreement of note content",
                        if hook_speaks { "injecting" } else { "silent" },
                        if route_speaks {
                            "carries a note"
                        } else {
                            "carries none"
                        }
                    ));
                }
                if let Some(want) = note_contains
                    && !note.contains(want.as_str())
                {
                    failures.push(format!(
                        "{id}: allow note must contain {want:?}, got {note:?}"
                    ));
                }
                if let Some(unwanted) = note_excludes
                    && note.contains(unwanted.as_str())
                {
                    failures.push(format!(
                        "{id}: allow note must not contain {unwanted:?}, got {note:?}"
                    ));
                }
            }
            (
                Decision::Deny(details),
                HookCase {
                    instead_contains: Some(want),
                    ..
                },
            ) if !details.instead().contains(want.as_str()) => {
                failures.push(format!(
                    "{id}: deny instead must contain {want:?}, got {:?}",
                    details.instead()
                ));
            }
            _ => {}
        }

        if let Some(want) = &case.facts {
            let expected = Facts {
                paths: want.paths.clone(),
                verb: want.verb.clone(),
                issue_numbers: want.issue_numbers.clone(),
                keywords: want.keywords.clone(),
            };
            if routed.facts != expected {
                failures.push(format!(
                    "{id}: expected facts {expected:?}, got {:?}",
                    routed.facts
                ));
            }
        }

        if !case.agrees {
            disagreements.push(format!(
                "{id}: hook {} / route {} -- {}",
                case.hook_behavior, actual, case.note
            ));
        }
    }

    // The PR body lists every disagreement for the operator; print them so a
    // `--nocapture` run hands the author the list verbatim.
    println!(
        "hook parity: {} rows, {} disagreements (route's Decision stands unless the operator rules otherwise):\n{}",
        cases.len(),
        disagreements.len(),
        disagreements.join("\n")
    );

    assert!(
        failures.is_empty(),
        "hook parity failures ({} of {} rows):\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

#[test]
fn a_call_missing_a_field_its_rule_reads_is_denied_not_a_panic() {
    // The issue's Error Handling: a tool_input missing a field its rule reads
    // yields the FR-CMD-016 deny for an unresolvable managed rule. Against the
    // shipped policy, a Read with no file_path and a Write with no file_path
    // each hit a rule that reads it.
    let policy = shipped_policy();
    for (tool, input) in [
        ("Read", serde_json::json!({})),
        ("Write", serde_json::json!({ "content": "x" })),
        (
            "Edit",
            serde_json::json!({ "old_string": "a", "new_string": "b" }),
        ),
        ("MultiEdit", serde_json::json!({ "edits": [] })),
    ] {
        let call = ToolCall {
            tool: tool.to_string(),
            input,
        };
        match route(&policy, &call, &Context::default()).decision {
            Decision::Deny(details) => assert!(
                details.reason().contains("'file_path'"),
                "{tool}: the deny must name the missing field, got {:?}",
                details.reason()
            ),
            other => panic!("{tool} without file_path must deny, got {other:?}"),
        }
    }
}

#[test]
fn a_field_predicate_policy_error_reports_its_json_pointer() {
    // Policy errors are reported by JSON pointer (the issue's Error Handling):
    // an arg predicate under a Fields tool names the exact predicate.
    let broken = r#"{"tools": {"Read": {"rules": [
        {"id": "r", "predicates": [{"kind": "arg-present", "arg": "-r"}], "outcome": {"kind": "allow"}}
    ]}}}"#;
    let err = parse_policy(broken).expect_err("an arg predicate under Read must not parse");
    assert_eq!(
        err,
        PolicyError::PredicateNotApplicable {
            pointer: "/tools/Read/rules/0/predicates/0/kind".to_string(),
            kind: "arg-present".to_string(),
            tool: "Read".to_string(),
        }
    );
    assert!(
        err.to_string()
            .starts_with("/tools/Read/rules/0/predicates/0/kind: ")
    );
}
