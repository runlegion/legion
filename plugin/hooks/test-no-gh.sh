#!/bin/bash
# Test runner for the no-gh PreToolUse hook.
#
# Verifies the hook catches gh invocations regardless of how the binary
# is referenced: bare command, absolute path, leading whitespace. Run
# from anywhere:
#
#   bash plugin/hooks/test-no-gh.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root no-gh.sh

# The hook gates on legion coverage; make the test repo covered via the
# stub's watch-list fixture.
export FAKE_WATCH="legion-test	/tmp/legion-test"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/no-gh.sh"

run_hook() {
  local cmd="$1"
  printf '%s' "{\"tool_input\":{\"command\":${cmd}},\"cwd\":\"/tmp/legion-test\",\"session_id\":\"test\"}" | bash "$HOOK"
}

assert_blocked() {
  local desc="$1" cmd="$2"
  assert_contains "$desc" "$(run_hook "$cmd")" '"permissionDecision": "deny"'
}

assert_allowed() {
  local desc="$1" cmd="$2"
  assert_not_contains "$desc" "$(run_hook "$cmd")" '"permissionDecision": "deny"'
}

echo "==> blocks bare gh invocations"
assert_blocked "bare gh subcommand"     '"gh pr merge 123"'
assert_blocked "bare gh with no args"   '"gh"'
assert_blocked "gh with leading space"  '"   gh issue list"'

echo "==> blocks absolute-path gh invocations"
assert_blocked "homebrew absolute path"  '"/opt/homebrew/bin/gh pr merge 123"'
assert_blocked "usr-local absolute path" '"/usr/local/bin/gh issue list"'
assert_blocked "usr-bin absolute path"   '"/usr/bin/gh pr view 1"'
assert_blocked "tilde absolute path"     '"~/bin/gh pr merge 123"'
assert_blocked "absolute path no args"   '"/opt/homebrew/bin/gh"'

echo "==> allows commands that merely mention gh"
assert_allowed "ghostscript"      '"ghostscript --version"'
assert_allowed "git status"       '"git status"'
assert_allowed "echo gh"          '"echo gh pr merge"'
assert_allowed "grep gh logs"     '"grep gh /var/log/foo"'
assert_allowed "path with gh dir" '"ls /opt/ghosts/"'

echo "==> uncovered repo passes through"
out=$(printf '%s' '{"tool_input":{"command":"gh pr merge 1"},"cwd":"/tmp/uncovered-repo","session_id":"test"}' | bash "$HOOK")
assert_empty "gh allowed in a repo legion does not cover" "$out"


# --- #828: exact redirect instead of a fixed menu ----------------------------

assert_suggests() {
  local desc="$1" cmd="$2" needle="$3"
  assert_contains "$desc" "$(run_hook "$cmd")" "$needle"
}

echo "==> translates the verb the agent actually typed"
assert_suggests "pr view"     '"gh pr view 42"'     'legion pr view --repo legion-test --number 42'
assert_suggests "pr checks"   '"gh pr checks 42"'   'legion pr checks --repo legion-test --number 42'
assert_suggests "pr comments" '"gh pr comments 42"' 'legion pr comments --repo legion-test --number 42'
assert_suggests "pr merge"    '"gh pr merge 123"'   'legion pr merge --repo legion-test --number 123'
assert_suggests "pr list"     '"gh pr list"'        'legion pr list --repo legion-test'
assert_suggests "issue view"  '"gh issue view 7"'   'legion issue view --repo legion-test --number 7'
assert_suggests "issue list"  '"gh issue list"'     'legion issue list --repo legion-test'

echo "==> maps verbs whose legion name differs"
assert_suggests "pr comment -> legion comment" '"gh pr comment 42 --body x"' 'legion comment --repo legion-test --number 42'
assert_suggests "run list -> pr checks"        '"gh run list"'               'legion pr checks --repo legion-test'
assert_suggests "pr diff -> pr view"           '"gh pr diff 42"'             'legion pr view --repo legion-test --number 42'

echo "==> reads the number from the --number flag form too"
assert_suggests "flag-form number" '"gh pr view --number 99"' '--number 99'

echo "==> no number given leaves a visible placeholder, not a wrong number"
assert_suggests "placeholder when absent" '"gh pr view"' '--number <n>'

echo "==> unmapped shapes point at group help and invent nothing"
assert_suggests "gh api falls back"   '"gh api /repos/x/y"' 'legion --help'
assert_suggests "unknown pr verb"     '"gh pr sync"'        'legion pr --help'
out_api=$(run_hook '"gh api /repos/x/y"')
assert_not_contains "no fabricated translation for gh api" "$out_api" 'legion api'

echo "==> the old fixed menu is gone"
out_view=$(run_hook '"gh pr view 42"')
assert_not_contains "does not print the 8-verb catalog" "$out_view" 'legion pr review --repo <name>'

finish_tests
