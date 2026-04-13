#!/bin/bash
# Legion SessionStart hook: focused bootstrap.
#
# Injects exactly three things:
#   1. Identity  -- who am I (domain: identity)
#   2. Last snooze -- what was I doing (domain: snooze)
#   3. Work source -- what's on my plate (kanban list)
#
# Everything else (bulk recall, surface, bullpen) is pulled on demand
# during the session via recall/consult/bullpen commands.
#
# Also warms the Tantivy index in the background so the first PreToolUse
# recall hit is fast (cold ~2.2s, warm ~170ms).

LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

# Shared warning helper -- see #209.
# shellcheck source=_legion-warn.sh
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-warn.sh"

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")

# Clean up per-session markers from prior session
CWD_HASH=$(echo "$CWD" | md5 -q 2>/dev/null || echo "$CWD" | md5sum 2>/dev/null | cut -d' ' -f1)
rm -f "/tmp/legion-reflected-${CWD_HASH}" 2>/dev/null

# Mark session as having done work (used by Stop hook to decide if reflect prompt fires)
touch "/tmp/legion-work-${CWD_HASH}"

# Warm the Tantivy index in the background
("$LEGION" recall --repo "$REPO" --context warmup --limit 1 >/dev/null 2>&1 &)

# Sync GitHub issues into kanban (5-second timeout, opt-out via LEGION_NO_SYNC=1)
if [ "${LEGION_NO_SYNC:-}" != "1" ]; then
  if command -v gtimeout >/dev/null 2>&1; then
    gtimeout 5 "$LEGION" sync --repo "$REPO" >/dev/null 2>>"$LOG"
  else
    perl -e 'alarm 5; exec @ARGV' -- "$LEGION" sync --repo "$REPO" >/dev/null 2>>"$LOG"
  fi
  SYNC_RC=$?
  if [ "$SYNC_RC" -ne 0 ] && [ "$SYNC_RC" -ne 124 ] && [ "$SYNC_RC" -ne 142 ]; then
    legion_check "$SYNC_RC" "sync"
  fi
fi

OUTPUT=""

# 1. Identity -- who am I
IDENTITY=$("$LEGION" recall --repo "$REPO" --domain identity --limit 1 2>>"$LOG")
legion_check $? "recall (identity)"
if [ -n "$IDENTITY" ]; then
  OUTPUT="$IDENTITY"
fi

# 2. Last snooze -- what was I doing
SNOOZE=$("$LEGION" recall --repo "$REPO" --domain snooze --limit 1 --preview 500 2>>"$LOG")
legion_check $? "recall (snooze)"
if [ -n "$SNOOZE" ]; then
  if [ -n "$OUTPUT" ]; then
    OUTPUT="${OUTPUT}"$'\n\n'"${SNOOZE}"
  else
    OUTPUT="$SNOOZE"
  fi
fi

# 3. Work source -- what's on my plate
KANBAN=$("$LEGION" kanban list --repo "$REPO" 2>>"$LOG")
legion_check $? "kanban list"
if [ -n "$KANBAN" ]; then
  KANBAN_BLOCK="[Legion] Current work:
${KANBAN}"
  if [ -n "$OUTPUT" ]; then
    OUTPUT="${OUTPUT}"$'\n\n'"${KANBAN_BLOCK}"
  else
    OUTPUT="$KANBAN_BLOCK"
  fi
fi

# Prepend warning block if any legion call failed
WARN=$(legion_warnings_block)
if [ -n "$WARN" ]; then
  if [ -n "$OUTPUT" ]; then
    OUTPUT="${WARN}"$'\n\n'"${OUTPUT}"
  else
    OUTPUT="$WARN"
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
