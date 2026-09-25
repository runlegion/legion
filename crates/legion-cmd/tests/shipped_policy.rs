//! Two concerns, kept apart so NFR-CMD-001 holds:
//!
//! 1. The shipped artifact (`plugin/legion-cmd/policy.json`) parses, is
//!    non-empty, and declares the names the splitter refuses to hold. This test
//!    reads the file but never calls `route` -- NFR-CMD-001's "no test of route
//!    requires a filesystem" is about route, and this validates only the
//!    artifact.
//! 2. route reaches every arm over a policy that mirrors the shipped rules,
//!    built from an inline JSON literal (the Behavior section's "tests build
//!    policies from inline JSON strings"), so no test of route touches disk.

use std::fs;

use legion_cmd::{Context, Decision, Policy, ProxyReason, ToolCall, parse_policy, route};

fn shipped_policy_text() -> String {
    let path = format!(
        "{}/../../plugin/legion-cmd/policy.json",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).expect("shipped policy is readable")
}

/// Mirrors the shipped rules, inline, so the route assertions below never touch
/// the filesystem. Kept structurally in step with `plugin/legion-cmd/policy.json`.
fn mirror_policy() -> Policy {
    parse_policy(
        r#"{
        "sym_jobs": [
            {"id": "find-content", "sym_command": "legion sym etc find-content",
             "interpreter_patterns": ["rglob", "read_text"]}
        ],
        "wrappers": [
            {"binary": "env",
             "flags": ["-", "-i", "--ignore-environment", "-0", "--null", "-v", "--debug"],
             "value_options": ["-u", "--unset", "-C", "--chdir", "-P"]},
            {"binary": "npx",
             "flags": ["-y", "--yes", "--no", "--workspaces", "--include-workspace-root"],
             "value_options": ["-p", "--package", "-w", "--workspace"]},
            {"binary": "pnpm", "required_subcommand": "exec",
             "flags": ["-r", "--recursive", "--parallel", "--report-summary"],
             "value_options": ["--resume-from"]}
        ],
        "interpreters": [
            {"binary": "sh", "flag": "-c", "body": "shell"},
            {"binary": "python3", "flag": "-c", "body": "foreign"}
        ],
        "script_carriers": [{"binary": "bash"}],
        "tools": {"Bash": {"families": {
            "grep": {"rules": [{"id": "grep-to-sym", "outcome": {"kind": "sym", "job": "find-content"}}]},
            "rm": {"rules": [
                {"id": "rm-recursive-force", "predicates": [{"kind": "arg-present", "arg": "-rf"}],
                 "outcome": {"kind": "deny", "reason": "unrecoverable", "instead": "trash it"}},
                {"id": "rm-other", "outcome": {"kind": "allow"}}
            ]},
            "curl": {"rules": [{"id": "curl-network", "outcome": {"kind": "ask", "question": "fetch?", "reason": "network", "needs_operator": true}}]},
            "xxd": {"rules": [{"id": "xxd-verbatim", "outcome": {"kind": "proxy", "reason": "binary"}}]}
        }}}
    }"#,
    )
    .expect("mirror policy parses")
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

// -- artifact validation (touches disk, never calls route) --------------------

#[test]
fn shipped_policy_parses_is_non_empty_and_declares_its_names() {
    let policy = parse_policy(&shipped_policy_text()).expect("shipped policy parses");
    assert!(!policy.is_empty());
    // The names the splitter refuses to hold live here as data.
    assert!(policy.matching_wrapper("env", &[]).is_some());
    assert!(
        policy
            .matching_wrapper("pnpm", &["exec".to_string(), "eslint".to_string()])
            .is_some()
    );
    assert!(policy.matching_interpreter("sh").is_some());
    assert!(policy.matching_interpreter("python3").is_some());
    assert!(policy.matching_script_carrier("bash").is_some());
    assert!(policy.sym_job("find-content").is_some());
}

