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

# For Explore agents (#413): run the full cheap chain before deciding.
# recall alone is too narrow -- consult catches cross-agent memory,
# surface catches recent cross-repo activity, sym catches code-intel
# answers when the prompt mentions identifiers, consult --symbol covers
# cross-repo code intel.
#
# Decision: deny when ANY source returns informative content. The closing
# guidance points at Read with concrete paths from the results, not Grep
# (the pre-#413 fallback shifted cost sideways instead of dropping it --
# see reflection 019d84c7).
DECISION="allow"
REASON="legion recall hits for the query"
if [ "$IS_EXPLORE" = true ]; then
  # Source 1 already done: HITS (recall). Score-based hit detection.
  RECALL_HIT=false
  if [ -n "$HITS" ]; then
    TOP_SCORE=$(echo "$HITS" | grep -oE 'score: [0-9]+\.[0-9]+' | head -1 | awk '{print $2}')
    THRESHOLD="${LEGION_EXPLORE_THRESHOLD:-0.60}"
    if [ -n "$TOP_SCORE" ] && awk "BEGIN{exit !($TOP_SCORE >= $THRESHOLD)}"; then
      RECALL_HIT=true
    fi
  fi

  # Source 2: cross-agent memory. Best-effort; never errors out the hook.
  CONSULT=$("$LEGION" consult --context "$QUERY" --limit 3 2>>"$LOG" || true)
  CONSULT_HIT=false
  if [ -n "$CONSULT" ] && echo "$CONSULT" | grep -q '^- '; then
    CONSULT_HIT=true
  fi

  # Source 3: recent cross-repo activity. Topical surface for in-flight work.
  SURFACE=$("$LEGION" surface --repo "$REPO" 2>>"$LOG" || true)
  SURFACE_HIT=false
  if [ -n "$SURFACE" ] && echo "$SURFACE" | grep -q '^- '; then
    SURFACE_HIT=true
  fi

  # Source 4: code intelligence. Only fires when the prompt mentions an
  # identifier-shaped token. Floor raised to 5 chars to filter out
  # English stopwords (the, and, this, that, etc.) and the
  # CASE_PATTERN excludes a small set of common-word-shapes that pass
  # the regex but are obviously natural language.
  #
  # Both same-repo (sym def) and cross-repo (consult --symbol). The
  # cross-repo flag may not exist yet (#285); the failure is silently
  # absorbed by the trailing `|| true`.
  IDENT=$(echo "$QUERY" \
    | grep -oE '[A-Z][A-Za-z0-9_]{4,}|[a-z_][a-z_0-9]{4,}' \
    | grep -viE '^(should|would|could|where|which|there|their|these|those|about|after|before|other|first|under|legion|claude|hooks|files|using)$' \
    | head -1)
  SYM=""
  SYM_XREPO=""
  SYM_HIT=false
  if [ -n "$IDENT" ]; then
    SYM=$("$LEGION" sym def --json "$IDENT" 2>>"$LOG" || true)
    if [ -n "$SYM" ] && [ "$SYM" != "[]" ] && [ "$SYM" != "null" ]; then
      SYM_HIT=true
    fi
    SYM_XREPO=$("$LEGION" consult --symbol "$IDENT" 2>>"$LOG" || true)
    if [ -n "$SYM_XREPO" ] && echo "$SYM_XREPO" | grep -q '^- '; then
      SYM_HIT=true
    fi
  fi

  if [ "$RECALL_HIT" = true ] || [ "$CONSULT_HIT" = true ] || [ "$SURFACE_HIT" = true ] || [ "$SYM_HIT" = true ]; then
    DECISION="deny"
    REASON="legion's cheap-paths chain answered this. Use the injected results, then Read specific files if you need detail."

    SECTIONS=""
    if [ -n "$HITS" ]; then
      SECTIONS="${SECTIONS}### legion recall (this repo)
${HITS}

"
    fi
    if [ -n "$CONSULT" ]; then
      SECTIONS="${SECTIONS}### legion consult (cross-agent memory)
${CONSULT}

"
    fi
    if [ -n "$SURFACE" ]; then
      SECTIONS="${SECTIONS}### legion surface (recent cross-repo activity)
${SURFACE}

"
    fi
    if [ -n "$SYM" ] && [ "$SYM" != "[]" ] && [ "$SYM" != "null" ]; then
      SECTIONS="${SECTIONS}### legion sym def \`${IDENT}\` (this repo, SCIP)
\`\`\`json
${SYM}
\`\`\`

"
    fi
    if [ -n "$SYM_XREPO" ]; then
      SECTIONS="${SECTIONS}### legion consult --symbol \`${IDENT}\` (cross-repo SCIP)
${SYM_XREPO}

"
    fi

    CTX="[Legion] Explore BLOCKED -- the cheap-paths chain already covers this:

${SECTIONS}Read the specific files referenced in these results for detail. Do NOT switch to Grep -- it is not cheaper. The point is to skip the file scan, not to do it under a different tool name."
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
