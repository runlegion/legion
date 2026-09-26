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
             "flags": ["-y", "--yes", "--workspaces", "--include-workspace-root"],
             "value_options": ["-p", "--package", "-w", "--workspace"]},
            {"binary": "pnpm", "required_subcommand": "exec",
             "flags": ["-r", "--recursive", "--parallel", "--report-summary", "-w", "--workspace-root"],
             "value_options": ["--resume-from", "-C", "--dir", "-F", "--filter"]},
            {"binary": "npm", "required_subcommand": "x",
             "flags": ["-y", "--yes", "--workspaces", "--include-workspace-root"],
             "value_options": ["-p", "--package", "-w", "--workspace", "--prefix"]},
            {"binary": "bun", "required_subcommand": "x",
             "flags": ["--bun", "--silent", "--verbose", "--no-install"],
             "value_options": ["-p", "--package"]},
            {"binary": "yarn", "required_subcommand": "exec",
             "flags": ["--silent", "--verbose"],
             "value_options": ["--cwd"],
             "selectors": ["workspace", "workspaces"]}
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
    // The shipped file declares sudo as a wrapper for every routing
    // decision; the no-go check also holds its own copy of every wrapper and
    // interpreter declared here, so wrapper variants of a no-go entry hold
    // without the file (FR-CMD-025).
    assert!(policy.matching_wrapper("sudo", &[]).is_some());
    // The shipped file adds no no-go entries; the built-ins apply on top.
    assert!(policy.no_go.is_empty());
    assert_eq!(
        policy.no_go_entries().len(),
        legion_cmd::builtin_no_go().len()
    );
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
        ("pnpm -r exec grep foo .", "grep"),
        ("pnpm -C web --filter app exec eslint .", "eslint"),
        ("pnpm dlx -s cowsay hi", "cowsay"),
        ("pnpx -s cowsay hi", "cowsay"),
        ("pnpm dlx --reporter silent cowsay hi", "cowsay"),
        ("pnpx --reporter=silent cowsay hi", "cowsay"),
        ("npm --prefix x exec --package=eslint -- eslint .", "eslint"),
        ("yarn --cwd web exec eslint .", "eslint"),
        ("yarn dlx -q cowsay hi", "cowsay"),
        ("bunx --silent --no-install cowsay hi", "cowsay"),
        // A subcommand alias is its own entry (#1293).
        ("npm x grep foo .", "grep"),
        ("npm x -w web -- grep foo .", "grep"),
        ("bun x grep foo .", "grep"),
        ("bun --bun x grep foo .", "grep"),
        // A workspace selector is the runner's own words (#1293).
        ("yarn workspace web exec grep foo .", "grep"),
        ("yarn workspaces foreach exec grep foo .", "grep"),
        ("yarn --cwd packages workspace web exec grep foo .", "grep"),
        ("yarn workspace web dlx -q cowsay hi", "cowsay"),
    ] {
        let (args, start) = shipped_payload_start(&policy, command);
        let start =
            start.unwrap_or_else(|| panic!("`{command}`: the declaration consumes its own words"));
        assert_eq!(
            args.get(start).map(String::as_str),
            Some(wrapped),
            "`{command}` must reach `{wrapped}`"
        );
    }

    // Shapes the declarations do not model are claimed by the wrapper and
    // refused, so route proxies them opaque -- never the allow default.
    // npm's `--no` is deliberately undeclared: its arity differs across npm
    // versions and reports, and leaving it out is safe either way. A `--`
    // before a runner's subcommand is refused the same way.
    for command in [
        "npx --no cowsay hi",
        "npm exec --no cowsay hi",
        "env -S 'make'",
        "pnpm -- exec grep foo .",
        "pnpm -r -- exec grep foo .",
        "npm -- exec grep foo .",
        "yarn -- exec grep foo .",
        // `workspaces foreach`'s own options are undeclared (#1293).
        "yarn workspaces foreach -A exec grep foo .",
    ] {
        assert_eq!(
            shipped_payload_start(&policy, command).1,
            None,
            "`{command}`"
        );
    }

    // A script run through a selector names no runner subcommand, so no
    // wrapper claims it: an ordinary invocation, like bare `yarn <script>`.
    for command in ["yarn workspace web grep", "yarn workspace web build"] {
        let args: Vec<String> = command
            .split_whitespace()
            .skip(1)
            .map(str::to_string)
            .collect();
        assert!(
            policy.matching_wrapper("yarn", &args).is_none(),
            "`{command}` stays ordinary"
        );
    }
}

/// Splits `command` on whitespace, finds the shipped wrapper its first word
/// names, and returns its arguments with where that wrapper's payload starts.
fn shipped_payload_start(policy: &Policy, command: &str) -> (Vec<String>, Option<usize>) {
    let mut words = command.split_whitespace().map(str::to_string);
    let binary = words.next().expect("a wrapper word");
    let args: Vec<String> = words.collect();
    let wrapper = policy
        .matching_wrapper(&binary, &args)
        .unwrap_or_else(|| panic!("`{command}`: `{binary}` is a shipped wrapper"));
    let start = wrapper.payload_start(&args);
    (args, start)
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

/// A runner reached through a subcommand alias or a workspace selector routes
/// like its declared form (#1293); an undeclared shape proxies opaque, and a
/// script run through a selector stays ordinary.
#[test]
fn mirror_policy_routes_runner_aliases_and_workspace_selectors() {
    let policy = mirror_policy();
    for command in [
        "npm x grep foo .",
        "bun x grep foo .",
        "yarn workspace web exec grep foo .",
        "yarn workspaces foreach exec grep foo .",
    ] {
        match decide(&policy, command) {
            Decision::Deny(d) => assert_eq!(d.instead(), "legion sym etc find-content"),
            other => panic!("`{command}` should route as grep, got {other:?}"),
        }
    }
    assert_eq!(
        decide(&policy, "yarn workspaces foreach -A exec grep foo ."),
        Decision::Proxy {
            reason: ProxyReason::Opaque
        }
    );
    assert_eq!(
        decide(&policy, "yarn workspace web grep"),
        Decision::Allow { note: None }
    );
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
