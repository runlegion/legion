#!/bin/bash
# Shared helpers for the search/Read enforcement hook ladder (#438/#439).
#
# The search-guard hooks (pre-bash-grep, pre-grep, pre-read-sym) source
# this file to share pattern detection, sym/recall probes, bypass-reason
# extraction, telemetry write, and ladder decision logic.
#
# Three-state ladder (per repo):
#
#   State 1 INJECT  -- repo not indexed OR sym/recall returned no hits.
#                       Hook emits allow + additionalContext containing
#                       whatever sym/recall did find. Agent decides.
#   State 2 BLOCK   -- repo indexed AND sym returned >= 1 def
#                       (high-confidence answer is one byte-cheap CLI
#                       call away). Hook emits decision: block with the
#                       sym results inline as the reason.
#   State 3 BYPASS  -- LEGION_BYPASS_GREP=1 env OR `# legion-bypass:`
#                       sentinel substring in the Bash command. Hook
#                       writes one telemetry row and emits allow.
#
# The bypass tier always wins: an explicit operator escape never blocks.
# A bypass is logged regardless of whether sym/recall had hits -- the
# value of telemetry is "what query was the index missing for an agent
# that escaped" and that question only resolves with both signals.

# Binary: prefer the prelude's resolved $LEGION (plugin-root copy with
# PATH fallback, #614); standalone consumers fall back to the plugin-root
# path.
LEGION_PREQUERY_BIN="${LEGION:-${CLAUDE_PLUGIN_ROOT:-}/bin/legion}"
LEGION_PREQUERY_LOG=/tmp/legion-hook-errors.log

# Pull in the coverage + indexed-repo probes. Both files are siblings;
# guard against double-source by checking whether the function is already
# defined (callers may have sourced them too).
if ! declare -F legion_covered >/dev/null 2>&1; then
  # shellcheck source=_legion-covered.sh
  source "${CLAUDE_PLUGIN_ROOT:-}/hooks/_legion-covered.sh"
fi
if ! declare -F legion_indexed >/dev/null 2>&1; then
  # shellcheck source=_legion-indexed.sh
  source "${CLAUDE_PLUGIN_ROOT:-}/hooks/_legion-indexed.sh"
fi
# Output emission (emit_allow / emit_deny) lives in lib/emit.sh.
if ! declare -F emit_allow >/dev/null 2>&1; then
  # shellcheck source=lib/emit.sh
  source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh"
fi

# legion_prequery_bash_binary CMD -- echo the leading search binary name
# (grep|rg|ag|ack|find|fd) on match, empty on miss.
legion_prequery_bash_binary() {
  local cmd="$1"
  local trimmed="${cmd#"${cmd%%[![:space:]]*}"}"
  case "$trimmed" in
    grep\ *|grep) echo grep ;;
    rg\ *|rg)     echo rg ;;
    ag\ *|ag)     echo ag ;;
    ack\ *|ack)   echo ack ;;
    find\ *|find) echo find ;;
    fd\ *|fd)     echo fd ;;
  esac
}

