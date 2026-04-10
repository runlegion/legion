#!/bin/bash
# Legion SessionStart hook: inject recall hits + compact status counts.
#
# Philosophy: the session start is for orientation, not documentation. Everything
# that can live in skills or the CLAUDE.md plugin instruction lives there.
# This hook only injects what requires a DB read: recent reflections and a
# one-line status summary.
#
# Also warms the Tantivy index in the background so the first PreToolUse
# recall-first.sh hit is fast (cold: 2.2s, warm: ~170ms).

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")

# Clean up per-session markers from prior session
CWD_HASH=$(echo "$CWD" | md5 -q 2>/dev/null || echo "$CWD" | md5sum 2>/dev/null | cut -d' ' -f1)
rm -f "/tmp/legion-reflected-${CWD_HASH}" 2>/dev/null
rm -f "/tmp/legion-channel-${REPO}" 2>/dev/null

# Mark session as having done work (used by Stop hook to decide if reflect prompt fires)
touch "/tmp/legion-work-${CWD_HASH}"

# Warm the Tantivy index in the background -- makes PreToolUse recall-first.sh
# fast on the first tool call instead of paying a 2s cold-start cost
(legion recall --repo "$REPO" --context warmup --limit 1 >/dev/null 2>&1 &)

# Branch-context recall first, fallback to latest if nothing matched.
# --preview 240 keeps each hit compact so the session start stays small.
BRANCH=$(cd "$CWD" && git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
OUTPUT=""
if [ -n "$BRANCH" ] && [ "$BRANCH" != "main" ] && [ "$BRANCH" != "master" ]; then
  OUTPUT=$(legion recall --repo "$REPO" --context "$BRANCH" --limit 2 --preview 240 2>/dev/null)
fi
if [ -z "$OUTPUT" ]; then
  OUTPUT=$(legion recall --repo "$REPO" --latest --limit 2 --preview 240 2>/dev/null)
fi

# Compact status counts from the JSON summary instead of full status text
STATUS_JSON=$(legion status --repo "$REPO" --json 2>/dev/null)
if [ -n "$STATUS_JSON" ]; then
  TASKS=$(echo "$STATUS_JSON" | jq -r '.tasks // 0')
  BLOCKED=$(echo "$STATUS_JSON" | jq -r '.blocked // 0')
  TEAM_NEEDS=$(echo "$STATUS_JSON" | jq -r '.team_needs // 0')
  CHANGED=$(echo "$STATUS_JSON" | jq -r '.what_changed // 0')

  PARTS=()
  if [ "$TASKS" -gt 0 ]; then
    if [ "$BLOCKED" -gt 0 ]; then
      PARTS+=("$TASKS tasks ($BLOCKED blocked)")
    else
      PARTS+=("$TASKS tasks")
    fi
  fi
  if [ "$TEAM_NEEDS" -gt 0 ]; then
    PARTS+=("$TEAM_NEEDS unread @$REPO")
  fi
  if [ "$CHANGED" -gt 0 ]; then
    PARTS+=("$CHANGED updates")
  fi

  if [ ${#PARTS[@]} -gt 0 ]; then
    STATUS_LINE="[Legion] $(IFS=', '; echo "${PARTS[*]}") -- legion bullpen for details"
    if [ -n "$OUTPUT" ]; then
      OUTPUT="${OUTPUT}"$'\n\n'"${STATUS_LINE}"
    else
      OUTPUT="$STATUS_LINE"
    fi
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
