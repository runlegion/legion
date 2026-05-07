#!/bin/bash
# Shared helper: decide whether a repo has a stored SCIP index. Block-state
# enforcement in the grep/Read hooks (#438/#439) requires an index to redirect
# to; without one, the hooks fall through to inject-only behavior. A repo is
# "indexed" if `legion index --status --json` (#437) returns at least one row
# matching it.
#
# Cached per (session, repo) under XDG_CACHE_HOME alongside `_legion-covered`.
# Cache file contents: "1" = indexed, "0" = not indexed.
#
# Usage from a hook script:
#
#   source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-indexed.sh"
#
#   if ! legion_indexed "$SESSION_ID" "$REPO"; then
#     # fall through to inject-only behavior (no block tier)
#   fi
#
# Fail-open posture: missing binary, missing jq, malformed JSON -> return
# "not indexed" (1) so the block tier is silently skipped rather than fired
# on bad data. This is the inverse of `_legion-covered` (which fails to
# "covered=true") because a missing/degraded probe must not let block-state
# fire on a repo with no real index to redirect to.

LEGION_INDEXED_BIN="${CLAUDE_PLUGIN_ROOT}/bin/legion"

# legion_indexed SESSION_ID REPO -> exit 0 if indexed, 1 if not.
legion_indexed() {
  local session_id="${1:-}"
  local repo="${2:-}"
  if [ -z "$repo" ]; then
    return 1
  fi

  local safe_session
  safe_session=$(printf '%s' "$session_id" | tr -c 'a-zA-Z0-9_-' '_')
  local safe_repo
  safe_repo=$(printf '%s' "$repo" | tr -c 'a-zA-Z0-9_-' '_')

  local cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/legion"
  mkdir -p "$cache_dir" 2>/dev/null
  local cache_file="${cache_dir}/indexed-${safe_session}-${safe_repo}"

  if [ -f "$cache_file" ]; then
    if [ "$(cat "$cache_file" 2>/dev/null)" = "1" ]; then
      return 0
    fi
    return 1
  fi

  # Degraded environment: missing legion binary or missing jq. Do NOT cache
  # the verdict -- mirrors _legion-covered.sh, so a transient missing-binary
  # state during the session does not become sticky once the binary appears.
  if [ ! -x "$LEGION_INDEXED_BIN" ] || ! command -v jq >/dev/null 2>&1; then
    return 1
  fi

  local indexed=0
  if "$LEGION_INDEXED_BIN" index --status --json "$repo" 2>/dev/null \
       | jq -e --arg r "$repo" 'any(.[]; .repo == $r)' >/dev/null 2>&1; then
    indexed=1
  fi

  printf '%s' "$indexed" > "$cache_file" 2>/dev/null

  if [ "$indexed" -eq 1 ]; then
    return 0
  fi
  return 1
}
