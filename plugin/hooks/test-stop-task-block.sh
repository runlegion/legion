#!/bin/bash
# Test runner for the stop-hook gates.
#
# Exercises stop.sh's two-layer enforcement at its CURRENT contract (#931):
# - Delegated work liveness gate (#778, card-free since #931): reads
#   `legion watch delegated-needs-attention --json`. Blocks with
#   decision:block when a delegated work item is no longer verifiably live
#   (top-level decision:block IS the documented Stop shape -- see
#   lib/emit.sh). The in-progress "Accepted kanban card" gate that used to
#   run alongside it is GONE (#931 removed the card surface with no
#   card-free replacement for local "accepted" claim state) -- there is no
#   longer a first gate to test here.
# - Turn-kind prompt (#1477, carried by #569's shape): fires on every Stop
#   via hookSpecificOutput.additionalContext (non-error, continues the
#   turn), NOT decision:block, and injects the issue's text and nothing else.
# Also covers post-task-state.sh (still writes the task event log for other
# consumers) and the watch-pty / session-end (#492/#493) bypass paths.
#
# Run from anywhere:
#   bash plugin/hooks/test-stop-task-block.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root stop.sh post-task-state.sh

POST_HOOK="$CLAUDE_PLUGIN_ROOT/hooks/post-task-state.sh"
STOP_HOOK="$CLAUDE_PLUGIN_ROOT/hooks/stop.sh"

SESSION="test-session-461"

CWD="/tmp/legion-test"

# The turn-kind prompt, byte for byte as #1477's acceptance criterion gives it.
read -r -d '' TURN_KIND_PROMPT <<'EOF'
Before you end this turn, name what kind of turn it was.
**Design:** Sean, a peer, or you were working out what something should be. The deliverable is understanding. End with what you learned, what you still don't know, and the question that moves it forward. In a design turn the next move is a question, not an issue, a brief or a build. If you made one anyway, say so.
**Delivery:** you were carrying out a design that was already agreed. Finish it, or say plainly what blocks it.
**Can't tell:** treat it as design, and ask.
**A peer's ask:** a reply is owed, not a build. Answer it. A question back counts as an answer.
**If you acted on anything this turn:** name what you acted on that you assumed rather than found, so it can be checked.
EOF

echo "==> post-task-state appends create event"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"Ship the thing\"},\"tool_response\":{\"id\":\"1\",\"status\":\"pending\"}}" | bash "$POST_HOOK"
LOG="$XDG_STATE_HOME/legion/tasks-${SESSION}.jsonl"
assert_file_contains "create event written" "$LOG" '"event":"create"'
assert_file_contains "create event carries task_id" "$LOG" '"task_id":"1"'

echo "==> post-task-state appends update event (in_progress)"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"1\",\"status\":\"in_progress\"}}" | bash "$POST_HOOK"
assert_file_contains "update event written" "$LOG" '"event":"update"'
assert_file_contains "update event carries status" "$LOG" '"status":"in_progress"'

echo "==> stop hook BLOCKS (decision:block) on a delegated work item that is no longer live (#778)"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" \
  | FAKE_DELEGATED_NEEDS_ATTENTION="item-42" bash "$STOP_HOOK")
assert_contains "delegated block decision present" "$out" '"decision": "block"'
assert_contains "lists the not-live work item id" "$out" 'item-42'
assert_contains "names bypass env on the delegated gate" "$out" 'LEGION_SKIP_STOP_BLOCK=1'

echo "==> stop hook does NOT block when no delegated work item needs attention"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"no-delegated-session\"}" | bash "$STOP_HOOK")
assert_not_contains "no dead-delegated work item -> no block from the gate" "$out" 'no longer verifiably live'

echo "==> clean board: stop injects exactly the turn-kind prompt via additionalContext (#1477, #569)"
# No FAKE_DELEGATED_NEEDS_ATTENTION -> the liveness gate does not fire. The
# stub would hand back an autonomy banner if the hook still asked for one;
# the injected context must be the prompt and nothing else.
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" \
  | FAKE_AUTONOMY_BANNER="SENTINEL_AUTONOMY_ABC" bash "$STOP_HOOK")
ctx=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext // empty')
assert_eq "injected context is exactly the turn-kind prompt" "$TURN_KIND_PROMPT" "$ctx"
assert_contains "prompt uses additionalContext" "$out" '"hookEventName": "Stop"'
assert_not_contains "prompt is NOT a block decision" "$out" '"decision": "block"'
assert_not_contains "no autonomy budget rides with it" "$out" "SENTINEL_AUTONOMY_ABC"

echo "==> the prompt fires on every turn, not once per session"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | bash "$STOP_HOOK")
ctx=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext // empty')
assert_eq "a second Stop in the same session prompts again" "$TURN_KIND_PROMPT" "$ctx"

echo "==> stop_hook_active suppresses the prompt (loop guard, #569)"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\",\"stop_hook_active\":true}" | bash "$STOP_HOOK")
assert_empty "stop_hook_active path produces no prompt" "$out"

echo "==> bypass via LEGION_SKIP_STOP_BLOCK=1 exits 0"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | FAKE_DELEGATED_NEEDS_ATTENTION="item-42" LEGION_SKIP_STOP_BLOCK=1 bash "$STOP_HOOK")
assert_empty "bypass produces no output" "$out"

echo "==> watch-pty (#492) skips both gates"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | FAKE_DELEGATED_NEEDS_ATTENTION="item-42" LEGION_SPAWN_SOURCE=watch-pty bash "$STOP_HOOK")
assert_empty "watch-pty bypass produces no output" "$out"

echo "==> watch-pty branch ignores non-matching values"
# Any value other than "watch-pty" must NOT trip the bypass -- with a dead
# delegated work item the liveness block should still fire.
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | FAKE_DELEGATED_NEEDS_ATTENTION="item-42" LEGION_SPAWN_SOURCE=manual-test bash "$STOP_HOOK")
assert_contains "manual-test does not trigger watch-pty bypass" "$out" '"decision": "block"'

echo "==> session-end handoff (#493) fires under watch-pty when wake-attempt set"
HANDOFF_LOG="$WORK/handoff.log"
: > "$HANDOFF_LOG"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" \
  | LEGION_SPAWN_SOURCE=watch-pty \
    LEGION_WAKE_ATTEMPT_ID="attempt-test-id" \
    LEGION_STUB_LOG="$HANDOFF_LOG" \
    bash "$STOP_HOOK")
assert_empty "handoff path still produces no output" "$out"
assert_file_contains "watch session-end CLI invoked with attempt-id" \
  "$HANDOFF_LOG" "watch session-end --attempt-id attempt-test-id"

echo "==> session-end handoff is gated on LEGION_WAKE_ATTEMPT_ID"
: > "$HANDOFF_LOG"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" \
  | LEGION_SPAWN_SOURCE=watch-pty \
    LEGION_STUB_LOG="$HANDOFF_LOG" \
    bash "$STOP_HOOK")
assert_file_not_contains "missing LEGION_WAKE_ATTEMPT_ID suppresses handoff" \
  "$HANDOFF_LOG" "watch session-end"

echo "==> no session_id: hook passes through"
out=$(echo "{\"cwd\":\"${CWD}\"}" | bash "$STOP_HOOK")
assert_not_contains "missing session_id passes through" "$out" '"decision": "block"'

echo "==> a session with no tool use still gets the prompt"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"no-card-session\"}" | bash "$STOP_HOOK")
assert_contains "prompt fires with no work marker" "$out" 'Before you end this turn, name what kind of turn it was.'

finish_tests
