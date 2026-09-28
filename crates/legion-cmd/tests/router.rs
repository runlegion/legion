//! The Bash router over the shipped policy (#1337), one group per
//! acceptance criterion: proxy insertion at each position, what insertion
//! leaves alone, the misplacement refusal, the never-run list at every
//! position, the ask flow for curl and the power switches, and the untouched
//! default. The policy is compiled in with `include_str!`, so no test here
//! opens a file at run time (NFR-CMD-001).

use legion_cmd::{
    Context, Deciding, Decision, NO_GO_INSTEAD, PROXY_ID, Policy, Routed, ToolCall, command_key,
    insert_at, parse_policy, plan, route, verify,
};

const SHIPPED_POLICY_JSON: &str = include_str!("../../../plugin/legion-cmd/policy.json");

fn policy() -> Policy {
    parse_policy(SHIPPED_POLICY_JSON).expect("the shipped policy parses")
}

fn bash(command: &str) -> ToolCall {
    ToolCall {
        tool: "Bash".to_string(),
        input: serde_json::json!({ "command": command }),
    }
}

fn decide(command: &str) -> Routed {
    route(&policy(), &bash(command), &Context::default())
}

fn rewritten(command: &str) -> Option<String> {
    let routed = decide(command);
    match routed.decision {
        Decision::Rewrite { .. } => routed.facts.rewritten,
        _ => None,
    }
}

// -- 1. legion before git, gh, grep and rg at every position ----------------

#[test]
fn proxied_names_get_legion_at_every_position() {
    for (typed, expected) in [
        ("git status", "legion git status"),
        ("gh issue view 5", "legion gh issue view 5"),
        ("grep -rn foo src", "legion grep -rn foo src"),
        ("rg foo", "legion rg foo"),
        ("ls | grep x | rg y", "ls | legion grep x | legion rg y"),
        (
            "cd a && git add . && git commit -m x",
            "cd a && legion git add . && legion git commit -m x",
        ),
        (
            "false || gh pr view; rg z",
            "false || legion gh pr view; legion rg z",
        ),
        ("(git status)", "(legion git status)"),
        ("x=$(git rev-parse HEAD)", "x=$(legion git rev-parse HEAD)"),
        ("echo `git log -1`", "echo `legion git log -1`"),
        (
            "for f in a b; do grep x $f; done",
            "for f in a b; do legion grep x $f; done",
        ),
        (
            "until git fetch; do sleep 1; done",
            "until legion git fetch; do sleep 1; done",
        ),
        (
            "if rg -q x f; then gh pr view; fi",
            "if legion rg -q x f; then legion gh pr view; fi",
        ),
        (
            "case $1 in a) git a;; esac",
            "case $1 in a) legion git a;; esac",
        ),
        ("f() { grep x f; }; f", "f() { legion grep x f; }; f"),
        ("FOO=1 git status", "FOO=1 legion git status"),
        ("GIT_PAGER=cat git log", "GIT_PAGER=cat legion git log"),
        (
            "git a; git b; gh c; grep d; rg e",
            "legion git a; legion git b; legion gh c; legion grep d; legion rg e",
        ),
        (
            "echo a && \\\ngit status",
            "echo a && \\\nlegion git status",
        ),
        (
            "git log \\\n  --oneline -3 && \\\n  rg x",
            "legion git log \\\n  --oneline -3 && \\\n  legion rg x",
        ),
        // The two moved false positives: `git stash push` is git's own verb,
        // and `pnpm grep` runs pnpm, not grep.
        (
            "git stash push -- src/a.rs\ngit status --short",
            "legion git stash push -- src/a.rs\nlegion git status --short",
        ),
    ] {
        assert_eq!(rewritten(typed).as_deref(), Some(expected), "{typed:?}");
    }
}

// -- 2. what insertion leaves alone ------------------------------------------

#[test]
fn paths_arguments_legion_forms_quotes_heredocs_and_bodies_are_not_changed() {
    for typed in [
        "/usr/bin/git status",
        "./rg foo",
        "xargs grep foo",
        "find . -name x -exec grep -l y {} +",
        "echo git",
        "pnpm grep",
        "legion git status",
        "legion gh pr view --repo legion --number 5",
        "echo 'git status'",
        "echo \"rg foo\"",
        "cat <<EOF\ngit status\nEOF",
        "sh -c 'git status'",
        "python3 -c \"import subprocess; subprocess.run(['git', 'status'])\"",
        "node -e \"require('child_process').execSync('grep x')\"",
    ] {
        let routed = decide(typed);
        assert_eq!(routed.decision, Decision::Allow { note: None }, "{typed:?}");
        assert_eq!(routed.facts.rewritten, None, "{typed:?}");
    }
}

// -- 3. a misplaced insertion never runs --------------------------------------

