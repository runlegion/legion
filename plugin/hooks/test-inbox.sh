#!/bin/bash
# Test runner for the hook-side inbox hook (#941, #1020).
#
# Verifies inbox.sh surfaces delivered bullpen posts as
# additionalContext, debounces repeat firings within one session, wraps its
# output in the fixed HOOK OUTPUT DOCTRINE result block with the directed
# (REQUIRES A REPLY) set last, and does all of this without ever invoking
# `legion mcp` -- this hook's whole point is a delivery path that does not
# depend on the MCP subprocess push existing at all.
#
# Run from anywhere:
#
#   bash plugin/hooks/test-inbox.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

echo "==> hooks.json: inbox.sh is the last hook command of the last group for UserPromptSubmit, PostToolUse, and Stop (#1020)"
for event in UserPromptSubmit PostToolUse Stop; do
  last_cmd=$(jq -r --arg ev "$event" '.hooks[$ev] | last | .hooks | last | .command' "$HOOKS_SRC_DIR/hooks.json")
  assert_contains "inbox.sh is the last hook for $event" "$last_cmd" "inbox.sh"
done

make_plugin_root inbox.sh

# The hook gates on legion coverage; make the test repo covered via the
# stub's watch-list fixture (mirrors test-recall-first.sh).
export FAKE_WATCH="legion-test	/tmp/legion-test"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/inbox.sh"
export LEGION_STUB_LOG="$WORK/stub.log"

run_hook() {
  printf '%s' "$1" | bash "$HOOK"
}

echo "==> mid-hook delivery: a post that landed between two hook invocations surfaces in additionalContext"
export FAKE_INBOX="[Legion] Bullpen (1 posts):
- [rafters] a post that landed mid-session (2026-08-16)"
out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-1","tool_name":"Edit"}')
assert_contains "surfaces the delivered post text" "$out" "a post that landed mid-session"
assert_contains "tags the firing event" "$out" '"hookEventName": "PostToolUse"'
unset FAKE_INBOX

echo "==> debounce: a second PostToolUse firing inside the debounce window makes no inbox call"
: > "$LEGION_STUB_LOG"
export LEGION_INBOX_DEBOUNCE_SECONDS=60
export FAKE_INBOX="[Legion] Bullpen (1 posts):
- [rafters] first delivery (2026-08-16)"

first_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-2","tool_name":"Edit"}')
assert_contains "first call delivers" "$first_out" "first delivery"
calls_after_first=$(grep -c '^inbox' "$LEGION_STUB_LOG")
assert_eq "one inbox call after the first firing" "$calls_after_first" "1"

second_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-2","tool_name":"Edit"}')
assert_empty "debounced second call emits nothing" "$second_out"
calls_after_second=$(grep -c '^inbox' "$LEGION_STUB_LOG")
assert_eq "still only one inbox call after the debounced firing" "$calls_after_second" "1"
unset LEGION_INBOX_DEBOUNCE_SECONDS FAKE_INBOX

echo "==> stop bypasses the debounce: a Stop within the window still delivers (#1000)"
: > "$LEGION_STUB_LOG"
export LEGION_INBOX_DEBOUNCE_SECONDS=60
export FAKE_INBOX="[Legion] Bullpen (1 posts):
- [rafters] turn-end delivery (2026-08-25)"

# Prime the sentinel with a PostToolUse delivery, so the debounce window is open.
prime_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-stop","tool_name":"Edit"}')
assert_contains "priming PostToolUse delivers" "$prime_out" "turn-end delivery"

# Regression guard: a non-Stop event inside the window is still debounced --
# the exemption must not have disabled the debounce wholesale.
pt_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-stop","tool_name":"Edit"}')
assert_empty "a second PostToolUse in the window is still debounced" "$pt_out"

# The fix: a Stop in the same window MUST still deliver -- it fires once per
# turn and is the last chance before the session goes idle.
stop_out=$(run_hook '{"hook_event_name":"Stop","cwd":"/tmp/legion-test","session_id":"inbox-test-stop","tool_name":""}')
assert_contains "Stop within the debounce window still delivers" "$stop_out" "turn-end delivery"
assert_contains "the Stop delivery tags the Stop event" "$stop_out" '"hookEventName": "Stop"'

# The Stop delivery wrote the sentinel, so a tool-call flurry right after it is
# debounced as before -- the exemption is for Stop only, not a reset of the
# window (#1000 Behavior: "A Stop delivery still writes the sentinel"). With
# FAKE_INBOX still set, a non-debounced PostToolUse would surface
# "turn-end delivery"; asserting empty proves the Stop-written sentinel held.
post_stop_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-stop","tool_name":"Edit"}')
assert_empty "a PostToolUse right after the Stop is debounced by the Stop-written sentinel" "$post_stop_out"
unset LEGION_INBOX_DEBOUNCE_SECONDS FAKE_INBOX

