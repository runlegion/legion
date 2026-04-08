#!/bin/bash
# Tests for .mcp.json placement and structure.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_ROOT="$(dirname "$SCRIPT_DIR")"
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

# -- Test 1: .mcp.json at plugin root, not inside .claude-plugin/ ------------

echo "Test 1: .mcp.json location"
assert_file_exists ".mcp.json exists at plugin root" "${PLUGIN_ROOT}/.mcp.json"
assert_file_not_exists ".mcp.json not inside .claude-plugin" "${PLUGIN_ROOT}/.claude-plugin/.mcp.json"

# -- Test 2: .mcp.json is valid JSON -----------------------------------------

echo "Test 2: .mcp.json is valid JSON"
if python3 -c "import json; json.load(open('${PLUGIN_ROOT}/.mcp.json'))" 2>/dev/null; then
  echo "  PASS: valid JSON"
  PASS=$((PASS + 1))
else
  echo "  FAIL: invalid JSON"
  FAIL=$((FAIL + 1))
fi

# -- Test 3: .mcp.json uses flat format (no mcpServers wrapper) --------------

echo "Test 3: flat format (server name as top-level key)"
HAS_LEGION=$(python3 -c "
import json
with open('${PLUGIN_ROOT}/.mcp.json') as f:
    d = json.load(f)
print('yes' if 'legion' in d else 'no')
")
assert_eq "has 'legion' as top-level key" "yes" "$HAS_LEGION"

HAS_WRAPPER=$(python3 -c "
import json
with open('${PLUGIN_ROOT}/.mcp.json') as f:
    d = json.load(f)
print('yes' if 'mcpServers' in d else 'no')
")
assert_eq "no mcpServers wrapper" "no" "$HAS_WRAPPER"

# -- Test 4: .mcp.json command points to wrapper script -----------------------

echo "Test 4: MCP command uses wrapper script"
COMMAND=$(python3 -c "
import json
with open('${PLUGIN_ROOT}/.mcp.json') as f:
    d = json.load(f)
print(d['legion']['args'][0])
")
# shellcheck disable=SC2016
assert_eq "args reference bin/legion-channel" '${CLAUDE_PLUGIN_ROOT}/bin/legion-channel' "$COMMAND"

# -- Test 5: wrapper script exists and is executable --------------------------

echo "Test 5: legion-channel wrapper exists and is executable"
assert_file_exists "bin/legion-channel exists" "${PLUGIN_ROOT}/bin/legion-channel"
if [ -x "${PLUGIN_ROOT}/bin/legion-channel" ]; then
  echo "  PASS: bin/legion-channel is executable"
  PASS=$((PASS + 1))
else
  echo "  FAIL: bin/legion-channel is not executable"
  FAIL=$((FAIL + 1))
fi

# -- Summary ------------------------------------------------------------------

echo ""
echo "Results: ${PASS} passed, ${FAIL} failed"
[ "$FAIL" -eq 0 ] || exit 1
