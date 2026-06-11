#!/bin/bash
# Test runner for the stop-hook gates.
#
# Exercises stop.sh's two-layer enforcement at its CURRENT contract:
# - In-progress gate (#461 -> #523): reads the KANBAN BOARD via
#   `legion kanban list --json`, not the per-session task log. Blocks with
#   decision:block when an Accepted card exists. The board-derived goal (#525)
#   rides along.
# - Reflection nudge (#569): fires via hookSpecificOutput.additionalContext
#   (non-error, continues the turn), NOT decision:block.
# Also covers post-task-state.sh (still writes the task event log for other
# consumers) and the watch-pty / session-end (#492/#493) bypass paths.
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

assert_not_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if echo "$haystack" | grep -q -- "$needle"; then
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected NOT to find: $needle" >&2
    echo "    in: $haystack" >&2
  else
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
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

mkdir -p "$WORK/plugin/bin" "$WORK/plugin/hooks" "$WORK/state/legion"
cp plugin/hooks/post-task-state.sh "$WORK/plugin/hooks/"
cp plugin/hooks/stop.sh "$WORK/plugin/hooks/"
mkdir -p "$WORK/plugin/hooks/lib"
cp plugin/hooks/lib/prelude.sh plugin/hooks/lib/emit.sh "$WORK/plugin/hooks/lib/"

# Stub legion. Logs every invocation to $LEGION_STUB_LOG when set (so the #493
# handoff tests can assert call shape). Returns an Accepted kanban card when
# STUB_ACCEPTED is set (so the in-progress gate fires), and a goal line when
# STUB_GOAL is set (so the #525 enrichment is exercised). Everything else is a
# silent no-op exit 0 -- matching a clean board / unsupported subcommand.
cat > "$WORK/plugin/bin/legion" <<'EOF'
#!/bin/bash
if [ -n "${LEGION_STUB_LOG:-}" ]; then
  echo "$@" >> "$LEGION_STUB_LOG"
fi
case "$*" in
  *"kanban list"*)
    if [ -n "${STUB_ACCEPTED:-}" ]; then
      echo '{"status":"accepted","id":"42","title":"Ship the thing"}'
    fi
    ;;
  "goal "*|"goal")
    [ -n "${STUB_GOAL:-}" ] && printf '%s\n' "${STUB_GOAL}"
    ;;
esac
exit 0
EOF
chmod +x "$WORK/plugin/bin/legion"

export CLAUDE_PLUGIN_ROOT="$WORK/plugin"
export XDG_STATE_HOME="$WORK/state"

POST_HOOK="$WORK/plugin/hooks/post-task-state.sh"
STOP_HOOK="$WORK/plugin/hooks/stop.sh"

SESSION="test-session-461"

# Seed work marker so the reflection nudge can fire.
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

echo "==> stop hook BLOCKS (decision:block) on an Accepted kanban card"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" \
  | STUB_ACCEPTED=1 STUB_GOAL="GOAL: ship the thing" bash "$STOP_HOOK")
assert_contains "block decision present" "$out" '"decision": "block"'
assert_contains "lists the accepted card title" "$out" 'Ship the thing'
assert_contains "names bypass env" "$out" 'LEGION_SKIP_STOP_BLOCK=1'
assert_contains "surfaces the board-derived goal" "$out" 'GOAL: ship the thing'

echo "==> clean board: stop nudges for reflection via additionalContext (#569)"
# No STUB_ACCEPTED -> in-progress gate does not fire -> reflection path. It must
# emit hookSpecificOutput.additionalContext, NOT decision:block.
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
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | STUB_ACCEPTED=1 LEGION_SKIP_STOP_BLOCK=1 bash "$STOP_HOOK")
assert_empty "bypass produces no output" "$out"

echo "==> watch-pty (#492) skips both gates"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | STUB_ACCEPTED=1 LEGION_SPAWN_SOURCE=watch-pty bash "$STOP_HOOK")
assert_empty "watch-pty bypass produces no output" "$out"

echo "==> watch-pty branch ignores non-matching values"
# Any value other than "watch-pty" must NOT trip the bypass -- with an Accepted
# card the in-progress block should still fire.
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"${SESSION}\"}" | STUB_ACCEPTED=1 LEGION_SPAWN_SOURCE=manual-test bash "$STOP_HOOK")
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
if grep -q "watch session-end --attempt-id attempt-test-id" "$HANDOFF_LOG"; then
  PASS=$((PASS + 1)); echo "  PASS: watch session-end CLI invoked with attempt-id"
else
  FAIL=$((FAIL + 1)); echo "  FAIL: watch session-end CLI not invoked"
  echo "    log contents:" >&2; cat "$HANDOFF_LOG" >&2
fi

echo "==> session-end handoff is gated on LEGION_WAKE_ATTEMPT_ID"
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
if echo "$out" | grep -q '"decision": "block"'; then
  FAIL=$((FAIL + 1)); echo "  FAIL: missing session_id should not block"
else
  PASS=$((PASS + 1)); echo "  PASS: missing session_id passes through"
fi

# Clean up reflection marker before final test
rm -f "/tmp/legion-reflected-${CWD_HASH}"

echo "==> clean board, work present: reflection nudge fires"
out=$(echo "{\"cwd\":\"${CWD}\",\"session_id\":\"no-card-session\"}" | bash "$STOP_HOOK")
assert_contains "nudge fires on a clean board" "$out" 'Drop one thing a teammate would not have known walking in cold'

echo
echo "==> $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
