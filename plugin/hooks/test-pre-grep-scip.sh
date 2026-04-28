#!/bin/bash
# Test runner for pre-grep-scip.sh.
#
# Stubs CLAUDE_PLUGIN_ROOT/bin/legion with a script whose `sym def` /
# `sym refs` output is configurable per test, then drives the hook
# with synthetic Grep/Glob hook JSON and asserts the expected
# additionalContext (or absence) is emitted on stdout.
#
# Run from the repo root:
#   bash plugin/hooks/test-pre-grep-scip.sh

set -u

HOOK="$(pwd)/plugin/hooks/pre-grep-scip.sh"
if [ ! -f "$HOOK" ]; then
  echo "FAIL: hook not found at $HOOK" >&2
  exit 1
fi

PASS=0
FAIL=0
SANDBOX=$(mktemp -d)
trap 'rm -rf "$SANDBOX"' EXIT
mkdir -p "$SANDBOX/plugin/bin" "$SANDBOX/plugin/hooks"
cp "$HOOK" "$SANDBOX/plugin/hooks/"

cat >"$SANDBOX/plugin/bin/legion" <<EOF
#!/bin/bash
# Stub legion. Tests set FAKE_DEF_OUT / FAKE_REFS_OUT in env to control
# the response to \`sym def\` / \`sym refs\`. Watch and stats commands
# emit fixtures so the coverage gate sees this repo as covered.
case "\$1 \$2" in
  "sym def")
    if [ -n "\${FAKE_DEF_OUT:-}" ]; then printf '%s\n' "\$FAKE_DEF_OUT"; fi
    ;;
  "sym refs")
    if [ -n "\${FAKE_REFS_OUT:-}" ]; then printf '%s\n' "\$FAKE_REFS_OUT"; fi
    ;;
esac
case "\$1" in
  watch) printf 'testrepo\t/tmp\n' ;;
  stats) printf 'testrepo: 1 reflections\n' ;;
esac
exit 0
EOF
chmod +x "$SANDBOX/plugin/bin/legion"

run_hook() {
  local pattern="$1"
  local tool="${2:-Grep}"
  local input
  input=$(jq -n \
    --arg p "$pattern" --arg t "$tool" --arg cwd "$SANDBOX" --arg sid "s" \
    '{tool_input:{pattern:$p}, tool_name:$t, cwd:$cwd, session_id:$sid}')
  CLAUDE_PLUGIN_ROOT="$SANDBOX/plugin" \
    XDG_CACHE_HOME="$SANDBOX/cache" \
    HOME="$SANDBOX/home" \
    LEGION_REPO="testrepo" \
    FAKE_DEF_OUT="${FAKE_DEF_OUT:-}" \
    FAKE_REFS_OUT="${FAKE_REFS_OUT:-}" \
    LEGION_SKIP_PRE_SCIP="${LEGION_SKIP_PRE_SCIP:-}" \
    bash "$SANDBOX/plugin/hooks/pre-grep-scip.sh" <<<"$input"
}

assert_empty_stdout() {
  local desc="$1"
  if [ -z "$2" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc -- got: $2" >&2
  fi
}

assert_contains() {
  local desc="$1"
  local needle="$2"
  local haystack="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc -- needle '$needle' missing" >&2
    echo "  haystack: $haystack" >&2
  fi
}

echo "Test: pattern with regex meta does not trigger SCIP"
out=$(FAKE_DEF_OUT="src/lib.rs:1:1 (testrepo/rust)" run_hook 'fn.*error')
assert_empty_stdout "regex pattern -> no injection" "$out"

echo "Test: short pattern does not trigger SCIP"
out=$(FAKE_DEF_OUT="src/lib.rs:1:1 (testrepo/rust)" run_hook 'a')
assert_empty_stdout "1-char pattern -> no injection" "$out"

echo "Test: bare symbol triggers injection when defs are non-empty"
out=$(FAKE_DEF_OUT="src/lib.rs:42:5 (testrepo/rust)" FAKE_REFS_OUT="" run_hook 'MyStruct')
assert_contains "injection emitted with file:line" "src/lib.rs:42:5" "$out"
assert_contains "additionalContext block titled" "SCIP symbol results" "$out"

echo "Test: qualified symbol with :: triggers injection"
out=$(FAKE_DEF_OUT="src/db.rs:10:1 (testrepo/rust)" FAKE_REFS_OUT="" run_hook 'foo::bar::Baz')
assert_contains "qualified path passes heuristic" "src/db.rs:10:1" "$out"

echo "Test: empty defs and refs -> no injection"
out=$(FAKE_DEF_OUT="" FAKE_REFS_OUT="" run_hook 'NoSuchSymbol')
assert_empty_stdout "no results -> no JSON" "$out"

echo "Test: LEGION_SKIP_PRE_SCIP=1 suppresses everything"
out=$(LEGION_SKIP_PRE_SCIP=1 FAKE_DEF_OUT="src/lib.rs:1:1 (testrepo/rust)" run_hook 'MyStruct')
assert_empty_stdout "skip override -> no injection" "$out"

echo "Test: Glob with wildcard does not trigger"
out=$(FAKE_DEF_OUT="x" run_hook 'src/**/auth.rs' Glob)
assert_empty_stdout "wildcard glob -> no injection" "$out"

echo
echo "Results: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
