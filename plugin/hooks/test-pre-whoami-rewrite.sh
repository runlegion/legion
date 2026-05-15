#!/bin/bash
# Test runner for the pre-whoami-rewrite PreToolUse hook.
#
# Stubs the legion binary to control whether an identity "exists" so we
# can exercise the block + bypass paths. Run from the repo root:
#
#   bash plugin/hooks/test-pre-whoami-rewrite.sh

set -u

PASS=0
FAIL=0

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/plugin/bin" "$WORK/plugin/hooks"
cp plugin/hooks/pre-whoami-rewrite.sh "$WORK/plugin/hooks/"

# Stub legion: switches behaviour on $LEGION_TEST_HAS_IDENTITY.
cat > "$WORK/plugin/bin/legion" <<'EOF'
#!/bin/bash
if [ "$1" = "whoami" ]; then
  if [ "${LEGION_TEST_HAS_IDENTITY:-0}" = "1" ]; then
    echo "=== WHO YOU ARE -- READ THIS ==="
    echo "[Legion] Identity for test:"
    echo "- I am the test agent. I do test things."
    echo ""
    echo "DECISION ORDER: do the right thing."
    exit 0
  fi
  echo "=== WHO YOU ARE -- READ THIS ==="
  echo "[Legion] Identity for test:"
  exit 0
fi
exit 0
EOF
chmod +x "$WORK/plugin/bin/legion"

export PATH="$WORK/plugin/bin:$PATH"
export CLAUDE_PLUGIN_ROOT="$WORK/plugin"
export LEGION_BIN="$WORK/plugin/bin/legion"

HOOK="$WORK/plugin/hooks/pre-whoami-rewrite.sh"

run_hook() {
  local cmd="$1"
  printf '%s' "{\"tool_input\":{\"command\":${cmd}}}" | bash "$HOOK"
}

assert_blocked() {
  local desc="$1" cmd="$2"
  local out
  out=$(LEGION_TEST_HAS_IDENTITY=1 run_hook "$cmd")
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
  local desc="$1" cmd="$2" has_id="${3:-1}"
  local out
  out=$(LEGION_TEST_HAS_IDENTITY=$has_id run_hook "$cmd")
  if echo "$out" | grep -q '"decision":[[:space:]]*"block"'; then
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc (expected allow, got block)" >&2
    echo "    cmd: $cmd" >&2
  else
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  fi
}

echo "==> blocks whoami rewrite when identity already exists"
assert_blocked "bare --whoami rewrite" \
  '"legion reflect --whoami --repo test --text \"new id\""'
assert_blocked "absolute-path legion" \
  '"/opt/homebrew/bin/legion reflect --whoami --repo test --text \"new id\""'
assert_blocked "--domain identity equivalent" \
  '"legion reflect --domain identity --repo test --text \"new id\""'

echo "==> allows when --force is set"
assert_allowed "--force bypass" \
  '"legion reflect --whoami --force --repo test --text \"new id\""'

echo "==> allows when --follows is set"
assert_allowed "--follows bypass" \
  '"legion reflect --whoami --follows 019abc --repo test --text \"new beat\""'

echo "==> allows first-time identity (no existing whoami)"
assert_allowed "first-time setup" \
  '"legion reflect --whoami --repo test --text \"initial id\""' \
  "0"

echo "==> ignores non-reflect legion commands"
assert_allowed "recall is not reflect" \
  '"legion recall --repo test --context foo"'
assert_allowed "kanban is not reflect" \
  '"legion kanban list --repo test"'

echo "==> ignores reflect without identity domain"
assert_allowed "regular reflect with no domain" \
  '"legion reflect --repo test --text \"a project lesson\""'
assert_allowed "reflect with non-identity domain" \
  '"legion reflect --domain checkpoint --repo test --text \"x\""'

echo "==> ignores commands not starting with legion"
assert_allowed "git command" \
  '"git status"'
assert_allowed "echo mentions legion reflect --whoami" \
  '"echo legion reflect --whoami --repo test"'

echo
echo "PASS: $PASS"
echo "FAIL: $FAIL"
[ "$FAIL" -eq 0 ] || exit 1
