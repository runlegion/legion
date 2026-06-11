#!/bin/bash
# Test runner for lib/prelude.sh + lib/emit.sh (#614).
#
# Covers the contracts every hook now leans on:
# - stdin parse into INPUT/CWD/TOOL/SESSION_ID/REPO
# - repo identity precedence: LEGION_REPO env over basename(cwd), uniformly
# - binary resolution: LEGION_BIN override > plugin-root copy > PATH
# - legion_hash_str byte-compatibility with the historical echo|md5 markers
# - the four emit shapes (allow/deny/block/context)
# - structural lock: every production hook sources lib/prelude.sh
#
# Run from anywhere:
#   bash plugin/hooks/test-lib.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

# No hooks to copy -- the lib itself is under test.
# shellcheck disable=SC2119
make_plugin_root

PRELUDE="$CLAUDE_PLUGIN_ROOT/hooks/lib/prelude.sh"

# run_parse EVENT_JSON [ENV=VAL...] -- parse the event in a fresh shell and
# print the extracted fields pipe-separated.
run_parse() {
  local event="$1"
  shift
  echo "$event" | env "$@" bash -c "source '$PRELUDE'; legion_hook_parse; printf '%s|%s|%s|%s' \"\$CWD\" \"\$TOOL\" \"\$SESSION_ID\" \"\$REPO\""
}

echo "==> legion_hook_parse extracts the common fields"
out=$(run_parse '{"cwd":"/tmp/myrepo","tool_name":"Grep","session_id":"s1"}')
assert_eq "cwd/tool/session/repo extracted" "$out" "/tmp/myrepo|Grep|s1|myrepo"

echo "==> repo precedence: LEGION_REPO overrides basename(cwd)"
out=$(run_parse '{"cwd":"/tmp/myrepo","tool_name":"Grep","session_id":"s1"}' LEGION_REPO=other-repo)
assert_eq "LEGION_REPO wins over basename" "$out" "/tmp/myrepo|Grep|s1|other-repo"

echo "==> missing cwd leaves repo empty (no basename guessing)"
out=$(run_parse '{"tool_name":"Grep","session_id":"s1"}')
assert_eq "no cwd -> empty repo" "$out" "|Grep|s1|"

echo "==> empty stdin returns 1 from legion_hook_parse"
printf '' | bash -c "source '$PRELUDE'; legion_hook_parse"
assert_rc "empty stdin -> rc 1" 1 $?

echo "==> legion_hook_field extracts tool_input fields"
out=$(echo '{"tool_input":{"command":"ls -la"}}' \
  | bash -c "source '$PRELUDE'; legion_hook_parse; legion_hook_field '.tool_input.command'")
assert_eq "field extraction" "$out" "ls -la"

echo "==> binary resolution: plugin-root copy wins by default"
out=$(bash -c "source '$PRELUDE'; printf '%s' \"\$LEGION\"" </dev/null)
assert_eq "plugin-root binary resolved" "$out" "$CLAUDE_PLUGIN_ROOT/bin/legion"

echo "==> binary resolution: LEGION_BIN override beats the plugin copy"
out=$(LEGION_BIN=/bin/ls bash -c "source '$PRELUDE'; printf '%s' \"\$LEGION\"" </dev/null)
assert_eq "LEGION_BIN override honored" "$out" "/bin/ls"

echo "==> binary resolution: PATH fallback when the plugin copy is absent"
FAKEBIN="$WORK/fakebin"
mkdir -p "$FAKEBIN"
printf '#!/bin/bash\nexit 0\n' > "$FAKEBIN/legion"
chmod +x "$FAKEBIN/legion"
out=$(CLAUDE_PLUGIN_ROOT="$WORK/nonexistent" PATH="$FAKEBIN:$PATH" \
  bash -c "source '$HOOKS_SRC_DIR/lib/prelude.sh' 2>/dev/null; printf '%s' \"\$LEGION\"" </dev/null)
assert_eq "PATH fallback resolves" "$out" "$FAKEBIN/legion"

