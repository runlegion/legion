#!/bin/bash
# Test runner for the no-harness-explore PreToolUse hook.
#
# Verifies the hook denies a built-in Explore subagent spawn and redirects to
# legion:legion-explore, while passing through the redirect target itself,
# other agent types, an absent subagent_type, and repos legion does not cover.
# Run from anywhere:
#
#   bash plugin/hooks/test-no-harness-explore.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root no-harness-explore.sh

# The hook gates on legion coverage; make the test repo covered via the
# stub's watch-list fixture.
export FAKE_WATCH="legion-test	/tmp/legion-test"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/no-harness-explore.sh"

run_hook() {
  local subagent="$1"
  printf '%s' "{\"tool_input\":{\"subagent_type\":${subagent}},\"cwd\":\"/tmp/legion-test\",\"session_id\":\"test\"}" | bash "$HOOK"
}

assert_blocked() {
  local desc="$1" sub="$2"
  assert_contains "$desc" "$(run_hook "$sub")" '"permissionDecision": "deny"'
}

assert_allowed() {
  local desc="$1" sub="$2"
  assert_not_contains "$desc" "$(run_hook "$sub")" '"permissionDecision": "deny"'
}

echo "==> blocks the built-in Explore subagent (case-insensitive)"
assert_blocked "exact Explore"     '"Explore"'
assert_blocked "lowercase explore" '"explore"'
assert_blocked "uppercase EXPLORE" '"EXPLORE"'

echo "==> the denial redirects to legion:legion-explore"
assert_contains "names the replacement agent" "$(run_hook '"Explore"')" 'legion:legion-explore'

echo "==> passes through the redirect target and other agents (exact match, not substring)"
assert_allowed "legion-explore"                   '"legion-explore"'
assert_allowed "namespaced legion:legion-explore" '"legion:legion-explore"'
assert_allowed "Plan"                             '"Plan"'
assert_allowed "general-purpose"                  '"general-purpose"'
assert_allowed "code-explorer contains explore"   '"code-explorer"'

echo "==> missing subagent_type passes through"
out=$(printf '%s' '{"tool_input":{},"cwd":"/tmp/legion-test","session_id":"test"}' | bash "$HOOK")
assert_empty "no subagent_type field" "$out"

echo "==> uncovered repo passes through"
out=$(printf '%s' '{"tool_input":{"subagent_type":"Explore"},"cwd":"/tmp/uncovered-repo","session_id":"test"}' | bash "$HOOK")
assert_empty "Explore allowed in a repo legion does not cover" "$out"

finish_tests
