#!/usr/bin/env bash
# release.sh: one-command release, generalized to any repo via release.toml
# (schema documented there; see #741). The version-of-record file (default
# Cargo.toml) is the source of truth for the version; everything else --
# changelog path, propagation targets, tag format -- is read from config,
# derived, and validated.
#
# Usage:
#   scripts/release.sh patch            # 0.18.2 -> 0.18.3
#   scripts/release.sh minor            # 0.18.2 -> 0.19.0
#   scripts/release.sh major            # 0.18.2 -> 1.0.0
#   scripts/release.sh 0.20.0           # explicit version
#   scripts/release.sh patch --dry-run  # print every step, mutate nothing
#   scripts/release.sh patch --activate # also build+install the binary and
#                                        # restart the local daemon (legion-
#                                        # specific; a no-op knob elsewhere)
#   scripts/release.sh patch --no-preflight  # skip the preflight gate (CI re-runs it)
#
# Entry contract: the CHANGELOG entry for the new version must already be written
# (by the `changelog` agent, or by hand) and left UNCOMMITTED in the working tree.
# release.sh validates it, bumps the version, syncs the manifests, and commits the
# whole release in one shot. The only file allowed to be dirty at entry is the
# configured changelog path; everything else must be committed. (Note: even
# --dry-run requires the "## <new>" CHANGELOG header to exist -- the header
# check is part of what a dry-run verifies.)
#
# What it does, in order (unchanged by #741 -- only WHERE paths come from
# changed, not the sequence):
#   1. Guards: on the configured branch, tree clean except the changelog,
#      in sync with origin.
#   2. Computes the new version from the bump level (or takes it explicitly).
#   3. Requires the changelog's "## <new>" top header to exist.
#   4. Runs the configured preflight commands (default: scripts/preflight.sh).
#   5. Bumps the version-of-record file, refreshes Cargo.lock (Rust sources only).
#   6. Syncs every configured target (scripts/sync-version.sh) and commits.
#   7. Tags per the configured tag format and atomically pushes the commit +
#      tag, firing release.yml.
#   8. With --activate: builds the release binary, installs it to the plugin data
#      dir atomically, and restarts the local daemon (verifying it comes back up).
#
# The pure helpers below (compute_new_version, is_semver, is_strictly_greater,
# non_changelog_dirty, field_leaf, render_tag, bump_source_file) are unit-tested
# by scripts/test-release.sh, which sources this file. main() runs only when the
# script is executed, not when it is sourced. Implementation is kept
# POSIX-portable (no GNU-only sed/sort) because the release is cut on darwin,
# where BSD sed/sort lack the GNU extensions.
set -euo pipefail

info() { printf '[release] %s\n' "$1" >&2; }
fail() { printf '[release] ERROR: %s\n' "$1" >&2; exit 1; }

# compute_new_version <current> <bump>
#   bump in {patch,minor,major} -> semver-increment current; otherwise echo bump
#   verbatim (the explicit-version path). Pure: no I/O, no globals.
compute_new_version() {
  local current="$1" bump="$2" maj min pat
  case "$bump" in
    patch|minor|major)
      IFS='.' read -r maj min pat <<EOF
$current
EOF
      case "$bump" in
        patch) pat=$((pat + 1)) ;;
        minor) min=$((min + 1)); pat=0 ;;
        major) maj=$((maj + 1)); min=0; pat=0 ;;
      esac
      printf '%s.%s.%s\n' "$maj" "$min" "$pat"
      ;;
    *)
      printf '%s\n' "$bump"
      ;;
  esac
}

