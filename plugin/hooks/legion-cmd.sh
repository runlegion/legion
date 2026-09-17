#!/bin/bash
# Legion PreToolUse hook: the legion-cmd router adapter (#1229).
#
# Thin on purpose (FR-CMD-014, FR-CMD-017): this script resolves the
# legion binary and pipes stdin straight to `legion cmd-check --hook`,
# relaying its stdout unchanged. Every routing decision -- allow, rewrite,
# proxy, deny, ask, the decision deadline, every internal-error
# fail-closed path -- lives in that Rust adapter (`src/cmd/hook.rs`), not
# here. This script's only job is guaranteeing SOME response reaches the
# harness even when the binary cannot be found, cannot be run, or exits
# without printing anything (FR-CMD-009: nothing fails silently, and a
# timed-out or crashed PreToolUse hook fails OPEN in Claude Code -- see
# `src/cmd/hook.rs`'s module doc for the hook contract this guards
# against).
#
# Deliberately does NOT source `lib/prelude.sh`: that file's own source
# line is conventionally guarded with `|| exit 0` (fail OPEN when the
# plugin root is missing or half-installed), which is the opposite of
# what this hook must do. `resolve_legion_bin` below duplicates
# `prelude.sh`'s `legion_resolve_bin` three-line precedence
# (`LEGION_BIN` > `${CLAUDE_PLUGIN_ROOT}/bin/legion` > `PATH`) rather than
# depending on a source that could itself fail silently.
#
# Not registered in `plugin/hooks/hooks.json` yet -- the cutover to a live
# PreToolUse hook is #1236, after the policy accounts for the guard hooks
# it replaces.

STATIC_DENY='{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"legion-cmd hook wrapper could not reach the legion binary -- failing closed rather than allowing an unrouted command"}}'

resolve_legion_bin() {
  if [ -n "${LEGION_BIN:-}" ]; then
    command -v "$LEGION_BIN" 2>/dev/null
    return
  fi
  if [ -x "${CLAUDE_PLUGIN_ROOT:-}/bin/legion" ]; then
    printf '%s\n' "${CLAUDE_PLUGIN_ROOT:-}/bin/legion"
    return
  fi
  command -v legion 2>/dev/null
}

INPUT=$(cat)
LEGION_BIN_PATH=$(resolve_legion_bin)

if [ -z "$LEGION_BIN_PATH" ] || [ ! -x "$LEGION_BIN_PATH" ]; then
  printf '%s\n' "$STATIC_DENY"
  exit 0
fi

OUTPUT=$(printf '%s' "$INPUT" | "$LEGION_BIN_PATH" cmd-check --hook 2>/dev/null)
STATUS=$?

if [ "$STATUS" -ne 0 ] || [ -z "$OUTPUT" ]; then
  printf '%s\n' "$STATIC_DENY"
  exit 0
fi

printf '%s\n' "$OUTPUT"
exit 0
