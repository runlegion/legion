#!/bin/bash
# Test runner for pre-bash-grep.sh and the _legion-prequery.sh helpers.
#
# Builds a fake CLAUDE_PLUGIN_ROOT with a stub `legion` binary so the
# tests don't depend on a real legion build, real watch.toml, or real
# SCIP indexes. Each test feeds synthetic hook JSON over stdin and
# asserts on stdout shape.
#
# Run from the repo root:
#   bash plugin/hooks/test-pre-bash-grep.sh
#
# Exits 0 on success, 1 on any failed assertion.

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

assert_eq() {
  local desc="$1" actual="$2" expected="$3"
  if [ "$actual" = "$expected" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc (expected '$expected', got '$actual')" >&2
  fi
}

# Set up a fake plugin root with stub binaries.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/plugin/bin" "$WORK/plugin/hooks" "$WORK/cache" "$WORK/state"
cp plugin/hooks/_legion-covered.sh "$WORK/plugin/hooks/"
cp plugin/hooks/_legion-indexed.sh "$WORK/plugin/hooks/"
cp plugin/hooks/_legion-prequery.sh "$WORK/plugin/hooks/"
cp plugin/hooks/pre-bash-grep.sh "$WORK/plugin/hooks/"

# Stub legion binary: dispatches on argv[1].
cat > "$WORK/plugin/bin/legion" <<'EOF'
#!/bin/bash
case "$1" in
  watch)
    [ "$2" = "list" ] && echo "legion /tmp/legion"
    ;;
  stats)
    echo "legion: 5 reflections"
    ;;
  index)
    if [ "$2" = "--status" ] && [ "$3" = "--json" ]; then
      echo '[{"repo":"legion","lang":"rust","size_bytes":100,"updated_at":"2026-01-01T00:00:00Z"}]'
    fi
    ;;
  sym)
    if [ "$2" = "def" ] && [ "$3" = "--json" ]; then
      case "$4" in
        Symbol*|fn_main|main)
          # Local-repo hit -- relevance gate (#458) keeps it.
          echo '[{"file":"src/main.rs","line":42,"symbol":"main","repo":"legion","lang":"rust"}]'
          ;;
        commonword|dictword)
          # Cluster-wide hit in an unrelated repo -- relevance gate
          # filters this out so the block tier does NOT fire on a
          # search in legion's own files.
          echo '[{"file":"src/foo.ts","line":10,"symbol":"commonword","repo":"huttspawn","lang":"typescript"}]'
          ;;
        *)
          echo '[]'
          ;;
      esac
    fi
    ;;
  telemetry)
    # Append the args to a marker file so tests can verify.
    shift
    echo "$@" >> "${LEGION_TEST_MARKER:-/dev/null}"
    ;;
esac
EOF
chmod +x "$WORK/plugin/bin/legion"

export CLAUDE_PLUGIN_ROOT="$WORK/plugin"
export XDG_CACHE_HOME="$WORK/cache"

HOOK="$WORK/plugin/hooks/pre-bash-grep.sh"

# ---------- _legion-prequery.sh helper unit tests ----------

# shellcheck source=plugin/hooks/_legion-prequery.sh
source "$WORK/plugin/hooks/_legion-prequery.sh"

echo "==> bash-binary detection"
assert_eq "leading grep -> grep"  "$(legion_prequery_bash_binary 'grep -r foo .')"   "grep"
assert_eq "leading rg -> rg"      "$(legion_prequery_bash_binary 'rg --no-heading x')" "rg"
assert_eq "leading find -> find"  "$(legion_prequery_bash_binary 'find . -name foo')" "find"
assert_eq "leading fd -> fd"      "$(legion_prequery_bash_binary 'fd Symbol src/')"   "fd"
assert_eq "leading ls -> empty"   "$(legion_prequery_bash_binary 'ls -la')"           ""
assert_eq "echo -> empty"         "$(legion_prequery_bash_binary 'echo grep is not running')" ""

echo "==> pattern extraction"
assert_eq "grep -r PAT ."            "$(legion_prequery_extract_pattern 'grep -r Symbol .' grep)" "Symbol"
assert_eq "grep --no-heading PAT ."  "$(legion_prequery_extract_pattern 'grep --no-heading SymbolName src/' grep)" "SymbolName"
assert_eq "grep -e PAT ."            "$(legion_prequery_extract_pattern 'grep -e MyFunc src/' grep)" "MyFunc"
assert_eq "grep --regexp=PAT ."      "$(legion_prequery_extract_pattern 'grep --regexp=Foo src/' grep)" "Foo"
assert_eq "rg PAT"                   "$(legion_prequery_extract_pattern 'rg fn_main src/' rg)" "fn_main"
assert_eq "find . -name PAT"         "$(legion_prequery_extract_pattern 'find . -name Symbol' find)" "Symbol"
assert_eq "find skips path arg"      "$(legion_prequery_extract_pattern 'find ./src -name Foo' find)" "Foo"

echo "==> symbol-shape detection"
legion_prequery_is_symbol "Symbol" && actual=0 || actual=1
assert_eq "CamelCase symbol"  "$actual" "0"
legion_prequery_is_symbol "fn_main" && actual=0 || actual=1
assert_eq "snake_case symbol" "$actual" "0"
legion_prequery_is_symbol "ab" && actual=0 || actual=1
assert_eq "too-short rejected" "$actual" "1"
legion_prequery_is_symbol "foo|bar" && actual=0 || actual=1
assert_eq "regex alternation rejected" "$actual" "1"
legion_prequery_is_symbol "two words" && actual=0 || actual=1
assert_eq "whitespace rejected" "$actual" "1"
legion_prequery_is_symbol "^anchored" && actual=0 || actual=1
assert_eq "anchored rejected" "$actual" "1"