# is_semver <v> -> 0 iff v is strictly X.Y.Z (no prerelease/build/extra segments).
is_semver() { [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; }

# is_strictly_greater <new> <current> -> 0 iff new > current. Field-by-field
# numeric compare (both args must be strict semver) -- avoids `sort -V`, which is
# a GNU-ism absent from stock BSD sort.
is_strictly_greater() {
  local na nb nc ca cb cc
  IFS='.' read -r na nb nc <<EOF
$1
EOF
  IFS='.' read -r ca cb cc <<EOF
$2
EOF
  if [ "$na" -ne "$ca" ]; then [ "$na" -gt "$ca" ]; return; fi
  if [ "$nb" -ne "$cb" ]; then [ "$nb" -gt "$cb" ]; return; fi
  [ "$nc" -gt "$cc" ]
}

# non_changelog_dirty <changelog_path>: read `git status --porcelain` on
# stdin, print the lines whose changed path is NOT exactly <changelog_path>
# (repo-root-relative, forward-slash form -- the configured changelog.path).
# Parses the porcelain path field (handling rename/copy "ORIG -> DEST" by
# testing DEST) so the guard allowlists the file itself, not any path that
# merely ends in that suffix.
non_changelog_dirty() {
  local changelog_path="$1"
  local line path
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    path="${line:3}"                       # drop the 2-char status + space
    case "$path" in *" -> "*) path="${path##* -> }" ;; esac
    [ "$path" = "$changelog_path" ] || printf '%s\n' "$line"
  done
}

# field_leaf <field>: the last "."-separated segment of a dotted field path
# (e.g. "package.version" -> "version"). Used to find the literal `key =
# "value"` / `"key": "value"` line a flat-file bump rewrites -- release.toml's
# [version].field uses the same dotted-path convention as `legion sym etc
# extract`, but the bump step below only supports a top-level (or
# single-table-nested) leaf key, not a deep JSON walk.
field_leaf() {
  local field="$1"
  printf '%s' "${field##*.}"
}

# render_tag <format> <version>: substitute "{version}" in a template string.
# Used for both release.toml's [git].tag_format (e.g. "v{version}") and
# [git].tag_message (e.g. "legion {version}") -- same substitution either way.
render_tag() {
  local format="$1" version="$2"
  printf '%s' "${format//\{version\}/$version}"
}

# bump_source_file <file> <field> <current> <new>: rewrite the version-of-
# record file in place. Detects strategy from the file extension; both
# strategies rewrite only the FIRST matching line, so a second identical
# version string elsewhere in the file is left untouched:
#   .toml -- awk-based exact-line replace on `<leaf> = "<current>"` (the
#            existing Cargo.toml strategy; portable, no GNU sed/awk needed).
#   .json -- awk-based substring replace on `"<leaf>": "<current>"`, tolerant
#            of surrounding indentation (unlike the .toml branch, a json line
#            is rarely an exact match on its own).
# Any other extension is unsupported; returns 1 without mutating anything so
# the caller can name the failure (this function never calls exit, so it stays
# unit-testable, including its failure path).
bump_source_file() {
  local file="$1" field="$2" current="$3" new="$4" leaf
  leaf="$(field_leaf "$field")"
  case "$file" in
    *.toml)
      awk -v old="${leaf} = \"${current}\"" -v new="${leaf} = \"${new}\"" '
        !done && $0 == old { print new; done = 1; next } { print }
      ' "$file" >"${file}.tmp" && mv "${file}.tmp" "$file"
      ;;
    *.json)
      awk -v old="\"${leaf}\": \"${current}\"" -v new="\"${leaf}\": \"${new}\"" '
        !done && index($0, old) > 0 { sub(old, new); done = 1 } { print }
      ' "$file" >"${file}.tmp" && mv "${file}.tmp" "$file"
      ;;
    *)
      return 1
      ;;
  esac
}

