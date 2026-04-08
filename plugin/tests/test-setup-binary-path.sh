#!/bin/bash
# Tests for setup-binary.sh path persistence to .legion-binary-path.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"
SETUP_SCRIPT="${PLUGIN_ROOT}/hooks/setup-binary.sh"
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

assert_file_exists() {
  local name="$1" path="$2"
  if [ -f "$path" ]; then
    echo "  PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $name (file not found: $path)"
    FAIL=$((FAIL + 1))
  fi
}

assert_file_not_exists() {
  local name="$1" path="$2"
  if [ ! -f "$path" ]; then
    echo "  PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $name (file exists but should not: $path)"
    FAIL=$((FAIL + 1))
  fi
}

# -- Setup: isolated temp directory -------------------------------------------

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

# -- Test 1: path file is written when binary exists --------------------------

echo "Test 1: setup-binary.sh writes .legion-binary-path when binary is present"

# Create a fake plugin layout with an already-installed binary
FAKE_ROOT="${TMPDIR}/test1-plugin"
mkdir -p "${FAKE_ROOT}/.claude-plugin" "${FAKE_ROOT}/hooks" "${FAKE_ROOT}/channel"
mkdir -p "${TMPDIR}/test1-data"

# Fake plugin.json with a version that matches our fake binary
cat > "${FAKE_ROOT}/.claude-plugin/plugin.json" << 'JSON'
{ "name": "legion", "version": "99.0.0" }
JSON

# Fake binary that reports version 99.0.0
FAKE_BIN="${TMPDIR}/test1-data/legion"
cat > "$FAKE_BIN" << 'SCRIPT'
#!/bin/bash
echo "legion 99.0.0"
SCRIPT
chmod +x "$FAKE_BIN"

# Copy setup-binary.sh into the fake plugin
cp "$SETUP_SCRIPT" "${FAKE_ROOT}/hooks/setup-binary.sh"

# Run setup -- binary already matches version so no download needed
CLAUDE_PLUGIN_ROOT="$FAKE_ROOT" \
CLAUDE_PLUGIN_DATA="${TMPDIR}/test1-data" \
bash "${FAKE_ROOT}/hooks/setup-binary.sh" 2>/dev/null

PATH_FILE="${FAKE_ROOT}/.legion-binary-path"
assert_file_exists ".legion-binary-path created" "$PATH_FILE"
assert_eq "path file contains binary path" "${TMPDIR}/test1-data/legion" "$(cat "$PATH_FILE")"

# -- Test 2: path file not written when binary is missing ---------------------

echo "Test 2: setup-binary.sh does not write .legion-binary-path when binary is missing"

FAKE_ROOT2="${TMPDIR}/test2-plugin"
mkdir -p "${FAKE_ROOT2}/.claude-plugin" "${FAKE_ROOT2}/hooks"
mkdir -p "${TMPDIR}/test2-data"

cat > "${FAKE_ROOT2}/.claude-plugin/plugin.json" << 'JSON'
{ "name": "legion", "version": "99.0.0" }
JSON

cp "$SETUP_SCRIPT" "${FAKE_ROOT2}/hooks/setup-binary.sh"

# Run setup with no binary present (download will fail since v99.0.0 doesn't exist)
CLAUDE_PLUGIN_ROOT="$FAKE_ROOT2" \
CLAUDE_PLUGIN_DATA="${TMPDIR}/test2-data" \
bash "${FAKE_ROOT2}/hooks/setup-binary.sh" 2>/dev/null || true

assert_file_not_exists ".legion-binary-path not created when no binary" "${FAKE_ROOT2}/.legion-binary-path"

# -- Summary ------------------------------------------------------------------

echo ""
echo "Results: ${PASS} passed, ${FAIL} failed"
[ "$FAIL" -eq 0 ] || exit 1
