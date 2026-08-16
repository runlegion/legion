#!/bin/bash
# Test runner for the hook-side delivery-drain hook (#941).
#
# Verifies delivery-drain.sh surfaces drained bullpen posts as
# additionalContext, debounces repeat firings within one session, and does
# all of this without ever invoking `legion mcp` -- this hook's whole
# point is a delivery path that does not depend on the MCP subprocess
# push existing at all.
#
# Run from anywhere:
#
#   bash plugin/hooks/test-delivery-drain.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root delivery-drain.sh

# The hook gates on legion coverage; make the test repo covered via the
# stub's watch-list fixture (mirrors test-recall-first.sh).
export FAKE_WATCH="legion-test	/tmp/legion-test"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/delivery-drain.sh"
export LEGION_STUB_LOG="$WORK/stub.log"

run_hook() {
  printf '%s' "$1" | bash "$HOOK"
}

echo "==> mid-hook delivery: a post that landed between two hook invocations surfaces in additionalContext"
export FAKE_DELIVER_DRAIN="[Legion] Bullpen (1 posts):
- [rafters] a post that landed mid-session (2026-08-16)"
out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"drain-test-1","tool_name":"Edit"}')
assert_contains "surfaces the drained post text" "$out" "a post that landed mid-session"
assert_contains "tags the firing event" "$out" '"hookEventName": "PostToolUse"'
unset FAKE_DELIVER_DRAIN

echo "==> debounce: a second PostToolUse firing inside the debounce window makes no deliver drain call"
: > "$LEGION_STUB_LOG"
export LEGION_DELIVERY_DRAIN_DEBOUNCE_SECONDS=60
export FAKE_DELIVER_DRAIN="[Legion] Bullpen (1 posts):
- [rafters] first drain (2026-08-16)"

first_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"drain-test-2","tool_name":"Edit"}')
assert_contains "first call drains" "$first_out" "first drain"
calls_after_first=$(grep -c '^deliver drain' "$LEGION_STUB_LOG")
assert_eq "one deliver-drain call after the first firing" "$calls_after_first" "1"

second_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"drain-test-2","tool_name":"Edit"}')
assert_empty "debounced second call emits nothing" "$second_out"
calls_after_second=$(grep -c '^deliver drain' "$LEGION_STUB_LOG")
assert_eq "still only one deliver-drain call after the debounced firing" "$calls_after_second" "1"
unset LEGION_DELIVERY_DRAIN_DEBOUNCE_SECONDS FAKE_DELIVER_DRAIN

echo "==> delivery with no MCP subprocess: the hook lane delivers without ever invoking legion mcp"
: > "$LEGION_STUB_LOG"
export FAKE_DELIVER_DRAIN="[Legion] Bullpen (1 posts):
- [rafters] delivered without mcp (2026-08-16)"
out=$(run_hook '{"hook_event_name":"Stop","cwd":"/tmp/legion-test","session_id":"drain-test-3","tool_name":"Edit"}')
assert_contains "delivers the post via the hook lane alone" "$out" "delivered without mcp"
assert_contains "tags the Stop event" "$out" '"hookEventName": "Stop"'
assert_not_contains "never starts an MCP subprocess" "$(cat "$LEGION_STUB_LOG")" "^mcp"
unset FAKE_DELIVER_DRAIN

echo "==> empty drain output emits nothing"
out=$(run_hook '{"hook_event_name":"UserPromptSubmit","cwd":"/tmp/legion-test","session_id":"drain-test-4","tool_name":""}')
assert_empty "nothing new -- no output" "$out"

echo "==> uncovered repo passes through without calling deliver drain"
: > "$LEGION_STUB_LOG"
export FAKE_DELIVER_DRAIN="[Legion] Bullpen (1 posts):
- [rafters] should not surface (2026-08-16)"
out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/uncovered-repo","session_id":"drain-test-5","tool_name":"Edit"}')
assert_empty "uncovered repo emits nothing" "$out"
assert_not_contains "uncovered repo never calls deliver drain" "$(cat "$LEGION_STUB_LOG")" "^deliver drain"
unset FAKE_DELIVER_DRAIN

finish_tests