main() {
  local REPO_ROOT CONFIG SOURCE_FILE CHANGELOG
  REPO_ROOT="$(git rev-parse --show-toplevel)"
  cd "$REPO_ROOT"
  CONFIG="${REPO_ROOT}/release.toml"

  command -v legion >/dev/null 2>&1 || fail "'legion' binary not found on PATH -- required to read release.toml"
  command -v python3 >/dev/null 2>&1 || fail "'python3' not found on PATH -- required to parse release.toml's json fields (preflight.commands, targets)"
  [ -f "$CONFIG" ] || fail "release.toml not found at repo root -- see scripts/release.sh header for the schema"

  # extract_cfg <field>: read one release.toml field, or fail loudly naming it.
  extract_cfg() {
    local field="$1" value
    value="$(legion sym etc extract "$CONFIG" --field "$field" 2>/dev/null)" \
      || fail "release.toml missing required field '${field}'"
    printf '%s' "$value"
  }
  # extract_cfg_default <field> <default>: like extract_cfg, but falls back
  # to <default> instead of failing when the field is absent.
  extract_cfg_default() {
    local field="$1" default="$2"
    legion sym etc extract "$CONFIG" --field "$field" 2>/dev/null || printf '%s' "$default"
  }

  local SOURCE_FILE_REL SOURCE_FIELD CHANGELOG_REL RELEASE_BRANCH TAG_FORMAT TAG_MESSAGE_FORMAT CO_AUTHORED_BY
  SOURCE_FILE_REL="$(extract_cfg version.file)"
  SOURCE_FIELD="$(extract_cfg version.field)"
  CHANGELOG_REL="$(extract_cfg changelog.path)"
  RELEASE_BRANCH="$(extract_cfg_default git.branch main)"
  TAG_FORMAT="$(extract_cfg_default git.tag_format 'v{version}')"
  TAG_MESSAGE_FORMAT="$(extract_cfg_default git.tag_message '{version}')"
  CO_AUTHORED_BY="$(extract_cfg_default git.co_authored_by 'Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"

  SOURCE_FILE="${REPO_ROOT}/${SOURCE_FILE_REL}"
  CHANGELOG="${REPO_ROOT}/${CHANGELOG_REL}"

  # -- arg parsing -----------------------------------------------------------
  local BUMP="" DRY_RUN=0 ACTIVATE=0 RUN_PREFLIGHT=1 arg
  for arg in "$@"; do
    case "$arg" in
      patch|minor|major) BUMP="$arg" ;;
      [0-9]*.[0-9]*.[0-9]*) BUMP="$arg" ;;
      --dry-run) DRY_RUN=1 ;;
      --activate) ACTIVATE=1 ;;
      --no-preflight) RUN_PREFLIGHT=0 ;;
      *) echo "[release] unknown argument: $arg" >&2; exit 2 ;;
    esac
  done
  [ -n "$BUMP" ] || {
    echo "[release] usage: release.sh <patch|minor|major|X.Y.Z> [--dry-run] [--activate] [--no-preflight]" >&2
    exit 2
  }

  # run: execute a mutating command, or just print it under --dry-run.
  run() {
    if [ "$DRY_RUN" = "1" ]; then
      printf '[dry-run] %s\n' "$*" >&2
    else
      "$@"
    fi
  }

  # -- 1. guards -------------------------------------------------------------
  local BRANCH LOCAL REMOTE DIRTY_OTHER
  BRANCH="$(git rev-parse --abbrev-ref HEAD)"
  [ "$BRANCH" = "$RELEASE_BRANCH" ] || fail "must be on ${RELEASE_BRANCH} to release (on '$BRANCH')"

  # The CHANGELOG entry is written-but-uncommitted at entry; everything else
  # must be clean. Allow only the configured changelog path to be dirty.
  DIRTY_OTHER="$(git status --porcelain --untracked-files=no | non_changelog_dirty "$CHANGELOG_REL")"
  [ -z "$DIRTY_OTHER" ] || fail "working tree has changes other than ${CHANGELOG_REL} -- commit or stash first:
