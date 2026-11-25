#!/bin/bash
# Test runner for the uncertainty engine hooks (#358).
#
# Exercises:
# - uncertainty-emit-on-task.sh: PostToolUse on TaskCreate writes a row to
#   the uncertainty-tasks-<session>.jsonl mapping file when emit succeeds.
# - uncertainty-witness-on-completion.sh: PostToolUse on TaskUpdate where
#   status=completed reads the mapping and fires witness with placeholder
#   correctness.
#
# Both hooks are non-blocking: missing jq, missing session_id, malformed
# tool input, or emit/witness CLI failures all log and exit 0 silently.
#
# Run from anywhere:
#   bash plugin/hooks/test-uncertainty-hooks.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root uncertainty-emit-on-task.sh uncertainty-witness-on-completion.sh

EMIT_HOOK="$CLAUDE_PLUGIN_ROOT/hooks/uncertainty-emit-on-task.sh"
WITNESS_HOOK="$CLAUDE_PLUGIN_ROOT/hooks/uncertainty-witness-on-completion.sh"

SESSION="test-session-358"
MAP_LOG="$XDG_STATE_HOME/legion/uncertainty-tasks-${SESSION}.jsonl"
export FAKE_WITNESS_LOG="$XDG_STATE_HOME/legion/witness-calls.log"

echo "==> emit hook writes mapping on TaskCreate"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"Port uncertainty engine\"},\"tool_response\":{\"id\":\"task-001\"}}" | bash "$EMIT_HOOK" >/dev/null 2>&1
assert_eq "emit hook exit code is 0 on happy path" "$?" "0"
assert_file_contains "mapping file has task-001 -> pred-fixed-1" "$MAP_LOG" '"task_id":"task-001"'
assert_file_contains "mapping file has prediction_id" "$MAP_LOG" '"prediction_id":"pred-fixed-1"'

echo "==> emit hook skips non-TaskCreate tools"
rm -f "$XDG_STATE_HOME/legion/uncertainty-tasks-skip.jsonl"
echo "{\"session_id\":\"skip\",\"tool_name\":\"Bash\",\"tool_input\":{},\"tool_response\":{}}" | bash "$EMIT_HOOK"
assert_eq "non-TaskCreate exit code is 0" "$?" "0"
assert_file_absent "no mapping file for non-TaskCreate" "$XDG_STATE_HOME/legion/uncertainty-tasks-skip.jsonl"

echo "==> emit hook skips when session_id is missing"
echo "{\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"x\"},\"tool_response\":{\"id\":\"task-002\"}}" | bash "$EMIT_HOOK"
assert_eq "missing session_id exit code is 0" "$?" "0"

echo "==> emit hook skips when task_id is missing"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"x\"},\"tool_response\":{}}" | bash "$EMIT_HOOK"
assert_eq "missing task_id exit code is 0" "$?" "0"

echo "==> emit hook is non-blocking on legion stub failure"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"fail-shape\"},\"tool_response\":{\"id\":\"task-003\"}}" | FAKE_BROKEN=1 bash "$EMIT_HOOK"
assert_eq "emit failure still exits 0" "$?" "0"

echo "==> witness hook fires on TaskUpdate completed"
rm -f "$FAKE_WITNESS_LOG"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"task-001\",\"status\":\"completed\"}}" | bash "$WITNESS_HOOK"
assert_eq "witness hook exit code is 0" "$?" "0"
assert_file_contains "witness was called with the prediction id" "$FAKE_WITNESS_LOG" 'pred-fixed-1'
assert_file_contains "witness uses placeholder shipped label" "$FAKE_WITNESS_LOG" '--outcome-label shipped'
assert_file_contains "witness uses correctness 1.0" "$FAKE_WITNESS_LOG" '--outcome-correctness 1.0'

echo "==> witness hook skips on non-completed status"
rm -f "$FAKE_WITNESS_LOG"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"task-001\",\"status\":\"in_progress\"}}" | bash "$WITNESS_HOOK"
assert_eq "non-completed status exit code is 0" "$?" "0"
assert_file_absent "witness not called on in_progress" "$FAKE_WITNESS_LOG"

echo "==> witness hook skips when mapping log is absent"
rm -f "$FAKE_WITNESS_LOG"
echo "{\"session_id\":\"missing\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"task-999\",\"status\":\"completed\"}}" | bash "$WITNESS_HOOK"
assert_eq "missing mapping exit code is 0" "$?" "0"
assert_file_absent "witness not called when mapping is absent" "$FAKE_WITNESS_LOG"

echo "==> witness hook skips when task_id is not in the mapping"
rm -f "$FAKE_WITNESS_LOG"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"task-not-in-map\",\"status\":\"completed\"}}" | bash "$WITNESS_HOOK"
assert_eq "unknown task_id exit code is 0" "$?" "0"
assert_file_absent "witness not called for unknown task_id" "$FAKE_WITNESS_LOG"

echo "==> LEGION_SKIP_UNCERTAINTY=1 disables both hooks"
rm -f "$FAKE_WITNESS_LOG"
SKIP_MAP="$XDG_STATE_HOME/legion/uncertainty-tasks-skip-session.jsonl"
echo "{\"session_id\":\"skip-session\",\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"x\"},\"tool_response\":{\"id\":\"task-skip\"}}" | LEGION_SKIP_UNCERTAINTY=1 bash "$EMIT_HOOK"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"task-001\",\"status\":\"completed\"}}" | LEGION_SKIP_UNCERTAINTY=1 bash "$WITNESS_HOOK"
assert_file_absent "skip flag suppresses emit mapping write" "$SKIP_MAP"
assert_file_absent "skip flag suppresses witness call" "$FAKE_WITNESS_LOG"

finish_tests
