//! Three concerns, kept apart so NFR-CMD-001 holds:
//!
//! 1. The shipped artifact (`plugin/legion-cmd/policy.json`) parses, is
//!    non-empty, and declares the names the splitter refuses to hold. These
//!    tests read the file at run time but never call `route` -- NFR-CMD-001's "no test of route
//!    requires a filesystem" is about route, and this validates only the
//!    artifact.
//! 2. route reaches every arm over a policy that mirrors the shipped rules,
//!    built from an inline JSON literal (the Behavior section's "tests build
//!    policies from inline JSON strings"), so no test of route touches disk.
//! 3. The shipped git global-option declaration routes a git command by its
//!    subcommand's family (#1294). That issue asks for these tests "with the
//!    shipped policy", so this one section routes over the artifact, compiled
//!    in with `include_str!` as `hook_parity.rs` does, so no test of route
//!    opens a file at run time (NFR-CMD-001).

use std::fs;

use legion_cmd::{
    AliasReading, Context, Deciding, Decision, Policy, ProxyReason, Routed, ToolCall, parse_policy,
    route, scan,
};

/// The shipped artifact, embedded at compile time so the route tests over it
/// need no filesystem (NFR-CMD-001).
const SHIPPED_POLICY_JSON: &str = include_str!("../../../plugin/legion-cmd/policy.json");

