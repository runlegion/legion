#!/bin/bash
# Legion PreToolUse hook: notify agent when there are unread board posts
# Only injects context when there is something to read.
# Skips when channel is active (channel handles real-time delivery).

# Hook subshells do not inherit the plugin bin dir on PATH -- only the Bash
# tool does. Invoke via full CLAUDE_PLUGIN_ROOT path (fixes #204).
LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

# Shared warning helper -- surfaces legion degradation via additionalContext
# instead of silently dropping bullpen notifications. See #209.
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-warn.sh"

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")

# Channel is active -- skip polling, events arrive in real time
if [ -f "/tmp/legion-channel-${REPO}" ]; then
  exit 0
fi

# Check board for unread posts. Route stderr to the breadcrumb log (was /dev/null)
# so broken-legion failures leave a trace instead of disappearing.
BOARD_COUNT=$("$LEGION" bullpen --count --repo "$REPO" 2>>"$LOG")
legion_check $? "bullpen --count"

WARN=$(legion_warnings_block)

if [ -n "$BOARD_COUNT" ] || [ -n "$WARN" ]; then
  if [ -n "$BOARD_COUNT" ]; then
    CTX="[Legion] ${BOARD_COUNT}. Run legion bullpen --repo ${REPO} to read them."
  else
    CTX=""
  fi
  if [ -n "$WARN" ]; then
    if [ -n "$CTX" ]; then
      CTX="${WARN}"$'\n\n'"${CTX}"
    else
      CTX="$WARN"
    fi
  fi
  jq -n --arg ctx "$CTX" '{
    "hookSpecificOutput": {
      "hookEventName": "PreToolUse",
      "permissionDecision": "allow",
      "permissionDecisionReason": "legion bullpen notification",
      "additionalContext": $ctx
    }
  }'
else
  exit 0
fi
