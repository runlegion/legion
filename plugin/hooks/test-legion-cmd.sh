#!/bin/bash
# Test runner for the legion-cmd PreToolUse hook wrapper (#1229).
#
# legion-cmd.sh is thin by design: it resolves the legion binary, pipes
# stdin to `legion cmd-check --hook`, and relays stdout verbatim. This
# suite does not re-test route's own decision logic (that lives in
# crates/legion-cmd's own tests and src/cmd/hook.rs's unit/integration
# tests) -- it tests the WRAPPER: binary resolution, pass-through of a
# real response, and the never-empty-response guarantee when the binary
# is missing, broken, or silent.
#
# Run from anywhere:
#
#   bash plugin/hooks/test-legion-cmd.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

HOOK_SRC="$HOOKS_SRC_DIR/legion-cmd.sh"

# A self-contained fake legion binary, not the shared multi-purpose stub
# `make_stub_legion` builds (that one answers ~20 unrelated subcommands
# and has no `cmd-check` case worth adding there for one hook's wrapper
# test): this one only ever answers `cmd-check --hook`, shaped by
# FAKE_CMD_CHECK_* env vars.
write_fake_legion() {
  local path="$1"
  cat > "$path" <<'EOF'
#!/bin/bash
cat >/dev/null # consume stdin like the real adapter always does
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
  if [ -n "${FAKE_CMD_CHECK_RESPONSE:-}" ]; then
    printf '%s\n' "$FAKE_CMD_CHECK_RESPONSE"
  else
    printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PreToolUse"}}'
  fi
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

run_hook_with_bin() {
  local bin="$1"
  LEGION_BIN="$bin" bash "$HOOK_SRC" <<<'{"tool_name":"Bash","tool_input":{"command":"ls"}}'
}

# -- a real adapter response passes through unchanged ------------------------

OUT=$(FAKE_CMD_CHECK_RESPONSE='{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"test reason"}}' \
  run_hook_with_bin "$FAKE_LEGION")
assert_contains "a real adapter response passes through unchanged" "$OUT" '"permissionDecisionReason":"test reason"'

# -- a missing binary still yields a response, never nothing -----------------

OUT=$(run_hook_with_bin "$WORK/no-such-binary")
assert_contains "a missing binary still denies with a reason" "$OUT" '"permissionDecision":"deny"'
assert_not_contains "a missing binary never allows" "$OUT" '"permissionDecision":"allow"'

# -- a broken binary (non-zero exit) still yields a response -----------------

OUT=$(FAKE_CMD_CHECK_EXIT_NONZERO=1 run_hook_with_bin "$FAKE_LEGION")
assert_contains "a broken binary (non-zero exit) still denies" "$OUT" '"permissionDecision":"deny"'

# -- a silent binary (exit 0, empty stdout) still yields a response ----------

OUT=$(FAKE_CMD_CHECK_EMPTY=1 run_hook_with_bin "$FAKE_LEGION")
assert_contains "a silent binary (empty stdout) still denies" "$OUT" '"permissionDecision":"deny"'

# within_seconds DESC MAX_SECS -- run the rest of the pipeline via stdin,
# asserting the elapsed wall time is at most MAX_SECS, using the shared
# assert_eq helper (a boolean-as-string comparison, not a hand-rolled
# PASS/FAIL count) rather than a bespoke numeric assertion.
within_seconds() {
  local desc="$1" max_secs="$2" elapsed="$3"
  local verdict="too slow"
  [ "$elapsed" -le "$max_secs" ] && verdict="within budget"
  assert_eq "$desc (${elapsed}s elapsed, budget ${max_secs}s)" "$verdict" "within budget"
}

# -- a fast-answering binary returns well under a second, not delayed by
#    the timeout machinery itself ---------------------------------------

START=$(date +%s)
OUT=$(run_hook_with_bin "$FAKE_LEGION")
END=$(date +%s)
assert_contains "a fast binary's response passes through" "$OUT" '"hookSpecificOutput"'
within_seconds "a fast binary is not delayed by the timeout watcher" 1 "$((END - START))"

# -- a hung binary is killed and denied within the configured timeout -------

START=$(date +%s)
OUT=$(FAKE_CMD_CHECK_HANG=1 LEGION_CMD_HOOK_TIMEOUT_SECS=1 run_hook_with_bin "$FAKE_LEGION")
END=$(date +%s)
assert_contains "a hung binary still denies with a reason" "$OUT" '"permissionDecision":"deny"'
within_seconds "a hung binary is killed well before its own 30s sleep finishes" 10 "$((END - START))"

# -- the wrapper itself always exits 0, even on every failure path above ----

run_hook_with_bin "$FAKE_LEGION" >/dev/null
assert_rc "the wrapper always exits 0 on a working binary" 0 "$?"
run_hook_with_bin "$WORK/no-such-binary" >/dev/null
assert_rc "the wrapper always exits 0 on a missing binary" 0 "$?"

finish_tests
