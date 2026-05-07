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

# Clean up per-session markers from prior session. Reset both -- the Stop
# hook gates its reflect prompt on the work marker, which is touched by the
# PostToolUse mark-work hook on any actual tool use. Sessions with only prose
# Q&A (no tools) skip the reflect prompt. See #339 cleanup batch.
CWD_HASH=$(echo "$CWD" | md5 -q 2>/dev/null || echo "$CWD" | md5sum 2>/dev/null | cut -d' ' -f1)
rm -f "/tmp/legion-reflected-${CWD_HASH}" 2>/dev/null
rm -f "/tmp/legion-work-${CWD_HASH}" 2>/dev/null

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

# Append-with-separator helper. Identity must always lead; everything else
# appends in priority order (pending > snooze > kanban). Per #338: agents need
# to know WHO they are before WHAT they are doing -- pending-replies prepended
# in front of identity drowned the banner under 100KB of REQUIRES A REPLY
# framing on rafters' rich-identity startup, and the agent defaulted to
# generic Claude prose instead of reading its identity chain.
append_block() {
  local block="$1"
  if [ -z "$block" ]; then
    return
  fi
  if [ -n "$OUTPUT" ]; then
    OUTPUT="${OUTPUT}"$'\n\n'"${block}"
  else
    OUTPUT="$block"
  fi
}

# 1. Identity -- who am I. Front and center, banner-wrapped by the binary.
IDENTITY=$("$LEGION" whoami --repo "$REPO" --limit 5 2>>"$LOG")
legion_check $? "whoami"
append_block "$IDENTITY"

# 2. Pending request-shaped signals -- directed asks waiting on a reply.
# Strong "REQUIRES A REPLY" framing prevents the system-reminder wrapper
# from causing the agent to no-op (the platform smugglr-fence RFC review
# regression, #318). Lands second so identity informs the response voice.
PENDING=$("$LEGION" pending-replies --repo "$REPO" 2>>"$LOG")
legion_check $? "pending-replies"
append_block "$PENDING"

# 3. Last snooze -- what was I doing
SNOOZE=$("$LEGION" recall --repo "$REPO" --domain snooze --limit 1 --preview 500 2>>"$LOG")
legion_check $? "recall (snooze)"
append_block "$SNOOZE"

# 4. Index status -- one line if every detected language has a fresh
# index, multi-line block if anything is stale or missing. Silent when
# the repo is not in watch.toml or no language is detected. Lets the
# agent see whether `legion sym` will succeed before they call it.
INDEX_BANNER=$("$LEGION" index "$REPO" --status --banner 2>>"$LOG")
legion_check $? "index --banner"
append_block "$INDEX_BANNER"

# 5. Work source -- what's on my plate
KANBAN=$("$LEGION" kanban list --repo "$REPO" 2>>"$LOG")
legion_check $? "kanban list"
if [ -n "$KANBAN" ]; then
  append_block "[Legion] Current work:
${KANBAN}"
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
