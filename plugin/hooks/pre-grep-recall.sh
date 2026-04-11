#!/bin/bash
# Legion PreToolUse hook: run recall on the extracted search query and inject
# matching reflections as additionalContext before Grep or Glob fires.
#
# This hook replaces recall-first.sh for Grep and Glob specifically.
# recall-first.sh still handles WebFetch and WebSearch.
#
# Differences from recall-first.sh for these two matchers:
# - Query extraction strips regex metacharacters from Grep patterns and
#   tokenizes on word boundaries, so regex noise does not become the query.
# - Glob patterns that are pure wildcards (**/*.rs, *.toml) skip injection
#   entirely -- there are no semantic terms to search for.
# - BM25 score threshold: hits below 0.3 are treated as noise. If every
#   returned hit is below the threshold, no injection happens.
# - LEGION_SKIP_PRE_GREP=1 skips the hook entirely for the current tool call,
#   for cases where the agent deliberately wants to search without recall
#   preemption (e.g. searching for a newly-introduced symbol).
# - LEGION_REPO env var takes precedence over basename($CWD) for repo
#   identity, matching the pattern in other legion hooks.
# - Explicit 5-second timeout on the recall subprocess via gtimeout or
#   perl's alarm(), matching session-start.sh's degradation pattern.
#
# Error handling: every failure mode exits 0 and emits a stderr warning
# prefixed "[legion-pre-grep]". The hook never blocks or errors.

LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

# Shared warning helper (optional; do not fail if missing).
if [ -f "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-warn.sh" ]; then
  # shellcheck source=_legion-warn.sh
  source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-warn.sh"
fi

# Explicit skip override -- exit before doing any work.
if [ "${LEGION_SKIP_PRE_GREP:-}" = "1" ]; then
  echo "[legion-pre-grep] skipped (LEGION_SKIP_PRE_GREP=1)" >&2
  exit 0
fi

INPUT=$(cat)
if [ -z "$INPUT" ]; then
  echo "[legion-pre-grep] empty stdin; skipping" >&2
  exit 0
fi

# Parse the hook JSON input. jq failures fall through to exit 0.
CWD=$(echo "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
PATTERN=$(echo "$INPUT" | jq -r '.tool_input.pattern // empty' 2>/dev/null)

if [ -z "$CWD" ] || [ -z "$TOOL" ]; then
  echo "[legion-pre-grep] missing cwd or tool_name; skipping" >&2
  exit 0
fi

# Repo identity: prefer LEGION_REPO env var, fall back to basename($CWD).
REPO="${LEGION_REPO:-$(basename "$CWD")}"
if [ -z "$REPO" ]; then
  echo "[legion-pre-grep] could not resolve repo name; skipping" >&2
  exit 0
fi

# Extract a semantic query from the tool input based on the tool type.
QUERY=""
case "$TOOL" in
  Grep)
    # Strip regex metacharacters, lowercase, tokenize on non-word runs.
    # The quoted character class is the regex metacharacters we want gone.
    STRIPPED=$(echo "$PATTERN" \
      | tr '[:upper:]' '[:lower:]' \
      | tr -d '\\\\[]{}()*+?^$|.' \
      | tr -s ' ' ' ')
    # Count tokens -- skip if fewer than 2 survive.
    TOKEN_COUNT=$(echo "$STRIPPED" | wc -w | tr -d ' ')
    if [ "${TOKEN_COUNT:-0}" -lt 2 ]; then
      echo "[legion-pre-grep] skipped (pattern too regex-heavy: ${PATTERN})" >&2
      exit 0
    fi
    QUERY="$STRIPPED"
    ;;
  Glob)
    # Pure-wildcard globs carry no semantic signal. Skip them.
    if [[ "$PATTERN" =~ ^\*+$ ]] || [[ "$PATTERN" =~ ^\*\*/ ]] || [[ "$PATTERN" =~ ^\*\. ]]; then
      echo "[legion-pre-grep] skipped (pure wildcard glob: ${PATTERN})" >&2
      exit 0
    fi
    # Extract semantic segments: split on /, drop segments that are * or **
    # or end in *, strip file extensions, lowercase, join with space.
    STRIPPED=$(echo "$PATTERN" \
      | tr '/' ' ' \
      | tr '[:upper:]' '[:lower:]' \
      | awk '{
          out = ""
          for (i = 1; i <= NF; i++) {
            t = $i
            if (t == "*" || t == "**") continue
            if (substr(t, length(t)) == "*") continue
            sub(/\.[a-z0-9]+$/, "", t)
            out = (out == "" ? t : out " " t)
          }
          print out
        }')
    TOKEN_COUNT=$(echo "$STRIPPED" | wc -w | tr -d ' ')
    if [ "${TOKEN_COUNT:-0}" -lt 1 ]; then
      echo "[legion-pre-grep] skipped (glob has no semantic segments: ${PATTERN})" >&2
      exit 0
    fi
    QUERY="$STRIPPED"
    ;;
  *)
    # This hook only handles Grep and Glob. Any other tool exits 0 silently.
    exit 0
    ;;
