#!/bin/bash
# Test runner for pre-read-sym.sh.
#
# Uses the shared fake plugin root + parameterized stub legion from
# tests/testutil.sh, plus a fake repo with one large and one small source
# file. Each test feeds synthetic hook JSON over stdin and asserts on
# stdout shape. Run from anywhere:
#
#   bash plugin/hooks/test-pre-read-sym.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

make_plugin_root pre-read-sym.sh

# Fixtures: repo "repo" is covered and indexed.
export FAKE_WATCH="repo	/tmp/repo"
export FAKE_STATS="repo:5"
export FAKE_INDEX_JSON='[{"repo":"repo","lang":"rust","size_bytes":100,"updated_at":"2026-01-01T00:00:00Z"}]'

# Fake source files: one large (>500 lines), one small.
mkdir -p "$WORK/repo/src"
yes 'fn dummy_line() {}' | head -800 > "$WORK/repo/src/big.rs"
yes 'fn dummy_line() {}' | head -100 > "$WORK/repo/src/small.rs"
echo "# README content" > "$WORK/repo/README.md"

HOOK="$CLAUDE_PLUGIN_ROOT/hooks/pre-read-sym.sh"

echo "==> non-Read tool passes through"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Bash\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/big.rs\"},\"session_id\":\"t\"}" | bash "$HOOK")
assert_empty "Bash tool ignored" "$out"

echo "==> non-source extension passes through"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/README.md\"},\"session_id\":\"t-md\"}" | bash "$HOOK")
assert_empty "README.md ignored" "$out"

echo "==> small file (under threshold) passes through"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/small.rs\"},\"session_id\":\"t-small\"}" | bash "$HOOK")
assert_empty "small.rs (100 lines) passes through" "$out"

echo "==> large file with explicit small limit passes through"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/big.rs\",\"limit\":150},\"session_id\":\"t-limit\"}" | bash "$HOOK")
assert_empty "limit=150 always allowed" "$out"

echo "==> large file with limit=200 (boundary) passes through"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/big.rs\",\"limit\":200},\"session_id\":\"t-limit2\"}" | bash "$HOOK")
assert_empty "limit=200 boundary allowed" "$out"

echo "==> large file with no limit -> BLOCK"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/big.rs\"},\"session_id\":\"t-block\"}" | bash "$HOOK")
assert_contains "deny decision present" "$out" '"permissionDecision": "deny"'
assert_contains "deny reason names line count" "$out" '800'
assert_contains "deny reason mentions sym hover" "$out" 'legion sym hover'
assert_contains "deny reason mentions limit alternative" "$out" 'limit=200'
assert_contains "deny reason offers env bypass" "$out" 'LEGION_BYPASS_READ=1'

echo "==> large file with limit=600 (above threshold) -> BLOCK"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/big.rs\",\"limit\":600},\"session_id\":\"t-block-large-limit\"}" | bash "$HOOK")
assert_contains "limit > threshold still blocks" "$out" '"permissionDecision": "deny"'

echo "==> bypass via env -> allow + telemetry"
export LEGION_TEST_MARKER="$WORK/state/bypass-marker.log"
rm -f "$LEGION_TEST_MARKER"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/big.rs\"},\"session_id\":\"t-bypass\"}" | LEGION_BYPASS_READ=1 bash "$HOOK")
assert_empty "env bypass exits 0 with no decision JSON" "$out"
assert_file_contains "env bypass writes telemetry row" "$LEGION_TEST_MARKER" "record-bypass"
assert_file_contains "tool=Read captured" "$LEGION_TEST_MARKER" "Read"

echo "==> skip via LEGION_SKIP_PRE_READ_SYM=1"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/big.rs\"},\"session_id\":\"t-skip\"}" | LEGION_SKIP_PRE_READ_SYM=1 bash "$HOOK")
assert_empty "skip env exits 0" "$out"

echo "==> nonexistent file passes through"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"/does/not/exist.rs\"},\"session_id\":\"t-missing\"}" | bash "$HOOK")
assert_empty "missing file passes through" "$out"

echo "==> LEGION_REPO precedence: env overrides basename(cwd)"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/big.rs\"},\"session_id\":\"t-repo-env\"}" | LEGION_REPO=uncovered-elsewhere bash "$HOOK")
assert_empty "LEGION_REPO redirects the coverage gate" "$out"

finish_tests
