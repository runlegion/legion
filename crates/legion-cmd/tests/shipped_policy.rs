//! Exercises the shipped default policy (`plugin/legion-cmd/policy.json`), not
//! just inline test policies: the artifact adapters load must parse, be
//! non-empty, and route real commands to the arms it declares. It holds only
//! enough rules to exercise the arms; the hook-parity issue fills it.

use std::fs;

use legion_cmd::{Context, Decision, Policy, ProxyReason, ToolCall, parse_policy, route};

fn shipped_policy() -> Policy {
    let path = format!(
        "{}/../../plugin/legion-cmd/policy.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = fs::read_to_string(&path).expect("shipped policy is readable");
    parse_policy(&text).expect("shipped policy parses")
}

fn bash(command: &str) -> ToolCall {
    ToolCall {
        tool: "Bash".to_string(),
        input: serde_json::json!({ "command": command }),
    }
}

fn decide(policy: &Policy, command: &str) -> Decision {
    route(policy, &bash(command), &Context::default()).decision
}

#[test]
fn shipped_policy_parses_and_is_not_empty() {
    assert!(!shipped_policy().is_empty());
}

#[test]
fn shipped_policy_routes_the_arms_it_declares() {
    let policy = shipped_policy();

    // allow default: no managed binary.
    assert_eq!(
        decide(&policy, "echo hello"),
        Decision::Allow { note: None }
    );

    // sym: grep routes to the find-content sym command.
    match decide(&policy, "grep -rn foo src") {
        Decision::Deny(details) => assert_eq!(details.instead(), "legion sym etc find-content"),
        other => panic!("grep should route to sym, got {other:?}"),
    }

    // ask: a bare git push.
    assert!(matches!(
        decide(&policy, "git push origin main"),
        Decision::Ask(_)
    ));

    // deny: a force push.
    assert!(matches!(
        decide(&policy, "git push --force origin main"),
        Decision::Deny(_)
    ));

    // proxy: git show must stay verbatim.
    assert_eq!(
        decide(&policy, "git show HEAD"),
        Decision::Proxy {
            reason: ProxyReason::FullPatch
        }
    );
}

#[test]
fn shipped_policy_re_enters_a_js_runner_and_a_shell_interpreter() {
    let policy = shipped_policy();
    // npx grep -> grep re-entered -> sym.
    match decide(&policy, "npx grep -rn foo .") {
        Decision::Deny(details) => assert_eq!(details.instead(), "legion sym etc find-content"),
        other => panic!("npx grep should route to sym, got {other:?}"),
    }
    // sh -c 'grep ...' -> grep re-entered -> sym.
    match decide(&policy, "sh -c 'grep -rn foo src'") {
        Decision::Deny(details) => assert_eq!(details.instead(), "legion sym etc find-content"),
        other => panic!("sh -c grep should route to sym, got {other:?}"),
    }
}

#[test]
fn shipped_policy_routes_a_python_search_one_liner_to_sym() {
    let policy = shipped_policy();
    let command = "python3 -c \"import pathlib; [print(f) for f in pathlib.Path('.').rglob('*.rs') if 'foo' in f.read_text()]\"";
    match decide(&policy, command) {
        Decision::Deny(details) => assert_eq!(details.instead(), "legion sym etc find-content"),
        other => panic!("python search should route to sym, got {other:?}"),
    }
}

#[test]
fn shipped_policy_proxies_an_opaque_script_file() {
    let policy = shipped_policy();
    assert_eq!(
        decide(&policy, "bash deploy.sh"),
        Decision::Proxy {
            reason: ProxyReason::Opaque
        }
    );
}