esac

# Bail if the final query collapsed to nothing.
if [ -z "$QUERY" ]; then
  echo "[legion-pre-grep] empty query after extraction; skipping" >&2
  exit 0
fi

# Bail if the legion binary is missing or not executable.
if [ ! -x "$LEGION" ]; then
  echo "[legion-pre-grep] legion binary not found at ${LEGION}; skipping" >&2
  exit 0
fi

# Run recall with an explicit 5s timeout. Prefer gtimeout if available,
# fall back to perl's alarm, matching session-start.sh's pattern.
if command -v gtimeout >/dev/null 2>&1; then
  HITS=$(gtimeout 5 "$LEGION" recall --repo "$REPO" --context "$QUERY" --limit 3 --preview 400 2>>"$LOG")
  RC=$?
else
  HITS=$(perl -e 'alarm 5; exec @ARGV' "$LEGION" recall --repo "$REPO" --context "$QUERY" --limit 3 --preview 400 2>>"$LOG")
  RC=$?
fi

# Timeout signal -- gtimeout returns 124, killed perl returns 142 (128+SIGALRM).
if [ "$RC" -eq 124 ] || [ "$RC" -eq 142 ]; then
  echo "[legion-pre-grep] recall timed out for query: ${QUERY}" >&2
  exit 0
fi

# Surface legion degradation via the shared warning helper if it exists.
# _legion-warn.sh provides legion_check and legion_warnings_block; guard calls.
if command -v legion_check >/dev/null 2>&1; then
  legion_check "$RC" "recall"
fi

WARN=""
if command -v legion_warnings_block >/dev/null 2>&1; then
  WARN=$(legion_warnings_block)
fi

# If recall failed hard and we have no hits, fall through to the warning-only
# injection path so the agent at least knows recall is broken.
if [ "$RC" -ne 0 ] && [ -z "$WARN" ]; then
  echo "[legion-pre-grep] recall exited $RC; skipping injection" >&2
  exit 0
fi

# Parse score lines from the recall output. Hits look like:
#   - ... (id: <uuid>, score: 0.82)
# If no score lines exist, treat all hits as passing (format may have
# changed). If every parsed score is below 0.3, skip injection.
if [ -n "$HITS" ]; then
  SCORES=$(echo "$HITS" | grep -oE 'score: [0-9]+\.[0-9]+' | sed 's/score: //')
  if [ -n "$SCORES" ]; then
    PASSING=$(echo "$SCORES" | awk '$1 >= 0.3 { print }')
    if [ -z "$PASSING" ]; then
      echo "[legion-pre-grep] skipped (all recall scores below 0.3 threshold)" >&2
      # Empty hits -- fall through to warning-only if warn is non-empty.
      HITS=""
    fi
  fi
fi

# No hits and no warning -- exit 0 without emitting any JSON.
if [ -z "$HITS" ] && [ -z "$WARN" ]; then
  exit 0
fi

# Build the additionalContext block. Warning first if present, then hits.
if [ -z "$HITS" ]; then
  CTX="$WARN"
else
  CTX="## Legion memory for this query

Before ${TOOL} on \`${QUERY}\`, legion recall returned:

${HITS}

If these answer your question, skip the ${TOOL}. Otherwise continue -- but consider whether the reflection explains WHY before you search for WHAT."
  if [ -n "$WARN" ]; then
    CTX="${WARN}"$'\n\n'"${CTX}"
  fi
fi

# Emit the PreToolUse hookSpecificOutput block.
jq -n --arg ctx "$CTX" '{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "legion recall hits for the query",
    "additionalContext": $ctx
  }
}'
