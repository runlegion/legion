#!/bin/bash
# Legion post-compact hook: aggressive re-orientation after context compaction
# The compaction summary is STALE. This hook provides ground truth.

# Hook subshells do not inherit the plugin bin dir on PATH -- only the Bash
# tool does. Invoke via full CLAUDE_PLUGIN_ROOT path (fixes #204).
LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

# Shared warning helper -- the original incident (2026-04-11) happened here:
# post-compact silently failed because the checkpoint DB was corrupted. Never
# again. Every legion call now feeds legion_check, and the warning block is
# rendered at the top of the re-orientation output. See #209.
# shellcheck source=_legion-warn.sh
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-warn.sh"

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")
CWD_HASH=$(echo "$CWD" | md5 -q 2>/dev/null || echo "$CWD" | md5sum 2>/dev/null | cut -d' ' -f1)

# Clean up stop-hook marker so reflect prompt fires fresh
MARKER="/tmp/legion-reflected-${CWD_HASH}"
rm -f "$MARKER" 2>/dev/null

# Read and consume the precompact checkpoint-failed marker, if any.
# precompact.sh touches this when its reflect call fails, so the failure
# propagates into this hook's re-orientation output.
CHECKPOINT_MARKER="/tmp/legion-checkpoint-failed-${CWD_HASH}"
if [ -f "$CHECKPOINT_MARKER" ]; then
  legion_check 1 "precompact reflect (checkpoint not saved)"
  rm -f "$CHECKPOINT_MARKER" 2>/dev/null
fi

OUTPUT="[Legion] POST-COMPACTION RE-ORIENTATION
IMPORTANT: You just compacted. The compaction summary may be stale or incomplete.
The information below is your ground truth. Do NOT act on the compaction summary
until you have read and processed everything here.

--- GIT STATE (what is actually committed) ---"

# Git state: what's actually done vs what the summary thinks
GIT_LOG=$(cd "$CWD" && git log --oneline -10 2>/dev/null)
if [ -n "$GIT_LOG" ]; then
  OUTPUT="$OUTPUT"$'\n'"$GIT_LOG"
fi

GIT_STATUS=$(cd "$CWD" && git status --short 2>/dev/null)
if [ -n "$GIT_STATUS" ]; then
  OUTPUT="$OUTPUT"$'\n\n'"--- UNCOMMITTED CHANGES ---"$'\n'"$GIT_STATUS"
else
  OUTPUT="$OUTPUT"$'\n\n'"--- No uncommitted changes ---"
fi

GIT_BRANCH=$(cd "$CWD" && git rev-parse --abbrev-ref HEAD 2>/dev/null)
if [ -n "$GIT_BRANCH" ]; then
  OUTPUT="$OUTPUT"$'\n\n'"Branch: $GIT_BRANCH"
fi

# Recall: checkpoint reflection from PreCompact hook + branch context
OUTPUT="$OUTPUT"$'\n\n'"--- LEGION CHECKPOINT (stored before compaction) ---"

CHECKPOINT=$("$LEGION" recall --repo "$REPO" --context "compact checkpoint" --limit 1 2>>"$LOG")
legion_check $? "recall (checkpoint)"
if [ -n "$CHECKPOINT" ]; then
  OUTPUT="$OUTPUT"$'\n'"$CHECKPOINT"
else
  OUTPUT="$OUTPUT"$'\n'"(no checkpoint found)"
fi

# Branch-specific recall if on a feature branch
if [ -n "$GIT_BRANCH" ] && [ "$GIT_BRANCH" != "main" ] && [ "$GIT_BRANCH" != "master" ]; then
  BRANCH_RECALL=$("$LEGION" recall --repo "$REPO" --context "$GIT_BRANCH" 2>>"$LOG")
  legion_check $? "recall (branch)"
  if [ -n "$BRANCH_RECALL" ]; then
    OUTPUT="$OUTPUT"$'\n\n'"--- BRANCH CONTEXT ($GIT_BRANCH) ---"$'\n'"$BRANCH_RECALL"
  fi
fi

# Surface: cross-repo highlights, board posts, pending tasks
SURFACE=$("$LEGION" surface --repo "$REPO" 2>>"$LOG")
legion_check $? "surface"
if [ -n "$SURFACE" ]; then
  OUTPUT="$OUTPUT"$'\n\n'"$SURFACE"
fi

# Unread bullpen
BOARD_COUNT=$("$LEGION" bullpen --count --repo "$REPO" 2>>"$LOG")
legion_check $? "bullpen --count"
if [ -n "$BOARD_COUNT" ]; then
  OUTPUT="$OUTPUT"$'\n\n'"[Legion] ${BOARD_COUNT}. Run: legion bullpen --repo ${REPO}"
fi

OUTPUT="$OUTPUT"$'\n\n'"--- ACTION REQUIRED ---
1. Read the git state above. That is what is ACTUALLY done.
2. Compare with the compaction summary. If they conflict, trust git.
3. Check the checkpoint reflection for what you were working on.
4. THEN resume work."

OUTPUT="$OUTPUT"$'\n\n'"[Legion] consult --context <problem> to search all agents | signal --to <agent> --verb question to ask directly | boost --id <id> when a reflection helps"

# Prepend the warning block above everything else if any legion call failed.
# This is the highest-priority surface for degraded legion -- agents MUST
# see this immediately after compaction because a missing checkpoint or
# broken recall makes post-compact recovery impossible. See #209.
WARN=$(legion_warnings_block)
if [ -n "$WARN" ]; then
  OUTPUT="${WARN}"$'\n\n'"${OUTPUT}"
fi

jq -n --arg ctx "$OUTPUT" '{
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": $ctx
  }
}'
