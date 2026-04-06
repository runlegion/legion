#!/bin/bash
# Legion Stop hook: one reflection if you learned something non-obvious.
# Full wind-down belongs in /snooze, not here.
INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")
CWD_HASH=$(echo "$CWD" | md5 -q 2>/dev/null || echo "$CWD" | md5sum 2>/dev/null | cut -d' ' -f1)

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