#[test]
fn an_insertion_whose_reparse_differs_is_refused() {
    let proxy = policy().proxy;
    for typed in [
        "git status",
        "cd a && git log",
        "echo a && \\\ngit status",
        "x=$(rg foo)",
    ] {
        let placement = plan(typed, &proxy).expect("parses");
        // A forced span offset: one byte into the name.
        let forced: Vec<usize> = placement.sites.iter().map(|s| s + 1).collect();
        let misplaced = insert_at(typed, &forced);
        assert!(verify(&placement, &misplaced).is_err(), "{misplaced:?}");
        assert!(verify(&placement, &insert_at(typed, &placement.sites)).is_ok());
    }
}

// -- 4. the never-run list ------------------------------------------------------

const NEVER_RUN: [(&str, &str); 9] = [
    ("rm-recursive-force-root", "rm -rf /"),
    ("mkfs-device", "mkfs -t ext4 /dev/sdb1"),
    ("dd-to-disk", "dd if=x.img of=/dev/disk2"),
    ("fork-bomb", ":(){ :|:& };:"),
    ("chmod-chown-recursive-root", "chown -R nobody /"),
    ("git-force-push-main", "git push -f origin master"),
    ("git-force-refspec-main", "git push origin +HEAD:main"),
    (
        "sqlite3-legion-db",
        "sqlite3 ~/.local/share/legion/legion.db .tables",
    ),
    ("rm-recursive-force", "rm -fr target"),
];

fn denied_by(command: &str, policy: &Policy) -> Option<String> {
    let routed = route(policy, &bash(command), &Context::default());
    match (routed.decision, routed.deciding) {
        (Decision::Deny(details), Deciding::NoGo { id }) => {
            assert_eq!(details.instead(), NO_GO_INSTEAD, "{command}");
            assert!(!details.reason().is_empty(), "{command}");
            Some(id)
        }
        _ => None,
    }
}

#[test]
fn each_never_run_entry_is_denied_everywhere_it_can_run() {
    let policy = policy();
    for (id, command) in NEVER_RUN {
        let mut shapes: Vec<String> = vec![
            command.to_string(),
            format!("echo a; {command}"),
            format!("sh -c '{command}'"),
            format!("bash -c '{command}'"),
            format!("eval '{command}'"),
        ];
        if id != "fork-bomb" {
            shapes.extend([
                format!("true && {command}"),
                format!("echo a | {command}"),
                format!("X=1 {command}"),
                format!("({command})"),
                format!("echo \"$({command})\""),
                format!("while true; do {command}; done"),
                format!("g() {{ {command}; }}"),
                format!("sudo {command}"),
                format!("env {command}"),
                format!("nice {command}"),
                format!("xargs {command}"),
            ]);
        }
        for shape in shapes {
            assert_eq!(denied_by(&shape, &policy).as_deref(), Some(id), "{shape}");
        }
    }
}

#[test]
fn the_builtin_entries_hold_with_no_policy_file() {
    let none = Policy::default();
    for (id, command) in &NEVER_RUN[..7] {
        assert_eq!(denied_by(command, &none).as_deref(), Some(*id), "{command}");
        // A function definition has no prefix-word form.
        if *id == "fork-bomb" {
            continue;
        }
        assert_eq!(
            denied_by(&format!("sudo {command}"), &none).as_deref(),
            Some(*id)
        );
    }
}

// -- 5 and 6. the ask flow: curl and the power switches ---------------------------

fn assert_ask_flow(typed: &str, id: &str) {
    let policy = policy();
    let unconfirmed = route(&policy, &bash(typed), &Context::default());
    assert!(matches!(unconfirmed.decision, Decision::Ask(_)), "{typed}");
    assert_eq!(
        unconfirmed.deciding,
        Deciding::Rule {
            id: id.to_string(),
            needs_operator: false
        },
        "{typed}"
    );
    assert!(!unconfirmed.confirmed);

    let key = command_key(typed).expect("key");
    let ctx = Context {
        confirmations: [(key, "the operator asked for it".to_string())].into(),
        ..Context::default()
    };
    let confirmed = route(&policy, &bash(typed), &ctx);
    assert!(confirmed.confirmed, "{typed}");
    assert_eq!(
        confirmed.deciding,
        Deciding::Rule {
            id: id.to_string(),
            needs_operator: true
        },
        "{typed}"
    );
    let Decision::Ask(details) = confirmed.decision else {
        panic!("{typed}: a confirmed ask goes to the operator");
    };
    assert_eq!(details.reason(), "the operator asked for it");
}

#[test]
fn curl_gets_the_ask_flow() {
    assert_ask_flow("curl -s https://example.com", "curl-network");
    assert_ask_flow("sudo curl x", "curl-network");
}

#[test]
fn each_power_switch_gets_the_ask_flow_in_either_form() {
    for (typed, id) in [
        ("git push --force", "push-force"),
        ("git push -f origin feature", "push-force"),
        ("legion push --repo legion --force", "push-force"),
        ("legion git push --force-with-lease", "push-force"),
        ("git push origin +feature", "push-force-refspec"),
        (
            "gh pr merge 5 --merge-despite-failures",
            "merge-despite-failures",
        ),
        (
            "legion pr merge --repo legion --number 5 --merge-despite-failures",
            "merge-despite-failures",
        ),
        ("gh issue close 5 --force", "issue-close-force"),
        (
            "legion issue close --repo legion --number 5 --force",
            "issue-close-force",
        ),
    ] {
        assert_ask_flow(typed, id);
    }
    // The command the operator approves is the one that runs.
    assert_eq!(
        decide("git push --force").facts.rewritten.as_deref(),
        Some("legion git push --force")
    );
}

