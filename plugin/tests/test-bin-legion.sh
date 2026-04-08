#!/bin/bash
# Tests for plugin/bin/legion wrapper binary resolution.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"
WRAPPER="${PLUGIN_ROOT}/bin/legion"
PASS=0
FAIL=0

assert_eq() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "  PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $name"
    echo "    expected: $expected"
    echo "    actual:   $actual"
    FAIL=$((FAIL + 1))
  fi
}

assert_contains() {
  local name="$1" needle="$2" haystack="$3"
  if echo "$haystack" | grep -qF "$needle"; then
    echo "  PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $name"
    echo "    expected to contain: $needle"
    echo "    actual: $haystack"
    FAIL=$((FAIL + 1))
  fi
}

# -- Setup: isolated temp directory -------------------------------------------

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

# Create a fake binary that prints its path
FAKE_BINARY="${TMPDIR}/data/legion"
mkdir -p "${TMPDIR}/data"
cat > "$FAKE_BINARY" << 'SCRIPT'
#!/bin/bash
echo "BINARY_PATH=$0"
echo "ARGS=$*"
SCRIPT
chmod +x "$FAKE_BINARY"

# -- Test 1: CLAUDE_PLUGIN_DATA takes priority --------------------------------

echo "Test 1: CLAUDE_PLUGIN_DATA path is used in hook context"
OUTPUT=$(CLAUDE_PLUGIN_DATA="${TMPDIR}/data" CLAUDE_PLUGIN_ROOT="$PLUGIN_ROOT" bash "$WRAPPER" --version 2>/dev/null)
assert_contains "dispatches to CLAUDE_PLUGIN_DATA binary" "BINARY_PATH=${TMPDIR}/data/legion" "$OUTPUT"
assert_contains "passes arguments through" "ARGS=--version" "$OUTPUT"

# -- Test 2: .legion-binary-path fallback -------------------------------------

echo "Test 2: .legion-binary-path is used outside hook context"
# Create a temp plugin root with the path file
FAKE_PLUGIN="${TMPDIR}/fake-plugin"
mkdir -p "${FAKE_PLUGIN}/bin"
cp "$WRAPPER" "${FAKE_PLUGIN}/bin/legion"
echo "${FAKE_BINARY}" > "${FAKE_PLUGIN}/.legion-binary-path"

OUTPUT=$(unset CLAUDE_PLUGIN_DATA; CLAUDE_PLUGIN_ROOT="$FAKE_PLUGIN" bash "${FAKE_PLUGIN}/bin/legion" reflect 2>/dev/null)
assert_contains "dispatches to path file binary" "BINARY_PATH=${FAKE_BINARY}" "$OUTPUT"
assert_contains "passes arguments through" "ARGS=reflect" "$OUTPUT"

# -- Test 3: missing path file and no CLAUDE_PLUGIN_DATA = failure ------------

echo "Test 3: exits 1 when binary cannot be found"
EMPTY_PLUGIN="${TMPDIR}/empty-plugin"
mkdir -p "${EMPTY_PLUGIN}/bin"
cp "$WRAPPER" "${EMPTY_PLUGIN}/bin/legion"

OUTPUT=$(unset CLAUDE_PLUGIN_DATA; CLAUDE_PLUGIN_ROOT="$EMPTY_PLUGIN" bash "${EMPTY_PLUGIN}/bin/legion" 2>&1 || true)
assert_contains "prints not found message" "binary not found" "$OUTPUT"

# -- Test 4: stale path file (binary deleted) = failure ----------------------

echo "Test 4: exits 1 when path file points to missing binary"
STALE_PLUGIN="${TMPDIR}/stale-plugin"
mkdir -p "${STALE_PLUGIN}/bin"
cp "$WRAPPER" "${STALE_PLUGIN}/bin/legion"
echo "/nonexistent/path/legion" > "${STALE_PLUGIN}/.legion-binary-path"

OUTPUT=$(unset CLAUDE_PLUGIN_DATA; CLAUDE_PLUGIN_ROOT="$STALE_PLUGIN" bash "${STALE_PLUGIN}/bin/legion" 2>&1 || true)
assert_contains "prints not found for stale path" "binary not found" "$OUTPUT"

# -- Test 5: CLAUDE_PLUGIN_DATA wins over path file ---------------------------

echo "Test 5: CLAUDE_PLUGIN_DATA takes priority over .legion-binary-path"
OTHER_BINARY="${TMPDIR}/other/legion"
mkdir -p "${TMPDIR}/other"
cat > "$OTHER_BINARY" << 'SCRIPT'
#!/bin/bash
echo "BINARY_PATH=$0"
SCRIPT
chmod +x "$OTHER_BINARY"

echo "${OTHER_BINARY}" > "${FAKE_PLUGIN}/.legion-binary-path"
OUTPUT=$(CLAUDE_PLUGIN_DATA="${TMPDIR}/data" CLAUDE_PLUGIN_ROOT="$FAKE_PLUGIN" bash "${FAKE_PLUGIN}/bin/legion" 2>/dev/null)
assert_contains "uses CLAUDE_PLUGIN_DATA not path file" "BINARY_PATH=${TMPDIR}/data/legion" "$OUTPUT"

# -- Summary ------------------------------------------------------------------

echo ""
echo "Results: ${PASS} passed, ${FAIL} failed"
[ "$FAIL" -eq 0 ] || exit 1
