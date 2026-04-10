#!/bin/bash
# Sync version from Cargo.toml to plugin.json and marketplace.json.
# Run as a git pre-commit hook or manually.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
CARGO_TOML="${REPO_ROOT}/Cargo.toml"
PLUGIN_JSON="${REPO_ROOT}/plugin/.claude-plugin/plugin.json"
MARKETPLACE_JSON="${REPO_ROOT}/.claude-plugin/marketplace.json"

VERSION=$(grep '^version' "$CARGO_TOML" | head -1 | sed 's/version = "\(.*\)"/\1/')

if [ -z "$VERSION" ]; then
  echo "[sync-version] could not read version from Cargo.toml" >&2
  exit 1
fi

UPDATED=false

# Update plugin.json
if [ -f "$PLUGIN_JSON" ]; then
  CURRENT=$(grep '"version"' "$PLUGIN_JSON" | head -1 | sed 's/.*"\([0-9][0-9.]*\)".*/\1/')
  if [ "$CURRENT" != "$VERSION" ]; then
    sed -i '' "s/\"version\": \"${CURRENT}\"/\"version\": \"${VERSION}\"/" "$PLUGIN_JSON"
    git add "$PLUGIN_JSON"
    echo "[sync-version] plugin.json: ${CURRENT} -> ${VERSION}" >&2
    UPDATED=true
  fi
fi

# Update marketplace.json plugin version (not the top-level schema version)
if [ -f "$MARKETPLACE_JSON" ]; then
  # Match the version inside the plugins array (indented line)
  CURRENT=$(grep -A20 '"plugins"' "$MARKETPLACE_JSON" | grep '"version"' | head -1 | sed 's/.*"\([0-9][0-9.]*\)".*/\1/')
  if [ -n "$CURRENT" ] && [ "$CURRENT" != "$VERSION" ]; then
    # Use python for precise JSON update to avoid sed ambiguity with multiple version fields
    python3 -c "
import json, sys
with open('$MARKETPLACE_JSON', 'r') as f:
    data = json.load(f)
for p in data.get('plugins', []):
    p['version'] = '$VERSION'
with open('$MARKETPLACE_JSON', 'w') as f:
    json.dump(data, f, indent=2)
    f.write('\n')
"
    git add "$MARKETPLACE_JSON"
    echo "[sync-version] marketplace.json: ${CURRENT} -> ${VERSION}" >&2
    UPDATED=true
  fi
fi

if [ "$UPDATED" = false ]; then
  echo "[sync-version] all versions match ${VERSION}" >&2
fi
