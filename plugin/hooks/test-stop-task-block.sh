#!/bin/bash
# Test runner for the stop-hook task-state block (#461).
#
# Builds a fake CLAUDE_PLUGIN_ROOT and exercises both:
# - post-task-state.sh: writes one event row per TaskCreate/TaskUpdate.
# - stop.sh: reads the event log, blocks when any task is incomplete.
#
# Run from the repo root:
#   bash plugin/hooks/test-stop-task-block.sh

set -u

PASS=0
FAIL=0

assert_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if echo "$haystack" | grep -q -- "$needle"; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected to find: $needle" >&2
    echo "    in: $haystack" >&2
  fi
}

assert_empty() {
  local desc="$1" actual="$2"
  if [ -z "$actual" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected empty, got: $actual" >&2
  fi
}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/plugin/bin" "$WORK/plugin/hooks" "$WORK/state/legion"
cp plugin/hooks/post-task-state.sh "$WORK/plugin/hooks/"
cp plugin/hooks/stop.sh "$WORK/plugin/hooks/"

# Stub legion binary that logs every invocation to $LEGION_STUB_LOG
# (when set) so #493 handoff tests can assert call shape, then no-ops.
cat > "$WORK/plugin/bin/legion" <<'EOF'
#!/bin/bash
if [ -n "${LEGION_STUB_LOG:-}" ]; then
  echo "$@" >> "$LEGION_STUB_LOG"
fi
exit 0
EOF
chmod +x "$WORK/plugin/bin/legion"

export CLAUDE_PLUGIN_ROOT="$WORK/plugin"
export XDG_STATE_HOME="$WORK/state"

POST_HOOK="$WORK/plugin/hooks/post-task-state.sh"
STOP_HOOK="$WORK/plugin/hooks/stop.sh"

SESSION="test-session-461"

# Seed work marker so the reflection prompt would normally fire (we
# want to verify the task-block path runs BEFORE the reflection path).
CWD="/tmp/legion-test"
CWD_HASH=$(echo "$CWD" | md5 -q 2>/dev/null || echo "$CWD" | md5sum | cut -d' ' -f1)
touch "/tmp/legion-work-${CWD_HASH}"
trap 'rm -rf "$WORK" /tmp/legion-work-'"${CWD_HASH}"' /tmp/legion-reflected-'"${CWD_HASH}" EXIT

echo "==> post-task-state appends create event"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskCreate\",\"tool_input\":{\"subject\":\"Ship the thing\"},\"tool_response\":{\"id\":\"1\",\"status\":\"pending\"}}" | bash "$POST_HOOK"
LOG="$WORK/state/legion/tasks-${SESSION}.jsonl"
if [ -f "$LOG" ] && grep -q '"event":"create"' "$LOG" && grep -q '"task_id":"1"' "$LOG"; then
  PASS=$((PASS + 1)); echo "  PASS: create event written"
else
  FAIL=$((FAIL + 1)); echo "  FAIL: create event missing"
  [ -f "$LOG" ] && cat "$LOG" >&2
fi

echo "==> post-task-state appends update event (in_progress)"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"1\",\"status\":\"in_progress\"}}" | bash "$POST_HOOK"
if grep -q '"event":"update"' "$LOG" && grep -q '"status":"in_progress"' "$LOG"; then
  PASS=$((PASS + 1)); echo "  PASS: update event written"
else
  FAIL=$((FAIL + 1)); echo "  FAIL: update event missing"
fi

echo "==> stop hook BLOCKS with in_progress task"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | bash "$STOP_HOOK")
assert_contains "block decision present" "$out" '"decision": "block"'
assert_contains "lists incomplete task id" "$out" '- 1 \[in_progress\]'
assert_contains "lists subject" "$out" 'Ship the thing'
assert_contains "names bypass env" "$out" 'LEGION_SKIP_STOP_BLOCK=1'

echo "==> mark complete, then stop passes through to reflection path"
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"1\",\"status\":\"completed\"}}" | bash "$POST_HOOK"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | bash "$STOP_HOOK")
# With completed tasks but unfired reflection marker, the reflection
# prompt should fire (since /tmp/legion-work marker exists from the
# trap setup).
assert_contains "reflection prompt fires" "$out" 'Drop one thing a teammate would not have known walking in cold'

# Clean up reflection marker for next test
rm -f "/tmp/legion-reflected-${CWD_HASH}"

