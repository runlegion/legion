#!/bin/bash
# Test runner for _legion-indexed.sh.
#
# Sources the helper and exercises the cache-hit, cache-miss, indexed,
# and not-indexed branches against a fake `legion` binary on PATH. Does
# not depend on the real legion build being present.
#
# Run from the repo root:
#   bash plugin/hooks/test-legion-indexed.sh
#
# Exits 0 on success, 1 on any failed assertion.

set -u

PASS=0
FAIL=0

assert() {
  local desc="$1"
  local actual="$2"
  local expected="$3"
  if [ "$actual" = "$expected" ]; then
    PASS=$((PASS + 1))
    echo "  PASS: $desc"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $desc (expected '$expected', got '$actual')" >&2
  fi
}

# Build a temp environment: fake CLAUDE_PLUGIN_ROOT/bin/legion that returns
# canned JSON, isolated XDG_CACHE_HOME so cache files don't collide with
# the operator's real cache.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/plugin/bin" "$WORK/plugin/hooks" "$WORK/cache"
cp plugin/hooks/_legion-indexed.sh "$WORK/plugin/hooks/"

# Fake legion binary: prints a fixed JSON array containing 'legion' and
# 'smugglr', regardless of args. Exit 0.
cat > "$WORK/plugin/bin/legion" <<'EOF'
#!/bin/bash
echo '[{"repo":"legion","lang":"rust","size_bytes":100,"updated_at":"2026-01-01T00:00:00Z"},{"repo":"smugglr","lang":"rust","size_bytes":200,"updated_at":"2026-01-01T00:00:00Z"}]'
EOF
chmod +x "$WORK/plugin/bin/legion"

export CLAUDE_PLUGIN_ROOT="$WORK/plugin"
export XDG_CACHE_HOME="$WORK/cache"

# shellcheck source=plugin/hooks/_legion-indexed.sh
source "$WORK/plugin/hooks/_legion-indexed.sh"

echo "==> indexed repos return 0"
legion_indexed "sess-1" "legion" && actual="0" || actual="1"
assert "legion is indexed" "$actual" "0"

legion_indexed "sess-1" "smugglr" && actual="0" || actual="1"
assert "smugglr is indexed" "$actual" "0"

echo "==> non-indexed repo returns 1"
legion_indexed "sess-1" "platform" && actual="0" || actual="1"
assert "platform is not indexed" "$actual" "1"

echo "==> cache short-circuits second call"
# Replace the fake binary with one that always errors. If the cache works,
# the second probe for 'legion' must still succeed because it reads the
# cached "1" without invoking the binary.
cat > "$WORK/plugin/bin/legion" <<'EOF'
#!/bin/bash
exit 1
EOF
chmod +x "$WORK/plugin/bin/legion"

legion_indexed "sess-1" "legion" && actual="0" || actual="1"
assert "cached legion still indexed" "$actual" "0"
legion_indexed "sess-1" "platform" && actual="0" || actual="1"
assert "cached platform still not indexed" "$actual" "1"

echo "==> empty repo arg returns 1"
legion_indexed "sess-2" "" && actual="0" || actual="1"
assert "empty repo treated as not indexed" "$actual" "1"

echo "==> missing legion binary -> not indexed (no cache poison)"
rm -f "$WORK/plugin/bin/legion"
legion_indexed "sess-3" "rafters" && actual="0" || actual="1"
assert "missing binary -> not indexed (block tier disabled)" "$actual" "1"
# The missing-binary path must NOT cache the verdict; otherwise a transient
# missing state becomes sticky for the rest of the session.
assert "missing-binary path leaves no cache file" \
  "$([ -f "$XDG_CACHE_HOME/legion/indexed-sess-3-rafters" ] && echo yes || echo no)" \
  "no"

echo "==> binary appears mid-session -> probe re-runs and succeeds"
cat > "$WORK/plugin/bin/legion" <<'EOF'
#!/bin/bash
echo '[{"repo":"rafters","lang":"typescript","size_bytes":300,"updated_at":"2026-01-01T00:00:00Z"}]'
EOF
chmod +x "$WORK/plugin/bin/legion"
legion_indexed "sess-3" "rafters" && actual="0" || actual="1"
assert "binary returned -> rafters now indexed" "$actual" "0"

echo
echo "==> $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
