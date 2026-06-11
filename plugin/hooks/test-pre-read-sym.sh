#!/bin/bash
# Test runner for pre-read-sym.sh.
#
# Builds a fake CLAUDE_PLUGIN_ROOT with a stub `legion` binary so the
# tests don't depend on a real legion build, real watch.toml, or real
# SCIP indexes. Each test feeds synthetic hook JSON over stdin and
# asserts on stdout shape.
#
# Run from the repo root:
#   bash plugin/hooks/test-pre-read-sym.sh

set -u

PASS=0
FAIL=0

assert_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if echo "$haystack" | grep -q -- "$needle"; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected to find: $needle" >&2
    echo "    in: $haystack" >&2
  fi
}

assert_empty() {
  local desc="$1" actual="$2"
  if [ -z "$actual" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc" >&2
    echo "    expected empty, got: $actual" >&2
  fi
}

# Set up a fake plugin root with stub binary + a fake repo with files.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/plugin/bin" "$WORK/plugin/hooks" "$WORK/cache" "$WORK/state" "$WORK/repo/src"
cp plugin/hooks/_legion-covered.sh "$WORK/plugin/hooks/"
cp plugin/hooks/_legion-indexed.sh "$WORK/plugin/hooks/"
cp plugin/hooks/_legion-prequery.sh "$WORK/plugin/hooks/"
cp plugin/hooks/pre-read-sym.sh "$WORK/plugin/hooks/"
mkdir -p "$WORK/plugin/hooks/lib"
cp plugin/hooks/lib/prelude.sh plugin/hooks/lib/emit.sh "$WORK/plugin/hooks/lib/"

# Fake source files: one large (>500 lines), one small.
yes 'fn dummy_line() {}' | head -800 > "$WORK/repo/src/big.rs"
yes 'fn dummy_line() {}' | head -100 > "$WORK/repo/src/small.rs"
echo "# README content" > "$WORK/repo/README.md"

# Stub legion binary.
cat > "$WORK/plugin/bin/legion" <<'EOF'
#!/bin/bash
case "$1" in
  watch) [ "$2" = "list" ] && echo "repo /tmp/repo" ;;
  stats) echo "repo: 5 reflections" ;;
  index)
    if [ "$2" = "--status" ] && [ "$3" = "--json" ]; then
      echo '[{"repo":"repo","lang":"rust","size_bytes":100,"updated_at":"2026-01-01T00:00:00Z"}]'
    fi
    ;;
  telemetry)
    shift
    echo "$@" >> "${LEGION_TEST_MARKER:-/dev/null}"
    ;;
esac
EOF
chmod +x "$WORK/plugin/bin/legion"

export CLAUDE_PLUGIN_ROOT="$WORK/plugin"
export XDG_CACHE_HOME="$WORK/cache"

HOOK="$WORK/plugin/hooks/pre-read-sym.sh"

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
assert_contains "block decision present" "$out" '"permissionDecision": "deny"'
assert_contains "block reason names line count" "$out" '800'
assert_contains "block reason mentions sym hover" "$out" 'legion sym hover'
assert_contains "block reason mentions limit alternative" "$out" 'limit=200'
assert_contains "block reason offers env bypass" "$out" 'LEGION_BYPASS_READ=1'

echo "==> large file with limit=600 (above threshold) -> BLOCK"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/big.rs\",\"limit\":600},\"session_id\":\"t-block-large-limit\"}" | bash "$HOOK")
assert_contains "limit > threshold still blocks" "$out" '"permissionDecision": "deny"'

echo "==> bypass via env -> allow + telemetry"
export LEGION_TEST_MARKER="$WORK/state/bypass-marker.log"
rm -f "$LEGION_TEST_MARKER"
out=$(LEGION_BYPASS_READ=1 echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/big.rs\"},\"session_id\":\"t-bypass\"}" | LEGION_BYPASS_READ=1 bash "$HOOK")
assert_empty "env bypass exits 0 with no decision JSON" "$out"
if [ -f "$LEGION_TEST_MARKER" ] && grep -q "record-bypass" "$LEGION_TEST_MARKER"; then
  PASS=$((PASS + 1)); echo "  PASS: env bypass writes telemetry row"
  if grep -q "Read" "$LEGION_TEST_MARKER"; then
    PASS=$((PASS + 1)); echo "  PASS: tool=Read captured"
  else
    FAIL=$((FAIL + 1)); echo "  FAIL: tool=Read not in telemetry args" >&2
  fi
else
  FAIL=$((FAIL + 1)); echo "  FAIL: env bypass did not write telemetry row" >&2
fi
unset LEGION_BYPASS_READ

echo "==> skip via LEGION_SKIP_PRE_READ_SYM=1"
out=$(LEGION_SKIP_PRE_READ_SYM=1 echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$WORK/repo/src/big.rs\"},\"session_id\":\"t-skip\"}" | LEGION_SKIP_PRE_READ_SYM=1 bash "$HOOK")
assert_empty "skip env exits 0" "$out"

echo "==> nonexistent file passes through"
out=$(echo "{\"cwd\":\"$WORK/repo\",\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"/does/not/exist.rs\"},\"session_id\":\"t-missing\"}" | bash "$HOOK")
assert_empty "missing file passes through" "$out"

echo
echo "==> $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
