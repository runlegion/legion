#!/bin/bash
# Sync version from Cargo.toml to plugin.json and marketplace.json.
#
# Cargo.toml is the single source of truth. This script:
#   1. Reads the version from Cargo.toml.
#   2. Refuses to proceed if plugin.json or marketplace.json carries a HIGHER
#      version than Cargo.toml -- never auto-downgrades. Bump Cargo.toml first.
#   3. Updates plugin.json and marketplace.json if they are at or behind Cargo.toml
#      and stages the resulting changes into the in-flight commit.
#   4. Requires plugin/CHANGELOG.md's top "## <version>" header to match
#      Cargo.toml. Fails loudly if the developer forgot to log the bump.
#
# Safe to run manually; intended to run as a git pre-commit hook via
# .githooks/pre-commit + scripts/install-hooks.sh.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
CARGO_TOML="${REPO_ROOT}/Cargo.toml"
PLUGIN_JSON="${REPO_ROOT}/plugin/.claude-plugin/plugin.json"
MARKETPLACE_JSON="${REPO_ROOT}/.claude-plugin/marketplace.json"
CHANGELOG="${REPO_ROOT}/plugin/CHANGELOG.md"

# Extract a "x.y.z" version string from a supported file type.
extract_version() {
  local file="$1"
  case "$file" in
    *Cargo.toml)
      grep '^version' "$file" | head -1 | sed 's/version = "\(.*\)"/\1/'
      ;;
    *plugin.json)
      grep '"version"' "$file" | head -1 | sed 's/.*"\([0-9][0-9.]*\)".*/\1/'
      ;;
    *marketplace.json)
      # Version inside the plugins[] array, NOT the top-level schema version.
      grep -A20 '"plugins"' "$file" | grep '"version"' | head -1 | sed 's/.*"\([0-9][0-9.]*\)".*/\1/'
      ;;
  esac
}

# Return 0 if $1 <= $2 by semver ordering, 1 otherwise. Uses `sort -V`.
version_leq() {
  [ "$1" = "$2" ] || [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -1)" = "$1" ]
}

# Portable in-place sed that works on both macOS and GNU sed.
# macOS sed -i requires an explicit backup suffix argument; GNU sed accepts
# -i alone. -i.bak works on both platforms; we then remove the .bak file.
portable_sed_inplace() {
  local expr="$1"
  local file="$2"
  sed -i.bak "$expr" "$file"
  rm -f "${file}.bak"
}

CARGO_VERSION=$(extract_version "$CARGO_TOML")
if [ -z "$CARGO_VERSION" ]; then
  echo "[sync-version] ERROR: could not read version from Cargo.toml" >&2
  exit 1
fi

# Abort if any tracked version file is AHEAD of Cargo.toml.
check_not_ahead() {
  local file="$1"
  local label="$2"
  [ -f "$file" ] || return 0
  local other
  other=$(extract_version "$file")
  [ -n "$other" ] || return 0
  if ! version_leq "$other" "$CARGO_VERSION"; then
    echo "[sync-version] ERROR: ${label} is at ${other}, ahead of Cargo.toml at ${CARGO_VERSION}" >&2
    echo "[sync-version] Refusing to downgrade ${label}. Bump Cargo.toml to >= ${other} first, then commit." >&2
    exit 1
  fi
}

# --- validation phase: everything that can reject the commit runs first ---
# Rationale: if we mutated plugin.json before discovering a stale CHANGELOG,
# the developer would land in a half-synced working tree that blocks the
# commit without any obvious undo path. Validate all invariants up front,
# exit 1 loudly, then mutate only once we know every check passes.

check_not_ahead "$PLUGIN_JSON" "plugin.json"
check_not_ahead "$MARKETPLACE_JSON" "marketplace.json"

# Require CHANGELOG top header to match Cargo.toml. Changelog body is
# human-authored -- we only enforce the header version tag.
if [ -f "$CHANGELOG" ]; then
  TOP_HEADER=$(grep -E '^## [0-9]' "$CHANGELOG" | head -1 | sed -E 's/^## //' | awk '{print $1}')
  if [ -z "$TOP_HEADER" ]; then
    echo "[sync-version] WARNING: no '## <version>' header found in ${CHANGELOG#"$REPO_ROOT/"}" >&2
  elif [ "$TOP_HEADER" != "$CARGO_VERSION" ]; then
    echo "[sync-version] ERROR: ${CHANGELOG#"$REPO_ROOT/"} top header is '## ${TOP_HEADER}', Cargo.toml is ${CARGO_VERSION}" >&2
    echo "[sync-version] Add a new '## ${CARGO_VERSION}' section at the top of ${CHANGELOG#"$REPO_ROOT/"} describing what shipped in this release, then commit." >&2
    exit 1
  fi
fi

# --- mutation phase: every invariant above passed, safe to edit files ---

UPDATED=false

# Update plugin.json to match Cargo.toml if needed.
if [ -f "$PLUGIN_JSON" ]; then
  CURRENT=$(extract_version "$PLUGIN_JSON")
  if [ -n "$CURRENT" ] && [ "$CURRENT" != "$CARGO_VERSION" ]; then
    portable_sed_inplace "s/\"version\": \"${CURRENT}\"/\"version\": \"${CARGO_VERSION}\"/" "$PLUGIN_JSON"
    git add "$PLUGIN_JSON"
    echo "[sync-version] plugin.json: ${CURRENT} -> ${CARGO_VERSION}" >&2
    UPDATED=true
  fi
fi

# Update marketplace.json via python for precise JSON handling (avoids sed
# ambiguity when multiple "version" fields are present at different depths).
# Pass path + version via env vars, NOT shell interpolation into the python
# string literal -- a path or version containing a single quote would
# otherwise break the python source. The heredoc is single-quoted so bash
# does not interpolate inside it; python reads from os.environ.
if [ -f "$MARKETPLACE_JSON" ]; then
  CURRENT=$(extract_version "$MARKETPLACE_JSON")
  if [ -n "$CURRENT" ] && [ "$CURRENT" != "$CARGO_VERSION" ]; then
    MARKETPLACE_JSON="$MARKETPLACE_JSON" CARGO_VERSION="$CARGO_VERSION" python3 <<'PY'
import json, os
path = os.environ['MARKETPLACE_JSON']
version = os.environ['CARGO_VERSION']
with open(path, 'r') as f:
    data = json.load(f)
for p in data.get('plugins', []):
    p['version'] = version
with open(path, 'w') as f:
    json.dump(data, f, indent=2)
    f.write('\n')
PY
    git add "$MARKETPLACE_JSON"
    echo "[sync-version] marketplace.json: ${CURRENT} -> ${CARGO_VERSION}" >&2
    UPDATED=true
  fi
fi

if [ "$UPDATED" = false ]; then
  echo "[sync-version] all versions match ${CARGO_VERSION}" >&2
fi
