#!/bin/bash
# Test runner for the legion-cmd PreToolUse hook wrapper (#1229).
#
# legion-cmd.sh is thin by design: it resolves the legion binary, feeds it
# stdin, and relays stdout. This suite does not re-test route's decisions
# (crates/legion-cmd and src/cmd/hook.rs own those); it tests the WRAPPER:
# binary resolution, pass-through of a real response, and the
# never-empty-response guarantee when the binary is missing, broken,
# silent, or hung. Since the cutover (#1236) it also runs the command line
# hooks.json registers, for every tool kind its matcher names, with the real
# binary present and absent (see the last section; it needs a `cargo build`).
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

# -- the registered command line, for every registered tool kind (#1236) ----
#
# Reads the PreToolUse entry for legion-cmd.sh out of hooks.json and runs its
# command line exactly as the harness does (`bash -c` with CLAUDE_PLUGIN_ROOT
# set), over a plugin root laid out like the installed one: the real
# bin/legion dispatcher, this hook, and the shipped policy. Every tool kind
# the matcher names gets one representative payload, first with the legion
# binary present and then with it absent, and each must answer with a hook
# response on stdout.
#
# The present case runs the real binary, found at LEGION_CMD_TEST_BIN, else
# ${CARGO_TARGET_DIR:-<repo>/target}/debug/legion; build it first with
# `cargo build`. A missing build is a failure, not a skip.

REPO_ROOT="$(cd "$HOOKS_SRC_DIR/../.." && pwd)"
HOOKS_JSON="$HOOKS_SRC_DIR/hooks.json"
REAL_LEGION="${LEGION_CMD_TEST_BIN:-${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/legion}"

REGISTERED=$(jq -c '[.hooks.PreToolUse[] | .matcher as $m | .hooks[]
  | select(.command | endswith("/hooks/legion-cmd.sh"))
  | {matcher: $m, command: .command, timeout: .timeout}]' "$HOOKS_JSON")
assert_eq "legion-cmd.sh is registered under exactly one PreToolUse matcher" \
  "$(printf '%s' "$REGISTERED" | jq 'length')" "1"
MATCHER=$(printf '%s' "$REGISTERED" | jq -r '.[0].matcher')
COMMAND_LINE=$(printf '%s' "$REGISTERED" | jq -r '.[0].command')
HOOK_TIMEOUT=$(printf '%s' "$REGISTERED" | jq -r '.[0].timeout')

# The script kills the binary at its own timeout and prints the static deny;
# the harness must still be waiting then, or it times the hook out and the
# call fails open. The `${...}` in the sed pattern is the literal text of the
# script's default, matched, not expanded.
# shellcheck disable=SC2016
WRAPPER_KILL_SECS=$(sed -n 's/^LEGION_CMD_HOOK_TIMEOUT_SECS="\${LEGION_CMD_HOOK_TIMEOUT_SECS:-\([0-9][0-9]*\)}"$/\1/p' "$HOOK_SRC")
if [ -n "$WRAPPER_KILL_SECS" ] && [ "$HOOK_TIMEOUT" -gt "$WRAPPER_KILL_SECS" ]; then
  verdict="above"
else
  verdict="not above"
fi
assert_eq "the registered timeout (${HOOK_TIMEOUT}s) is above the script's kill (${WRAPPER_KILL_SECS:-unset}s)" \
  "$verdict" "above"

# tool_input_for KIND -- one representative tool_input per tool kind. An
# unknown kind prints nothing, which the loop below reports as a failure.
tool_input_for() {
  case "$1" in
    Bash) printf '%s' '{"command":"git status"}' ;;
    Grep) printf '%s' '{"pattern":"fn main","path":"."}' ;;
    Glob) printf '%s' '{"pattern":"**/*.rs"}' ;;
    Read) printf '%s' '{"file_path":"/tmp/legion-test/README.md"}' ;;
    Write) printf '%s' '{"file_path":"/tmp/legion-test/notes.txt","content":"x"}' ;;
    Edit) printf '%s' '{"file_path":"/tmp/legion-test/notes.txt","old_string":"a","new_string":"b"}' ;;
    MultiEdit) printf '%s' '{"file_path":"/tmp/legion-test/notes.txt","edits":[{"old_string":"a","new_string":"b"}]}' ;;
    Agent | Task) printf '%s' '{"subagent_type":"Explore","description":"map","prompt":"map the hooks"}' ;;
    WebFetch) printf '%s' '{"url":"https://example.com","prompt":"summarize"}' ;;
    WebSearch) printf '%s' '{"query":"legion hooks"}' ;;
  esac
}

