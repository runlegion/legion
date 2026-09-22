#!/bin/bash
# Legion PreToolUse hook: the legion-cmd router adapter (#1229).
#
# Thin on purpose (FR-CMD-014, FR-CMD-017): resolve the legion binary, run
# `legion cmd-check --hook` over this hook's stdin, relay its stdout
# unchanged. Every decision -- allow, rewrite, proxy, deny, ask, the
# decision deadline, every fail-closed path -- lives in the Rust adapter
# (`src/cmd/hook.rs`). This script's one job is that SOME response reaches
# the harness even when the binary is missing, cannot run, hangs, or exits
# without printing: a PreToolUse hook that times out or exits non-zero
# fails OPEN in Claude Code, silently, and the model is never told
# (FR-CMD-009).
#
# Deliberately does NOT source `lib/prelude.sh`: its source line is
# conventionally `|| exit 0` (fail open when the plugin root is missing or
# half-installed), the opposite of what this hook must do. The three-line
# binary precedence (`LEGION_BIN` > `${CLAUDE_PLUGIN_ROOT}/bin/legion` >
# `PATH`) is repeated here instead of depending on a source that could
# itself fail silently.
#
# Not registered in `plugin/hooks/hooks.json`: the cutover issue registers
# it, after the policy accounts for the hooks it replaces, and must set the
# entry's `timeout` above LEGION_CMD_HOOK_TIMEOUT_SECS below (today's Bash
# hook entries use 5 and 6 seconds, below even the adapter's own 7000 ms
# default).

STATIC_DENY='{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"legion-cmd could not run the legion binary (missing, failed, or silent) -- failing closed rather than running an unrouted command. Instead: legion cmd-check -- <command>"}}'

# Seconds to wait for `legion cmd-check --hook` before killing it and
# printing the static deny. Three numbers stay ordered: the adapter's own
# deadline (route.deadline_ms, default 7000 ms) < this timeout (a buffer for
# process start and output) < the hooks.json entry timeout the cutover sets.
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
# background with its input and output in files, not pipes. With a pipe,
# `$!` names the pipeline's subshell rather than the binary, and `$(...)`
# waits for every process holding the pipe open, so a hung binary would
# still block past the kill. A file is read once `wait` returns.
INPUT_FILE=$(mktemp)
OUTPUT_FILE=$(mktemp)
trap 'rm -f "$INPUT_FILE" "$OUTPUT_FILE"' EXIT
printf '%s' "$INPUT" > "$INPUT_FILE"

"$LEGION_BIN_PATH" cmd-check --hook < "$INPUT_FILE" > "$OUTPUT_FILE" 2>/dev/null &
CHILD=$!

# The watcher kills CHILD once the timeout passes. Its own output goes to
# /dev/null: a watcher holding this script's stdout would keep the
# harness's read open until the watcher exits, delaying every answer by
# the full timeout. Its `sleep` is a child it can `wait` on, and the TERM
# trap kills that `sleep` when the fast path stops the watcher, so nothing
# is left running after this script answers.
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
