#!/bin/bash
# Smoke test for scripts/sync-version.sh -- and, per #741, the proof that its
# release.toml-driven propagation generalizes beyond legion's own shape.
#
# Scenarios 1-4 build a temporary fake-repo fixture with the same directory
# shape as legion (Cargo.toml, plugin/.claude-plugin/plugin.json,
# .claude-plugin/marketplace.json, plugin/CHANGELOG.md, release.toml), run
# sync-version.sh, and assert the expected exit code + output sentinels:
#   1. All versions match   -> exit 0, "all versions match"
#   2. Cargo ahead of others -> exit 0, plugin+marketplace auto-updated
#   3. plugin.json AHEAD    -> exit 1, "Refusing to downgrade"
#   4. CHANGELOG behind     -> exit 1, "top header" mismatch
#
# Scenario 5 builds a deliberately NON-legion-shaped fixture (a package.json
# version source, a root-level CHANGELOG.md, one flat json target and one
# nested-by-object-key json target -- none of which are legion's paths or
# field names) and asserts the same propagation behavior holds, proving the
# generalization criterion of #741 rather than merely asserting it.
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

if ! command -v legion >/dev/null 2>&1; then
  echo "SKIP: 'legion' binary not found on PATH -- scripts/sync-version.sh now reads" >&2
  echo "  release.toml via 'legion sym etc extract'; this test cannot run without it." >&2
  echo "  (Not wired into CI -- see .github/workflows/ci.yml -- so this only affects" >&2
  echo "  a local preflight run on a machine without the legion plugin installed.)" >&2
  exit 0
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

  cat >"$root/release.toml" <<'EOF'
[version]
file = "Cargo.toml"
field = "package.version"

[changelog]
path = "plugin/CHANGELOG.md"

[[targets]]
file = "plugin/.claude-plugin/plugin.json"
field = "version"

[[targets]]
file = ".claude-plugin/marketplace.json"
field = "plugins.0.version"
EOF

  # Minimal git init so git rev-parse + git add inside sync-version.sh work.
  # Hermetic identity: -c user.name/-c user.email/-c commit.gpgsign=false with
  # an explicit current_dir, never the ambient .git/config.
  (
    cd "$root"
    git init -q
    git add -A
    git -c user.email=test@test -c user.name=test -c commit.gpgsign=false commit -q -m init
  )
}

# Build a fixture deliberately shaped UNLIKE legion: a package.json version
# source (flat top-level field, not a nested Cargo.toml table), a root-level
# CHANGELOG.md (not plugin/CHANGELOG.md), a flat json target, and a
# nested-by-object-key json target (proving the dotted-path json writer
# generalizes beyond "array index", legion's only nested case).
#   $1 -- fixture root
#   $2 -- package.json version
#   $3 -- manifest.json (flat "version") value
#   $4 -- config/app.json meta.version value
#   $5 -- CHANGELOG.md top header version
build_foreign_fixture() {
  local root="$1"
  local pkg_v="$2"
  local manifest_v="$3"
  local app_v="$4"
  local changelog_v="$5"

  mkdir -p "$root/config"
  cat >"$root/package.json" <<EOF
{
  "name": "@rafters/fixture",
  "version": "$pkg_v"
}
EOF

  cat >"$root/manifest.json" <<EOF
{
  "version": "$manifest_v"
}
EOF

  cat >"$root/config/app.json" <<EOF
{
  "meta": {
    "version": "$app_v"
  }
}
EOF

  cat >"$root/CHANGELOG.md" <<EOF
# Fixture Changelog

## $changelog_v

Test fixture.
EOF

  cat >"$root/release.toml" <<'EOF'
[version]
file = "package.json"
field = "version"

[changelog]
path = "CHANGELOG.md"

[[targets]]
file = "manifest.json"
field = "version"

[[targets]]
file = "config/app.json"
field = "meta.version"
EOF

  (
    cd "$root"
    git init -q
    git add -A
    git -c user.email=test@test -c user.name=test -c commit.gpgsign=false commit -q -m init
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

echo "[test-sync-version] running 5 scenarios..."

T1=$(mktemp -d)
T2=$(mktemp -d)
T3=$(mktemp -d)
T4=$(mktemp -d)
T5=$(mktemp -d)
trap 'rm -rf "$T1" "$T2" "$T3" "$T4" "$T5"' EXIT

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

# Scenario 5 (#741 generalization proof): a non-legion-shaped repo -- package.json
# source, root CHANGELOG.md, a flat target AND a nested-by-object-key target,
# none of which are legion's paths or field names -- propagates identically.
build_foreign_fixture "$T5" 0.2.0 0.1.0 0.1.0 0.2.0
run_case "5: foreign repo shape, sync down" "$T5" 0 "manifest.json: 0.1.0 -> 0.2.0"
if grep -q -- "config/app.json: 0.1.0 -> 0.2.0" "$T5/_stderr.log"; then
  pass "5b: nested (object-key) target propagated"
else
  fail "5b: expected 'config/app.json: 0.1.0 -> 0.2.0' in stderr:"
  sed 's/^/      /' <"$T5/_stderr.log" >&2
fi
if [ "$(python3 -c "import json; print(json.load(open('$T5/config/app.json'))['meta']['version'])")" = "0.2.0" ]; then
  pass "5c: nested target value actually written"
else
  fail "5c: config/app.json meta.version was not rewritten to 0.2.0"
fi

if [ "$FAILS" -eq 0 ]; then
  echo "[test-sync-version] all scenarios passed"
  exit 0
else
  echo "[test-sync-version] $FAILS scenario(s) failed" >&2
  exit 1
fi
