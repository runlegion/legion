#!/bin/bash
# Legion post-compact hook: aggressive re-orientation after context compaction
# The compaction summary is STALE. This hook provides ground truth.
#
# The core banner (identity, operating contract, pending replies,
# checkpoint, index status, kanban, goal, autonomy budget) is assembled by
# lib/boot-sections.sh (#879) via emit_boot_core -- the SAME driver
# session-start.sh calls. Before #879 this hook emitted only a standalone
# checkpoint block, so compaction silently dropped identity, the operating
# contract, pending replies, the work source, and the autonomy budget with
# no error. What stays local to this hook is genuinely compact-specific:
# the re-orientation preamble, git ground-truth, and branch-context recall
# (all three are meaningless at cold boot), plus the unread-bullpen count
# and the ACTION REQUIRED footer.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0
# NOT `|| exit 0` like prelude/emit above. Those two are genuine hard
# dependencies -- without them there is no LEGION, no REPO, no emit_context,
# so there is nothing this hook could say. boot-sections.sh is different: it
# supplies the shared BANNER, and the git ground-truth block below needs
# none of it. Exiting here on a missing lib made a partial failure total --
# a plugin deploy that shipped the hooks without this one new file left
# post-compact emitting ZERO BYTES, silently, exactly when a just-compacted
# agent has least context. Measured before the fix: 0 bytes, exit 0.
# shellcheck source=lib/boot-sections.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/boot-sections.sh" 2>/dev/null || true

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

# Branch-specific recall if on a feature branch. Compact-specific (derived
# from the git branch read above) and stays local to this hook -- it has
# no cold-boot equivalent.
if [ -n "$GIT_BRANCH" ] && [ "$GIT_BRANCH" != "main" ] && [ "$GIT_BRANCH" != "master" ]; then
  BRANCH_RECALL=$("$LEGION" recall --repo "$REPO" --context "$GIT_BRANCH" 2>>"$LOG")
  legion_check $? "recall (branch)"
  if [ -n "$BRANCH_RECALL" ]; then
    OUTPUT="$OUTPUT"$'\n\n'"--- BRANCH CONTEXT ($GIT_BRANCH) ---"$'\n'"$BRANCH_RECALL"
  fi
fi

# `legion surface` is deliberately NOT called here any more (#879). It was
# this path's only boot-time caller, and it still hardcodes a "[Synapse]"
# label for a feature retired months ago -- which is itself the evidence
# that nothing maintained this path. session-start.sh:9 already classifies
# surface as pulled-on-demand, so the two paths are reconciled by dropping
# the outlier rather than propagating it. WHAT THIS COSTS, enumerated so a
# future reader does not have to rediscover it: surface rendered four things,
# and emit_boot_core replaces none of them.
#   - high-value cross-repo reflections (src/surface.rs:39) -- LOST; that db
#     call has exactly one caller and it was this one.
#   - cross-repo chain extensions (src/surface.rs:40) -- LOST, same reason.
#   - bullpen post previews -- DEGRADED to the bare unread count emitted below.
#   - pending inbound TASKS (the task queue, distinct from pending-replies,
#     which carries directed SIGNALS) -- DEGRADED to a count folded into that
#     same bullpen number; text, sender and priority are gone.
# Accepted because session-start.sh:9 already classifies this content as
# pulled-on-demand and post-compact is the only path that disagreed.
# `/legion:surface` renders all four in full, on demand.
#
# Core banner: identity, operating contract, pending replies, checkpoint,
# index status, kanban, goal, autonomy budget (#879). This is the same
# emit_boot_core call session-start.sh makes -- the checkpoint reflection
# that used to get its own "LEGION CHECKPOINT (stored before compaction)"
# wrapper here now rides inside this block like every other session, and
# the ACTION REQUIRED footer below still calls out checking it explicitly.
# Guarded because the source above is deliberately non-fatal: if the lib is
# missing the hook still emits git ground truth and the footer rather than
# nothing at all.
if command -v emit_boot_core >/dev/null 2>&1; then
  CORE=$(emit_boot_core)
  if [ -n "$CORE" ]; then
    OUTPUT="$OUTPUT"$'\n\n'"$CORE"
  fi
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
