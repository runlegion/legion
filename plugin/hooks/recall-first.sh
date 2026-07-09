#!/bin/bash
# Legion PreToolUse hook: run recall on the search query and inject matching
# reflections as additionalContext before the search tool fires.
#
# Fires on WebFetch and WebSearch. Grep and Glob are handled by pre-grep.sh
# which has better query sanitization for those. Agent/Explore spawns are
# not handled here -- no-harness-explore.sh is the sole decider for Explore
# (#672); this hook is no longer wired to the Agent matcher in hooks.json.
#
# Relies on session-start.sh having warmed the Tantivy index; cold ~2.2s,
# warm ~170ms per call.
#
# Error handling: legion invocations append stderr to /tmp/legion-hook-errors.log
# instead of dropping it, so a silently-broken legion (missing binary, bad
# migration, DB schema mismatch) leaves a breadcrumb. The hook still exits 0
# on legion failure so the tool call proceeds as if nothing was injected.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0

LOG="$LEGION_HOOK_LOG"

legion_hook_parse || exit 0

if [ -z "$CWD" ] || [ -z "$TOOL" ]; then
  exit 0
fi

if [ ! -x "$LEGION" ]; then
  exit 0
fi

# Skip injection in repos legion does not cover (#353).
legion_hook_covered || exit 0

# Check the clamp list -- skip recall for tools that burn tokens without value
CLAMP_FILE="${CLAUDE_PLUGIN_ROOT}/hooks/recall-clamp.conf"
if [ -f "$CLAMP_FILE" ] && grep -qx "$TOOL" "$CLAMP_FILE"; then
  exit 0
fi

# Extract the search query based on the tool.
QUERY=""
case "$TOOL" in
  WebFetch)
    # Use the prompt (user intent), not the URL -- URL path components
    # produce garbage BM25 matches. The prompt carries actual topic signal.
    QUERY=$(legion_hook_field '.tool_input.prompt')
    ;;
  WebSearch)
    QUERY=$(legion_hook_field '.tool_input.query')
    ;;
  *)
    exit 0
    ;;
esac

# Skip empty or trivially short queries (single chars, punctuation patterns)
if [ -z "$QUERY" ] || [ ${#QUERY} -lt 4 ]; then
  exit 0
fi

# Run recall. Short limit + preview truncation keep injected context compact.
HITS=$("$LEGION" recall --repo "$REPO" --context "$QUERY" --limit 3 --preview 200 2>>"$LOG")

# No hits -- don't inject anything, let the tool fire as normal. Recall
# failures leave their breadcrumb in $LOG (#383).
if [ -z "$HITS" ]; then
  exit 0
fi

CTX="[Legion] Before ${TOOL}, recall hits for: ${QUERY}

${HITS}

If these answer your question, skip the ${TOOL}. Otherwise continue -- but consider whether the reflection explains WHY before you search for WHAT."

emit_allow "$CTX" "legion recall hits for the query"
