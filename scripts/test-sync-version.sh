#!/bin/bash
# Smoke test for scripts/sync-version.sh.
#
# Builds a temporary fake-repo fixture with the same directory shape as legion
# (Cargo.toml, plugin/.claude-plugin/plugin.json, .claude-plugin/marketplace.json,
# plugin/CHANGELOG.md), runs sync-version.sh under four scenarios, and asserts
# the expected exit code + output sentinels.
#
# Scenarios:
#   1. All versions match   -> exit 0, "all versions match"
#   2. Cargo ahead of others -> exit 0, plugin+marketplace auto-updated
#   3. plugin.json AHEAD    -> exit 1, "Refusing to downgrade"
#   4. CHANGELOG behind     -> exit 1, "top header" mismatch
#
# Run manually: bash scripts/test-sync-version.sh
# Exits 0 if every scenario behaves as expected, 1 otherwise.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
SYNC_SCRIPT="${REPO_ROOT}/scripts/sync-version.sh"

if [ ! -f "$SYNC_SCRIPT" ]; then
  echo "ERROR: $SYNC_SCRIPT not found" >&2
  exit 1
fi

FAILS=0
pass() { printf '  [PASS] %s\n' "$1"; }
fail() {
  printf '  [FAIL] %s\n' "$1" >&2
  FAILS=$((FAILS + 1))
}

# Build a fake-repo fixture at $1 with the given versions.
#   $1 -- fixture root
#   $2 -- Cargo.toml version
#   $3 -- plugin.json version
#   $4 -- marketplace.json plugins[0].version
#   $5 -- CHANGELOG top header version
build_fixture() {
  local root="$1"
  local cargo_v="$2"
  local plugin_v="$3"
  local market_v="$4"
  local changelog_v="$5"

  mkdir -p "$root/plugin/.claude-plugin" "$root/.claude-plugin" "$root/plugin"
  cat >"$root/Cargo.toml" <<EOF
[package]
name = "legion-fixture"
version = "$cargo_v"
edition = "2024"
EOF

  cat >"$root/plugin/.claude-plugin/plugin.json" <<EOF
{
  "name": "legion",
  "version": "$plugin_v"
}
EOF

  cat >"$root/.claude-plugin/marketplace.json" <<EOF
{
  "version": "1.0.0",
  "plugins": [
    {
      "name": "legion",
      "version": "$market_v"
    }
  ]
}
EOF

  cat >"$root/plugin/CHANGELOG.md" <<EOF
# Legion Changelog

## $changelog_v

Test fixture.
EOF

  # Minimal git init so git rev-parse + git add inside sync-version.sh work.
  (
    cd "$root"
    git init -q
    git add -A
    git -c user.email=test@test -c user.name=test commit -q -m init
  )
}

run_case() {
  local name="$1"
  local root="$2"
  local expected_exit="$3"
  local expected_sentinel="$4"

  local stderr_file="$root/_stderr.log"
  local actual_exit=0
  ( cd "$root" && bash "$SYNC_SCRIPT" ) 2>"$stderr_file" || actual_exit=$?

  if [ "$actual_exit" -ne "$expected_exit" ]; then
    fail "$name: expected exit $expected_exit, got $actual_exit. stderr:"
    sed 's/^/      /' <"$stderr_file" >&2
    return
  fi
  if ! grep -q -- "$expected_sentinel" "$stderr_file"; then
    fail "$name: expected stderr to contain '$expected_sentinel'. actual stderr:"
    sed 's/^/      /' <"$stderr_file" >&2
    return
  fi
  pass "$name"
}

echo "[test-sync-version] running 4 scenarios..."

T1=$(mktemp -d)
T2=$(mktemp -d)
T3=$(mktemp -d)
T4=$(mktemp -d)
trap 'rm -rf "$T1" "$T2" "$T3" "$T4"' EXIT

# Scenario 1: all versions match 0.6.5
build_fixture "$T1" 0.6.5 0.6.5 0.6.5 0.6.5
run_case "1: all match" "$T1" 0 "all versions match 0.6.5"

# Scenario 2: Cargo ahead, plugin + marketplace behind, CHANGELOG matches Cargo
build_fixture "$T2" 0.7.0 0.6.5 0.6.5 0.7.0
run_case "2: Cargo ahead, sync down" "$T2" 0 "plugin.json: 0.6.5 -> 0.7.0"

# Scenario 3: plugin.json ahead of Cargo.toml -- refuse to downgrade
build_fixture "$T3" 0.6.5 0.7.0 0.6.5 0.6.5
run_case "3: plugin ahead -> refuse" "$T3" 1 "Refusing to downgrade"

# Scenario 4: Cargo matches plugin/marketplace but CHANGELOG top header is stale
build_fixture "$T4" 0.7.0 0.7.0 0.7.0 0.6.5
run_case "4: CHANGELOG behind" "$T4" 1 "top header"

if [ "$FAILS" -eq 0 ]; then
  echo "[test-sync-version] all scenarios passed"
  exit 0
else
  echo "[test-sync-version] $FAILS scenario(s) failed" >&2
  exit 1
fi
