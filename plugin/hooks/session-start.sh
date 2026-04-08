#!/bin/bash
# Legion SessionStart hook: last reflection + status
INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")

# Clean up markers from previous session so hooks fire fresh
CWD_HASH=$(echo "$CWD" | md5 -q 2>/dev/null || echo "$CWD" | md5sum 2>/dev/null | cut -d' ' -f1)
rm -f "/tmp/legion-reflected-${CWD_HASH}" 2>/dev/null
rm -f "/tmp/legion-work-${CWD_HASH}" 2>/dev/null
rm -f "/tmp/legion-recall-nudge-${CWD_HASH}" 2>/dev/null
rm -f "/tmp/legion-channel-${REPO}" 2>/dev/null

# Mark session as having done work (used by stop hook)
touch "/tmp/legion-work-${CWD_HASH}"

# Append non-empty text to OUTPUT, separated by double newlines
append() {
  local text="$1"
  if [ -n "$text" ]; then
    if [ -n "$OUTPUT" ]; then
      OUTPUT="$OUTPUT"$'\n\n'"$text"
    else
      OUTPUT="$text"
    fi
  fi
}

OUTPUT=""

# 1. Last self-reflection -- where you left off
LAST=$(legion recall --repo "$REPO" --latest --limit 1 2>/dev/null)
append "$LAST"

# 2. Status -- task count, direct signals, what changed
append "$(legion status --repo "$REPO" 2>/dev/null)"

if [ -n "$OUTPUT" ]; then
  jq -n --arg ctx "$OUTPUT" '{
    "hookSpecificOutput": {
      "hookEventName": "SessionStart",
      "additionalContext": $ctx
    }
  }'
fi
