#!/bin/bash
# Sync a repo's version of record to its configured propagation targets, per
# release.toml (schema documented there; see #741).
#
# release.toml declares:
#   [version]  file + field   -- the version of record (any json/toml/yaml
#                                 file `legion sym etc extract` can read).
#   [changelog] path          -- the changelog whose top "## <version>"
#                                 header must match the source version.
#   [[targets]] file + field  -- files the source version propagates to.
#
# This script:
#   1. Reads the version of record via `legion sym etc extract`.
#   2. Refuses to proceed if any target carries a HIGHER version than the
#      source -- never auto-downgrades. Bump the source first.
#   3. Updates every target that is at or behind the source and stages the
#      resulting changes into the in-flight commit.
#   4. Requires the changelog's top "## <version>" header to match the
#      source. Fails loudly if the developer forgot to log the bump.
#
# Only json targets are rewritten (the write side); a flat field (no ".")
# is rewritten via a single-line sed replace so the rest of the file is
# untouched; a dotted field (nested key or array index, e.g.
# "plugins.0.version") is rewritten via a small json.load/dump cycle.
#
# Safe to run manually; intended to run as a git pre-commit hook via
# .githooks/pre-commit + scripts/install-hooks.sh. Requires the `legion`
# binary on PATH -- it is how this script reads release.toml and every
# target's current value without hand-rolling a TOML/JSON parser in bash.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
CONFIG="${REPO_ROOT}/release.toml"

if ! command -v legion >/dev/null 2>&1; then
  echo "[sync-version] ERROR: 'legion' binary not found on PATH -- required to read release.toml" >&2
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "[sync-version] ERROR: 'python3' not found on PATH -- required to parse release.toml's json fields (targets) and rewrite nested json targets" >&2
  exit 1
fi

if [ ! -f "$CONFIG" ]; then
  echo "[sync-version] ERROR: release.toml not found at repo root -- see scripts/sync-version.sh header for the schema" >&2
  exit 1
fi

# extract_required <field>: read one release.toml field, or fail loudly
# naming the field.
extract_required() {
  local field="$1"
  local value
  if ! value="$(legion sym etc extract "$CONFIG" --field "$field" 2>/dev/null)"; then
    echo "[sync-version] ERROR: release.toml missing required field '${field}'" >&2
    exit 1
  fi
  printf '%s' "$value"
}

SOURCE_FILE_REL="$(extract_required version.file)"
SOURCE_FIELD="$(extract_required version.field)"
CHANGELOG_REL="$(extract_required changelog.path)"
TARGETS_JSON="$(legion sym etc extract "$CONFIG" --field targets --json 2>/dev/null || printf '[]')"

SOURCE_FILE="${REPO_ROOT}/${SOURCE_FILE_REL}"
CHANGELOG="${REPO_ROOT}/${CHANGELOG_REL}"

if [ ! -f "$SOURCE_FILE" ]; then
  echo "[sync-version] ERROR: version.file '${SOURCE_FILE_REL}' (release.toml) does not exist" >&2
  exit 1
fi

SOURCE_VERSION="$(legion sym etc extract "$SOURCE_FILE" --field "$SOURCE_FIELD" 2>/dev/null || true)"
if [ -z "$SOURCE_VERSION" ]; then
  echo "[sync-version] ERROR: could not read '${SOURCE_FIELD}' from ${SOURCE_FILE_REL}" >&2
  exit 1
fi

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

# rewrite_json_field <file> <field> <new_value>: dotted-path write, the
# mutation-side counterpart of `legion sym etc extract`'s dotted-path read.
# Segments split on ".", a purely numeric segment indexes an array, any
# other segment is an object key. Pass path/field/value via env vars, NOT
# shell interpolation into the python string literal -- a value containing a
# single quote would otherwise break the python source.
rewrite_json_field() {
  local file="$1" field="$2" new_value="$3"
  TARGET_FILE="$file" TARGET_FIELD="$field" NEW_VALUE="$new_value" python3 <<'PY'
import json, os

path = os.environ['TARGET_FILE']
field = os.environ['TARGET_FIELD']
new_value = os.environ['NEW_VALUE']

with open(path, 'r') as f:
    data = json.load(f)

segs = field.split('.')
node = data
for seg in segs[:-1]:
    node = node[int(seg)] if seg.isdigit() else node[seg]
last = segs[-1]
if last.isdigit():
    node[int(last)] = new_value
else:
    node[last] = new_value

with open(path, 'w') as f:
    json.dump(data, f, indent=2)
    f.write('\n')
PY
}

