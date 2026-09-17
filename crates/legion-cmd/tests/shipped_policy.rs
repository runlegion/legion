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

// -- Invocation sym jobs (FR-CMD-007): a visible grep/find invocation
// reaches sym without going through an interpreter body at all.

#[test]
fn grep_recursive_piped_into_head_reaches_find_content() {
    assert_eq!(
        deny_instead("grep -rn foo . | head"),
        "legion sym etc find-content"
    );
}

#[test]
fn sh_c_grep_recursive_reaches_find_content() {
    assert_eq!(
        deny_instead("sh -c 'grep -rn foo src'"),
        "legion sym etc find-content"
    );
}

#[test]
fn grep_recursive_precedes_a_sibling_opaque_part() {
    // The sym-job precedence step checks invocations before folding, so a
    // sibling opaque region (which would otherwise proxy) cannot change
    // the outcome -- the same invariant `sym_job_decision_is_never_overridden_by_a_weaker_or_stronger_sibling_part`
    // in evaluate.rs's tests proves for the interpreter-body case.
    assert_eq!(
        deny_instead("grep -rn foo . | ./notify.sh"),
        "legion sym etc find-content"
    );
}

#[test]
fn find_by_name_reaches_find_file() {
    assert_eq!(
        deny_instead("find . -name '*.rs'"),
        "legion sym etc find-file"
    );
}

// -- Negative: plain grep usage is not sym territory -----------------------

#[test]
fn grep_reading_a_pipe_is_not_a_sym_job() {
    let routed = route(
        &policy(),
        &bash_call("cat f | grep foo"),
        &Context::default(),
    );
    assert!(matches!(routed.decision, Decision::Allow { .. }));
}

#[test]
fn grep_on_a_single_named_file_is_not_a_sym_job() {
    let routed = route(
        &policy(),
        &bash_call("grep foo src/main.rs"),
        &Context::default(),
    );
    assert!(matches!(routed.decision, Decision::Allow { .. }));
}

// -- rg/ag: a second, targeted recall pass on #1227's invocation sym jobs.
// rg/ag recurse from a directory by default with no flag, so the shape
// that matters is the last operand, not a recursive flag. Measured on the
// real corpus: a bare "." (already covered) and a directory-looking last
// operand together outnumbered it by roughly 10:1 and hand-checked clean;
// "no path operand at all" looked material too (33 hits) but hand-checking
// it found it firing on `rg --version` (no search at all) and on `rg
// pattern` fed by a preceding pipe (searching piped text, not files on
// disk) far more often than on a genuine implicit-cwd search, so that
// shape was left out rather than forced in.

#[test]
fn rg_bare_dot_reaches_find_content() {
    assert_eq!(deny_instead("rg -n foo ."), "legion sym etc find-content");
}

#[test]
fn rg_directory_operand_reaches_find_content() {
    assert_eq!(deny_instead("rg foo src"), "legion sym etc find-content");
    assert_eq!(
        deny_instead("rg foo packages/ui/src"),
        "legion sym etc find-content"
    );
}

// -- Negative: a file-looking last operand is single-file territory -------

#[test]
fn rg_on_a_single_named_file_is_not_a_sym_job() {
    let routed = route(
        &policy(),
        &bash_call("rg foo src/main.rs"),
        &Context::default(),
    );
    assert!(matches!(routed.decision, Decision::Allow { .. }));
}

// -- Negative: no path operand is excluded (piped stdin / --version risk) --

#[test]
fn rg_with_no_path_operand_is_not_a_sym_job() {
    let routed = route(&policy(), &bash_call("rg foo"), &Context::default());
    assert!(matches!(routed.decision, Decision::Allow { .. }));
}

#[test]
fn rg_reading_a_pipe_is_not_a_sym_job() {
    let routed = route(
        &policy(),
        &bash_call("git branch -a | rg foo"),
        &Context::default(),
    );
    assert!(matches!(routed.decision, Decision::Allow { .. }));
}

#[test]
fn rg_version_is_not_a_sym_job() {
    let routed = route(&policy(), &bash_call("rg --version"), &Context::default());
    assert!(matches!(routed.decision, Decision::Allow { .. }));
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

// -- Real-corpus RECALL gaps found by a second, targeted measurement pass
// (same rules: local uncommitted mining toy, counts and hand-written
// synthetic shapes only, nothing from the corpus copied here). The first
// pass measured precision on matched bodies; this pass measured how many
// unmatched bodies still carried an obvious traversal token. Of ~9,700
// interpreter bodies, os.walk(/os.listdir( appeared in 79 unmatched
// bodies that were never routed to sym at all -- the policy only ever
// matched os.walk(/rglob(/glob.glob( when paired with a content-read
// token, so a pure file-listing job (traverse and print names, no
// content read) fell through to opaque proxy every time.

#[test]
fn oswalk_alone_with_no_content_read_reaches_find_file() {
    let command = r#"python3 -c "
import os
for root, dirs, files in os.walk('src'):
    for f in files:
        if f.endswith('.rs'):
            print(os.path.join(root, f))
""#;
    assert_eq!(deny_instead(command), "legion sym etc find-file");
}

#[test]
fn oslistdir_alone_with_no_content_read_reaches_find_file() {
    let command = r#"python3 -c "
import os
for f in sorted(os.listdir('src')):
    print(f)
""#;
    assert_eq!(deny_instead(command), "legion sym etc find-file");
}

#[test]
fn oslistdir_with_read_reaches_find_content() {
    let command = r#"python3 -c "
import os
for f in sorted(os.listdir('src')):
    content = open(f).read()
    if 'needle' in content:
        print(f)
""#;
    assert_eq!(deny_instead(command), "legion sym etc find-content");
}

#[test]
fn subprocess_find_single_quoted_reaches_find_file() {
    let command = r#"python3 -c "
import subprocess
out = subprocess.run(['find', '.', '-name', '*.rs'], capture_output=True, text=True)
print(out.stdout)
""#;
    assert_eq!(deny_instead(command), "legion sym etc find-file");
}

#[test]
fn subprocess_find_double_quoted_reaches_find_file() {
    let command = r#"python3 -c '
import subprocess
out = subprocess.run(["find", ".", "-name", "*.rs"], capture_output=True, text=True)
print(out.stdout)
'"#;
    assert_eq!(deny_instead(command), "legion sym etc find-file");
}

// -- Negative: a subprocess call unrelated to find must not match --------

#[test]
fn subprocess_without_find_matches_no_sym_job() {
    let routed = route(
        &policy(),
        &bash_call(r#"python3 -c "import subprocess; print(subprocess.run(['ls']).stdout)""#),
        &Context::default(),
    );
    assert_eq!(
        routed.decision,
        Decision::Proxy {
            reason: legion_cmd::ProxyReason::Opaque
        }
    );
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
