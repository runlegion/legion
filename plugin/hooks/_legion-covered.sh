#!/bin/bash
# Shared helper: decide whether a repo is "legion-covered" so universal
# enforcement hooks can short-circuit cleanly in repos that legion does
# not manage. A repo is covered if either:
#
#   1. It has an entry in `legion watch list` (the operator told legion
#      "this is one of mine"), OR
#   2. It has at least one reflection in the legion DB (legion has been
#      used here before, so accumulated context exists).
#
# Either signal alone is enough -- a brand-new repo added to watch.toml
# but with no reflections yet should still be covered, and a repo that
# was reflected on but never added to watch.toml (rare but possible)
# should also count.
#
# The answer is cached per (session, repo) under XDG_CACHE_HOME so the
# coverage probe pays one fork+exec per repo per session, not per hook
# fire. Cache file contents: "1" = covered, "0" = not covered.
#
# Usage from a hook script:
#
#   source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-covered.sh"
#
#   if ! legion_covered "$SESSION_ID" "$REPO"; then
#     exit 0
#   fi
#
# The function never writes to stdout; diagnostic noise goes to stderr
# only on hard failures (missing legion binary, etc), and even then the
# function returns "covered" (0) by default so a degraded legion does
# not silently disable enforcement everywhere.

LEGION_COVERED_BIN="${CLAUDE_PLUGIN_ROOT}/bin/legion"

# legion_covered SESSION_ID REPO -> exit 0 if covered, 1 if not.
legion_covered() {
  local session_id="${1:-}"
  local repo="${2:-}"
  if [ -z "$repo" ]; then
    return 0
  fi

  local safe_session
  safe_session=$(printf '%s' "$session_id" | tr -c 'a-zA-Z0-9_-' '_')
  local safe_repo
  safe_repo=$(printf '%s' "$repo" | tr -c 'a-zA-Z0-9_-' '_')

  local cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/legion"
  mkdir -p "$cache_dir" 2>/dev/null
  local cache_file="${cache_dir}/covered-${safe_session}-${safe_repo}"

  if [ -f "$cache_file" ]; then
    if [ "$(cat "$cache_file" 2>/dev/null)" = "1" ]; then
      return 0
    fi
    return 1
  fi

  if [ ! -x "$LEGION_COVERED_BIN" ]; then
    # Legion binary missing -- err on the side of "covered" so we don't
    # silently disable enforcement when the user has just installed but
    # not yet built. The hook itself will fail open elsewhere anyway.
    return 0
  fi

  local covered=0

  # 1. watch list match.
  if "$LEGION_COVERED_BIN" watch list 2>/dev/null \
       | awk '{print $1}' \
       | grep -qx "$repo"; then
    covered=1
  fi

  # 2. reflections present (only check if not already proven covered).
  if [ "$covered" -eq 0 ]; then
    if "$LEGION_COVERED_BIN" stats --repo "$repo" 2>/dev/null \
         | grep -qE "^${repo}: [0-9]+ reflections"; then
      covered=1
    fi
  fi

  printf '%s' "$covered" > "$cache_file" 2>/dev/null

  if [ "$covered" -eq 1 ]; then
    return 0
  fi
  return 1
}