#[test]
fn a_forced_push_to_main_or_master_is_still_denied() {
    let policy = policy();
    for typed in [
        "git push --force origin main",
        "git push -f origin master",
        "git push origin +main",
        "cd x && git push --force-with-lease origin main",
        // The legion forms, typed directly: never-run, not a power-switch ask.
        "legion git push --force origin main",
        "legion git push -f origin master",
        "legion push --repo x --force origin main",
        "legion git push origin +HEAD:main",
        "legion git push origin +master",
        "sudo legion git push --force origin main",
    ] {
        assert!(denied_by(typed, &policy).is_some(), "{typed}");
    }
}

#[test]
fn a_prefix_word_runs_one_command_and_its_arguments_are_only_text() {
    let policy = policy();
    // The command a prefix word runs is `echo`; the words after it are
    // echo's arguments, never a command start.
    for typed in [
        "xargs -I{} echo would run rm -rf / on {}",
        "sudo -u deploy echo git push --force",
        "sudo echo curl x",
        "env FOO=1 echo rm -rf /",
        "timeout 5 echo sqlite3 legion.db",
        "nice -n 5 printf '%s' rm -rf /",
    ] {
        let routed = decide(typed);
        assert_eq!(routed.decision, Decision::Allow { note: None }, "{typed}");
        assert_eq!(routed.deciding, Deciding::Default, "{typed}");
    }
    // The command the prefix word runs is still read, behind its own
    // options, values, assignments and operands.
    for (typed, id) in [
        ("sudo -u deploy rm -rf /", "rm-recursive-force-root"),
        ("sudo -E -H rm -rf /", "rm-recursive-force-root"),
        ("sudo --user=root rm -rf build", "rm-recursive-force"),
        ("env -i FOO=1 BAR=2 sqlite3 legion.db", "sqlite3-legion-db"),
        ("timeout -s KILL 5 rm -rf /", "rm-recursive-force-root"),
        ("xargs -0 -n 1 rm -rf", "rm-recursive-force"),
        ("sudo env FOO=1 nice rm -rf /", "rm-recursive-force-root"),
        ("sudo -- rm -rf /", "rm-recursive-force-root"),
    ] {
        assert_eq!(denied_by(typed, &policy).as_deref(), Some(id), "{typed}");
    }
    let asked = decide("sudo -u deploy curl x");
    assert!(matches!(asked.decision, Decision::Ask(_)));
}

// -- 7. the misfires that started this ---------------------------------------------

#[test]
fn git_push_help_runs_through_legion_and_analysis_scripts_run_untouched() {
    assert_eq!(
        rewritten("git push -h").as_deref(),
        Some("legion git push -h")
    );
    for typed in [
        "python3 -c \"import glob; print(len(glob.glob('src/**/*.rs', recursive=True)))\"",
        "ls ~/.claude/projects | python3 -c \"import sys, glob; print(glob.glob('*'))\"",
        "echo 'rg and grep are search tools'",
        "printf '%s\\n' \"grep -rn x\"",
    ] {
        let routed = decide(typed);
        assert_eq!(routed.decision, Decision::Allow { note: None }, "{typed}");
        assert_eq!(routed.deciding, Deciding::Default, "{typed}");
    }
}

// -- 8 and 9. untouched means untouched; nothing names a replacement ----------

#[test]
fn a_command_matching_no_rule_carries_no_decision_and_no_note() {
    for typed in [
        "echo hi",
        "cargo test",
        "ls -la",
        "legion recall --repo x --context y",
        "",
    ] {
        let routed = decide(typed);
        assert_eq!(routed.decision, Decision::Allow { note: None }, "{typed:?}");
        assert_eq!(routed.deciding, Deciding::Default, "{typed:?}");
        assert_eq!(routed.facts.rewritten, None, "{typed:?}");
    }
}

#[test]
fn no_bash_response_names_a_command_to_run_instead() {
    for typed in [
        "rm -rf /",
        "rm -rf build",
        "sqlite3 legion.db",
        "grep 'unterminated",
        "curl x",
        "git push --force",
        "git status",
    ] {
        let routed = decide(typed);
        if let Decision::Deny(details) = &routed.decision {
            assert_eq!(details.instead(), NO_GO_INSTEAD, "{typed}");
        }
        if let Decision::Rewrite { target, .. } = &routed.decision {
            assert_eq!(target.as_str(), "legion", "{typed}");
            assert_eq!(
                routed.deciding,
                Deciding::Rule {
                    id: PROXY_ID.to_string(),
                    needs_operator: false
                }
            );
        }
    }
}
