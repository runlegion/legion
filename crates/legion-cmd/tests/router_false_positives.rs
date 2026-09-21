//! Picks up the two false-positive rows moved out of the splitter's battery
//! for #1227 (`tests/fixtures/router-candidate-false-positives.json`) and
//! asserts route's Decision on each. Both turn on what a resolved command
//! MEANS -- a bare `pnpm <name>` is not a package runner, and a `git push`
//! rule must not fire on `git stash push` -- which is the router's judgment,
//! not the splitter's (FR-CMD-007, FR-CMD-011).
//!
//! The policy below deliberately carries a `git push` family and a `grep` sym
//! rule, so that a wrong reading would be visible: `git stash push` would deny
//! if the family fired on the wrong subcommand, and `pnpm grep` would route to
//! sym if `pnpm` were read as a runner. Both must be Allow (benign).

use std::fs;

use legion_cmd::{Context, Decision, ToolCall, parse_policy, route};
use serde_json::Value;

fn discriminating_policy() -> legion_cmd::Policy {
    parse_policy(
        r#"{
            "sym_jobs": [
                {"id": "find-content", "sym_command": "legion sym find-content",
                 "interpreter_patterns": ["rglob"]}
            ],
            "wrappers": [
                {"binary": "pnpm", "required_subcommand": "exec"},
                {"binary": "pnpm", "required_subcommand": "dlx"}
            ],
            "tools": {"Bash": {"families": {
                "git push": {"rules": [
                    {"id": "git-push", "outcome": {"kind": "deny",
                     "reason": "force-push guard", "instead": "git push --force-with-lease"}}
                ]},
                "grep": {"rules": [
                    {"id": "grep-sym", "outcome": {"kind": "sym", "job": "find-content"}}
                ]}
            }}}
        }"#,
    )
    .expect("valid discriminating policy")
}

fn bash(command: &str) -> ToolCall {
    ToolCall {
        tool: "Bash".to_string(),
        input: serde_json::json!({ "command": command }),
    }
}

#[test]
fn both_router_candidate_false_positives_route_benign() {
    let text = fs::read_to_string("tests/fixtures/router-candidate-false-positives.json")
        .expect("fixture file is readable");
    let rows: Vec<Value> = serde_json::from_str(&text).expect("fixture is valid JSON");
    assert_eq!(
        rows.len(),
        2,
        "the fixture carries exactly the two moved rows"
    );

    let policy = discriminating_policy();
    for row in rows {
        let id = row["id"].as_str().expect("row id");
        let cmd = row["cmd"].as_str().expect("row cmd");
        assert_eq!(
            row["expected"].as_str(),
            Some("benign"),
            "{id} is a benign row"
        );
        let routed = route(&policy, &bash(cmd), &Context::default());
        assert_eq!(
            routed.decision,
            Decision::Allow { note: None },
            "{id} ({cmd:?}) must route benign (Allow), not act on an absent command"
        );
    }
}
