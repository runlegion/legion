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
#
# ---------------------------------------------------------------------------
# THE BOUNDARY (#860). Read this before you write a hook that denies anything.
#
# Claude-layer hooks fire on the AGENT'S TOOL CALL ONLY. They never see child
# processes. A script, a Makefile, a test suite, a `bash -c` wrapper, or any
# tool that shells out executes unimpeded. The harness's own permissions.deny
# has the same reach: it is matched against the command the agent submits, not
# against what that command goes on to spawn.
#
# Verified 2026-08-04, twice. A shell script containing `gh --version` ran with
# no interception although a direct `gh` call is denied by no-gh.sh; and a test
# fixture's `git push -u origin main` reached GitHub and rewrote main with no
# Claude-layer interception at all (incident writeup: reflection 019fce1a).
#
# This is REQUIRED by design, not a bug awaiting a patch -- see no-git-push.sh's
# header. The alternative, a PATH shim over `git`, would recurse, because legion
# is itself a git consumer. Interception has to happen at the agent's tool call,
# and the agent's tool call is exactly one process deep.
#
# CONSEQUENCE: hooks SHAPE AGENT BEHAVIOUR. They are not an enforcement
# boundary. Anything that must be TOTAL lives at the git layer (.githooks/*),
# in the binary (e.g. REFUSED_BRANCHES in src/cli/push.rs), or in remote branch
# protection. A hook may front such a rule with a better error message; it can
# never be the rule.
#
# COROLLARY: bypass.jsonl cannot record what the hooks never saw, so a
# script-shaped escape writes no row. Absence of bypass rows is not absence of
# bypass.
#
# Every guard in this directory carries an ADVISORY / MUST-BE-TOTAL verdict, and
# each MUST-BE-TOTAL row names the layer that actually enforces it (or says that
# nothing does). New hook that denies something: add its row.
#   -> plugin/hooks/README.md
# ---------------------------------------------------------------------------

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
