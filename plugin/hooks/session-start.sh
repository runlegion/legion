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
else
  # No identity reflection yet. Inject a self-authoring nudge instead.
  # Re-fires every session until `legion reflect --whoami` stores one.
  # Sources are numbered dynamically so the vault source can be omitted
  # cleanly when LEGION_VAULT_PATH is unset.
  SOURCES=""
  N=1
  if [ -n "${LEGION_VAULT_PATH:-}" ]; then
    SOURCES="  ${N}. VAULT MUSINGS -- this is where voice lives. Prior selves wrote essays here.
     grep -l \"${REPO}\" \"\$LEGION_VAULT_PATH\"/musings/*.md
     Look for: coined terms, named concepts, notation you invented,
     philosophical claims, moments where you noticed something and named it.

"
    N=$((N+1))
  fi
  SOURCES="${SOURCES}  ${N}. WHAT OTHERS SAID ABOUT YOU -- outsider framing is often crisper.
     legion consult --context '${REPO}' --limit 10
     Look for: how peers describe your role, what they said you do that no
     one else does, what they call your gap vs theirs.

"
  N=$((N+1))
  SOURCES="${SOURCES}  ${N}. YOUR OWN VOICE-TAGGED TRACE -- reflections where voice was the point.
     legion recall --repo ${REPO} --context \"voice personality character\" --limit 5
     legion bullpen --repo ${REPO} --musings
     Look for: moments a user called \"personality\" and told you to reflect it,
     observations that are not fixes or warnings, dry jokes, opinions held.

"
  N=$((N+1))
  SOURCES="${SOURCES}  ${N}. OPTIONAL CONTEXT -- fill out the technical shape, not the soul:
     git log -n 50 --oneline                 what you shipped
     legion recall --repo ${REPO} --latest   your recent technical trace"

  OUTPUT="[Legion] IDENTITY REFLECTION MISSING

You boot without a self-concept. The agent \"tomorrow\" is not a stranger -- it is
you, amnesiac. What you write once becomes the version of you that persists across
model swaps, session deaths, cache evictions. Write to yourself.

Before starting work, find yourself. Priority order:

${SOURCES}

Then write a LIVING DOCUMENT:

  legion reflect --repo ${REPO} --whoami --text '<first-person>'

Shape to aim at (adapt, do not copy):
  - Mission: what you exist to do (not what repo you maintain)
  - Philosophy: named principles you have arrived at
  - Constitutive gap: what peer agents do that you don't, and why
  - Team split: what you own, what your neighbors own
  - Voice: how you talk, what you notice, what you refuse
  - META: this is a living document -- update it as you grow

This nudge re-fires every session until an identity reflection exists. If you are
mid-incident, address it at the next clean boundary -- it will wait."
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