# make_registered_root DIR -- a plugin root shaped like the installed one.
make_registered_root() {
  local root="$1"
  mkdir -p "$root/hooks" "$root/bin" "$root/legion-cmd"
  cp "$HOOK_SRC" "$root/hooks/legion-cmd.sh"
  cp "$REPO_ROOT/plugin/bin/legion" "$root/bin/legion"
  cp "$REPO_ROOT/plugin/legion-cmd/policy.json" "$root/legion-cmd/policy.json"
}

# run_registered ROOT PLUGIN_DATA KIND -- the registered command line over one
# payload, with a sandboxed HOME, store and state dir, and a PATH holding no
# legion, so the binary is found only through the plugin layout. LEGION_REPO
# names the repo a recall lookup is scoped to, since the payload's cwd is not
# a checkout.
run_registered() {
  local root="$1" plugin_data="$2" kind="$3" input payload
  input=$(tool_input_for "$kind")
  payload=$(jq -cn --arg tool "$kind" --argjson input "${input:-null}" \
    '{tool_name: $tool, tool_input: $input, cwd: "/tmp/legion-test",
      session_id: "s", tool_use_id: "t", hook_event_name: "PreToolUse"}')
  printf '%s' "$payload" | env -i \
    PATH=/usr/bin:/bin \
    TMPDIR="${TMPDIR:-/tmp}" \
    HOME="$WORK/home" \
    CLAUDE_PLUGIN_ROOT="$root" \
    CLAUDE_PLUGIN_DATA="$plugin_data" \
    LEGION_DATA_DIR="$WORK/data" \
    LEGION_REPO=legion \
    XDG_STATE_HOME="$WORK/state" \
    bash -c "$COMMAND_LINE"
}

mkdir -p "$WORK/home" "$WORK/data" "$WORK/state" "$WORK/present-data" "$WORK/absent-data"
make_registered_root "$WORK/present-root"
make_registered_root "$WORK/absent-root"
if [ -x "$REAL_LEGION" ]; then
  ln -s "$REAL_LEGION" "$WORK/present-data/legion"
else
  FAIL=$((FAIL + 1))
  echo "  FAIL: no legion build at $REAL_LEGION (run cargo build, or set LEGION_CMD_TEST_BIN)" >&2
fi

IFS='|' read -r -a KINDS <<< "$MATCHER"
for kind in "${KINDS[@]}"; do
  if [ -z "$(tool_input_for "$kind")" ]; then
    FAIL=$((FAIL + 1))
    echo "  FAIL: the matcher names $kind, which has no representative payload here" >&2
    continue
  fi

  OUT=$(run_registered "$WORK/present-root" "$WORK/present-data" "$kind")
  EVENT=$(printf '%s' "$OUT" | jq -r '.hookSpecificOutput.hookEventName' 2>/dev/null)
  assert_eq "$kind: the binary answers the registered command line with a hook response" \
    "$EVENT" "PreToolUse"
  assert_not_contains "$kind: the response is the binary's, not the static deny" \
    "$OUT" "could not run the legion binary"
  assert_not_contains "$kind: route decided the call, not an adapter failure" \
    "$OUT" "legion-cmd could not decide"

  OUT=$(run_registered "$WORK/absent-root" "$WORK/absent-data" "$kind")
  DECISION=$(printf '%s' "$OUT" | jq -r '.hookSpecificOutput.permissionDecision' 2>/dev/null)
  assert_eq "$kind: with the binary absent the registered command line still denies" \
    "$DECISION" "deny"
  assert_contains "$kind: the absent-binary deny names the failure" \
    "$OUT" "could not run the legion binary"
done

finish_tests