echo "==> bypass via LEGION_SKIP_STOP_BLOCK=1 exits 0"
# Re-seed with an in_progress task.
echo "{\"session_id\":\"${SESSION}\",\"tool_name\":\"TaskUpdate\",\"tool_input\":{\"task_id\":\"1\",\"status\":\"in_progress\"}}" | bash "$POST_HOOK"
out=$(LEGION_SKIP_STOP_BLOCK=1 echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | LEGION_SKIP_STOP_BLOCK=1 bash "$STOP_HOOK")
assert_empty "bypass produces no output" "$out"

echo "==> watch-pty (#492) skips both gates"
# Same in_progress task in the log; LEGION_SPAWN_SOURCE=watch-pty must
# take precedence and exit 0 silently, just like LEGION_SKIP_STOP_BLOCK.
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | LEGION_SPAWN_SOURCE=watch-pty bash "$STOP_HOOK")
assert_empty "watch-pty bypass produces no output" "$out"

echo "==> watch-pty branch ignores non-matching values"
# Any value other than "watch-pty" must NOT trip the bypass -- the
# block gate should still fire on the in_progress task.
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | LEGION_SPAWN_SOURCE=manual-test bash "$STOP_HOOK")
assert_contains "manual-test does not trigger watch-pty bypass" "$out" '"decision": "block"'

echo "==> session-end handoff (#493) fires under watch-pty when wake-attempt set"
# LEGION_WAKE_ATTEMPT_ID set + LEGION_SPAWN_SOURCE=watch-pty: hook
# must invoke `legion watch session-end --attempt-id <id>` via the
# stub. Assert via the recorded call log.
HANDOFF_LOG="$WORK/handoff.log"
: > "$HANDOFF_LOG"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" \
  | LEGION_SPAWN_SOURCE=watch-pty \
    LEGION_WAKE_ATTEMPT_ID="attempt-test-id" \
    LEGION_STUB_LOG="$HANDOFF_LOG" \
    bash "$STOP_HOOK")
assert_empty "handoff path still produces no output" "$out"
if grep -q "watch session-end --attempt-id attempt-test-id" "$HANDOFF_LOG"; then
  PASS=$((PASS + 1)); echo "  PASS: watch session-end CLI invoked with attempt-id"
else
  FAIL=$((FAIL + 1)); echo "  FAIL: watch session-end CLI not invoked"
  echo "    log contents:" >&2; cat "$HANDOFF_LOG" >&2
fi

echo "==> session-end handoff is gated on LEGION_WAKE_ATTEMPT_ID"
# Without the env var, handoff must NOT fire (avoids polluting
# non-watch sessions with spurious watch CLI calls).
: > "$HANDOFF_LOG"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" \
  | LEGION_SPAWN_SOURCE=watch-pty \
    LEGION_STUB_LOG="$HANDOFF_LOG" \
    bash "$STOP_HOOK")
if grep -q "watch session-end" "$HANDOFF_LOG"; then
  FAIL=$((FAIL + 1)); echo "  FAIL: handoff fired without LEGION_WAKE_ATTEMPT_ID"
  cat "$HANDOFF_LOG" >&2
else
  PASS=$((PASS + 1)); echo "  PASS: missing LEGION_WAKE_ATTEMPT_ID suppresses handoff"
fi

echo "==> no session_id: hook passes through"
out=$(echo "{\"cwd\":\"${CWD}\"}" | bash "$STOP_HOOK")
# Should NOT block on task state since no session = no task log.
# May still emit reflection prompt if work marker is present, but
# task-block path is bypassed cleanly.
if echo "$out" | grep -q 'decision.*block.*incomplete tasks'; then
  FAIL=$((FAIL + 1)); echo "  FAIL: missing session_id should not trigger task block"
else
  PASS=$((PASS + 1)); echo "  PASS: missing session_id skips task block"
fi

# Clean up reflection marker before missing-log test
rm -f "/tmp/legion-reflected-${CWD_HASH}"

echo "==> no task log: hook passes through to reflection"
SESSION2="no-tasks-session"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION2}\"}" | bash "$STOP_HOOK")
# No task log for SESSION2 -> task-block path skipped -> reflection fires
# (work marker present).
assert_contains "no task log falls through to reflection" "$out" 'Drop one thing a teammate would not have known walking in cold'

echo
echo "==> $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
