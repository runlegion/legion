//! The rules for tools other than Bash, over the shipped policy.
//!
//! `tests/fixtures/tool_cases.json` carries one row per case class of the
//! tool-field hooks the router replaced (#1233): what the hook did
//! (`hook_behavior`) beside what route does against the SHIPPED
//! `plugin/legion-cmd/policy.json` (`route`), and whether the agent sees the
//! same thing from both (`agrees`). #1337 left these rules unchanged; the
//! harness Grep and Glob rows left with their rules, which a separate issue
//! takes up. The Bash router is tested in `router.rs`.
//!
//! Row shape: `hook`, `case` (unique), `tool`, `input`, `hook_behavior`
//! (`deny` | `rewrite` | `allow` | `inject`), `route` (the expected Decision
//! arm), `agrees`, `note`, and optionally `context` (`{"recall": "found" |
//! "empty" | "not-fetched"}`, the same for `consult`), `target` (a rewrite's
//! target), `note_contains` / `note_excludes` (an allow's note), and
//! `instead_contains` (a deny's instead).

use std::collections::BTreeSet;

use legion_cmd::{Context, Decision, Lookup, PolicyError, ToolCall, ToolKind, parse_policy, route};
use serde::Deserialize;
use serde_json::Value;

const POLICY_JSON: &str = include_str!("../../../plugin/legion-cmd/policy.json");
const CASES_JSON: &str = include_str!("fixtures/tool_cases.json");

/// The tools the shipped policy carries rules for (#1337's Interface).
const RULED_TOOLS: [ToolKind; 8] = [
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
const DECISION_ARMS: [&str; 4] = ["allow", "rewrite", "deny", "ask"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCase {
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

impl ToolCase {
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

fn load_cases() -> Vec<ToolCase> {
    serde_json::from_str(CASES_JSON).unwrap_or_else(|e| panic!("tool_cases.json is not valid: {e}"))
}

fn shipped_policy() -> legion_cmd::Policy {
    parse_policy(POLICY_JSON)
        .unwrap_or_else(|e: PolicyError| panic!("the shipped policy.json must parse: {e}"))
}

fn arm_name(decision: &Decision) -> &'static str {
    match decision {
        Decision::Allow { .. } => "allow",
        Decision::Rewrite { .. } => "rewrite",
        Decision::Deny(_) => "deny",
        Decision::Ask(_) => "ask",
    }
}

#[test]
fn tool_cases_fixture_is_well_formed_and_honest() {
    let cases = load_cases();
    assert!(!cases.is_empty(), "tool_cases.json must carry rows");
    let mut seen = BTreeSet::new();
    let mut problems = Vec::new();
    for case in &cases {
        let id = case.id();
        if !seen.insert(case.case.as_str()) {
            problems.push(format!("{id}: duplicate case id"));
        }
        if !HOOK_BEHAVIORS.contains(&case.hook_behavior.as_str()) {
            problems.push(format!("{id}: unknown hook_behavior"));
        }
        if !DECISION_ARMS.contains(&case.route.as_str()) {
            problems.push(format!("{id}: unknown route arm"));
        }
        if ToolKind::parse(&case.tool).is_none() {
            problems.push(format!("{id}: {} is not a ruled tool", case.tool));
        }
        if case.agrees && case.hook_arm() != case.route {
            problems.push(format!("{id}: agrees is true but the arms differ"));
        }
        if !case.agrees && case.note.trim().is_empty() {
            problems.push(format!("{id}: a disagreement must carry a note"));
        }
    }
    assert!(
        problems.is_empty(),
        "malformed rows:\n{}",
        problems.join("\n")
    );
}

#[test]
fn every_ruled_tool_has_rules_and_a_row() {
    let policy = shipped_policy();
    let cases = load_cases();
    for kind in RULED_TOOLS {
        assert!(
            policy
                .tools
                .get(&kind)
                .is_some_and(|rules| !rules.is_empty()),
            "the shipped policy must carry rules for {}",
            kind.as_str()
        );
        assert!(
            cases.iter().any(|c| c.tool == kind.as_str()),
            "no tool_cases.json row exercises {}",
            kind.as_str()
        );
    }
    // The harness Grep and Glob rules left with the sym jobs (#1337).
    assert!(!policy.tools.contains_key(&ToolKind::Grep));
    assert!(!policy.tools.contains_key(&ToolKind::Glob));
}

#[test]
fn every_tool_case_matches_routes_actual_decision() {
    let policy = shipped_policy();
    let mut failures: Vec<String> = Vec::new();
    for case in &load_cases() {
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
        match &routed.decision {
            Decision::Rewrite { target, .. } => {
                if let Some(want) = &case.target
                    && target.as_str() != want
                {
                    failures.push(format!("{id}: expected target {want:?}, got {target:?}"));
                }
            }
            Decision::Allow { note } => {
                let note = note.as_deref().unwrap_or("");
                if let Some(want) = &case.note_contains
                    && !note.contains(want.as_str())
                {
                    failures.push(format!("{id}: note must contain {want:?}, got {note:?}"));
                }
                if let Some(unwanted) = &case.note_excludes
                    && note.contains(unwanted.as_str())
                {
                    failures.push(format!("{id}: note must not contain {unwanted:?}"));
                }
            }
            Decision::Deny(details) => {
                if let Some(want) = &case.instead_contains
                    && !details.instead().contains(want.as_str())
                {
                    failures.push(format!(
                        "{id}: instead must contain {want:?}, got {:?}",
                        details.instead()
                    ));
                }
            }
            Decision::Ask(_) => {}
        }
    }
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

#[test]
fn a_call_missing_a_field_its_rule_reads_is_denied_not_a_panic() {
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
fn a_tool_rule_policy_error_reports_its_json_pointer() {
    let broken = r#"{"tools": {"Read": {"rules": [
        {"id": "r", "predicates": [{"kind": "arg-present", "arg": "-r"}], "outcome": {"kind": "allow"}}
    ]}}}"#;
    let err = parse_policy(broken).expect_err("an arg predicate is gone");
    assert_eq!(
        err,
        PolicyError::UnknownPredicateKind {
            pointer: "/tools/Read/rules/0/predicates/0/kind".to_string(),
            kind: "arg-present".to_string(),
        }
    );
}