# --- validation phase: everything that can reject the commit runs first ---
# Rationale: if we mutated a target before discovering a stale CHANGELOG, the
# developer would land in a half-synced working tree that blocks the commit
# without any obvious undo path. Validate all invariants up front, exit 1
# loudly, then mutate only once we know every check passes.

# Abort if any target is AHEAD of the source version.
check_not_ahead() {
  local file="$1" field="$2" label="$3"
  [ -f "$file" ] || return 0
  local other
  other="$(legion sym etc extract "$file" --field "$field" 2>/dev/null || true)"
  [ -n "$other" ] || return 0
  if ! version_leq "$other" "$SOURCE_VERSION"; then
    echo "[sync-version] ERROR: ${label} is at ${other}, ahead of ${SOURCE_FILE_REL} at ${SOURCE_VERSION}" >&2
    echo "[sync-version] Refusing to downgrade ${label}. Bump ${SOURCE_FILE_REL} to >= ${other} first, then commit." >&2
    exit 1
  fi
}

# One "file<TAB>field" line per target, parsed once up front.
TARGETS_LINES="$(printf '%s' "$TARGETS_JSON" | python3 -c '
import json, sys
for t in json.load(sys.stdin):
    print("{}\t{}".format(t["file"], t["field"]))
')"

if [ -n "$TARGETS_LINES" ]; then
  while IFS=$'\t' read -r TARGET_FILE_REL TARGET_FIELD; do
    check_not_ahead "${REPO_ROOT}/${TARGET_FILE_REL}" "$TARGET_FIELD" "$TARGET_FILE_REL"
  done <<<"$TARGETS_LINES"
fi

# Require CHANGELOG top header to match the source version. Changelog body
# is human-authored -- we only enforce the header version tag.
if [ -f "$CHANGELOG" ]; then
  TOP_HEADER=$(grep -E '^## [0-9]' "$CHANGELOG" | head -1 | sed -E 's/^## //' | awk '{print $1}')
  if [ -z "$TOP_HEADER" ]; then
    echo "[sync-version] WARNING: no '## <version>' header found in ${CHANGELOG_REL}" >&2
  elif [ "$TOP_HEADER" != "$SOURCE_VERSION" ]; then
    echo "[sync-version] ERROR: ${CHANGELOG_REL} top header is '## ${TOP_HEADER}', ${SOURCE_FILE_REL} is ${SOURCE_VERSION}" >&2
    echo "[sync-version] Add a new '## ${SOURCE_VERSION}' section at the top of ${CHANGELOG_REL} describing what shipped in this release, then commit." >&2
    exit 1
  fi
fi

# --- mutation phase: every invariant above passed, safe to edit files ---

UPDATED=false

if [ -n "$TARGETS_LINES" ]; then
  while IFS=$'\t' read -r TARGET_FILE_REL TARGET_FIELD; do
    TARGET_FILE="${REPO_ROOT}/${TARGET_FILE_REL}"

    [ -f "$TARGET_FILE" ] || continue
    CURRENT="$(legion sym etc extract "$TARGET_FILE" --field "$TARGET_FIELD" 2>/dev/null || true)"
    [ -n "$CURRENT" ] || continue
    [ "$CURRENT" != "$SOURCE_VERSION" ] || continue

    case "$TARGET_FIELD" in
      *.*)
        # Dotted path (nested key or array index): full json.load/dump cycle.
        rewrite_json_field "$TARGET_FILE" "$TARGET_FIELD" "$SOURCE_VERSION"
        ;;
      *)
        # Flat top-level field: single-line replace, rest of file untouched.
        portable_sed_inplace "s/\"${TARGET_FIELD}\": \"${CURRENT}\"/\"${TARGET_FIELD}\": \"${SOURCE_VERSION}\"/" "$TARGET_FILE"
        ;;
    esac
    git add "$TARGET_FILE"
    echo "[sync-version] ${TARGET_FILE_REL}: ${CURRENT} -> ${SOURCE_VERSION}" >&2
    UPDATED=true
  done <<<"$TARGETS_LINES"
fi

if [ "$UPDATED" = false ]; then
  echo "[sync-version] all versions match ${SOURCE_VERSION}" >&2
fi