/// Reads the shipped artifact at run time. Only the artifact-validation tests
/// use it; none of them calls route.
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
        "global_options": [
            {"binary": "git",
             "flags": ["-p", "--paginate", "-P", "--no-pager", "--bare"],
             "value_options": ["-C", "-c", "--config-env", "--git-dir", "--work-tree"],
             "inline_alias": {"options": ["-c"], "prefix": "alias."}}
        ],
        "tools": {"Bash": {"families": {
            "git push": {"rules": [
                {"id": "git-push-to-legion", "outcome": {"kind": "rewrite", "target": "legion push",
                 "reason": "the audited push path",
                 "translatable": {"flags": ["-u", "--set-upstream", "-q", "--quiet", "-v", "--verbose", "--progress"]}}}
            ]},
            "git commit": {"rules": [
                {"id": "git-commit-without-message", "predicates": [
                    {"kind": "arg-absent", "arg": "-m"}, {"kind": "arg-absent", "arg": "--message"},
                    {"kind": "arg-absent", "arg": "-F"}, {"kind": "arg-absent", "arg": "--file"}],
                 "outcome": {"kind": "deny", "reason": "opens an editor", "instead": "legion commit"}},
                {"id": "git-commit-to-legion", "outcome": {"kind": "rewrite", "target": "legion commit",
                 "reason": "the audited commit path",
                 "translatable": {"valued_flags": ["-m", "--message", "-F", "--file"]}}}
            ]},
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
    // decision; the no-go check also resolves every wrapper and interpreter
    // declared here from this file embedded in the binary, so wrapper
    // variants of a no-go entry hold without the file (FR-CMD-025).
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

// -- git global options over the compiled-in shipped artifact (#1294) --------

fn shipped_route(policy: &Policy, command: &str) -> Routed {
    route(policy, &bash(command), &Context::default())
}

#[test]
fn shipped_git_global_options_route_by_the_subcommand_family() {
    let policy = parse_policy(SHIPPED_POLICY_JSON).expect("shipped policy parses");
    for (command, rule, verb, option) in [
        ("git -C /tmp push", "git-push-to-legion", "push", "-C"),
        (
            "git -c user.name=x commit -m y",
            "git-commit-to-legion",
            "commit",
            "-c",
        ),
        (
            "git --git-dir=.git push",
            "git-push-to-legion",
            "push",
            "--git-dir=.git",
        ),
    ] {
        let routed = shipped_route(&policy, command);
        // The family's own rule decides, as it does for the bare subcommand.
        assert_eq!(
            routed.deciding,
            Deciding::Rule {
                id: rule.to_string(),
                needs_operator: false
            },
            "`{command}`"
        );
        assert_eq!(routed.facts.verb.as_deref(), Some(verb), "`{command}`");
        // That rule rewrites only when every argument translates; the global
        // option has no lossless translation, so it is refused by name
        // rather than dropped (FR-CMD-008).
        match &routed.decision {
            Decision::Deny(details) => assert!(
                details.reason().contains(&format!("`{option}`")),
                "`{command}`: {}",
                details.reason()
            ),
            other => panic!("`{command}` should be denied naming `{option}`, got {other:?}"),
        }
    }
}

#[test]
fn shipped_git_undeclared_global_option_is_proxied_opaque() {
    let policy = parse_policy(SHIPPED_POLICY_JSON).expect("shipped policy parses");
    for command in [
        "git --bogus push",
        "git --bogus status",
        "git -C /tmp -Z commit -m y",
    ] {
        let routed = shipped_route(&policy, command);
        assert_eq!(
            routed.decision,
            Decision::Proxy {
                reason: ProxyReason::Opaque
            },
            "`{command}`"
        );
        assert_eq!(routed.deciding, Deciding::Default, "`{command}`");
    }
    // Declared global options before an unmanaged subcommand still reach the
    // allow default: the declaration changes which word is the subcommand,
    // not what an unmanaged one earns.
    for command in [
        "git -C /tmp status",
        "git --no-pager log --oneline",
        "git -C push status",
        "git --version",
    ] {
        assert_eq!(
            shipped_route(&policy, command).decision,
            Decision::Allow { note: None },
            "`{command}`"
        );
    }
}

// -- inline git aliases (#1298) ------------------------------------------------

/// The shipped policy's reading of the subcommand word of the first command
/// in `command`, parsed by the splitter so quoting is kept as route sees it.
/// Reads the compiled-in artifact and never calls route (NFR-CMD-001).
fn shipped_alias_reading(command: &str) -> AliasReading {
    let policy = parse_policy(SHIPPED_POLICY_JSON).expect("shipped policy parses");
    let parsed = scan(command).expect("command parses");
    let invocation = &parsed.invocations[0];
    let start = policy
        .subcommand_start(&invocation.binary, &invocation.args)
        .unwrap_or_else(|| panic!("`{command}`: the global options are declared"));
    policy.inline_alias_reading(&invocation.binary, &invocation.args, start)
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

#[test]
fn shipped_git_inline_alias_reads_the_alias_value_in_place_of_the_subcommand() {
    for (command, args, start) in [
        (
            "git -c alias.p=push p origin main",
            "-c alias.p=push push origin main",
            2,
        ),
        (
            "git -c alias.c=commit c -m x",
            "-c alias.c=commit commit -m x",
            2,
        ),
        ("git -c alias.s=status s", "-c alias.s=status status", 2),
        // The last definition of a name wins, as in git.
        (
            "git -c alias.p=status -c alias.p=push p",
            "-c alias.p=status -c alias.p=push push",
            4,
        ),
        // Git compares config keys and alias names without regard to case.
        ("git -c ALIAS.P=push p", "-c ALIAS.P=push push", 2),
    ] {
        assert_eq!(
            shipped_alias_reading(command),
            AliasReading::Expanded {
                args: words(args),
                start
            },
            "`{command}`"
        );
    }
    // An alias value of several words, including a global option it brings
    // in: the subcommand is read after that option.
    let mut args: Vec<String> = vec!["-c".to_string(), "'alias.p=-p push -u'".to_string()];
    args.extend(words("-p push -u origin"));
    assert_eq!(
        shipped_alias_reading("git -c 'alias.p=-p push -u' p origin"),
        AliasReading::Expanded { args, start: 3 }
    );
}

#[test]
fn shipped_git_inline_alias_route_cannot_read_is_opaque() {
    for command in [
        // A shell alias.
        "git -c 'alias.p=!git push' p",
        // A value set from the environment, which route cannot see; last wins.
        "git --config-env=alias.p=X p",
        "git -c alias.p=push --config-env=alias.p=X p",
        // An empty value, or none at all.
        "git -c alias.p= p",
        "git -c alias.p p",
        // An alias naming another inline alias.
        "git -c alias.p=q -c alias.q=push p",
        // A value the shell or git would still transform.
        "git -c alias.p=$X p",
        "git -c 'alias.p=pu\"sh\"' p",
        // An option the alias brings in that the declaration does not name.
        "git -c 'alias.p=--bogus push' p",
    ] {
        assert_eq!(
            shipped_alias_reading(command),
            AliasReading::Opaque,
            "`{command}`"
        );
    }
    for command in [
        "git -c alias.p=push status",
        "git -c user.name=x p",
        "git status",
    ] {
        assert_eq!(
            shipped_alias_reading(command),
            AliasReading::NotAlias,
            "`{command}`"
        );
    }
}

#[test]
fn mirror_policy_routes_an_inline_git_alias_by_what_it_runs() {
    let policy = mirror_policy();
    for (command, rule, verb) in [
        (
            "git -c alias.p=push p origin main",
            "git-push-to-legion",
            "push",
        ),
        (
            "git -c alias.c=commit c -m x",
            "git-commit-to-legion",
            "commit",
        ),
        (
            "git -c alias.p=status -c alias.p=push p",
            "git-push-to-legion",
            "push",
        ),
        // Git runs a builtin, never an alias of the same name, and route
        // cannot tell which names are builtins: the stricter reading holds.
        (
            "git -c alias.push=status push",
            "git-push-to-legion",
            "push",
        ),
    ] {
        let routed = route(&policy, &bash(command), &Context::default());
        assert_eq!(
            routed.deciding,
            Deciding::Rule {
                id: rule.to_string(),
                needs_operator: false
            },
            "`{command}`"
        );
        assert_eq!(routed.facts.verb.as_deref(), Some(verb), "`{command}`");
        // The rule rewrites only when every argument translates; `-c` does
        // not, so it is refused by name (FR-CMD-008).
        match &routed.decision {
            Decision::Deny(details) => {
                assert!(details.reason().contains("`-c`"), "`{command}`")
            }
            other => panic!("`{command}` should be denied naming `-c`, got {other:?}"),
        }
    }
    for command in ["git -c alias.s=status s", "git -c alias.p=push status"] {
        assert_eq!(
            decide(&policy, command),
            Decision::Allow { note: None },
            "`{command}`"
        );
    }
    for command in [
        "git -c 'alias.p=!git push' p",
        "git --config-env=alias.p=X p",
    ] {
        let routed = route(&policy, &bash(command), &Context::default());
        assert_eq!(
            routed.decision,
            Decision::Proxy {
                reason: ProxyReason::Opaque
            },
            "`{command}`"
        );
        assert_eq!(routed.deciding, Deciding::Default, "`{command}`");
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
