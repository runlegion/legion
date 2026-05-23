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

# Session-end handoff (#493): optimistic expediter for the watch
# reaper. Lets the reaper skip a poll cycle when the agent has cleanly
# exited and CC fires this hook. PTY EOF + PID-poll remain the
# authoritative completion signal; this is a speed-up only, so the
# `|| true` swallows any failure -- the reaper still converges via EOF
# even if the CLI errors or does not exist yet (forward-compatible).
#
# The CLI subcommand `legion watch session-end --attempt-id <id>` lands
# in the watch.rs bundle PR alongside #489/#490/#491. Until then this
# call is a no-op that proves out the wire-up. Function must be defined
# before any caller; placed up here so all subsequent bypass paths can
# invoke it cleanly.
session_end_handoff() {
  if [ -n "${LEGION_WAKE_ATTEMPT_ID:-}" ] && [ -x "$LEGION_BIN" ]; then
    "$LEGION_BIN" watch session-end \
      --attempt-id "$LEGION_WAKE_ATTEMPT_ID" \
      >/dev/null 2>&1 || true
  fi
}

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
  # Explicit operator session-end: still fire the #493 handoff so a
  # watch-spawned session that exits via this bypass records its
  # exit_observed_at timestamp for the reaper.
  session_end_handoff
  exit 0
fi

# Watch-pty wakes (#492): the two gates below are calibrated for
# operator-attended sessions. Watch-spawned PTY wakes are atomic units
# that exit through this hook on every wake; running the gates risks
# the 8-block stop-hook cap in CC 2.1.143 and adds noise without
# proportionate signal. Reaper observes EOF and continues regardless.
# Rationale + per-gate decisions in docs/decisions/2026-05-watch-pty-env-audit.md.
#
# Forward-compatible: LEGION_SPAWN_SOURCE is not set by anything in the
# legion code today; #489 introduces the PTY spawn branch that stamps
# it. Until then this block is dead code that proves out the wire-up.
if [ "${LEGION_SPAWN_SOURCE:-}" = "watch-pty" ]; then
  if [ -x "$LEGION_BIN" ] && [ -n "$SESSION_ID" ]; then
    "$LEGION_BIN" telemetry record-bypass \
      --repo "$REPO" \
      --session-id "$SESSION_ID" \
      --tool Stop \
      --pattern "watch-pty-skip" \
      --bypass-reason "env:LEGION_SPAWN_SOURCE=watch-pty" \
      2>/dev/null || true
  fi
  session_end_handoff
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
  # Agent is exiting (no block fired); fire the #493 handoff so the
  # reaper can skip a poll cycle.
  session_end_handoff
  exit 0
fi

# Skip if session had no real work
WORK_MARKER="/tmp/legion-work-${CWD_HASH}"
if [ ! -f "$WORK_MARKER" ]; then
  # Same as above -- clean exit path, fire the handoff.
  session_end_handoff
  exit 0
fi

touch "$MARKER"

jq -n --arg reason "Drop one thing a teammate would not have known walking in cold -- a gotcha, a hidden invariant, how something actually works. Not what you did; the finding itself. Store it: legion reflect --repo $REPO --text '<finding>'. Skip if nothing surprising came up." '{
  "decision": "block",
  "reason": $reason
}'
