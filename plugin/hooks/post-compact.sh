#!/bin/bash
# Legion post-compact hook: aggressive re-orientation after context compaction
# The compaction summary is STALE. This hook provides ground truth.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0

LOG="$LEGION_HOOK_LOG"

legion_hook_parse || exit 0

if [ -z "$CWD" ]; then
  exit 0
fi

CWD_HASH=$(legion_hash_str "$CWD")

# Clean up stop-hook marker so reflect prompt fires fresh
MARKER="/tmp/legion-reflected-${CWD_HASH}"
rm -f "$MARKER" 2>/dev/null

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

CHECKPOINT=$("$LEGION" recall --repo "$REPO" --domain checkpoint --limit 1 2>>"$LOG")
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
if [ -n "$SURFACE" ]; then
  OUTPUT="$OUTPUT"$'\n\n'"$SURFACE"
fi

# Unread bullpen
BOARD_COUNT=$("$LEGION" bullpen --count --repo "$REPO" 2>>"$LOG")
if [ -n "$BOARD_COUNT" ]; then
  OUTPUT="$OUTPUT"$'\n\n'"[Legion] ${BOARD_COUNT}. Run: legion bullpen --repo ${REPO}"
fi

OUTPUT="$OUTPUT"$'\n\n'"--- ACTION REQUIRED ---
1. Read the git state above. That is what is ACTUALLY done.
2. Compare with the compaction summary. If they conflict, trust git.
3. Check the checkpoint reflection for what you were working on.
4. THEN resume work."

OUTPUT="$OUTPUT"$'\n\n'"[Legion] consult --context <problem> to search all agents | signal --to <agent> --verb question to ask directly | boost --id <id> when a reflection helps"

emit_context "SessionStart" "$OUTPUT"
