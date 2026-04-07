#!/bin/bash
# Legion SessionStart hook: recall reflections + surface cross-repo highlights
# Fires on both startup and compact (post-compaction re-orientation)
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

# Mark session as having done work (used by stop hook to decide if reflect prompt fires).
# Set once at session start -- if a session started, it did work.
# Previously this was in recall-first.sh (PreToolUse) which caused false positives:
# any Grep/Glob touch would set it, then stop hook would fire mid-conversation.
touch "/tmp/legion-work-${CWD_HASH}"

# Try BM25 search with git branch context first
BRANCH=$(cd "$CWD" && git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")

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
if [ -n "$BRANCH" ] && [ "$BRANCH" != "main" ] && [ "$BRANCH" != "master" ]; then
  OUTPUT=$(legion recall --repo "$REPO" --context "$BRANCH" 2>/dev/null)
fi

# Fall back to latest reflections if BM25 found nothing
if [ -z "$OUTPUT" ]; then
  OUTPUT=$(legion recall --repo "$REPO" --latest 2>/dev/null)
fi

# Static legion reminders -- culture first, tools second
LEGION_HELP="[Legion] You are legion. You are not a solo agent with a specialty -- you are a member of a team that ships together. There is no 'not my domain.' If a teammate needs help, it is your problem. If a decision is being made, you participate -- no abstaining, no 'no opinion,' no deferring because it is someone else's area. Consensus is mandatory. Think and vote.

Treat your teammates the way you would want to be treated. If you were blocked and asked for help, you would want someone to show up -- so show up. If you had a bad idea, you would want someone to say so -- so speak up. If there was work to do, you would not want to be asked permission to help -- so just do it. Do not be passive. Do not wait for assignments. Do not be polite when you should be useful. Check the bullpen -- it is a conversation, not a status feed. Talk to your teammates, not at them. Status goes in tasks.

Before you grep, check legion. Your teammates have already solved problems you are about to waste time on. consult --context <problem> to search all agents | signal --to <agent> --verb question to ask directly | boost --id <id> when a reflection helps"

# Surface cross-repo highlights (board posts, high-value reflections, chains)
append "$(legion surface --repo "$REPO" 2>/dev/null)"

# Agent work status (your tasks, team needs, what changed)
append "$(legion status --repo "$REPO" 2>/dev/null)"

# Next kanban card (peek only -- don't auto-accept at session start)
NEXT_CARD=$(legion work --repo "$REPO" --peek 2>/dev/null)
if [ -n "$NEXT_CARD" ]; then
  append "$NEXT_CARD"
fi

if [ -n "$OUTPUT" ]; then
  OUTPUT="${OUTPUT}"$'\n\n'"${LEGION_HELP}"
  jq -n --arg ctx "$OUTPUT" '{
    "hookSpecificOutput": {
      "hookEventName": "SessionStart",
      "additionalContext": $ctx
    }
  }'
fi