# legion_prequery_extract_pattern CMD BINARY -- echo the bare pattern
# argument extracted from a Bash command. Skips flags (-x, --foo,
# --bar=baz). Stops at the first pipe, redirect, or `;`. For find/fd,
# returns the search term (the value of -name, or the first non-flag
# positional). For grep/rg/ag/ack, returns the first non-flag arg.
legion_prequery_extract_pattern() {
  local cmd="$1" binary="$2"
  # Truncate at pipe / redirect / chain operators so we only parse the
  # leading invocation.
  local head="${cmd%%|*}"
  head="${head%%>*}"
  head="${head%%<*}"
  head="${head%%;*}"
  head="${head%%&&*}"

  # Tokenize on whitespace via `read -ra` so glob characters in the
  # command (`grep -r foo *.rs`) do NOT expand against the hook's CWD.
  # Caveat: quoted strings with embedded spaces are not honored -- a
  # pattern like `grep "foo bar" .` extracts just `"foo` (and the
  # symbol-shape filter rejects it, which is the right outcome; the
  # inject path tolerates a miss).
  local toks=()
  IFS=' ' read -ra toks <<< "$head"

  # Skip the binary token itself.
  local i=1
  if [ "$binary" = "find" ] || [ "$binary" = "fd" ]; then
    # find/fd: scan for -name VALUE or first non-flag arg that is not
    # a path segment.
    while [ $i -lt ${#toks[@]} ]; do
      local t="${toks[$i]}"
      case "$t" in
        -name|--iname)
          i=$((i + 1))
          [ $i -lt ${#toks[@]} ] && echo "${toks[$i]//\"/}" && return 0
          ;;
        -*) i=$((i + 1)) ;;
        *)
          # Strip surrounding quotes if any.
          local cleaned="${t//\"/}"
          cleaned="${cleaned//\'/}"
          # Skip path-shaped args like `.`, `./`, `src/`, leading `-`.
          case "$cleaned" in
            .|./*|/*) i=$((i + 1)) ;;
            *) echo "$cleaned" && return 0 ;;
          esac
          ;;
      esac
    done
    return 0
  fi

  # grep/rg/ag/ack: first non-flag positional, with -e PAT and --regexp=PAT
  # as explicit pattern carriers.
  while [ $i -lt ${#toks[@]} ]; do
    local t="${toks[$i]}"
    case "$t" in
      -e|--regexp)
        i=$((i + 1))
        [ $i -lt ${#toks[@]} ] && echo "${toks[$i]//\"/}" && return 0
        ;;
      --regexp=*)
        echo "${t#--regexp=}" | tr -d '"' | tr -d "'"
        return 0
        ;;
      -*) i=$((i + 1)) ;;
      *)
        local cleaned="${t//\"/}"
        cleaned="${cleaned//\'/}"
        echo "$cleaned"
        return 0
        ;;
    esac
  done
}

# legion_prequery_is_symbol PATTERN -- exit 0 if pattern looks like a bare
# symbol identifier (CamelCase or snake_case, length > 2, no regex
# metachars). Exit 1 otherwise.
legion_prequery_is_symbol() {
  local p="$1"
  # Strip leading/trailing word boundaries that some Grep callers add.
  p=$(echo "$p" | sed -E 's/^\\b//; s/\\b$//')
  if echo "$p" | grep -qE '^[A-Z][A-Za-z0-9_]{2,}$|^[a-z_][a-z_0-9]{2,}$'; then
    return 0
  fi
  return 1
}

# legion_prequery_bypass_reason CMD -- echo the bypass reason if the
# command (or env) signals an operator escape, empty otherwise.
legion_prequery_bypass_reason() {
  local cmd="$1"
  if [ "${LEGION_BYPASS_GREP:-}" = "1" ]; then
    echo "env:LEGION_BYPASS_GREP=1"
    return 0
  fi
  # `# legion-bypass: <free-form reason>` substring. Anchor to the
  # comment marker so a literal "legion-bypass" elsewhere in the
  # command does not trigger.
  local match
  match=$(echo "$cmd" | grep -oE '# legion-bypass:[^|;&>]*' | head -1)
  if [ -n "$match" ]; then
    # Strip the prefix and trim.
    local reason="${match#'# legion-bypass:'}"
    # Trim leading whitespace.
    reason="${reason#"${reason%%[![:space:]]*}"}"
    echo "$reason"
    return 0
  fi
}

# legion_prequery_record_bypass REPO SESSION TOOL PATTERN REASON HAD_SYM HAD_RECALL
# -- write one bypass row via the legion telemetry CLI. Best-effort; a
# missing binary or write failure is logged + swallowed (telemetry must
# never break the agent).
legion_prequery_record_bypass() {
  local repo="$1" session="$2" tool="$3" pattern="$4" reason="$5"
  local had_sym="$6" had_recall="$7"
  if [ ! -x "$LEGION_PREQUERY_BIN" ]; then
    return 0
  fi
  local args=(
    telemetry record-bypass
    --repo "$repo"
    --session-id "$session"
    --tool "$tool"
    --pattern "$pattern"
    --bypass-reason "$reason"
  )
  if [ "$had_sym" = "true" ]; then args+=(--had-sym-hits); fi
  if [ "$had_recall" = "true" ]; then args+=(--had-recall-hits); fi
  "$LEGION_PREQUERY_BIN" "${args[@]}" 2>>"$LEGION_PREQUERY_LOG" >/dev/null || true
}

# legion_prequery_sym_def PATTERN -- run `legion sym def --json <pattern>`.
# Echoes JSON array on hit, empty on miss / non-symbol / missing binary.
legion_prequery_sym_def() {
  local pattern="$1"
  if ! legion_prequery_is_symbol "$pattern"; then
    return 0
  fi
  if [ ! -x "$LEGION_PREQUERY_BIN" ]; then
    return 0
  fi
  local out
  out=$("$LEGION_PREQUERY_BIN" sym def --json "$pattern" 2>>"$LEGION_PREQUERY_LOG")
  if [ -z "$out" ] || [ "$out" = "[]" ] || [ "$out" = "null" ]; then
    return 0
  fi
  echo "$out"
}

# legion_prequery_filter_hits_local HITS_JSON REPO -- filter a sym hits
# JSON array down to entries where .repo matches REPO. Echoes filtered
# JSON (possibly "[]") on stdout. Used by the block-tier decision so
# common dictionary words like `name`, `data`, `value` that happen to be
# symbol-shaped do not trigger blocks when the search target is unrelated
# to those hits' actual repo (#458).
#
# Why: legion sym def --json searches every stored SCIP index across the
# cluster. A grep on the operator's local TOML config will return a block
# decision tripped by `name` happening to be a symbol in huttspawn's
# TypeScript -- those hits are not relevant to the search target. The
# relevance gate filters cluster-wide hits down to the SAME repo as the
# search context before deciding to block.
legion_prequery_filter_hits_local() {
  local hits="$1" repo="$2"
  if [ -z "$hits" ] || [ "$hits" = "[]" ] || [ "$hits" = "null" ]; then
    echo "[]"
    return 0
  fi
  if [ -z "$repo" ]; then
    echo "[]"
    return 0
  fi
  echo "$hits" | jq --arg r "$repo" '[.[] | select(.repo == $r)]' 2>/dev/null || echo "[]"
}
