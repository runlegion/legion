#!/bin/bash
# Legion PreToolUse hook: the legion-cmd router adapter (#1229).
#
# Thin on purpose (FR-CMD-014, FR-CMD-017): this script resolves the
# legion binary, runs `legion cmd-check --hook` against this hook's stdin
# (relayed through a temp file, not a pipe -- see the timeout section
# below for why), and relays its stdout unchanged. Every routing
# decision -- allow, rewrite, proxy, deny, ask, the decision deadline,
# every internal-error fail-closed path -- lives in that Rust adapter
# (`src/cmd/hook.rs`), not here. This script's only job is guaranteeing
# SOME response reaches the harness even when the binary cannot be
# found, cannot be run, hangs, or exits
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

# Seconds to wait for `legion cmd-check --hook` before killing it and
# emitting the static deny -- well under the harness's own hook timeout
# (see src/cmd/hook.rs's module doc), so a hung binary is backstopped
# here rather than relying on the harness's fail-OPEN timeout behavior.
LEGION_CMD_HOOK_TIMEOUT_SECS="${LEGION_CMD_HOOK_TIMEOUT_SECS:-8}"

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

# Portable timeout, no external `timeout`/`gtimeout` binary required (not
# preinstalled on macOS): background the binary directly, with both its
# input and its output redirected to plain files -- NOT a pipe in either
# direction. Two independent reasons:
#   1. `a | b &` makes `$!` the PID of a subshell wrapping the whole
#      pipeline, not of `b` itself, so a SIGTERM sent to it never reaches
#      the binary and a hang runs to completion regardless.
#   2. Capturing output via `OUTPUT=$(cmd &  ...; wait)` has the same
#      problem in the other direction: `$(...)` blocks until EOF on its
#      read end of the pipe, and EOF only happens once EVERY process
#      holding that pipe's write end has exited -- including a grandchild
#      the killed binary spawned and left running, which inherits the
#      same fd and keeps the pipe open for its own remaining lifetime.
#      Measured: with `OUTPUT=$(... &)`, killing the direct child at the
#      1s timeout still left the wrapper blocked for the fake binary's
#      full 30s hang, because its orphaned `sleep 30` never closed the
#      inherited stdout pipe. A plain file has no such reader/writer
#      handshake -- reading it after `wait` returns whatever was written
#      up to that point, no matter who still has it open.
INPUT_FILE=$(mktemp)
OUTPUT_FILE=$(mktemp)
trap 'rm -f "$INPUT_FILE" "$OUTPUT_FILE"' EXIT
printf '%s' "$INPUT" > "$INPUT_FILE"

"$LEGION_BIN_PATH" cmd-check --hook < "$INPUT_FILE" > "$OUTPUT_FILE" 2>/dev/null &
CHILD=$!
(
  sleep "$LEGION_CMD_HOOK_TIMEOUT_SECS"
  kill -TERM "$CHILD" 2>/dev/null
) &
WATCHER=$!
wait "$CHILD" 2>/dev/null
STATUS=$?
kill "$WATCHER" 2>/dev/null
wait "$WATCHER" 2>/dev/null
OUTPUT=$(cat "$OUTPUT_FILE")

if [ "$STATUS" -ne 0 ] || [ -z "$OUTPUT" ]; then
  printf '%s\n' "$STATIC_DENY"
  exit 0
fi

printf '%s\n' "$OUTPUT"
exit 0