echo "==> legion_hash_str matches the historical echo|md5 marker hash"
expected=$(echo "/tmp/some/cwd" | md5 -q 2>/dev/null || echo "/tmp/some/cwd" | md5sum 2>/dev/null | cut -d' ' -f1)
assert_eq "hash byte-compatible with echo|md5" "$(legion_hash_str "/tmp/some/cwd")" "$expected"

# ---------- emit shapes ----------

# shellcheck source=lib/emit.sh
source "$CLAUDE_PLUGIN_ROOT/hooks/lib/emit.sh"

echo "==> emit_allow shape"
out=$(emit_allow "some context" "why not")
assert_eq "event name" "$(echo "$out" | jq -r '.hookSpecificOutput.hookEventName')" "PreToolUse"
assert_eq "decision" "$(echo "$out" | jq -r '.hookSpecificOutput.permissionDecision')" "allow"
assert_eq "reason" "$(echo "$out" | jq -r '.hookSpecificOutput.permissionDecisionReason')" "why not"
assert_eq "context" "$(echo "$out" | jq -r '.hookSpecificOutput.additionalContext')" "some context"

echo "==> emit_deny shape (with and without context)"
out=$(emit_deny "refused")
assert_eq "decision" "$(echo "$out" | jq -r '.hookSpecificOutput.permissionDecision')" "deny"
assert_eq "reason" "$(echo "$out" | jq -r '.hookSpecificOutput.permissionDecisionReason')" "refused"
assert_eq "no context key when absent" "$(echo "$out" | jq -r '.hookSpecificOutput | has("additionalContext")')" "false"
out=$(emit_deny "refused" "with ctx")
assert_eq "context rides along when given" "$(echo "$out" | jq -r '.hookSpecificOutput.additionalContext')" "with ctx"

echo "==> emit_block shape (Stop events)"
out=$(emit_block "cannot stop yet")
assert_eq "top-level decision" "$(echo "$out" | jq -r '.decision')" "block"
assert_eq "top-level reason" "$(echo "$out" | jq -r '.reason')" "cannot stop yet"

echo "==> emit_context shape"
out=$(emit_context "SessionStart" "boot context")
assert_eq "event name parameterized" "$(echo "$out" | jq -r '.hookSpecificOutput.hookEventName')" "SessionStart"
assert_eq "context delivered" "$(echo "$out" | jq -r '.hookSpecificOutput.additionalContext')" "boot context"
assert_eq "no permissionDecision on context-only" "$(echo "$out" | jq -r '.hookSpecificOutput | has("permissionDecision")')" "false"

# ---------- structural lock ----------

echo "==> every production hook sources lib/prelude.sh"
# setup-binary.sh is exempt: it CREATES the binary the prelude resolves and
# parses no hook event. _legion-*.sh are sourced helpers, not registered
# hooks; test-*.sh are harnesses.
for f in "$HOOKS_SRC_DIR"/*.sh; do
  base=$(basename "$f")
  case "$base" in
    test-*|_legion-*|setup-binary.sh) continue ;;
  esac
  if grep -q 'hooks/lib/prelude.sh' "$f"; then
    PASS=$((PASS + 1))
    echo "  PASS: $base sources lib/prelude.sh"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $base does not source lib/prelude.sh" >&2
  fi
done

echo "==> no hook carries its own legacy PreToolUse block dialect"
# The legacy top-level {decision:block} shape is allowed only via
# lib/emit.sh's emit_block (Stop) and precompact.sh's deliberate static
# heredoc (PreCompact reason goes to the user; must survive a missing jq).
for f in "$HOOKS_SRC_DIR"/*.sh; do
  base=$(basename "$f")
  case "$base" in
    test-*|precompact.sh) continue ;;
  esac
  if grep -q '"decision":' "$f" || grep -q "decision: \"block\"" "$f"; then
    FAIL=$((FAIL + 1))
    echo "  FAIL: $base hand-builds a decision JSON instead of using lib/emit.sh" >&2
  else
    PASS=$((PASS + 1))
    echo "  PASS: $base emits through lib/emit.sh"
  fi
done

finish_tests