$DIRTY_OTHER"

  git fetch origin "$RELEASE_BRANCH" --quiet || fail "git fetch failed"
  LOCAL="$(git rev-parse @)"
  REMOTE="$(git rev-parse '@{u}' 2>/dev/null || echo "")"
  [ -n "$REMOTE" ] || fail "no upstream for ${RELEASE_BRANCH}"
  [ "$LOCAL" = "$REMOTE" ] || fail "local ${RELEASE_BRANCH} is not in sync with origin/${RELEASE_BRANCH} -- pull/push first"

  # -- 2. compute + validate the new version ---------------------------------
  local CURRENT NEW TAG TAG_MESSAGE
  CURRENT="$(legion sym etc extract "$SOURCE_FILE" --field "$SOURCE_FIELD" 2>/dev/null || true)"
  [ -n "$CURRENT" ] || fail "could not read '${SOURCE_FIELD}' from ${SOURCE_FILE_REL}"
  is_semver "$CURRENT" || fail "${SOURCE_FILE_REL} version '$CURRENT' is not strict X.Y.Z"

  NEW="$(compute_new_version "$CURRENT" "$BUMP")"
  is_semver "$NEW" || fail "invalid version '$NEW' (expected X.Y.Z)"
  is_strictly_greater "$NEW" "$CURRENT" || fail "refusing non-increment $CURRENT -> $NEW"

  TAG="$(render_tag "$TAG_FORMAT" "$NEW")"
  TAG_MESSAGE="$(render_tag "$TAG_MESSAGE_FORMAT" "$NEW")"

  # Refuse a tag that already exists (a re-run after a partial failure, or a typo
  # colliding with history) before we mutate anything.
  ! git rev-parse "$TAG" >/dev/null 2>&1 || fail "tag ${TAG} already exists"

  info "releasing ${CURRENT} -> ${NEW}"

  # -- 3. require the CHANGELOG entry (human/agent-authored, never generated) -
  local TOP_HEADER
  TOP_HEADER="$(grep -E '^## [0-9]' "$CHANGELOG" | head -1 | sed -E 's/^## //' | awk '{print $1}')"
  if [ "$TOP_HEADER" != "$NEW" ]; then
    fail "${CHANGELOG_REL} top header is '## ${TOP_HEADER:-<none>}', expected '## ${NEW}'.
       Have the changelog agent (or you) add a '## ${NEW}' section at the top, then re-run."
  fi
  info "CHANGELOG header matches ${NEW}"

  # -- 4. preflight ----------------------------------------------------------
  if [ "$RUN_PREFLIGHT" = "1" ]; then
    local PREFLIGHT_JSON PREFLIGHT_CMDS
    PREFLIGHT_JSON="$(legion sym etc extract "$CONFIG" --field preflight.commands --json 2>/dev/null || printf '["bash scripts/preflight.sh"]')"
    PREFLIGHT_CMDS="$(printf '%s' "$PREFLIGHT_JSON" | python3 -c '
import json, sys
for c in json.load(sys.stdin):
    print(c)
')"
    info "running preflight: $(printf '%s' "$PREFLIGHT_CMDS" | tr '\n' ',' | sed 's/,$//')"
    while IFS= read -r cmd; do
      [ -n "$cmd" ] || continue
      run bash -c "$cmd"
    done <<<"$PREFLIGHT_CMDS"
  else
    info "preflight skipped (--no-preflight)"
  fi

  # -- 5. bump the version-of-record file + refresh Cargo.lock ---------------
  if [ "$DRY_RUN" = "1" ]; then
    printf '[dry-run] set %s %s -> %s\n' "$SOURCE_FILE_REL" "$CURRENT" "$NEW" >&2
  else
    bump_source_file "$SOURCE_FILE" "$SOURCE_FIELD" "$CURRENT" "$NEW" \
      || fail "unsupported version.file format '${SOURCE_FILE_REL}' (bump supports .toml and .json)"
  fi
  # Cargo.lock only exists (and only needs refreshing) for a Rust source file.
  if [ "${SOURCE_FILE_REL##*/}" = "Cargo.toml" ]; then
    run cargo build --quiet   # refreshes Cargo.lock's own version entry to ${NEW}
  fi

  # -- 6. sync manifests + commit --------------------------------------------
  # Run sync-version explicitly so the manifests are correct even if the
  # pre-commit hook is not installed in this clone (the #656 failure mode).
  run bash "${REPO_ROOT}/scripts/sync-version.sh"
  local ADD_FILES=("$SOURCE_FILE" "$CHANGELOG")
  if [ "${SOURCE_FILE_REL##*/}" = "Cargo.toml" ] && [ -f "${REPO_ROOT}/Cargo.lock" ]; then
    ADD_FILES+=("${REPO_ROOT}/Cargo.lock")
  fi
  local TARGETS_JSON TARGET_FILES_REL
  TARGETS_JSON="$(legion sym etc extract "$CONFIG" --field targets --json 2>/dev/null || printf '[]')"
  TARGET_FILES_REL="$(printf '%s' "$TARGETS_JSON" | python3 -c '
