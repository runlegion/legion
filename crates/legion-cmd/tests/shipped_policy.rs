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

// -- rg/ag: since they recurse from a directory by default with no flag,
// the shape that matters is the last operand, not a recursive flag.

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

// -- A managed binary's own global value option must not hide its
// subcommand from family resolution (correctness review on PR #1240).

#[test]
fn git_with_a_global_value_option_before_push_still_denies_force() {
    assert_eq!(
        deny_instead("git -C /tmp push --force"),
        "git push --force-with-lease"
    );
}

#[test]
fn git_with_a_short_value_option_before_push_still_denies_force() {
    assert_eq!(
        deny_instead("git -c user.name=x push --force"),
        "git push --force-with-lease"
    );
}

#[test]
fn gh_with_a_global_value_option_before_pr_merge_still_asks() {
    let routed = route(
        &policy(),
        &bash_call("gh --repo owner/repo pr merge 42 --admin"),
        &Context::default(),
    );
    assert!(matches!(routed.decision, Decision::Ask(_)));
}

#[test]
fn git_with_a_global_value_option_before_an_ungoverned_subcommand_still_allows() {
    let routed = route(
        &policy(),
        &bash_call("git -C /tmp status"),
        &Context::default(),
    );
    assert!(matches!(routed.decision, Decision::Allow { .. }));
}

#[test]
fn git_with_an_unlisted_global_flag_still_fails_closed_to_deny() {
    // "--foo" is not in git's declared global_value_options, so
    // subcommand_word misreads "bar" (--foo's value) as the subcommand
    // and "git bar" resolves to no family. "push" is still literally
    // present in the args, so this must deny, never fall through to the
    // no-managed-binary allow default.
    let routed = route(
        &policy(),
        &bash_call("git --foo bar push --force"),
        &Context::default(),
    );
    assert!(matches!(routed.decision, Decision::Deny(_)));
}

#[test]
fn git_grep_option_naming_a_governed_verb_still_allows() {
    // subcommand_word finds "log" as the very first argument -- no flag
    // was skipped, so the resolution is confident, not doubtful. "push"
    // only appears as --grep's search text, not a real subcommand, so
    // the fail-closed net must not fire on it.
    let routed = route(
        &policy(),
        &bash_call("git log --grep push"),
        &Context::default(),
    );
    assert!(matches!(routed.decision, Decision::Allow { .. }));
}

#[test]
fn git_branch_operand_naming_a_governed_verb_still_allows() {
    // Same shape: "branch" resolves confidently as the first argument,
    // and "push" here is a branch name being deleted, not a subcommand.
    let routed = route(
        &policy(),
        &bash_call("git branch -d push"),
        &Context::default(),
    );
    assert!(matches!(routed.decision, Decision::Allow { .. }));
}

#[test]
fn chmod_777_denies() {
    assert_eq!(deny_instead("chmod 777 x"), "chmod 755");
}

// `legion issue list` requires `--repo`; the shipped target carries the
// `{repo}` placeholder the adapter substitutes from `Context.repo` (the
// only placeholder this contract supports -- see
// `PolicyError::UnsupportedRewriteTargetPlaceholder`). `route` passes the
// target through unchanged, so every assertion below checks for the
// literal placeholder text, not a filled-in repo name.
const GH_ISSUE_LIST_TARGET: &str = "legion issue list --repo {repo}";

#[test]
fn gh_issue_list_rewrites_to_legion_issue_list() {
    let routed = route(&policy(), &bash_call("gh issue list"), &Context::default());
    match routed.decision {
        Decision::Rewrite { target, .. } => assert_eq!(target.as_str(), GH_ISSUE_LIST_TARGET),
        other => panic!("expected Rewrite, got {other:?}"),
    }
}

// -- FR-CMD-008: gh-issue-list's rewrite is lossless only for a bare
// invocation, and only when it is the whole command. `--repo other/org`
// would otherwise silently rewrite against whatever repo the adapter
// defaults to via `{repo}`, not the repo the agent named -- the exact bug
// this rule's `translatable` declaration exists to close. Operator
// correction (2026-09-16): nothing carries an argument into a rewrite
// target today, so `--label`/`--state`/`--draft` deny for the same reason
// `--repo` does, not because they lack a nominal equivalent.

#[test]
fn gh_issue_list_with_repo_flag_denies_instead_of_dropping_it() {
    assert_eq!(
        deny_instead("gh issue list --repo other/org"),
        GH_ISSUE_LIST_TARGET
    );
}

#[test]
fn gh_issue_list_with_label_flag_denies_instead_of_dropping_it() {
    assert_eq!(
        deny_instead("gh issue list --label bug"),
        GH_ISSUE_LIST_TARGET
    );
}

#[test]
fn gh_issue_list_with_state_flag_denies_instead_of_dropping_it() {
    assert_eq!(
        deny_instead("gh issue list --state closed"),
        GH_ISSUE_LIST_TARGET
    );
}

#[test]
fn gh_issue_list_with_draft_flag_denies_instead_of_dropping_it() {
    // FR-CMD-008 correction requirement 3: an unknown flag on a governed
    // verb must never fall to the permissive side.
    assert_eq!(deny_instead("gh issue list --draft"), GH_ISSUE_LIST_TARGET);
}

#[test]
fn gh_issue_list_piped_denies_instead_of_rewriting_the_whole_command() {
    // The harness would replace the WHOLE command string with the
    // rewrite's target, silently dropping `| head` if this rewrote.
    assert_eq!(deny_instead("gh issue list | head"), GH_ISSUE_LIST_TARGET);
}

#[test]
fn gh_issue_list_redirected_denies_instead_of_rewriting_the_whole_command() {
    assert_eq!(
        deny_instead("gh issue list > out.txt"),
        GH_ISSUE_LIST_TARGET
    );
}

#[test]
fn gh_issue_list_after_and_and_denies_instead_of_rewriting_the_whole_command() {
    assert_eq!(deny_instead("cd x && gh issue list"), GH_ISSUE_LIST_TARGET);
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
