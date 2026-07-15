#!/bin/bash
# Test runner for the stop-hook gates.
#
# Exercises stop.sh's two-layer enforcement at its CURRENT contract:
# - In-progress gate (#461 -> #523): reads the KANBAN BOARD via
#   `legion kanban list --json`, not the per-session task log. Blocks with
#   decision:block when an Accepted card exists (top-level decision:block
#   IS the documented Stop shape -- see lib/emit.sh). The board-derived
#   goal (#525) rides along.
# - Reflection nudge (#569): fires via hookSpecificOutput.additionalContext
#   (non-error, continues the turn), NOT decision:block.
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

# Seed work marker so the reflection nudge can fire. legion_hash_str comes
# from the prelude (sourced by make_plugin_root) -- the same hash the hooks
# use, so the marker paths agree by construction.
CWD="/tmp/legion-test"
CWD_HASH=$(legion_hash_str "$CWD")
touch "/tmp/legion-work-${CWD_HASH}"
# make_plugin_root installed a $WORK-cleanup trap; extend it with the /tmp markers.
trap 'rm -rf "$WORK" /tmp/legion-work-'"${CWD_HASH}"' /tmp/legion-reflected-'"${CWD_HASH}" EXIT

echo "==> post-task-state appends create event"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"Ship the thing\"},\"tool_response\":{\"id\":\"1\",\"status\":\"pending\"}}" | bash "$POST_HOOK"
LOG="$XDG_STATE_HOME/legion/tasks-${SESSION}.jsonl"
assert_file_contains "create event written" "$LOG" '"event":"create"'
assert_file_contains "create event carries task_id" "$LOG" '"task_id":"1"'

echo "==> post-task-state appends update event (in_progress)"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"1\",\"status\":\"in_progress\"}}" | bash "$POST_HOOK"
assert_file_contains "update event written" "$LOG" '"event":"update"'
assert_file_contains "update event carries status" "$LOG" '"status":"in_progress"'

echo "==> stop hook BLOCKS (decision:block) on an Accepted kanban card"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" \
  | FAKE_KANBAN_ACCEPTED="Ship the thing" FAKE_GOAL="GOAL: ship the thing" bash "$STOP_HOOK")
assert_contains "block decision present" "$out" '"decision": "block"'
assert_contains "lists the accepted card title" "$out" 'Ship the thing'
assert_contains "names bypass env" "$out" 'LEGION_SKIP_STOP_BLOCK=1'
assert_contains "surfaces the board-derived goal" "$out" 'GOAL: ship the thing'

echo "==> stop hook BLOCKS (decision:block) on a delegated card that is no longer live (#778)"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" \
  | FAKE_KANBAN_DELEGATED_DEAD="Ship the delegated thing" bash "$STOP_HOOK")
assert_contains "delegated block decision present" "$out" '"decision": "block"'
assert_contains "lists the not-live delegated card title" "$out" 'Ship the delegated thing'
assert_contains "delegated block names the undelegate escape" "$out" 'legion kanban undelegate --id <id>'
assert_contains "names bypass env on the delegated gate too" "$out" 'LEGION_SKIP_STOP_BLOCK=1'

echo "==> stop hook does NOT block when no delegated card needs attention"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"no-delegated-session\"}" | bash "$STOP_HOOK")
assert_not_contains "no dead-delegated card -> no block from gate 1b" "$out" 'delegated to a watch-spawned attempt'
# That call fell through to the reflection nudge and touched the shared
# per-CWD marker; clear it so the dedicated nudge tests below still see a
# clean slate (same cleanup discipline used throughout this file).
rm -f "/tmp/legion-reflected-${CWD_HASH}"

echo "==> clean board: stop nudges for reflection via additionalContext (#569)"
# No FAKE_KANBAN_ACCEPTED -> in-progress gate does not fire -> reflection path.
# It must emit hookSpecificOutput.additionalContext, NOT decision:block.
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | bash "$STOP_HOOK")
assert_contains "reflection nudge fires" "$out" 'Drop one thing a teammate would not have known walking in cold'
assert_contains "nudge uses additionalContext" "$out" '"hookEventName": "Stop"'
assert_not_contains "nudge is NOT a block decision" "$out" '"decision": "block"'

# Clean up reflection marker for next test
rm -f "/tmp/legion-reflected-${CWD_HASH}"

echo "==> reflection nudge is one-shot (marker guard)"
# First call set the marker above; a second call must not re-nudge.
touch "/tmp/legion-reflected-${CWD_HASH}"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | bash "$STOP_HOOK")
assert_empty "marker suppresses a second nudge" "$out"
rm -f "/tmp/legion-reflected-${CWD_HASH}"

echo "==> stop_hook_active suppresses the nudge (loop guard, #569)"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\",\"stop_hook_active\":true}" | bash "$STOP_HOOK")
assert_empty "stop_hook_active path produces no nudge" "$out"
rm -f "/tmp/legion-reflected-${CWD_HASH}"

echo "==> bypass via LEGION_SKIP_STOP_BLOCK=1 exits 0"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | FAKE_KANBAN_ACCEPTED="Ship the thing" LEGION_SKIP_STOP_BLOCK=1 bash "$STOP_HOOK")
assert_empty "bypass produces no output" "$out"

echo "==> watch-pty (#492) skips both gates"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | FAKE_KANBAN_ACCEPTED="Ship the thing" LEGION_SPAWN_SOURCE=watch-pty bash "$STOP_HOOK")
assert_empty "watch-pty bypass produces no output" "$out"

echo "==> watch-pty branch ignores non-matching values"
# Any value other than "watch-pty" must NOT trip the bypass -- with an Accepted
# card the in-progress block should still fire.
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | FAKE_KANBAN_ACCEPTED="Ship the thing" LEGION_SPAWN_SOURCE=manual-test bash "$STOP_HOOK")
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

# Clean up reflection marker before final test
rm -f "/tmp/legion-reflected-${CWD_HASH}"

echo "==> clean board, work present: reflection nudge fires"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"no-card-session\"}" | bash "$STOP_HOOK")
assert_contains "nudge fires on a clean board" "$out" 'Drop one thing a teammate would not have known walking in cold'

finish_tests
