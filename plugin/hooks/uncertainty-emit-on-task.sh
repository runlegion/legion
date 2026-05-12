#!/bin/bash
# Legion PostToolUse hook (#358): auto-emit an uncertainty prediction when
# the agent creates a new task via the harness TaskCreate tool.
#
# Pillar 2: predictions need volume to calibrate. Manual `legion uncertainty
# emit` works for tests; the production path is hook-driven so every task
# the agent takes on produces a row for the calibrator.
#
# Until SCIP feature extraction lands (#283), we ship a conservative
# placeholder: feature_key = "task.generic", claimed_confidence = 0.5,
# payload = {task_id, subject}. The calibration roller in #359 will still
# learn a per-(surface, model) reliability curve from these rows even
# without the richer feature_key dimension.
#
# Maps task_id -> prediction_id in a per-session JSONL so the witness
# hook (uncertainty-witness-on-completion.sh) can find the row to update
# when the task completes.
#
# Non-blocking: failures (missing jq, malformed input, legion CLI error)
# log to stderr and exit 0. The hook must never break the agent.
#
# Skip via LEGION_SKIP_UNCERTAINTY=1.

set -u

if [ "${LEGION_SKIP_UNCERTAINTY:-}" = "1" ]; then
  exit 0
fi

if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

INPUT=$(cat)
if [ -z "$INPUT" ]; then
  exit 0
fi

TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)

if [ "$TOOL" != "TaskCreate" ] || [ -z "$SESSION_ID" ]; then
  exit 0
fi

TASK_ID=$(echo "$INPUT" | jq -r '.tool_response.id // empty' 2>/dev/null)
SUBJECT=$(echo "$INPUT" | jq -r '.tool_input.subject // empty' 2>/dev/null)

if [ -z "$TASK_ID" ] || [ -z "$SUBJECT" ]; then
  exit 0
fi

# Cap subject at 1024 chars so a pathological agent prompt does not
# balloon the DB row. Task subjects are typically 1-3 sentences -- a
# 1KB ceiling is far above real usage and well below "bloat the table"
# scale.
SUBJECT=$(printf '%s' "$SUBJECT" | cut -c 1-1024)

# Resolve legion binary. Prefer the plugin-bundled copy if present.
LEGION_BIN="${LEGION_BIN:-legion}"
if ! command -v "$LEGION_BIN" >/dev/null 2>&1; then
  exit 0
fi

# Fingerprint the task by (subject) so identical subjects share a cohort
# row across sessions. sha256 keeps the field compact and url-safe.
if command -v sha256sum >/dev/null 2>&1; then
  FINGERPRINT=$(printf '%s' "$SUBJECT" | sha256sum | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
  FINGERPRINT=$(printf '%s' "$SUBJECT" | shasum -a 256 | cut -d' ' -f1)
else
  exit 0
fi

# Conservative defaults until #283 wires SCIP features.
SURFACE="legion.task"
FEATURE_KEY="task.generic"
MODEL="${LEGION_AGENT_MODEL:-claude-opus-4-7}"
MODEL_VERSION="${LEGION_AGENT_MODEL_VERSION:-unknown}"
CLAIMED_CONFIDENCE="0.5"
ORPHAN_TTL_DAYS="30"

PAYLOAD=$(jq -c -n --arg task_id "$TASK_ID" --arg subject "$SUBJECT" \
  '{task_id: $task_id, subject: $subject}')

EMIT_OUT=$("$LEGION_BIN" uncertainty emit \
  --surface "$SURFACE" \
  --feature-key "$FEATURE_KEY" \
  --input-fingerprint "$FINGERPRINT" \
  --model "$MODEL" \
  --model-version "$MODEL_VERSION" \
  --claimed-confidence "$CLAIMED_CONFIDENCE" \
  --payload "$PAYLOAD" \
  --orphan-ttl-days "$ORPHAN_TTL_DAYS" 2>/dev/null)

if [ -z "$EMIT_OUT" ]; then
  exit 0
fi

PREDICTION_ID=$(echo "$EMIT_OUT" | jq -r '.id // empty' 2>/dev/null)
if [ -z "$PREDICTION_ID" ]; then
  exit 0
fi

# Map task_id -> prediction_id so the witness hook can find the row when
# the task transitions to completed. Same per-session JSONL pattern the
# #461 task-state hook uses; isolated namespace by filename.
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/legion"
mkdir -p "$STATE_DIR" 2>/dev/null
LOG="${STATE_DIR}/uncertainty-tasks-${SESSION_ID}.jsonl"
TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)

jq -c -n \
  --arg ts "$TS" \
  --arg task_id "$TASK_ID" \
  --arg prediction_id "$PREDICTION_ID" \
  '{ts: $ts, task_id: $task_id, prediction_id: $prediction_id}' \
  >> "$LOG" 2>/dev/null

exit 0
