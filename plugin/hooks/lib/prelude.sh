#!/bin/bash
# Shared hook preamble (#614). Every hook used to longhand-copy the
# stdin-read / jq-extract / repo-resolve / coverage-gate block, and the
# copies had diverged (9 hooks honored LEGION_REPO, 7 did not). This file
# decides those questions ONCE.
#
# Usage from a hook script:
#
#   # shellcheck source=lib/prelude.sh
#   source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
#
#   legion_hook_parse || exit 0     # sets INPUT, CWD, TOOL, SESSION_ID, REPO
#   [ -n "$REPO" ] || exit 0
#   legion_hook_covered || exit 0   # universal enforcement gate (#353)
#
# The `|| exit 0` on the source line keeps hooks fail-open when the plugin
# root is missing or half-installed: a broken prelude must never block a
# tool call.
#
# Repo identity (the single decision point): the LEGION_REPO env var takes
# precedence over basename($CWD), in EVERY hook. An operator setting
# LEGION_REPO must get the same repo identity from enforcement, lifecycle,
# and memory hooks alike -- per-hook divergence here meant enforcement
# could apply to one repo set and memory to another (#614 audit).
#
# Binary resolution: ${CLAUDE_PLUGIN_ROOT}/bin/legion first, PATH lookup
# second. Hook subshells do not inherit the plugin bin dir on PATH -- only
# the Bash tool does (#204) -- so the plugin-root path must lead; the PATH
# fallback covers system-wide installs where the plugin copy is absent
# (the setup-binary.sh pattern). The LEGION_BIN env var overrides both
# (operator escape + test seam). $LEGION is empty when nothing resolves;
# callers check [ -x "$LEGION" ] and fail open.
#
# Skip/bypass env-var tiers (the closed set, documented once):
#   LEGION_SKIP_<HOOK>=1 -- disable one hook entirely, silently
#   LEGION_BYPASS_<X>=1  -- telemetried escape from a block tier
#   LEGION_NO_<FEATURE>=1 -- disable a feature (sync, daemon), not a hook

# Double-source guard.
if [ -n "${LEGION_PRELUDE_SOURCED:-}" ]; then
  return 0
fi
LEGION_PRELUDE_SOURCED=1

LEGION_HOOK_LOG=/tmp/legion-hook-errors.log
export LEGION_HOOK_LOG

# legion_resolve_bin -- echo the resolved legion binary path, empty when
# nothing resolves. LEGION_BIN (resolved through command -v so both bare
# names and absolute paths work) > plugin-root copy > PATH lookup.
legion_resolve_bin() {
  if [ -n "${LEGION_BIN:-}" ]; then
    command -v "$LEGION_BIN" 2>/dev/null || true
    return 0
  fi
  if [ -x "${CLAUDE_PLUGIN_ROOT:-}/bin/legion" ]; then
    printf '%s\n' "${CLAUDE_PLUGIN_ROOT:-}/bin/legion"
    return 0
  fi
  command -v legion 2>/dev/null || true
}

LEGION=$(legion_resolve_bin)

# legion_hook_parse -- read the hook event JSON from stdin and extract the
# common fields into INPUT, CWD, TOOL, SESSION_ID, REPO. Returns 1 on
# empty stdin so callers can `legion_hook_parse || exit 0`. jq failures
# leave the fields empty (fail-open).
legion_hook_parse() {
  INPUT=$(cat)
  CWD=""
  TOOL=""
  SESSION_ID=""
  REPO=""
  if [ -z "$INPUT" ]; then
    return 1
  fi
  CWD=$(echo "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
  # TOOL is part of the hook-facing API (consumed by sourcing hooks).
  # shellcheck disable=SC2034
  TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
  SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)
  if [ -n "${LEGION_REPO:-}" ]; then
    REPO="$LEGION_REPO"
  elif [ -n "$CWD" ]; then
    REPO=$(basename "$CWD")
  fi
  return 0
}

# legion_hook_field JQ_PATH -- extract one field from the parsed event,
# e.g. legion_hook_field '.tool_input.command'. Empty on miss/jq failure.
legion_hook_field() {
  echo "$INPUT" | jq -r "${1} // empty" 2>/dev/null
}

# legion_hash_str STR -- portable md5 of a string (+ trailing newline, to
# stay byte-compatible with the historical `echo | md5` marker paths).
# Used for the /tmp marker-file protocol (work/reflected markers, debounce
# locks, dedup keys); writers and readers MUST hash identically.
legion_hash_str() {
  printf '%s\n' "$1" | md5 -q 2>/dev/null \
    || printf '%s\n' "$1" | md5sum 2>/dev/null | cut -d' ' -f1
}

# Coverage gate (#353). Sourced here so every hook shares one probe; the
# declare -F guard tolerates hooks that sourced it themselves.
if ! declare -F legion_covered >/dev/null 2>&1; then
  # shellcheck source=../_legion-covered.sh
  source "${CLAUDE_PLUGIN_ROOT:-}/hooks/_legion-covered.sh"
fi

# legion_hook_covered -- the universal enforcement gate over the parsed
# event: exit-0 passthrough territory when the repo is not legion-covered.
# Hooks opt IN by calling this; repo-resolution-only hooks (markers,
# task-state mirrors) simply don't call it.
legion_hook_covered() {
  legion_covered "$SESSION_ID" "$REPO"
}
