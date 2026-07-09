#!/usr/bin/env bash
# test-release.sh: unit tests for the pure helpers in scripts/release.sh -- the
# version computation and validation that turn into a pushed git tag, so a
# regression here ("a typo becomes a pushed tag") is exactly what must be caught.
#
# Sources release.sh; main() is BASH_SOURCE-guarded, so sourcing defines the
# helpers without triggering a release.
set -u

DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=release.sh disable=SC1091
source "${DIR}/release.sh"
set +e   # release.sh enables errexit on source; tests manage their own codes.

PASS=0
FAIL=0

eq() { # eq <label> <expected> <actual>
  if [ "$2" = "$3" ]; then PASS=$((PASS + 1)); else
    FAIL=$((FAIL + 1)); printf 'FAIL: %s -- expected [%s] got [%s]\n' "$1" "$2" "$3" >&2
  fi
}
ok() { # ok <label> <cmd...>   (expect exit 0)
  local label="$1"; shift
  if "$@"; then PASS=$((PASS + 1)); else FAIL=$((FAIL + 1)); printf 'FAIL: %s -- expected success\n' "$label" >&2; fi
}
no() { # no <label> <cmd...>   (expect non-zero)
  local label="$1"; shift
  if "$@"; then FAIL=$((FAIL + 1)); printf 'FAIL: %s -- expected failure\n' "$label" >&2; else PASS=$((PASS + 1)); fi
}

# compute_new_version
eq "patch"         0.18.3 "$(compute_new_version 0.18.2 patch)"
eq "minor"         0.19.0 "$(compute_new_version 0.18.2 minor)"
eq "major"         1.0.0  "$(compute_new_version 0.18.2 major)"
eq "patch carry"   1.4.10 "$(compute_new_version 1.4.9 patch)"
eq "minor resets"  2.1.0  "$(compute_new_version 2.0.7 minor)"
eq "major resets"  3.0.0  "$(compute_new_version 2.9.9 major)"
eq "explicit"      0.20.0 "$(compute_new_version 0.18.2 0.20.0)"

# is_semver
ok "semver ok"          is_semver 0.18.3
no "semver prerelease"  is_semver 1.2.3-beta
no "semver short"       is_semver 1.2
no "semver long"        is_semver 1.2.3.4
no "semver nonnum"      is_semver 1.2.x
no "semver empty"       is_semver ""

# is_strictly_greater (numeric, not lexical -- 0.19.0 > 0.18.9, 0.18.10 > 0.18.9)
ok "gt patch"   is_strictly_greater 0.18.3 0.18.2
no "gt noop"    is_strictly_greater 0.18.2 0.18.2
no "gt down"    is_strictly_greater 0.18.1 0.18.2
ok "gt numeric" is_strictly_greater 0.19.0 0.18.9
ok "gt twodigit" is_strictly_greater 0.18.10 0.18.9
ok "gt major"   is_strictly_greater 1.0.0 0.18.2
no "gt majdown" is_strictly_greater 0.18.2 1.0.0

# non_changelog_dirty: only the configured changelog path is allowed dirty;
# anything else (including a rename whose destination is not the changelog)
# is "other dirty".
eq "dirty: changelog only allowed" "" \
  "$(printf ' M plugin/CHANGELOG.md\n' | non_changelog_dirty 'plugin/CHANGELOG.md')"
eq "dirty: other file flagged" " M src/main.rs" \
  "$(printf ' M plugin/CHANGELOG.md\n M src/main.rs\n' | non_changelog_dirty 'plugin/CHANGELOG.md')"
eq "dirty: suffix lookalike flagged" " M docs/plugin/CHANGELOG.md" \
  "$(printf ' M docs/plugin/CHANGELOG.md\n' | non_changelog_dirty 'plugin/CHANGELOG.md')"
eq "dirty: rename dest to changelog allowed" "" \
  "$(printf 'R  old.md -> plugin/CHANGELOG.md\n' | non_changelog_dirty 'plugin/CHANGELOG.md')"
eq "dirty: empty tree is clean" "" "$(printf '' | non_changelog_dirty 'plugin/CHANGELOG.md')"
eq "dirty: honors a different configured path" "" \
  "$(printf ' M CHANGELOG.md\n' | non_changelog_dirty 'CHANGELOG.md')"

# field_leaf: last "."-separated segment.
eq "leaf: nested"    "version" "$(field_leaf package.version)"
eq "leaf: flat"       "version" "$(field_leaf version)"
eq "leaf: array index" "version" "$(field_leaf plugins.0.version)"

# render_tag: "{version}" substitution.
eq "tag: default format"  "v0.20.0"        "$(render_tag 'v{version}' 0.20.0)"
eq "tag: no placeholder"  "release"        "$(render_tag release 0.20.0)"
eq "tag: custom format"   "ledger@0.2.0"   "$(render_tag '{version}' ledger@0.2.0)"

# bump_source_file: rewrites the version-of-record file in place per its
# extension, and never mutates + returns 1 on an unsupported one -- this is
# #741's proof that the bump step generalizes beyond Cargo.toml (a package.json
# source, e.g. a JS repo's `[version] file = "package.json"`, bumps the same way
# a flat json target does in scripts/sync-version.sh).
BUMP_DIR=$(mktemp -d)
trap 'rm -rf "$BUMP_DIR"' EXIT

cat >"${BUMP_DIR}/Cargo.toml" <<'EOF'
[package]
name = "fixture"
version = "0.6.5"
edition = "2024"
EOF
ok "bump: toml source" bump_source_file "${BUMP_DIR}/Cargo.toml" package.version 0.6.5 0.7.0
eq "bump: toml result" 'version = "0.7.0"' "$(grep '^version' "${BUMP_DIR}/Cargo.toml")"

cat >"${BUMP_DIR}/package.json" <<'EOF'
{
  "name": "@rafters/fixture",
  "version": "0.2.0"
}
EOF
ok "bump: json source" bump_source_file "${BUMP_DIR}/package.json" version 0.2.0 0.3.0
eq "bump: json result" '  "version": "0.3.0"' "$(grep '"version"' "${BUMP_DIR}/package.json")"
eq "bump: json rest untouched" '  "name": "@rafters/fixture",' "$(sed -n '2p' "${BUMP_DIR}/package.json")"

echo "unsupported" >"${BUMP_DIR}/version.yaml"
no "bump: unsupported format refused" bump_source_file "${BUMP_DIR}/version.yaml" version 0.1.0 0.2.0
eq "bump: unsupported format untouched" "unsupported" "$(cat "${BUMP_DIR}/version.yaml")"

printf '\n[test-release] %d passed, %d failed\n' "$PASS" "$FAIL" >&2
[ "$FAIL" -eq 0 ]
