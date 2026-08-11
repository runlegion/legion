#!/bin/bash
# Test runner for the no-harness-explore PreToolUse hook.
#
# Verifies the hook REWRITES a built-in Explore subagent spawn to
# legion:legion-explore (updatedInput carries the substituted subagent_type
# with prompt/description preserved verbatim), while passing through the
# redirect target itself, other agent types, an absent subagent_type, and
# repos legion does not cover. Run from anywhere:
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

# run_hook_full TOOL_INPUT_JSON -- same as run_hook but the caller supplies
# the whole tool_input object, so prompt/description carry-through can be
# asserted on.
run_hook_full() {
  local tool_input="$1"
  printf '%s' "{\"tool_input\":${tool_input},\"cwd\":\"/tmp/legion-test\",\"session_id\":\"test\"}" | bash "$HOOK"
}

assert_rewritten() {
  local desc="$1" sub="$2"
  assert_contains "$desc" "$(run_hook "$sub")" '"updatedInput"'
}

assert_not_rewritten() {
  local desc="$1" sub="$2"
  assert_not_contains "$desc" "$(run_hook "$sub")" '"updatedInput"'
}

echo "==> rewrites the built-in Explore subagent to legion:legion-explore (case-insensitive)"
assert_rewritten "exact Explore"     '"Explore"'
assert_rewritten "lowercase explore" '"explore"'
assert_rewritten "uppercase EXPLORE" '"EXPLORE"'

echo "==> the rewrite is an allow with permissionDecision=allow, not a deny"
out=$(run_hook '"Explore"')
assert_not_contains "no deny decision" "$out" '"permissionDecision": "deny"'
decision="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.permissionDecision' 2>/dev/null)"
assert_eq "permissionDecision is allow" "$decision" "allow"

echo "==> updatedInput carries the substituted subagent_type"
target="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.updatedInput.subagent_type' 2>/dev/null)"
assert_eq "subagent_type rewritten to legion:legion-explore" "$target" "legion:legion-explore"

echo "==> updatedInput preserves prompt and description verbatim (lossless substitution)"
full_out=$(run_hook_full '{"subagent_type":"Explore","prompt":"map the wake FSM","description":"map FSM states"}')
prompt="$(printf '%s' "$full_out" | jq -r '.hookSpecificOutput.updatedInput.prompt' 2>/dev/null)"
description="$(printf '%s' "$full_out" | jq -r '.hookSpecificOutput.updatedInput.description' 2>/dev/null)"
assert_eq "prompt carried through unmodified" "$prompt" "map the wake FSM"
assert_eq "description carried through unmodified" "$description" "map FSM states"

echo "==> the substitution is announced via additionalContext (never silent)"
ctx="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext' 2>/dev/null)"
assert_contains "additionalContext names the substitution" "$ctx" "legion:legion-explore"
assert_contains "additionalContext is non-empty" "$ctx" "."

echo "==> the rewrite output is structurally valid JSON (not just a matching substring)"
# The reason/ctx text carries backticks, em-dashes, quotes, and embedded
# newlines; assert jq can parse the payload, so a malformed-but-substring-
# present blob cannot pass.
assert_contains "parses as JSON with permissionDecision=allow" "$decision" "allow"

echo "==> passes through the redirect target and other agents (exact match, not substring)"
assert_not_rewritten "legion-explore"                   '"legion-explore"'
assert_not_rewritten "namespaced legion:legion-explore" '"legion:legion-explore"'
assert_not_rewritten "Plan"                             '"Plan"'
assert_not_rewritten "general-purpose"                  '"general-purpose"'
assert_not_rewritten "code-explorer contains explore"   '"code-explorer"'

echo "==> missing subagent_type passes through"
out=$(printf '%s' '{"tool_input":{},"cwd":"/tmp/legion-test","session_id":"test"}' | bash "$HOOK")
assert_empty "no subagent_type field" "$out"

echo "==> uncovered repo passes through"
out=$(printf '%s' '{"tool_input":{"subagent_type":"Explore"},"cwd":"/tmp/uncovered-repo","session_id":"test"}' | bash "$HOOK")
assert_empty "Explore allowed in a repo legion does not cover" "$out"

finish_tests
