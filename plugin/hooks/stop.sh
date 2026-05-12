#!/bin/bash
# Legion Stop hook.
#
# Two-layer enforcement on every Stop event:
#
# 1. INCOMPLETE-WORK GATE (#461). If the harness has pending/in_progress
#    TaskList items in this session, block the Stop with a directive that
#    forces the agent to either complete, mark blocked, mark needs_input,
#    or cancel each one. No silent abandonment of multi-step plans.
#
# 2. REFLECTION PROMPT. If work happened this session and the reflection
#    hasn't fired yet, prompt for one. Skip when nothing was learned.
#
# Bypass: LEGION_SKIP_STOP_BLOCK=1 env skips both gates. Writes a
# telemetry row via `legion telemetry record-bypass` so the escape is
# visible to #440's summary.

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")
CWD_HASH=$(echo "$CWD" | md5 -q 2>/dev/null || echo "$CWD" | md5sum 2>/dev/null | cut -d' ' -f1)

LEGION_BIN="${CLAUDE_PLUGIN_ROOT}/bin/legion"

# Bypass: skip both gates, log the escape if telemetry is available.
if [ "${LEGION_SKIP_STOP_BLOCK:-}" = "1" ]; then
  if [ -x "$LEGION_BIN" ] && [ -n "$SESSION_ID" ]; then
    "$LEGION_BIN" telemetry record-bypass \
      --repo "$REPO" \
      --session-id "$SESSION_ID" \
      --tool Stop \
      --pattern "session-end" \
      --bypass-reason "env:LEGION_SKIP_STOP_BLOCK=1" \
      2>/dev/null || true
  fi
  exit 0
fi

# ---------- (1) Incomplete-work gate ----------
#
# Reduce over the session task-state log written by post-task-state.sh
# (#461 PostToolUse hook). Last-status-wins per task_id; block if any
# task ends in pending or in_progress.

STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/legion"
TASK_LOG="${STATE_DIR}/tasks-${SESSION_ID}.jsonl"

if [ -n "$SESSION_ID" ] && [ -f "$TASK_LOG" ] && command -v jq >/dev/null 2>&1; then
  # Walk the event log, folding each event into a per-task accumulator
  # that preserves the original subject (from the create event) while
  # tracking the latest status. Output lines for tasks still in pending
  # or in_progress.
  INCOMPLETE=$(jq -s -r '
    reduce .[] as $e ({};
      .[$e.task_id] = (
        (.[$e.task_id] // {}) +
        {status: $e.status, subject: ($e.subject // (.[$e.task_id].subject // ""))}
      )
    )
    | to_entries
    | map(select(.value.status == "pending" or .value.status == "in_progress"))
    | map("- " + .key + " [" + .value.status + "]: " + (.value.subject // "<no subject>"))
    | .[]
  ' "$TASK_LOG" 2>/dev/null)

  if [ -n "$INCOMPLETE" ]; then
    REASON="You have incomplete tasks in this session and cannot stop yet. Open items:

${INCOMPLETE}

Each one needs an explicit terminal state before you stop:
- complete the work and run TaskUpdate(status=completed)
- mark deleted with a reason if it is no longer relevant
- if you are stuck, EXPLICITLY say so -- either post a blocked-note to legion or open a kanban needs_input card so the human-side queue picks it up

\"Should I keep going?\" is not a question to ask the operator after every step. The plan is the permission. Keep going.

To bypass (rare, diagnostics or explicit operator session-end), set LEGION_SKIP_STOP_BLOCK=1. The bypass writes one row to bypass.jsonl."

    jq -n --arg reason "$REASON" '{decision: "block", reason: $reason}'
    exit 0
  fi
fi

# ---------- (2) Reflection prompt ----------
#
# Prevent re-fires: one reflect prompt per session
MARKER="/tmp/legion-reflected-${CWD_HASH}"
if [ -f "$MARKER" ]; then
  exit 0
fi

# Skip if session had no real work
WORK_MARKER="/tmp/legion-work-${CWD_HASH}"
if [ ! -f "$WORK_MARKER" ]; then
  exit 0
fi

touch "$MARKER"

jq -n --arg reason "What would you tell another agent who hits this same problem tomorrow? Store it: legion reflect --repo $REPO --text '<your reflection>'. Skip if nothing surprising happened." '{
  "decision": "block",
  "reason": $reason
}'
