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
# emitting the static deny. Three numbers have to stay ordered:
#   Rust adapter deadline (route.deadline_ms, default 7000ms = 7s, see
#     src/cmd/hook.rs's module doc)
#     < this wrapper's own timeout (9s: a 2s buffer over the adapter's
#       own deadline, for process startup and stdout-serialization time
#       the Rust deadline itself does not cover)
#     < the harness's configured hook timeout for this entry in
#       hooks.json, once #1236 registers it.
# Today's existing Bash hook entries in hooks.json use 5-6s timeouts --
# BELOW even the Rust adapter's own 7s deadline. #1236 (the cutover) must
# raise this hook's own hooks.json timeout above this wrapper's 9s, not
# merely above 7000ms, or the harness would kill (and fail OPEN on) a
# call this wrapper was about to answer correctly.
LEGION_CMD_HOOK_TIMEOUT_SECS="${LEGION_CMD_HOOK_TIMEOUT_SECS:-9}"

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

# Portable timeout (macOS ships no `timeout`): run the binary in the
# background with its input and output in plain files, not pipes. With a
# pipe, `$!` names the pipeline's subshell rather than the binary, and
# `$(...)` waits for every process holding the pipe -- including anything
# a killed binary left behind -- so a hang would still block. A file is
# read once `wait` returns, whoever still has it open.
INPUT_FILE=$(mktemp)
OUTPUT_FILE=$(mktemp)
trap 'rm -f "$INPUT_FILE" "$OUTPUT_FILE"' EXIT
printf '%s' "$INPUT" > "$INPUT_FILE"

"$LEGION_BIN_PATH" cmd-check --hook < "$INPUT_FILE" > "$OUTPUT_FILE" 2>/dev/null &
CHILD=$!

# The watcher kills CHILD once the timeout passes. Its output goes to
# /dev/null: a watcher holding this script's stdout would keep the
# harness's read open until the watcher exits, delaying every answer by
# the full timeout. Its `sleep` is its own child so it can `wait` on it,
# and the TERM trap kills that `sleep` when the fast path stops the
# watcher, so nothing is left running after this script answers.
(
  sleep "$LEGION_CMD_HOOK_TIMEOUT_SECS" &
  SLEEP_PID=$!
  trap 'kill "$SLEEP_PID" 2>/dev/null; exit 0' TERM
  wait "$SLEEP_PID"
  kill -TERM "$CHILD" 2>/dev/null
) >/dev/null 2>&1 &
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
