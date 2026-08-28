#!/bin/bash
# Test runner for the hook-side delivery-drain hook (#941, #1020).
#
# Verifies delivery-drain.sh surfaces drained bullpen posts as
# additionalContext, debounces repeat firings within one session, wraps its
# output in the fixed HOOK OUTPUT DOCTRINE result block with the directed
# (REQUIRES A REPLY) set last, and does all of this without ever invoking
# `legion mcp` -- this hook's whole point is a delivery path that does not
# depend on the MCP subprocess push existing at all.
#
# Run from anywhere:
#
#   bash plugin/hooks/test-delivery-drain.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

echo "==> hooks.json: delivery-drain.sh is the last hook command of the last group for UserPromptSubmit, PostToolUse, and Stop (#1020)"
for event in UserPromptSubmit PostToolUse Stop; do
  last_cmd=$(jq -r --arg ev "$event" '.hooks[$ev] | last | .hooks | last | .command' "$HOOKS_SRC_DIR/hooks.json")
  assert_contains "delivery-drain.sh is the last hook for $event" "$last_cmd" "delivery-drain.sh"
done

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

echo "==> stop bypasses the debounce: a Stop within the window still drains (#1000)"
: > "$LEGION_STUB_LOG"
export LEGION_DELIVERY_DRAIN_DEBOUNCE_SECONDS=60
export FAKE_DELIVER_DRAIN="[Legion] Bullpen (1 posts):
- [rafters] turn-end drain (2026-08-25)"

# Prime the sentinel with a PostToolUse drain, so the debounce window is open.
prime_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"drain-test-stop","tool_name":"Edit"}')
assert_contains "priming PostToolUse drains" "$prime_out" "turn-end drain"

# Regression guard: a non-Stop event inside the window is still debounced --
# the exemption must not have disabled the debounce wholesale.
pt_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"drain-test-stop","tool_name":"Edit"}')
assert_empty "a second PostToolUse in the window is still debounced" "$pt_out"

# The fix: a Stop in the same window MUST still drain -- it fires once per
# turn and is the last chance before the session goes idle.
stop_out=$(run_hook '{"hook_event_name":"Stop","cwd":"/tmp/legion-test","session_id":"drain-test-stop","tool_name":""}')
assert_contains "Stop within the debounce window still drains" "$stop_out" "turn-end drain"
assert_contains "the Stop drain tags the Stop event" "$stop_out" '"hookEventName": "Stop"'

# The Stop drain wrote the sentinel, so a tool-call flurry right after it is
# debounced as before -- the exemption is for Stop only, not a reset of the
# window (#1000 Behavior: "A Stop drain still writes the sentinel"). With
# FAKE_DELIVER_DRAIN still set, a non-debounced PostToolUse would surface
# "turn-end drain"; asserting empty proves the Stop-written sentinel held.
post_stop_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"drain-test-stop","tool_name":"Edit"}')
assert_empty "a PostToolUse right after the Stop is debounced by the Stop-written sentinel" "$post_stop_out"
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

echo "==> result block: fixed opening/closing lines wrap the drain, with the directed (REQUIRES A REPLY) entry after the musing (#1020)"
: > "$LEGION_STUB_LOG"
export FAKE_DELIVER_DRAIN="[Legion] Bullpen (1 posts):
- [rafters] a musing before the ask (2026-08-27)
---
You were auto-woken by legion watch. The following signal(s) are directed at you (legion-test).

REQUIRES A REPLY -- these are directed questions and requests.

- [from rafters] question: which lane owns retries (id: sig-1)"
out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"drain-test-split","tool_name":"Edit"}')

ctx_text=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext')
first_line=$(printf '%s\n' "$ctx_text" | head -n 1)
last_line=$(printf '%s\n' "$ctx_text" | tail -n 1)
assert_eq "first line is the fixed opening result line" "$first_line" "[Legion] Delivery drain result:"
assert_eq "last line is the fixed closing result line" "$last_line" "[Legion] End delivery drain result."

before_directed="${ctx_text%%which lane owns retries*}"
assert_contains "the musing appears before the directed entry" "$before_directed" "a musing before the ask"
assert_contains "the directed entry carries the REQUIRES A REPLY framing" "$ctx_text" "REQUIRES A REPLY"
# The hook must actually invoke --split (not merely be configured with
# stub content that happens to look combined) -- without this the
# fixture above would pass identically if the hook stopped passing
# --split entirely (caught by mutation review, #1020).
assert_file_contains "the hook calls deliver drain with --split" "$LEGION_STUB_LOG" \
  "deliver drain --repo legion-test --split"
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
