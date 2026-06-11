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

finish_tests