/// The shipped wrapper declarations consume each wrapper's own words up to
/// the wrapped command (#1286). Checked against the artifact itself, so a
/// declaration with the wrong arity -- a value option listed as a flag, a
/// missing operand -- fails here rather than shipping green. This reads the
/// file but never calls route.
#[test]
fn shipped_wrapper_declarations_reach_the_wrapped_command() {
    let policy = parse_policy(&shipped_policy_text()).expect("shipped policy parses");
    for (command, wrapped) in [
        ("timeout 5 chmod -R 777 /", "chmod"),
        ("timeout -s KILL 5 mkfs.ext4 /dev/sda1", "mkfs.ext4"),
        ("stdbuf -oL mkfs.ext4 /dev/sda1", "mkfs.ext4"),
        ("xargs -I{} mkfs.ext4 {}", "mkfs.ext4"),
        ("sudo -u root mkfs.ext4 /dev/sda1", "mkfs.ext4"),
        ("sudo -u root git push", "git"),
        ("nice -n 10 make", "make"),
        ("nohup make", "make"),
        ("env -i FOO=1 make", "FOO=1"),
        ("npx -y eslint .", "eslint"),
        ("pnpm exec -r eslint .", "eslint"),
    ] {
        let mut words = command.split_whitespace().map(str::to_string);
        let binary = words.next().expect("a wrapper word");
        let args: Vec<String> = words.collect();
        let wrapper = policy
            .matching_wrapper(&binary, &args)
            .unwrap_or_else(|| panic!("`{binary}` is a shipped wrapper"));
        let start = wrapper
            .payload_start(&args)
            .unwrap_or_else(|| panic!("`{command}`: the declaration consumes its own words"));
        assert_eq!(
            args.get(start).map(String::as_str),
            Some(wrapped),
            "`{command}` must reach `{wrapped}`"
        );
    }
}

// -- route behavior (inline policy, never touches disk) -----------------------

#[test]
fn mirror_policy_routes_every_arm() {
    let policy = mirror_policy();

    // allow default: no managed binary.
    assert_eq!(
        decide(&policy, "echo hello"),
        Decision::Allow { note: None }
    );
    // allow within a family: rm without the -rf form.
    assert_eq!(
        decide(&policy, "rm notes.txt"),
        Decision::Allow { note: None }
    );

    // sym: grep routes to the find-content sym command.
    match decide(&policy, "grep -rn foo src") {
        Decision::Deny(d) => assert_eq!(d.instead(), "legion sym etc find-content"),
        other => panic!("grep should route to sym, got {other:?}"),
    }

    // deny: a combined recursive force delete.
    assert!(matches!(decide(&policy, "rm -rf build"), Decision::Deny(_)));
    // ask: a network fetch.
    assert!(matches!(
        decide(&policy, "curl example.com"),
        Decision::Ask(_)
    ));
    // proxy: binary output stays verbatim.
    assert_eq!(
        decide(&policy, "xxd payload.bin"),
        Decision::Proxy {
            reason: ProxyReason::Binary
        }
    );
}

#[test]
fn mirror_policy_re_enters_a_js_runner_and_a_shell_interpreter() {
    let policy = mirror_policy();
    for command in ["npx grep -rn foo .", "sh -c 'grep -rn foo src'"] {
        match decide(&policy, command) {
            Decision::Deny(d) => assert_eq!(d.instead(), "legion sym etc find-content"),
            other => panic!("`{command}` should route to sym, got {other:?}"),
        }
    }
}

#[test]
fn mirror_policy_routes_a_python_search_one_liner_to_sym() {
    let policy = mirror_policy();
    let command = "python3 -c \"import pathlib; [print(f) for f in pathlib.Path('.').rglob('*.rs') if 'foo' in f.read_text()]\"";
    match decide(&policy, command) {
        Decision::Deny(d) => assert_eq!(d.instead(), "legion sym etc find-content"),
        other => panic!("python search should route to sym, got {other:?}"),
    }
}

#[test]
fn mirror_policy_proxies_an_opaque_script_file() {
    assert_eq!(
        decide(&mirror_policy(), "bash deploy.sh"),
        Decision::Proxy {
            reason: ProxyReason::Opaque
        }
    );
}
