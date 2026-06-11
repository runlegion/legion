#!/bin/bash
# Legion PreToolUse hook (#614): the Grep/Glob search guard. Merges the
# former pre-grep-recall.sh + pre-grep-scip.sh pair (which ran as TWO
# forked hooks per Grep/Glob call, each re-parsing stdin and re-probing
# coverage) into the same three-state ladder pre-bash-grep.sh applies to
# shell searches, so Grep-tool and Bash-grep enforcement agree on the
# same pattern by construction:
#
#   1. INJECT  -- repo not indexed, or sym found only cluster-wide hits:
#                 emit what sym found as additionalContext. When the
#                 pattern is not symbol-shaped (or sym found nothing),
#                 fall through to a recall probe on the semantic tokens
#                 and inject those hits instead.
#   2. BLOCK   -- repo indexed AND `legion sym def` resolves the pattern
#                 in THIS repo (#458 relevance gate): deny with the sym
#                 results inline. Soft fallback guidance only -- the
#                 mandatory gate is the operator's permissions.deny, see
#                 docs/decisions/2026-06-02-grep-blocking-is-operator-permissions.md.
#   3. BYPASS  -- LEGION_BYPASS_GREP=1 (env only; Grep/Glob carry no
#                 command string for the `# legion-bypass:` sentinel).
#                 REFUSED for symbol patterns with a local hit; otherwise
#                 telemetried and allowed -- mirroring pre-bash-grep.
#
# Skip discipline:
# - LEGION_SKIP_PRE_GREP=1 skips this hook (LEGION_SKIP_PRE_GREP_SCIP=1
#   is honored as a back-compat alias from the merged-away sibling).
# - tool not Grep/Glob, missing pattern, repo not legion-covered: pass
#   through silently.
#
# Recall tier (from pre-grep-recall.sh):
# - Grep patterns are regex-stripped and tokenized; fewer than 2
#   surviving tokens skip injection (regex noise is not a query).
# - Glob patterns keep semantic path segments; pure wildcards skip.
# - BM25 score threshold 0.3: if every hit scores below it, skip.
# - recall runs under an explicit 5s timeout (gtimeout or perl alarm),
#   matching session-start.sh's degradation pattern.
#
# Error handling: every failure mode exits 0 with a stderr breadcrumb
# prefixed "[legion-pre-grep]"; legion stderr goes to $LEGION_HOOK_LOG.

if [ "${LEGION_SKIP_PRE_GREP:-}" = "1" ] || [ "${LEGION_SKIP_PRE_GREP_SCIP:-}" = "1" ]; then
  echo "[legion-pre-grep] skipped (LEGION_SKIP_PRE_GREP=1)" >&2
  exit 0
fi

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

case "$TOOL" in
  Grep|Glob) ;;
  *) exit 0 ;;
esac

PATTERN=$(legion_hook_field '.tool_input.pattern')

if [ -z "$CWD" ] || [ -z "$PATTERN" ] || [ -z "$REPO" ]; then
  exit 0
fi

# Shared ladder library: symbol predicate, sym probes, #458 relevance
# gate, bypass telemetry. Sources _legion-indexed.sh and lib/emit.sh.
# shellcheck source=_legion-prequery.sh
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-prequery.sh"

# Universal gate: skip uncovered repos (#353).
legion_hook_covered || exit 0

LOG="$LEGION_HOOK_LOG"

# ---------- sym tier ----------
# Strip leading/trailing word boundaries (a common Grep convention), then
# apply the shared symbol-shape predicate -- the SAME predicate
# pre-bash-grep uses, so the two guards cannot disagree on a pattern.
SYMBOL=$(echo "$PATTERN" | sed -E 's/^\\b//; s/\\b$//')

HITS=""
LOCAL_HITS=""
HAD_SYM="false"
if legion_prequery_is_symbol "$SYMBOL"; then
  HITS=$(legion_prequery_sym_def "$SYMBOL")
  if [ -n "$HITS" ]; then
    HAD_SYM="true"
  fi
  if [ "$HAD_SYM" = "true" ] && legion_indexed "$SESSION_ID" "$REPO"; then
    LOCAL_HITS=$(legion_prequery_filter_hits_local "$HITS" "$REPO")
  fi
fi

# Bypass tier: refused when the pattern resolves to a real local symbol
# (the escape exists for free-text searches, not symbol queries dressed
# up as text); telemetried and allowed otherwise.
if [ "${LEGION_BYPASS_GREP:-}" = "1" ]; then
  if [ -n "$LOCAL_HITS" ] && [ "$LOCAL_HITS" != "[]" ]; then
    emit_deny "Soft bypass refused: \`${SYMBOL}\` resolves to a symbol in this repo's SCIP index. Use \`legion sym def ${SYMBOL} --repo ${REPO}\` (or \`sym refs\` / \`sym hover\`) instead. LEGION_BYPASS_GREP exists for free-text searches; it cannot route around sym for symbol queries.