echo "==> delivery with no MCP subprocess: the hook lane delivers without ever invoking legion mcp"
: > "$LEGION_STUB_LOG"
export FAKE_INBOX="[Legion] Bullpen (1 posts):
- [rafters] delivered without mcp (2026-08-16)"
out=$(run_hook '{"hook_event_name":"Stop","cwd":"/tmp/legion-test","session_id":"inbox-test-3","tool_name":"Edit"}')
assert_contains "delivers the post via the hook lane alone" "$out" "delivered without mcp"
assert_contains "tags the Stop event" "$out" '"hookEventName": "Stop"'
assert_not_contains "never starts an MCP subprocess" "$(cat "$LEGION_STUB_LOG")" "^mcp"
unset FAKE_INBOX

echo "==> result block: fixed opening/closing lines wrap the delivery, with the directed (REQUIRES A REPLY) entry after the musing (#1020)"
: > "$LEGION_STUB_LOG"
export FAKE_INBOX="[Legion] Bullpen (1 posts):
- [rafters] a musing before the ask (2026-08-27)
---
You were auto-woken by legion watch. The following signal(s) are directed at you (legion-test).

REQUIRES A REPLY -- these are directed questions and requests.

- [from rafters] question: which lane owns retries (id: sig-1)"
out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-split","tool_name":"Edit"}')

ctx_text=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext')
first_line=$(printf '%s\n' "$ctx_text" | head -n 1)
last_line=$(printf '%s\n' "$ctx_text" | tail -n 1)
assert_eq "first line is the fixed opening result line" "$first_line" "[Legion] Inbox:"
assert_eq "last line is the fixed closing result line" "$last_line" "[Legion] End inbox."

before_directed="${ctx_text%%which lane owns retries*}"
assert_contains "the musing appears before the directed entry" "$before_directed" "a musing before the ask"
assert_contains "the directed entry carries the REQUIRES A REPLY framing" "$ctx_text" "REQUIRES A REPLY"
# The hook must actually invoke --split (not merely be configured with
# stub content that happens to look combined) -- without this the
# fixture above would pass identically if the hook stopped passing
# --split entirely (caught by mutation review, #1020).
assert_file_contains "the hook calls inbox with --split" "$LEGION_STUB_LOG" \
  "inbox --repo legion-test --split"
unset FAKE_INBOX

echo "==> empty inbox output emits nothing"
out=$(run_hook '{"hook_event_name":"UserPromptSubmit","cwd":"/tmp/legion-test","session_id":"inbox-test-4","tool_name":""}')
assert_empty "nothing new -- no output" "$out"

echo "==> uncovered repo passes through without calling inbox"
: > "$LEGION_STUB_LOG"
export FAKE_INBOX="[Legion] Bullpen (1 posts):
- [rafters] should not surface (2026-08-16)"
out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/uncovered-repo","session_id":"inbox-test-5","tool_name":"Edit"}')
assert_empty "uncovered repo emits nothing" "$out"
assert_not_contains "uncovered repo never calls inbox" "$(cat "$LEGION_STUB_LOG")" "^inbox"
unset FAKE_INBOX

echo "==> compat fallback: a binary that does not know 'inbox' still delivers via 'deliver drain'"
: > "$LEGION_STUB_LOG"
export FAKE_STUB_FAIL_INBOX=1
export FAKE_INBOX="[Legion] Bullpen (1 posts):
- [rafters] delivered through the compat fallback (2026-09-08)"
out=$(run_hook '{"hook_event_name":"Stop","cwd":"/tmp/legion-test","session_id":"inbox-test-compat","tool_name":""}')
assert_contains "post still reaches the session" "$out" "delivered through the compat fallback"
assert_file_contains "the new spelling is tried first" "$LEGION_STUB_LOG" "^inbox --repo legion-test --split"
assert_file_contains "then the retired spelling" "$LEGION_STUB_LOG" "^deliver drain --repo legion-test --split"
unset FAKE_STUB_FAIL_INBOX

echo "==> a first call that printed and THEN failed keeps its output (the cursor is already spent)"
: > "$LEGION_STUB_LOG"
export FAKE_STUB_PARTIAL_INBOX=1
out=$(run_hook '{"hook_event_name":"Stop","cwd":"/tmp/legion-test","session_id":"inbox-test-partial","tool_name":""}')
assert_contains "posts the failed call already printed still reach the session" "$out" \
  "delivered through the compat fallback"
assert_contains "and they are still wrapped as a result block" "$out" "Inbox:"
unset FAKE_STUB_PARTIAL_INBOX

echo "==> compat fallback does NOT fire when the new spelling works"
: > "$LEGION_STUB_LOG"
out=$(run_hook '{"hook_event_name":"Stop","cwd":"/tmp/legion-test","session_id":"inbox-test-nocompat","tool_name":""}')
assert_contains "post is delivered by the first call" "$out" "delivered through the compat fallback"
assert_file_not_contains "no second shell-out on the common path" "$LEGION_STUB_LOG" "^deliver drain"
unset FAKE_INBOX

finish_tests
