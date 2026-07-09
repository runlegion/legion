#!/bin/bash
# Test runner for the recall-first PreToolUse hook (#672).
#
# Verifies recall-first.sh still injects recall hits before WebFetch
# (unchanged; WebSearch is clamped off by default via recall-clamp.conf).
# recall-first.sh is no longer wired to the Agent matcher in hooks.json
# (#672: no-harness-explore.sh is the sole decider for Agent/Explore), but
# these Agent-input cases are still exercised by direct invocation here as
# a regression guard against re-wiring or the case statement growing an
# Agent branch back.
#
# Run from anywhere:
#
#   bash plugin/hooks/test-recall-first.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root recall-first.sh recall-clamp.conf

# The hook gates on legion coverage; make the test repo covered via the
# stub's watch-list fixture.
export FAKE_WATCH="legion-test	/tmp/legion-test"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/recall-first.sh"

run_hook() {
  printf '%s' "$1" | bash "$HOOK"
}

echo "==> WebFetch with recall hits injects context and allows"
export FAKE_RECALL="- some reflection body (score: 0.91)"
out=$(run_hook '{"tool_input":{"prompt":"how does the sync engine work"},"cwd":"/tmp/legion-test","session_id":"test","tool_name":"WebFetch"}')
assert_contains "injects recall hits" "$out" "some reflection body"
assert_not_contains "does not deny" "$out" '"permissionDecision": "deny"'
assert_contains "allows" "$out" '"permissionDecision": "allow"'

echo "==> WebSearch is clamped by default (recall-clamp.conf), unchanged"
out=$(run_hook '{"tool_input":{"query":"how does the sync engine work"},"cwd":"/tmp/legion-test","session_id":"test","tool_name":"WebSearch"}')
assert_empty "WebSearch clamped -- no output" "$out"
unset FAKE_RECALL

echo "==> WebFetch with no recall hits injects nothing"
out=$(run_hook '{"tool_input":{"prompt":"how does the sync engine work"},"cwd":"/tmp/legion-test","session_id":"test","tool_name":"WebFetch"}')
assert_empty "no hits, no output" "$out"

echo "==> Explore agent spawn: no permissionDecision from recall-first (#672 criterion 2)"
export FAKE_RECALL="- some reflection body (score: 0.91)"
out=$(run_hook '{"tool_input":{"subagent_type":"Explore","prompt":"how does the sync engine work"},"cwd":"/tmp/legion-test","session_id":"test","tool_name":"Agent"}')
assert_empty "recall-first never decides on Explore" "$out"

echo "==> non-Explore agent spawn: unchanged pass-through"
out=$(run_hook '{"tool_input":{"subagent_type":"Plan","prompt":"how does the sync engine work"},"cwd":"/tmp/legion-test","session_id":"test","tool_name":"Agent"}')
assert_empty "Plan agent passes through" "$out"
unset FAKE_RECALL

echo "==> uncovered repo passes through"
out=$(run_hook '{"tool_input":{"prompt":"how does the sync engine work"},"cwd":"/tmp/uncovered-repo","session_id":"test","tool_name":"WebFetch"}')
assert_empty "WebFetch allowed to pass through in a repo legion does not cover" "$out"

finish_tests
