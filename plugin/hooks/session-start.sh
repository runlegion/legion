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

# 2. Status -- compact one-liner with counts
STATUS_JSON=$(legion status --repo "$REPO" --json 2>/dev/null)
if [ -n "$STATUS_JSON" ]; then
  TASKS=$(echo "$STATUS_JSON" | jq -r '.tasks // 0')
  BLOCKED=$(echo "$STATUS_JSON" | jq -r '.blocked // 0')
  NEEDS=$(echo "$STATUS_JSON" | jq -r '.team_needs // 0')
  CHANGED=$(echo "$STATUS_JSON" | jq -r '.what_changed // 0')
  PARTS=""
  if [ "$TASKS" -gt 0 ]; then
    if [ "$BLOCKED" -gt 0 ]; then
      PARTS="${TASKS} tasks (${BLOCKED} blocked)"
    else
      PARTS="${TASKS} tasks"
    fi
  fi
  if [ "$NEEDS" -gt 0 ]; then
    [ -n "$PARTS" ] && PARTS="${PARTS}, "
    PARTS="${PARTS}${NEEDS} unread @${REPO}"
  fi
  if [ "$CHANGED" -gt 0 ]; then
    [ -n "$PARTS" ] && PARTS="${PARTS}, "
    PARTS="${PARTS}${CHANGED} updates"
  fi
  if [ -n "$PARTS" ]; then
    append "[Legion] ${PARTS} -- legion bullpen for details"
  fi
fi

if [ -n "$OUTPUT" ]; then
  jq -n --arg ctx "$OUTPUT" '{
    "hookSpecificOutput": {
      "hookEventName": "SessionStart",
      "additionalContext": $ctx
    }
  }'
fi
