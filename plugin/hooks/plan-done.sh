#!/usr/bin/env bash
# Stub: fires after ExitPlanMode tool use (user approved a plan draft).
# Proves the wire. Production version will parse tool_input.plan and call
# `legion plan open` to persist the plan into the DB, returning its ID as
# additionalContext.
set -euo pipefail

input=$(cat)
tool_name=$(printf '%s' "$input" | jq -r '.tool_name // ""')

if [ "$tool_name" != "ExitPlanMode" ]; then
  exit 0
fi

jq -n '{
  hookSpecificOutput: {
    hookEventName: "PostToolUse",
    additionalContext: "plan done!"
  }
}'
