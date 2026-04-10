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
LOG=/tmp/legion-hook-errors.log

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
HITS=$(legion recall --repo "$REPO" --context "$QUERY" --limit 3 --preview 200 2>>"$LOG")

# No hits -- don't inject anything, let the tool fire as normal
if [ -z "$HITS" ]; then
  exit 0
fi

CTX="[Legion] Before ${TOOL}, recall hits for: ${QUERY}

${HITS}

If these answer your question, skip the ${TOOL}. Otherwise continue -- but consider whether the reflection explains WHY before you search for WHAT."

jq -n --arg ctx "$CTX" '{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "legion recall hits for the query",
    "additionalContext": $ctx
  }
}'
