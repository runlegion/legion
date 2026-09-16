//! Verifies the artifact the adapter will actually read
//! (`plugin/legion-cmd/policy.json`), not just the inline test policies in
//! `evaluate.rs`. A policy that parses cleanly can still mis-order its
//! `sym_jobs` so a broader job shadows a more specific one -- exactly the
//! failure this issue's toy was meant to preempt -- so every declared job
//! here gets at least one case that must reach it and not a job listed
//! before it.

use legion_cmd::{Context, Decision, ToolCall, parse_policy, route};

const POLICY_JSON: &str = include_str!("../../../plugin/legion-cmd/policy.json");

fn policy() -> legion_cmd::Policy {
    parse_policy(POLICY_JSON).expect("the shipped policy.json must parse")
}

fn bash_call(command: &str) -> ToolCall {
    ToolCall {
        tool: "Bash".to_string(),
        input: serde_json::json!({ "command": command }),
    }
}

fn deny_instead(command: &str) -> String {
    let routed = route(&policy(), &bash_call(command), &Context::default());
    match routed.decision {
        Decision::Deny(details) => details.instead().to_string(),
        other => panic!("expected Deny for {command:?}, got {other:?}"),
    }
}

#[test]
fn shipped_policy_parses_and_is_not_empty() {
    let policy = policy();
    assert!(!policy.is_empty());
    assert!(!policy.sym_jobs.is_empty());
}

// -- Every declared sym job is reachable, in the order it is declared -----

#[test]
fn pathlib_rglob_and_read_text_reaches_find_content() {
    let command = r#"python3 -c "import pathlib; [print(f) for f in pathlib.Path('.').rglob('*.rs') if 'x' in f.read_text()]""#;
    assert_eq!(deny_instead(command), "legion sym etc find-content");
}

#[test]
fn oswalk_and_read_reaches_find_content_not_an_earlier_or_later_job() {
    let command = r#"python3 -c "import os; [open(f).read() for f in os.walk('.') for _ in [1]]""#;
    assert_eq!(deny_instead(command), "legion sym etc find-content");
}

#[test]
fn node_readdir_and_readfile_reaches_find_content() {
    let command =
        r#"node -e "require('fs').readdirSync('.').forEach(f=>require('fs').readFileSync(f))""#;
    assert_eq!(deny_instead(command), "legion sym etc find-content");
}

#[test]
fn pathlib_rglob_alone_reaches_find_file_not_find_content() {
    let command = r#"python3 -c "print(list(__import__('pathlib').Path('.').rglob('*.rs')))""#;
    assert_eq!(deny_instead(command), "legion sym etc find-file");
}

#[test]
fn node_readdir_alone_reaches_find_file_not_find_content() {
    let command = r#"node -e "console.log(require('fs').readdirSync('.'))""#;
    assert_eq!(deny_instead(command), "legion sym etc find-file");
}

// -- Real-corpus gaps found by measuring against real agent Bash commands
// (a local, uncommitted mining toy over a corpus that never enters this
// repo -- see the PR body for counts). These are hand-written synthetic
// shapes reproducing what the toy found, not copies of any real command.

#[test]
fn oswalk_with_line_by_line_iteration_and_no_read_call_reaches_find_content() {
    // A real miss: `for line in enumerate(f, 1)` (or any bare iteration
    // over the open file object) is a genuine per-line content search,
    // but carries no literal `.read()` call for the original oswalk
    // content job to match on.
    let command = r#"python3 -c "
import os
for root, dirs, files in os.walk('src'):
    for fn in files:
        if fn.endswith('.rs'):
            path = os.path.join(root, fn)
            with open(path) as f:
                for i, line in enumerate(f, 1):
                    if 'needle' in line:
                        print(path, i, line)
""#;
    assert_eq!(deny_instead(command), "legion sym etc find-content");
}

#[test]
fn glob_glob_with_read_reaches_find_content() {
    // A real miss: glob.glob(...) is as common a traversal call in the
    // corpus as os.walk(...) or rglob(...), but the shipped policy had no
    // glob.glob job at all until this measurement found it.
    let command = r#"python3 -c "
import glob
for f in glob.glob('packages/**/*.ts', recursive=True):
    text = open(f).read()
    if 'needle' in text:
        print(f)
""#;
    assert_eq!(deny_instead(command), "legion sym etc find-content");
}

#[test]
fn glob_glob_with_line_iteration_reaches_find_content() {
    let command = r#"python3 -c "
import glob
for f in glob.glob('src/**/*.rs', recursive=True):
    with open(f) as fh:
        for i, line in enumerate(fh, 1):
            if 'needle' in line:
                print(f, i, line)
""#;
    assert_eq!(deny_instead(command), "legion sym etc find-content");
}

#[test]
fn glob_glob_alone_reaches_find_file_not_find_content() {
    let command = r#"python3 -c "
import glob
for f in glob.glob('**/biome.json', recursive=True):
    print(f)
""#;
    assert_eq!(deny_instead(command), "legion sym etc find-file");
}

// -- Negative: a read with no traversal token matches no sym job ----------

#[test]
fn read_text_with_no_traversal_matches_no_sym_job() {
    let routed = route(
        &policy(),
        &bash_call(r#"python3 -c "print(open('x').read())""#),
        &Context::default(),
    );
    assert_eq!(
        routed.decision,
        Decision::Proxy {
            reason: legion_cmd::ProxyReason::Opaque
        }
    );
}

// -- Every Bash family rule in the shipped policy is reachable -------------

#[test]
fn git_push_force_denies() {
    assert_eq!(
        deny_instead("git push --force"),
        "git push --force-with-lease"
    );
}

#[test]
fn chmod_777_denies() {
    assert_eq!(deny_instead("chmod 777 x"), "chmod 755");
}

#[test]
fn gh_issue_list_rewrites_to_legion_issue_list() {
    let routed = route(&policy(), &bash_call("gh issue list"), &Context::default());
    match routed.decision {
        Decision::Rewrite { target, .. } => assert_eq!(target.as_str(), "legion issue list"),
        other => panic!("expected Rewrite, got {other:?}"),
    }
}

#[test]
fn gh_pr_merge_admin_asks() {
    let routed = route(
        &policy(),
        &bash_call("gh pr merge 42 --admin"),
        &Context::default(),
    );
    assert!(matches!(routed.decision, Decision::Ask(_)));
}

#[test]
fn ordinary_git_push_is_unaffected() {
    let routed = route(&policy(), &bash_call("git push"), &Context::default());
    assert!(matches!(routed.decision, Decision::Allow { .. }));
}