echo "==> bypass reason detection"
LEGION_BYPASS_GREP=1 reason=$(legion_prequery_bypass_reason 'grep foo .')
assert_eq "env var triggers bypass" "$reason" "env:LEGION_BYPASS_GREP=1"
unset LEGION_BYPASS_GREP

reason=$(legion_prequery_bypass_reason 'grep foo . # legion-bypass: searching literal string')
assert_eq "comment sentinel triggers bypass" "$reason" "searching literal string"

reason=$(legion_prequery_bypass_reason 'grep foo .')
assert_empty "no bypass reason on plain command" "$reason"

# ---------- pre-bash-grep.sh end-to-end tests ----------

echo "==> hook end-to-end: non-search command passes through"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"ls -la"},"session_id":"t"}' | bash "$HOOK")
assert_empty "ls passes through silently" "$out"

echo "==> hook end-to-end: non-symbol pattern passes through"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r foo|bar src/"},"session_id":"t"}' | bash "$HOOK")
assert_empty "regex pattern -> pass through" "$out"

echo "==> hook end-to-end: indexed repo + symbol-shape pattern with LOCAL-REPO hit -> BLOCK"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r Symbol src/"},"session_id":"block-t"}' | bash "$HOOK")
assert_contains "block decision present" "$out" '"decision": "block"'
assert_contains "block reason mentions sym def" "$out" 'legion sym def'
assert_contains "block reason names the pattern" "$out" 'Symbol'
assert_contains "block reason offers env bypass" "$out" 'LEGION_BYPASS_GREP=1'
assert_contains "block reason offers sentinel bypass" "$out" '# legion-bypass:'

echo "==> #458 relevance gate: cluster-wide hit but NOT in this repo -> pass through (no block)"
# Stub legion returns commonword hits in huttspawn, but the grep target is in /tmp/legion.
# Pre-#458 behavior: would block on those cross-repo hits.
# Post-#458 behavior: relevance gate filters to local hits (empty) and falls through.
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r commonword src/"},"session_id":"relevance-t"}' | bash "$HOOK")
if echo "$out" | grep -q '"decision": "block"'; then
  FAIL=$((FAIL + 1)); echo "  FAIL: cross-repo-only hits should NOT trigger block in target repo"
  echo "    output: $out" >&2
else
  PASS=$((PASS + 1)); echo "  PASS: cross-repo-only hits do not trigger block"
fi

echo "==> hook end-to-end: bypass via env -> allow + telemetry"
export LEGION_TEST_MARKER="$WORK/state/bypass-marker.log"
out=$(LEGION_BYPASS_GREP=1 echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r Symbol src/"},"session_id":"bypass-env-t"}' | LEGION_BYPASS_GREP=1 bash "$HOOK")
assert_empty "env bypass exits 0 with no decision JSON" "$out"
if [ -f "$LEGION_TEST_MARKER" ] && grep -q "record-bypass" "$LEGION_TEST_MARKER"; then
  PASS=$((PASS + 1)); echo "  PASS: env bypass writes telemetry row"
else
  FAIL=$((FAIL + 1)); echo "  FAIL: env bypass did not write telemetry row" >&2
fi
unset LEGION_BYPASS_GREP

echo "==> hook end-to-end: bypass via comment sentinel -> allow + telemetry"
rm -f "$LEGION_TEST_MARKER"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r Symbol src/ # legion-bypass: testing"},"session_id":"bypass-sentinel-t"}' | bash "$HOOK")
assert_empty "sentinel bypass exits 0 with no decision JSON" "$out"
if [ -f "$LEGION_TEST_MARKER" ] && grep -q "record-bypass" "$LEGION_TEST_MARKER"; then
  PASS=$((PASS + 1)); echo "  PASS: sentinel bypass writes telemetry row"
  if grep -q "testing" "$LEGION_TEST_MARKER"; then
    PASS=$((PASS + 1)); echo "  PASS: bypass reason captured"
  else
    FAIL=$((FAIL + 1)); echo "  FAIL: bypass reason not captured in telemetry args" >&2
  fi
else
  FAIL=$((FAIL + 1)); echo "  FAIL: sentinel bypass did not write telemetry row" >&2
fi

echo "==> hook end-to-end: skip via LEGION_SKIP_PRE_BASH_GREP=1"
out=$(LEGION_SKIP_PRE_BASH_GREP=1 echo '{"cwd":"/tmp/legion","tool_name":"Bash","tool_input":{"command":"grep -r Symbol src/"},"session_id":"skip-t"}' | LEGION_SKIP_PRE_BASH_GREP=1 bash "$HOOK")
assert_empty "skip env exits 0" "$out"
unset LEGION_SKIP_PRE_BASH_GREP

echo "==> hook end-to-end: non-Bash tool passes through"
out=$(echo '{"cwd":"/tmp/legion","tool_name":"Read","tool_input":{"command":"grep -r Symbol src/"},"session_id":"t"}' | bash "$HOOK")
assert_empty "non-Bash tool ignored" "$out"

echo
echo "==> $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
