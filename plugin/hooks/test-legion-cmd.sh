#!/bin/bash
# Test runner for the legion-cmd PreToolUse hook wrapper (#1229).
#
# legion-cmd.sh is thin by design: it resolves the legion binary, feeds it
# stdin, and relays stdout. This suite does not re-test route's decisions
# (crates/legion-cmd and src/cmd/hook.rs own those); it tests the WRAPPER:
# binary resolution, pass-through of a real response, and the
# never-empty-response guarantee when the binary is missing, broken,
# silent, or hung.
#
# Run from anywhere:
#
#   bash plugin/hooks/test-legion-cmd.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

HOOK_SRC="$HOOKS_SRC_DIR/legion-cmd.sh"

# A fake legion that only answers `cmd-check --hook`, shaped by
# FAKE_CMD_CHECK_* env vars. Not the shared `make_stub_legion`: that stub
# answers twenty unrelated subcommands, and this wrapper's failure modes
# (exit non-zero, print nothing, hang) are its own.
write_fake_legion() {
  local path="$1"
  cat > "$path" <<'EOF'
#!/bin/bash
cat >/dev/null
if [ "${FAKE_CMD_CHECK_EXIT_NONZERO:-}" = "1" ]; then
  exit 1
fi
if [ "${FAKE_CMD_CHECK_EMPTY:-}" = "1" ]; then
  exit 0
fi
if [ "${FAKE_CMD_CHECK_HANG:-}" = "1" ]; then
  sleep 30
  exit 0
fi
if [ "${1:-}" = "cmd-check" ] && [ "${2:-}" = "--hook" ]; then
  printf '%s\n' "${FAKE_CMD_CHECK_RESPONSE:-{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\"\}\}}"
  exit 0
fi
exit 1
EOF
  chmod +x "$path"
}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FAKE_LEGION="$WORK/legion"
write_fake_legion "$FAKE_LEGION"

PAYLOAD='{"tool_name":"Bash","tool_input":{"command":"ls"},"cwd":"/tmp/legion-test","session_id":"s","tool_use_id":"t"}'

run_hook_with_bin() {
  local bin="$1"
  printf '%s' "$PAYLOAD" | LEGION_BIN="$bin" bash "$HOOK_SRC"
}

# -- a real adapter response passes through unchanged ------------------------

OUT=$(FAKE_CMD_CHECK_RESPONSE='{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"test reason"}}' \
  run_hook_with_bin "$FAKE_LEGION")
assert_contains "a real adapter response passes through unchanged" "$OUT" '"permissionDecisionReason":"test reason"'

# -- a missing binary still yields a response, never nothing -----------------

OUT=$(run_hook_with_bin "$WORK/no-such-binary")
assert_contains "a missing binary denies with a reason" "$OUT" '"permissionDecision":"deny"'
assert_contains "a missing binary names the command to run instead" "$OUT" 'legion cmd-check'
assert_not_contains "a missing binary never allows" "$OUT" '"permissionDecision":"allow"'

# -- a broken binary (non-zero exit) still yields a response -----------------

OUT=$(FAKE_CMD_CHECK_EXIT_NONZERO=1 run_hook_with_bin "$FAKE_LEGION")
assert_contains "a broken binary (non-zero exit) denies" "$OUT" '"permissionDecision":"deny"'

# -- a silent binary (exit 0, empty stdout) still yields a response ----------

OUT=$(FAKE_CMD_CHECK_EMPTY=1 run_hook_with_bin "$FAKE_LEGION")
assert_contains "a silent binary (empty stdout) denies" "$OUT" '"permissionDecision":"deny"'

# within_seconds DESC MAX_SECS ELAPSED -- ELAPSED (whole seconds) is at
# most MAX_SECS.
within_seconds() {
  local desc="$1" max_secs="$2" elapsed="$3"
  local verdict="too slow"
  [ "$elapsed" -le "$max_secs" ] && verdict="within budget"
  assert_eq "$desc (${elapsed}s elapsed, budget ${max_secs}s)" "$verdict" "within budget"
}

# -- a fast binary is not delayed by the timeout machinery -------------------

START=$(date +%s)
OUT=$(run_hook_with_bin "$FAKE_LEGION")
END=$(date +%s)
assert_contains "a fast binary's response passes through" "$OUT" '"hookSpecificOutput"'
within_seconds "a fast binary is not delayed by the watcher" 1 "$((END - START))"

# -- a hung binary is killed and denied within the configured timeout -------

START=$(date +%s)
OUT=$(FAKE_CMD_CHECK_HANG=1 LEGION_CMD_HOOK_TIMEOUT_SECS=1 run_hook_with_bin "$FAKE_LEGION")
END=$(date +%s)
assert_contains "a hung binary denies with a reason" "$OUT" '"permissionDecision":"deny"'
within_seconds "a hung binary is killed well before its own 30s sleep ends" 10 "$((END - START))"

# -- the wrapper always exits 0 ---------------------------------------------

run_hook_with_bin "$FAKE_LEGION" >/dev/null
assert_rc "the wrapper exits 0 on a working binary" 0 "$?"
run_hook_with_bin "$WORK/no-such-binary" >/dev/null
assert_rc "the wrapper exits 0 on a missing binary" 0 "$?"
FAKE_CMD_CHECK_EXIT_NONZERO=1 run_hook_with_bin "$FAKE_LEGION" >/dev/null
assert_rc "the wrapper exits 0 on a broken binary" 0 "$?"

finish_tests
