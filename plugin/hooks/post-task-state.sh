#!/bin/bash
# Legion PostToolUse hook (#461): mirror TaskCreate and TaskUpdate events
# into a per-session task-state log so the Stop hook can refuse to let
# the agent end with incomplete work.
#
# The harness owns the live TaskList state but doesn't expose it on a
# stable on-disk path. Legion maintains its own append-only event log at
# ${XDG_STATE_HOME}/legion/tasks-<session>.jsonl. The Stop hook reduces
# over the log to determine current task statuses.
#
# Event shape (one JSON object per line):
#   {"ts":"...","event":"create","task_id":"...","subject":"...","status":"pending"}
#   {"ts":"...","event":"update","task_id":"...","status":"in_progress"}
#   {"ts":"...","event":"update","task_id":"...","status":"completed"}
#
# Skip via LEGION_SKIP_TASK_STATE=1.
#
# Fail-open: missing jq, missing session_id, malformed input -- exit 0
# silently. Failing this hook should never block the agent's actual
# tool call.

set -u

if [ "${LEGION_SKIP_TASK_STATE:-}" = "1" ]; then
  exit 0
fi

if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

if [ -z "$SESSION_ID" ]; then
  exit 0
fi

case "$TOOL" in
  TaskCreate|TaskUpdate) ;;
  *) exit 0 ;;
esac

STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/legion"
mkdir -p "$STATE_DIR" 2>/dev/null
LOG="${STATE_DIR}/tasks-${SESSION_ID}.jsonl"

TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)

if [ "$TOOL" = "TaskCreate" ]; then
  # TaskCreate returns an id in the tool result; the input carries the
  # subject. The harness's TaskCreate response shape is {"id": "...",
  # "subject": "...", "status": "pending"}. We pull both id and subject.
  TASK_ID=$(legion_hook_field '.tool_response.id')
  SUBJECT=$(legion_hook_field '.tool_input.subject')
  if [ -z "$TASK_ID" ]; then
    # Some harness versions report the id under a different field; skip
    # silently rather than write a half-event.
    exit 0
  fi
  jq -c -n --arg ts "$TS" --arg id "$TASK_ID" --arg subject "$SUBJECT" \
    '{ts: $ts, event: "create", task_id: $id, subject: $subject, status: "pending"}' \
    >> "$LOG" 2>/dev/null
else
  # TaskUpdate carries task_id + the status field that changed.
  TASK_ID=$(legion_hook_field '.tool_input.task_id')
  STATUS=$(legion_hook_field '.tool_input.status')
  if [ -z "$TASK_ID" ] || [ -z "$STATUS" ]; then
    exit 0
  fi
  jq -c -n --arg ts "$TS" --arg id "$TASK_ID" --arg status "$STATUS" \
    '{ts: $ts, event: "update", task_id: $id, status: $status}' \
    >> "$LOG" 2>/dev/null
fi

exit 0
