#!/bin/bash
# Legion PreToolUse hook: run recall on the search query and inject matching
# reflections as additionalContext before the search tool fires.
#
# Fires on WebFetch, WebSearch, and Agent (Explore only). Grep and Glob are
# handled by pre-grep-recall.sh which has better query sanitization for those.
#
# For Explore agent spawns: if recall has strong hits (score >= threshold),
# the hook DENIES the spawn and returns recall results instead. This prevents
# agents from burning tokens on Explore when legion already has the answer.
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
# shellcheck source=_legion-warn.sh
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-warn.sh"

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')
TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty')

if [ -z "$CWD" ] || [ -z "$TOOL" ]; then
  exit 0
fi

REPO=$(basename "$CWD")

# Skip injection in repos legion does not cover (#353).
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)
if [ -f "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-covered.sh" ]; then
  # shellcheck source=_legion-covered.sh
  source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-covered.sh"
  if ! legion_covered "$SESSION_ID" "$REPO"; then
    exit 0
  fi
fi

# Check the clamp list -- skip recall for tools that burn tokens without value
CLAMP_FILE="${CLAUDE_PLUGIN_ROOT}/hooks/recall-clamp.conf"
if [ -f "$CLAMP_FILE" ] && grep -qx "$TOOL" "$CLAMP_FILE"; then
  exit 0
fi

# Extract the search query based on the tool
QUERY=""
IS_EXPLORE=false
case "$TOOL" in
  WebFetch)
    # Use the prompt (user intent), not the URL -- URL path components
    # produce garbage BM25 matches. The prompt carries actual topic signal.
    QUERY=$(echo "$INPUT" | jq -r '.tool_input.prompt // empty')
    ;;
  WebSearch)
    QUERY=$(echo "$INPUT" | jq -r '.tool_input.query // empty')
    ;;
  Agent)
    # For Explore agents, try recall first and deny the spawn if we have
    # strong hits. Other agent types pass through.
    AGENT_TYPE=$(echo "$INPUT" | jq -r '.tool_input.subagent_type // empty')
    if [ "$AGENT_TYPE" = "Explore" ]; then
      IS_EXPLORE=true
      QUERY=$(echo "$INPUT" | jq -r '.tool_input.prompt // empty')
    else
      exit 0
    fi
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

# For Explore agents: deny the spawn if recall has a strong hit.
# Extract the top score -- format is "score: 0.XX)" at end of each hit line.
# If top score >= threshold, deny and return recall results instead.
DECISION="allow"
REASON="legion recall hits for the query"
if [ "$IS_EXPLORE" = true ] && [ -n "$HITS" ]; then
  TOP_SCORE=$(echo "$HITS" | grep -oE 'score: [0-9]+\.[0-9]+' | head -1 | awk '{print $2}')
  THRESHOLD="${LEGION_EXPLORE_THRESHOLD:-0.60}"
  if [ -n "$TOP_SCORE" ] && awk "BEGIN{exit !($TOP_SCORE >= $THRESHOLD)}"; then
    DECISION="deny"
    REASON="legion recall answered this (score: ${TOP_SCORE}). Use the recall results instead of spawning Explore."
    CTX="[Legion] Explore BLOCKED -- recall already has what you need:

${HITS}

Use these results directly. If they are genuinely insufficient, rephrase your question and try a direct Grep or Read instead of Explore."
  fi
fi

jq -n --arg ctx "$CTX" --arg decision "$DECISION" --arg reason "$REASON" '{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": $decision,
    "permissionDecisionReason": $reason,
    "additionalContext": $ctx
  }
}'