import json, sys
for t in json.load(sys.stdin):
    print(t["file"])
')"
  if [ -n "$TARGET_FILES_REL" ]; then
    while IFS= read -r t; do
      [ -n "$t" ] || continue
      ADD_FILES+=("${REPO_ROOT}/${t}")
    done <<<"$TARGET_FILES_REL"
  fi
  run git add "${ADD_FILES[@]}"
  run git commit -m "chore(release): ${NEW}

Co-Authored-By: ${CO_AUTHORED_BY}"

  # -- 7. tag + atomic push --------------------------------------------------
  # --atomic so the branch and the tag land together: a half-push that left
  # the commit without its tag would leave release.yml unfired.
  run git tag -a "$TAG" -m "$TAG_MESSAGE"
  run git push --atomic origin "$RELEASE_BRANCH" "$TAG"
  info "pushed ${TAG} -- release.yml will build + publish the platform binaries"

  # -- 8. optional local activation ------------------------------------------
  if [ "$ACTIVATE" = "1" ]; then
    activate_local "$REPO_ROOT" "$NEW" "$DRY_RUN"
  fi

  info "done: ${NEW}"
}

# activate_local <repo_root> <new> <dry_run>: build the release binary, install it
# to the plugin data dir atomically, and restart the daemon -- verifying both that
# the old one died and that the new one actually came back up on ${new}.
activate_local() {
  local REPO_ROOT="$1" NEW="$2" DRY_RUN="$3"
  local DATA_DIR DATA_BIN OLDPID HEALTH _i
  DATA_DIR="${CLAUDE_PLUGIN_DATA:-${HOME}/.claude/plugins/data/legion-legion}"
  DATA_BIN="${DATA_DIR}/legion"
  info "activating ${NEW} locally: build + install + daemon restart"

  if [ "$DRY_RUN" = "1" ]; then
    printf '[dry-run] cargo build --release\n' >&2
    printf '[dry-run] install target/release/legion -> %s (atomic)\n' "$DATA_BIN" >&2
    printf '[dry-run] restart legion daemon on port 3131 and verify /health\n' >&2
    return 0
  fi

  cargo build --release --quiet
  mkdir -p "$DATA_DIR"
  cp "${REPO_ROOT}/target/release/legion" "${DATA_BIN}.new"
  chmod +x "${DATA_BIN}.new"
  mv -f "${DATA_BIN}.new" "$DATA_BIN"
  info "installed $("$DATA_BIN" --version) to ${DATA_BIN}"

  # Stop the old daemon and CONFIRM it died before starting a new one -- a
  # replacement that races the old one loses the port bind and dies silently.
  if OLDPID="$(pgrep -f 'legion daemon --port 3131' | head -1)" && [ -n "$OLDPID" ]; then
    kill "$OLDPID" || true
    for _i in 1 2 3 4 5; do
      pgrep -f 'legion daemon --port 3131' >/dev/null 2>&1 || break
      sleep 1
    done
    if pgrep -f 'legion daemon --port 3131' >/dev/null 2>&1; then
      fail "old daemon (pid ${OLDPID}) did not exit -- refusing to start a competing daemon"
    fi
  fi

  nohup "$DATA_BIN" daemon --port 3131 >/tmp/legion-daemon.log 2>&1 &
  disown || true
  sleep 2

  # Verify the new daemon actually came up on the new version. An empty body
  # means it failed to bind/crashed -- do NOT report success on a dead daemon.
  HEALTH="$(curl -s http://127.0.0.1:3131/health 2>/dev/null || true)"
  case "$HEALTH" in
    "")          fail "daemon did not come back up (empty /health) -- see /tmp/legion-daemon.log" ;;
    *"${NEW}"*)  info "daemon restarted on ${NEW}: ${HEALTH}" ;;
    *)           fail "daemon /health does not report ${NEW}: ${HEALTH}" ;;
  esac
}

# Run main only when executed directly, so scripts/test-release.sh can source
# this file to unit-test the pure helpers without triggering a release.
if [ "${BASH_SOURCE[0]:-$0}" = "${0}" ]; then
  main "$@"
fi
