#!/bin/bash
# Test runner for _legion-indexed.sh.
#
# Sources the helper and exercises the cache-hit, cache-miss, indexed,
# and not-indexed branches against the shared stub legion (FAKE_INDEX_JSON
# fixture). Run from anywhere:
#
#   bash plugin/hooks/test-legion-indexed.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"

# No hooks to copy -- the helpers under test ship in every plugin root.
# shellcheck disable=SC2119
make_plugin_root

export FAKE_INDEX_JSON='[{"repo":"legion","lang":"rust","size_bytes":100,"updated_at":"2026-01-01T00:00:00Z"},{"repo":"smugglr","lang":"rust","size_bytes":200,"updated_at":"2026-01-01T00:00:00Z"}]'

# shellcheck source=_legion-indexed.sh
source "$CLAUDE_PLUGIN_ROOT/hooks/_legion-indexed.sh"

echo "==> indexed repos return 0"
legion_indexed "sess-1" "legion" && actual="0" || actual="1"
assert_eq "legion is indexed" "$actual" "0"

legion_indexed "sess-1" "smugglr" && actual="0" || actual="1"
assert_eq "smugglr is indexed" "$actual" "0"

echo "==> non-indexed repo returns 1"
legion_indexed "sess-1" "platform" && actual="0" || actual="1"
assert_eq "platform is not indexed" "$actual" "1"

echo "==> cache short-circuits second call"
# Break the stub binary. If the cache works, the second probe for 'legion'
# must still succeed because it reads the cached "1" without invoking it.
export FAKE_BROKEN=1
legion_indexed "sess-1" "legion" && actual="0" || actual="1"
assert_eq "cached legion still indexed" "$actual" "0"
legion_indexed "sess-1" "platform" && actual="0" || actual="1"
assert_eq "cached platform still not indexed" "$actual" "1"
unset FAKE_BROKEN

echo "==> empty repo arg returns 1"
legion_indexed "sess-2" "" && actual="0" || actual="1"
assert_eq "empty repo treated as not indexed" "$actual" "1"

echo "==> missing legion binary -> not indexed (no cache poison)"
rm -f "$CLAUDE_PLUGIN_ROOT/bin/legion"
legion_indexed "sess-3" "rafters" && actual="0" || actual="1"
assert_eq "missing binary -> not indexed (block tier disabled)" "$actual" "1"
# The missing-binary path must NOT cache the verdict; otherwise a transient
# missing state becomes sticky for the rest of the session.
assert_eq "missing-binary path leaves no cache file" \
  "$([ -f "$XDG_CACHE_HOME/legion/indexed-sess-3-rafters" ] && echo yes || echo no)" \
  "no"

echo "==> binary appears mid-session -> probe re-runs and succeeds"
make_stub_legion "$CLAUDE_PLUGIN_ROOT/bin/legion"
export FAKE_INDEX_JSON='[{"repo":"rafters","lang":"typescript","size_bytes":300,"updated_at":"2026-01-01T00:00:00Z"}]'
legion_indexed "sess-3" "rafters" && actual="0" || actual="1"
assert_eq "binary returned -> rafters now indexed" "$actual" "0"

finish_tests
