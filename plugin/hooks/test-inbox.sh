#!/bin/bash
# Test runner for the hook-side inbox hook (#941, #1020).
#
# Verifies inbox.sh surfaces delivered bullpen posts as
# additionalContext, checks at every firing with no debounce (#1478), wraps its
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

echo "==> hooks.json: the PostToolUse inbox group has no matcher, so it runs on every tool call (#1478)"
inbox_matcher=$(jq -r '.hooks.PostToolUse[] | select(any(.hooks[]; .command | contains("inbox.sh"))) | .matcher // "none"' "$HOOKS_SRC_DIR/hooks.json")
assert_eq "PostToolUse inbox group matches every tool" "$inbox_matcher" "none"

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

echo "==> every tool call checks the inbox: back-to-back PostToolUse firings each make an inbox call (#1478)"
: > "$LEGION_STUB_LOG"
run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-2","tool_name":"Edit"}' >/dev/null
run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-2","tool_name":"Edit"}' >/dev/null
calls=$(grep -c '^inbox' "$LEGION_STUB_LOG")
assert_eq "two firings, two inbox calls" "$calls" "2"

echo "==> a post that arrives right after a check lands at the very next tool call (#1478)"
empty_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-next","tool_name":"Edit"}')
assert_empty "nothing new, nothing emitted" "$empty_out"
export FAKE_INBOX="[Legion] Bullpen (1 posts):
- [rafters] arrived a moment after the last check (2026-11-05)"
next_out=$(run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-next","tool_name":"Edit"}')
assert_contains "the next tool call delivers it" "$next_out" "arrived a moment after the last check"
unset FAKE_INBOX

echo "==> Stop delivers too (#1000)"
export FAKE_INBOX="[Legion] Bullpen (1 posts):
- [rafters] turn-end delivery (2026-08-25)"
run_hook '{"hook_event_name":"PostToolUse","cwd":"/tmp/legion-test","session_id":"inbox-test-stop","tool_name":"Edit"}' >/dev/null
stop_out=$(run_hook '{"hook_event_name":"Stop","cwd":"/tmp/legion-test","session_id":"inbox-test-stop","tool_name":""}')
assert_contains "Stop right after a tool call still delivers" "$stop_out" "turn-end delivery"
assert_contains "the Stop delivery tags the Stop event" "$stop_out" '"hookEventName": "Stop"'
unset FAKE_INBOX

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
