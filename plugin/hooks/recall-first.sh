#!/bin/bash
# Legion PreToolUse hook: run recall on the search query and inject matching
# reflections as additionalContext before the search tool fires.
#
# Fires on Grep, Glob, WebFetch, WebSearch. The agent may decide not to run
# the search after seeing the reflections -- that is the point.
#
# Relies on session-start.sh having warmed the Tantivy index; cold ~2.2s,
# warm ~170ms per call.
#
# Error handling: legion invocations append stderr to /tmp/legion-hook-errors.log
# instead of dropping it, so a silently-broken legion (missing binary, bad
# migration, DB schema mismatch) leaves a breadcrumb. The hook still exits 0
# on legion failure so the tool call proceeds as if nothing was injected.

# Hook subshells do not inherit the plugin bin dir on PATH -- only the Bash
# tool does. Invoke via full CLAUDE_PLUGIN_ROOT path (fixes #204).
LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

# Shared warning helper -- surfaces legion degradation instead of letting
# it silently hide recall context from the agent. See #209.
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-warn.sh"

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')
TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty')

if [ -z "$CWD" ] || [ -z "$TOOL" ]; then
  exit 0
fi

REPO=$(basename "$CWD")

# Extract the search query based on the tool
QUERY=""
case "$TOOL" in
  Grep|Glob)
    QUERY=$(echo "$INPUT" | jq -r '.tool_input.pattern // empty')
    ;;
  WebFetch)
    # Use the URL path components as the query -- the domain + path usually
    # carries the topic better than the full URL
    QUERY=$(echo "$INPUT" | jq -r '.tool_input.url // empty' \
      | sed -E 's|https?://||; s|[/?#&=]| |g')
    ;;
  WebSearch)
    QUERY=$(echo "$INPUT" | jq -r '.tool_input.query // empty')
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
legion_check $? "recall"

WARN=$(legion_warnings_block)

# No hits and no warning -- don't inject anything, let the tool fire as normal.
# If there's a warning, surface it even without hits so the agent knows recall
# is degraded and shouldn't trust the absence of reflections as "nothing relevant".
if [ -z "$HITS" ] && [ -z "$WARN" ]; then
  exit 0
fi

if [ -z "$HITS" ]; then
  CTX="$WARN"
else
  CTX="[Legion] Before ${TOOL}, recall hits for: ${QUERY}

${HITS}

If these answer your question, skip the ${TOOL}. Otherwise continue -- but consider whether the reflection explains WHY before you search for WHAT."
  if [ -n "$WARN" ]; then
    CTX="${WARN}"$'\n\n'"${CTX}"
  fi
fi

jq -n --arg ctx "$CTX" '{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "legion recall hits for the query",
    "additionalContext": $ctx
  }
}'
