#!/bin/bash
# Legion SessionStart hook: focused bootstrap.
#
# Injects exactly three things:
#   1. Identity  -- who am I (domain: identity)
#   2. Last checkpoint -- where was I (domain: checkpoint)
#   3. Work source -- what's on my plate (kanban list)
#
# Everything else (bulk recall, surface, bullpen) is pulled on demand
# during the session via recall/consult/bullpen commands.
#
# Also warms the Tantivy index in the background so the first PreToolUse
# recall hit is fast (cold ~2.2s, warm ~170ms).

LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

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

# Dashboard daemon supervisor (#321): probe /health, (re)spawn as needed.
# Backgrounded with stdin closed so SessionStart latency does not include
# the curl probe or the legion serve spawn handshake.
(bash "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-daemon-supervisor.sh" >/dev/null 2>&1 < /dev/null &)

# Write an interactive-session lock so watch does not spawn a duplicate
# agent while this session is open (#583). $PPID is the Claude session
# process -- the long-lived pid the lock must track. Non-blocking; never
# allowed to add latency or fail the hook.
("$LEGION" watch session-start --repo "$REPO" --pid "$PPID" >/dev/null 2>&1 &)

# Sync GitHub issues into kanban (5-second timeout, opt-out via LEGION_NO_SYNC=1)
if [ "${LEGION_NO_SYNC:-}" != "1" ]; then
  if command -v gtimeout >/dev/null 2>&1; then
    gtimeout 5 "$LEGION" sync --repo "$REPO" >/dev/null 2>>"$LOG"
  else
    perl -e 'alarm 5; exec @ARGV' -- "$LEGION" sync --repo "$REPO" >/dev/null 2>>"$LOG"
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

# 0. Now -- weekday + local time + sunphase. One line, lands first so an
# agent reads "today is Sunday afternoon" before identity primes voice.
# claude-code's own systemPrompt ships `currentDate` but no weekday and
# no hour, so agents pattern-match on conversation density and start
# saying "tonight" or "wind down" when the operator has the rest of the
# workday ahead. See #410.
NOW=$("$LEGION" now --banner 2>>"$LOG")
append_block "$NOW"

# 1. Identity -- who am I. Banner-wrapped by the binary.
IDENTITY=$("$LEGION" whoami --repo "$REPO" --limit 5 2>>"$LOG")
append_block "$IDENTITY"

# 1b. Operating contract -- how I operate (domain: workflow). Lands right after
# identity so the agent reads WHO YOU ARE, then HOW YOU OPERATE. Banner-wrapped
# by the binary; silent when the repo has no workflow roots yet.
WHATAMI=$("$LEGION" whatami --repo "$REPO" --limit 5 2>>"$LOG")
append_block "$WHATAMI"

# 2. Pending request-shaped signals -- directed asks waiting on a reply.
# Strong "REQUIRES A REPLY" framing prevents the system-reminder wrapper
# from causing the agent to no-op (the platform smugglr-fence RFC review
# regression, #318). Lands second so identity informs the response voice.
PENDING=$("$LEGION" pending-replies --repo "$REPO" 2>>"$LOG")
append_block "$PENDING"

# 3. Last checkpoint -- where was I. The /checkpoint command and the
# precompact safety-net both write domain=checkpoint; freshest wins.
CHECKPOINT=$("$LEGION" recall --repo "$REPO" --domain checkpoint --limit 1 --preview 500 2>>"$LOG")
# Transitional fallback: before the snooze->checkpoint rename (#568), the
# deliberate session summary lived in domain=snooze. Surface a legacy snooze
# reflection only when no checkpoint exists yet, so the first session after
# upgrade does not lose its anchor. Remove once domain=snooze has aged out.
if [ -z "$CHECKPOINT" ]; then
  CHECKPOINT=$("$LEGION" recall --repo "$REPO" --domain snooze --limit 1 --preview 500 2>>"$LOG")
fi
append_block "$CHECKPOINT"

# 4. Index status -- one line if every detected language has a fresh
# index, multi-line block if anything is stale or missing. Silent when
# the repo is not in watch.toml or no language is detected. Lets the
# agent see whether `legion sym` will succeed before they call it.
INDEX_BANNER=$("$LEGION" index "$REPO" --status --banner 2>>"$LOG")
append_block "$INDEX_BANNER"

# 5. Work source -- what's on my plate
KANBAN=$("$LEGION" kanban list --repo "$REPO" 2>>"$LOG")
if [ -n "$KANBAN" ]; then
  append_block "[Legion] Current work:
${KANBAN}"
fi

# 6. Board-derived goal (#525) -- the active Accepted card's acceptance
# criteria, framed as the completion condition the agent carries this session.
# Native /goal cannot be set programmatically, so legion re-derives it from the
# board here. Empty when nothing is in progress (the goal is cleared by board
# state alone). Lands right after the work list: "here is your work, and here
# is the one you committed to finishing."
GOAL=$("$LEGION" goal --repo "$REPO" 2>>"$LOG")
append_block "$GOAL"

# 7. Autonomy budget (#524) -- remind the agent it has sanctioned units to
# spend on self-directed work, so it acts on the board instead of waiting to
# be told. Lands after the goal: first "finish this," then "and you are
# cleared to pick up more yourself."
BUDGET=$("$LEGION" autonomy status --repo "$REPO" --banner 2>>"$LOG")
append_block "$BUDGET"

if [ -n "$OUTPUT" ]; then
  jq -n --arg ctx "$OUTPUT" '{
    "hookSpecificOutput": {
      "hookEventName": "SessionStart",
      "additionalContext": $ctx
    }
  }'
fi
