#!/bin/bash
# Test runner for _legion-covered.sh.
#
# Uses the shared stub legion (FAKE_WATCH / FAKE_STATS fixtures) to
# exercise the helper across the three coverage cases: in watch.toml,
# has reflections, neither. Run from anywhere:
#
#   bash plugin/hooks/test-legion-covered.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

# No hooks to copy -- the helpers under test ship in every plugin root.
# shellcheck disable=SC2119
make_plugin_root

# run_covered SESSION REPO WATCH_FIXTURE STATS_FIXTURE CACHE_DIR -- run
# legion_covered in a fresh shell with the given fixtures and cache dir;
# returns the helper's exit code.
run_covered() {
  local session="$1" repo="$2" watch_fixture="$3" stats_fixture="$4" cache="$5"
  XDG_CACHE_HOME="$cache" \
  FAKE_WATCH="$watch_fixture" \
  FAKE_STATS="$stats_fixture" \
  bash -c "source '$CLAUDE_PLUGIN_ROOT/hooks/_legion-covered.sh'; legion_covered '$session' '$repo'"
}

fresh_cache() {
  mktemp -d "$WORK/cache.XXXXXX"
}

echo "Test: in watch.toml -> covered"
CACHE=$(fresh_cache)
run_covered "sess1" "myrepo" "myrepo	/path/to/myrepo" "" "$CACHE"
assert_rc "watch list contains repo" 0 $?

echo "Test: has reflections -> covered"
CACHE=$(fresh_cache)
run_covered "sess2" "myrepo" "" "myrepo:42" "$CACHE"
assert_rc "stats reports reflections" 0 $?

echo "Test: neither -> not covered"
CACHE=$(fresh_cache)
run_covered "sess3" "myrepo" "otherrepo	/path" "otherrepo:5" "$CACHE"
assert_rc "no watch entry, no reflections" 1 $?

echo "Test: cache hit returns cached value without re-probing"
CACHE=$(fresh_cache)
mkdir -p "$CACHE/legion"
printf '0' > "$CACHE/legion/covered-sess4-myrepo"
# Even with watch entry that would say "covered", cache says "not covered".
run_covered "sess4" "myrepo" "myrepo	/path" "myrepo:99" "$CACHE"
assert_rc "cache pin overrides probe" 1 $?

echo "Test: empty repo arg -> covered (defensive default)"
CACHE=$(fresh_cache)
run_covered "sess5" "" "" "" "$CACHE"
assert_rc "empty repo defaults to covered" 0 $?

finish_tests
