#!/bin/bash
# Legion PostToolUse hook (#358): auto-witness the prediction associated
# with a task when the task transitions to completed via TaskUpdate.
#
# Pairs with uncertainty-emit-on-task.sh: emit logs task_id -> prediction_id
# in ${XDG_STATE_HOME}/legion/uncertainty-tasks-<session>.jsonl when a task
# is created; this hook reads back the mapping when the task completes and
# fires `legion uncertainty witness <prediction-id>`.
#
# Correctness measurement: until #283 wires SCIP features (lets us compute
# token-actual vs token-predicted) and witness pulls live data from
# `legion usage --since <emit> --until <completion>`, this MVP path
# witnesses with outcome_correctness = 1.0 (task completed = positive
# outcome) and outcome_label = "shipped". The reliability curve still
# learns from the volume even with placeholder correctness values; once
# the measurement loop closes, only this script changes.
#
# Non-blocking: failures log to stderr and exit 0.
#
# Skip via LEGION_SKIP_UNCERTAINTY=1.

set -u

if [ "${LEGION_SKIP_UNCERTAINTY:-}" = "1" ]; then
  exit 0
fi

if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

if [ "$TOOL" != "TaskUpdate" ] || [ -z "$SESSION_ID" ]; then
  exit 0
fi

STATUS=$(legion_hook_field '.tool_input.status')
TASK_ID=$(legion_hook_field '.tool_input.task_id')

if [ "$STATUS" != "completed" ] || [ -z "$TASK_ID" ]; then
  exit 0
fi

STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/legion"
LOG="${STATE_DIR}/uncertainty-tasks-${SESSION_ID}.jsonl"
if [ ! -f "$LOG" ]; then
  exit 0
fi

# Last-emit-wins lookup: walk the file backwards to pick the most recent
# prediction id recorded for the task_id. tac is portable across
# coreutils + bsd userland; reverse via sed if absent.
if command -v tac >/dev/null 2>&1; then
  REVERSED=$(tac "$LOG")
else
  REVERSED=$(awk '{a[NR]=$0} END {for (i=NR; i>=1; i--) print a[i]}' "$LOG")
fi

PREDICTION_ID=$(echo "$REVERSED" | jq -r --arg task_id "$TASK_ID" \
  'select(.task_id == $task_id) | .prediction_id' 2>/dev/null | head -n1)

if [ -z "$PREDICTION_ID" ]; then
  exit 0
fi

# Binary resolved by the prelude: plugin-bundled copy first, PATH fallback
# (#614 -- the old PATH-only lookup left this hook silently inert in hook
# subshells, which do not inherit the plugin bin dir on PATH).
if [ -z "$LEGION" ] || [ ! -x "$LEGION" ]; then
  exit 0
fi

# Placeholder until #283 wires real measurement; see header.
"$LEGION" uncertainty witness "$PREDICTION_ID" \
  --outcome-label "shipped" \
  --outcome-correctness "1.0" \
  --payload '{"placeholder":true}' >/dev/null 2>&1

exit 0
