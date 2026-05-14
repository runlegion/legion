#!/bin/bash
# Test runner for the no-gh PreToolUse hook.
#
# Verifies the hook catches gh invocations regardless of how the binary
# is referenced: bare command, absolute path, leading whitespace. Run
# from the repo root:
#
#   bash plugin/hooks/test-no-gh.sh

set -u

PASS=0
FAIL=0

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/plugin/hooks"
cp plugin/hooks/no-gh.sh "$WORK/plugin/hooks/"
# Stub _legion-covered.sh to always return covered so the hook runs the
# match logic instead of falling through.
cat > "$WORK/plugin/hooks/_legion-covered.sh" <<'EOF'
legion_covered() {
  return 0
}
EOF

export CLAUDE_PLUGIN_ROOT="$WORK/plugin"

HOOK="$WORK/plugin/hooks/no-gh.sh"

run_hook() {
  local cmd="$1"
  printf '%s' "{\"tool_input\":{\"command\":${cmd}},\"cwd\":\"/tmp/legion-test\",\"session_id\":\"test\"}" | bash "$HOOK"
}

assert_blocked() {
  local desc="$1" cmd="$2"
  local out
  out=$(run_hook "$cmd")
  if echo "$out" | grep -q '"decision":[[:space:]]*"block"'; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    cmd: $cmd" >&2
    echo "    out: $out" >&2
  fi
}

assert_allowed() {
  local desc="$1" cmd="$2"
  local out
  out=$(run_hook "$cmd")
  if echo "$out" | grep -q '"decision":[[:space:]]*"block"'; then
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc (expected allow, got block)" >&2
    echo "    cmd: $cmd" >&2
  else
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  fi
}

echo "==> blocks bare gh invocations"
assert_blocked "bare gh subcommand"     '"gh pr merge 123"'
assert_blocked "bare gh with no args"   '"gh"'
assert_blocked "gh with leading space"  '"   gh issue list"'

echo "==> blocks absolute-path gh invocations"
assert_blocked "homebrew absolute path"  '"/opt/homebrew/bin/gh pr merge 123"'
assert_blocked "usr-local absolute path" '"/usr/local/bin/gh issue list"'
assert_blocked "usr-bin absolute path"   '"/usr/bin/gh pr view 1"'
assert_blocked "tilde absolute path"     '"~/bin/gh pr merge 123"'
assert_blocked "absolute path no args"   '"/opt/homebrew/bin/gh"'

echo "==> allows commands that merely mention gh"
assert_allowed "ghostscript"      '"ghostscript --version"'
assert_allowed "git status"       '"git status"'
assert_allowed "echo gh"          '"echo gh pr merge"'
assert_allowed "grep gh logs"     '"grep gh /var/log/foo"'
assert_allowed "path with gh dir" '"ls /opt/ghosts/"'

echo
echo "PASS: $PASS"
echo "FAIL: $FAIL"
[ "$FAIL" -eq 0 ] || exit 1
