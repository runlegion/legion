#!/bin/bash
# Test runner for _legion-covered.sh.
#
# Stubs the legion binary with a fixture that returns deterministic
# `watch list` and `stats --repo` output, then exercises the helper
# across the three coverage cases: in watch.toml, has reflections,
# neither.
#
# Run from the repo root:
#   bash plugin/hooks/test-legion-covered.sh
#
# Exits 0 on success, 1 on any failed assertion.

set -u

HELPER="${CLAUDE_PLUGIN_ROOT:-$(pwd)/plugin}/hooks/_legion-covered.sh"
if [ ! -f "$HELPER" ]; then
  echo "FAIL: helper not found at $HELPER" >&2
  exit 1
fi

PASS=0
FAIL=0

# Build a fake legion binary in a temp dir. The fixture answers two
# subcommands: `watch list` and `stats --repo NAME`. Subcommand and repo
# fixtures are fed via FAKE_WATCH and FAKE_STATS env vars at call time.
FAKE_DIR=$(mktemp -d)
trap 'rm -rf "$FAKE_DIR"' EXIT

cat >"$FAKE_DIR/legion" <<'EOF'
#!/bin/bash
case "$1" in
  watch)
    if [ "$2" = "list" ]; then
      printf '%s\n' "${FAKE_WATCH:-}"
    fi
    ;;
  stats)
    # Args: stats --repo NAME
    repo="$3"
    case "${FAKE_STATS:-}" in
      "$repo:"*)
        n="${FAKE_STATS#*:}"
        printf '%s: %s reflections (2026-01-01 to 2026-01-01)\n' "$repo" "$n"
        ;;
      *)
        printf 'no reflections stored yet\n'
        ;;
    esac
    ;;
esac
EOF
chmod +x "$FAKE_DIR/legion"

mkdir -p "$FAKE_DIR/plugin/bin" "$FAKE_DIR/plugin/hooks"
cp "$FAKE_DIR/legion" "$FAKE_DIR/plugin/bin/legion"
cp "$HELPER" "$FAKE_DIR/plugin/hooks/_legion-covered.sh"

# Each test uses a fresh XDG_CACHE_HOME so cache files don't leak between cases.
fresh_cache() {
  local d
  d=$(mktemp -d)
  echo "$d"
}

run_helper() {
  local session="$1"
  local repo="$2"
  local watch_fixture="$3"
  local stats_fixture="$4"
  local cache="$5"
  CLAUDE_PLUGIN_ROOT="$FAKE_DIR/plugin" \
  XDG_CACHE_HOME="$cache" \
  FAKE_WATCH="$watch_fixture" \
  FAKE_STATS="$stats_fixture" \
  bash -c "source '$FAKE_DIR/plugin/hooks/_legion-covered.sh'; legion_covered '$session' '$repo'"
}

assert() {
  local desc="$1"
  local expected_rc="$2"
  local actual_rc="$3"
  if [ "$actual_rc" -eq "$expected_rc" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc (expected rc=$expected_rc, got rc=$actual_rc)" >&2
  fi
}

echo "Test: in watch.toml -> covered"
CACHE=$(fresh_cache)
run_helper "sess1" "myrepo" "myrepo	/path/to/myrepo" "" "$CACHE"
assert "watch list contains repo" 0 $?
rm -rf "$CACHE"

echo "Test: has reflections -> covered"
CACHE=$(fresh_cache)
run_helper "sess2" "myrepo" "" "myrepo:42" "$CACHE"
assert "stats reports reflections" 0 $?
rm -rf "$CACHE"

echo "Test: neither -> not covered"
CACHE=$(fresh_cache)
run_helper "sess3" "myrepo" "otherrepo	/path" "otherrepo:5" "$CACHE"
assert "no watch entry, no reflections" 1 $?
rm -rf "$CACHE"

echo "Test: cache hit returns cached value without re-probing"
CACHE=$(fresh_cache)
mkdir -p "$CACHE/legion"
printf '0' > "$CACHE/legion/covered-sess4-myrepo"
# Even with watch entry that would say "covered", cache says "not covered".
run_helper "sess4" "myrepo" "myrepo	/path" "myrepo:99" "$CACHE"
assert "cache pin overrides probe" 1 $?
rm -rf "$CACHE"

echo "Test: empty repo arg -> covered (defensive default)"
CACHE=$(fresh_cache)
run_helper "sess5" "" "" "" "$CACHE"
assert "empty repo defaults to covered" 0 $?
rm -rf "$CACHE"

echo
echo "Results: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