\`legion sym def ${SYMBOL} --repo ${REPO}\` returned:

\`\`\`json
${LOCAL_HITS}
\`\`\`

There is no env-var hard escape -- the mandatory search block is the operator's permissions.deny."
    exit 0
  fi
  legion_prequery_record_bypass \
    "$REPO" "$SESSION_ID" "$TOOL" "$PATTERN" "env:LEGION_BYPASS_GREP=1" \
    "$HAD_SYM" "false"
  exit 0
fi

# State 2 BLOCK: indexed repo + local hit. Cluster-wide hits in unrelated
# repos never justify blocking (#458) -- they fall through to inject.
if [ -n "$LOCAL_HITS" ] && [ "$LOCAL_HITS" != "[]" ]; then
  emit_deny "Use \`legion sym def ${SYMBOL} --repo ${REPO}\` -- it answered this in bytes from the SCIP index. ${TOOL} on \`${PATTERN}\` would scan files and bill cache_read.

\`legion sym def ${SYMBOL} --repo ${REPO}\` returned:

\`\`\`json
${LOCAL_HITS}
\`\`\`

The soft bypass (LEGION_BYPASS_GREP=1) is REFUSED for symbol-shaped patterns that resolve in this repo's SCIP index -- it exists for free-text searches, not symbol queries dressed up as text. For symbols use \`legion sym def ${SYMBOL}\` / \`sym refs\` / \`sym list\`. There is no env-var hard escape -- the mandatory search block is the operator's permissions.deny."
  exit 0
fi

# State 1 INJECT (sym): hits exist but the block tier did not fire (repo
# not indexed, or hits live in other repos). Surface them and let the
# agent decide; best-effort refs count for context.
if [ "$HAD_SYM" = "true" ]; then
  REFS_LINE=""
  if [ -x "$LEGION" ]; then
    REFS=$("$LEGION" sym refs --json "$SYMBOL" 2>>"$LOG" || true)
    if [ -n "$REFS" ] && [ "$REFS" != "[]" ] && [ "$REFS" != "null" ]; then
      REF_COUNT=$(echo "$REFS" | jq 'length' 2>/dev/null || echo "0")
      REFS_LINE="(${REF_COUNT} reference(s) found via \`legion sym refs ${SYMBOL}\`)"
    fi
  fi

  CTX="## Legion sym for \`${SYMBOL}\` (${TOOL})

Before ${TOOL} on \`${PATTERN}\`, legion's pre-computed SCIP index returned:

\`\`\`json
${HITS}
\`\`\`

${REFS_LINE}

If this answers your symbol question, skip the ${TOOL}. SCIP is byte-cheap; file scans are not."
  emit_allow "$CTX" "legion sym hits for the symbol"
  exit 0
fi

# ---------- recall tier ----------
# Extract a semantic query from the pattern based on the tool type.
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
esac

# Bail if the final query collapsed to nothing, or legion is missing.
if [ -z "$QUERY" ]; then
  echo "[legion-pre-grep] empty query after extraction; skipping" >&2
  exit 0
fi
if [ ! -x "$LEGION" ]; then
  echo "[legion-pre-grep] legion binary not found; skipping" >&2
  exit 0
fi

# Run recall with an explicit 5s timeout. Prefer gtimeout if available,
# fall back to perl's alarm, matching session-start.sh's pattern.
if command -v gtimeout >/dev/null 2>&1; then
  RECALL_HITS=$(gtimeout 5 "$LEGION" recall --repo "$REPO" --context "$QUERY" --limit 3 --preview 400 2>>"$LOG")
  RC=$?
else
  RECALL_HITS=$(perl -e 'alarm 5; exec @ARGV' "$LEGION" recall --repo "$REPO" --context "$QUERY" --limit 3 --preview 400 2>>"$LOG")
  RC=$?
fi

# Timeout signal -- gtimeout returns 124, killed perl returns 142 (128+SIGALRM).
if [ "$RC" -eq 124 ] || [ "$RC" -eq 142 ]; then
  echo "[legion-pre-grep] recall timed out for query: ${QUERY}" >&2
  exit 0
fi
if [ "$RC" -ne 0 ]; then
  echo "[legion-pre-grep] recall exited $RC; skipping injection" >&2
  exit 0
fi

# Parse score lines from the recall output. Hits look like:
#   - ... (id: <uuid>, score: 0.82)
# If no score lines exist, treat all hits as passing (format may have
# changed). If every parsed score is below 0.3, skip injection.
if [ -n "$RECALL_HITS" ]; then
  SCORES=$(echo "$RECALL_HITS" | grep -oE 'score: [0-9]+\.[0-9]+' | sed 's/score: //')
  if [ -n "$SCORES" ]; then
    PASSING=$(echo "$SCORES" | awk '$1 >= 0.3 { print }')
    if [ -z "$PASSING" ]; then
      echo "[legion-pre-grep] skipped (all recall scores below 0.3 threshold)" >&2
      exit 0
    fi
  fi
fi

# No hits -- exit 0 without emitting any JSON.
if [ -z "$RECALL_HITS" ]; then
  exit 0
fi

CTX="## Legion memory for this query

Before ${TOOL} on \`${QUERY}\`, legion recall returned:

${RECALL_HITS}

If these answer your question, skip the ${TOOL}. Otherwise continue -- but consider whether the reflection explains WHY before you search for WHAT."

emit_allow "$CTX" "legion recall hits for the query"
