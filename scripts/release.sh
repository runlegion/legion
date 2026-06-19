#!/usr/bin/env bash
# release.sh: one-command release for legion. Cargo.toml is the source of truth
# for the version; everything else is derived and validated.
#
# Usage:
#   scripts/release.sh patch            # 0.18.2 -> 0.18.3
#   scripts/release.sh minor            # 0.18.2 -> 0.19.0
#   scripts/release.sh major            # 0.18.2 -> 1.0.0
#   scripts/release.sh 0.20.0           # explicit version
#   scripts/release.sh patch --dry-run  # print every step, mutate nothing
#   scripts/release.sh patch --activate # also build+install the binary and
#                                        # restart the local daemon
#   scripts/release.sh patch --no-preflight  # skip the cargo/scip gate (CI re-runs it)
#
# Entry contract: the CHANGELOG entry for the new version must already be written
# (by the `changelog` agent, or by hand) and left UNCOMMITTED in the working tree.
# release.sh validates it, bumps the version, syncs the manifests, and commits the
# whole release in one shot. The only file allowed to be dirty at entry is
# plugin/CHANGELOG.md; everything else must be committed. (Note: even --dry-run
# requires the "## <new>" CHANGELOG header to exist -- the header check is part of
# what a dry-run verifies.)
#
# What it does, in order:
#   1. Guards: on main, tree clean except plugin/CHANGELOG.md, in sync with origin.
#   2. Computes the new version from the bump level (or takes it explicitly).
#   3. Requires plugin/CHANGELOG.md's "## <new>" top header to exist.
#   4. Runs scripts/preflight.sh (fmt, clippy --all-targets, test, SCIP regen).
#   5. Bumps Cargo.toml, refreshes Cargo.lock.
#   6. Syncs plugin.json + marketplace.json (scripts/sync-version.sh) and commits.
#   7. Tags v<new> and atomically pushes the commit + tag, firing release.yml.
#   8. With --activate: builds the release binary, installs it to the plugin data
#      dir atomically, and restarts the local daemon (verifying it comes back up).
#
# The pure helpers below (compute_new_version, is_semver, is_strictly_greater,
# non_changelog_dirty) are unit-tested by scripts/test-release.sh, which sources
# this file. main() runs only when the script is executed, not when it is sourced.
# Implementation is kept POSIX-portable (no GNU-only sed/sort) because the release
# is cut on darwin, where BSD sed/sort lack the GNU extensions.
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

# non_changelog_dirty: read `git status --porcelain` on stdin, print the lines
# whose changed path is NOT exactly plugin/CHANGELOG.md. Parses the porcelain
# path field (handling rename/copy "ORIG -> DEST" by testing DEST) so the guard
# allowlists the file itself, not any path that merely ends in that suffix.
non_changelog_dirty() {
  local line path
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    path="${line:3}"                       # drop the 2-char status + space
    case "$path" in *" -> "*) path="${path##* -> }" ;; esac
    [ "$path" = "plugin/CHANGELOG.md" ] || printf '%s\n' "$line"
  done
}

main() {
  local REPO_ROOT CARGO_TOML CHANGELOG
  REPO_ROOT="$(git rev-parse --show-toplevel)"
  cd "$REPO_ROOT"
  CARGO_TOML="${REPO_ROOT}/Cargo.toml"
  CHANGELOG="${REPO_ROOT}/plugin/CHANGELOG.md"

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
  [ "$BRANCH" = "main" ] || fail "must be on main to release (on '$BRANCH')"

  # The CHANGELOG entry is written-but-uncommitted at entry; everything else
  # must be clean. Allow only plugin/CHANGELOG.md to be dirty.
  DIRTY_OTHER="$(git status --porcelain --untracked-files=no | non_changelog_dirty)"
  [ -z "$DIRTY_OTHER" ] || fail "working tree has changes other than plugin/CHANGELOG.md -- commit or stash first:
$DIRTY_OTHER"

  git fetch origin main --quiet || fail "git fetch failed"
  LOCAL="$(git rev-parse @)"
  REMOTE="$(git rev-parse '@{u}' 2>/dev/null || echo "")"
  [ -n "$REMOTE" ] || fail "no upstream for main"
  [ "$LOCAL" = "$REMOTE" ] || fail "local main is not in sync with origin/main -- pull/push first"

  # -- 2. compute + validate the new version ---------------------------------
  local CURRENT NEW
  CURRENT="$(grep -m1 '^version' "$CARGO_TOML" | sed 's/version = "\(.*\)"/\1/' || true)"
  [ -n "$CURRENT" ] || fail "could not read version from Cargo.toml"
  is_semver "$CURRENT" || fail "Cargo.toml version '$CURRENT' is not strict X.Y.Z"

  NEW="$(compute_new_version "$CURRENT" "$BUMP")"
  is_semver "$NEW" || fail "invalid version '$NEW' (expected X.Y.Z)"
  is_strictly_greater "$NEW" "$CURRENT" || fail "refusing non-increment $CURRENT -> $NEW"

  # Refuse a tag that already exists (a re-run after a partial failure, or a typo
  # colliding with history) before we mutate anything.
  ! git rev-parse "v${NEW}" >/dev/null 2>&1 || fail "tag v${NEW} already exists"

  info "releasing ${CURRENT} -> ${NEW}"

  # -- 3. require the CHANGELOG entry (human/agent-authored, never generated) -
  local TOP_HEADER
  TOP_HEADER="$(grep -E '^## [0-9]' "$CHANGELOG" | head -1 | sed -E 's/^## //' | awk '{print $1}')"
  if [ "$TOP_HEADER" != "$NEW" ]; then
    fail "plugin/CHANGELOG.md top header is '## ${TOP_HEADER:-<none>}', expected '## ${NEW}'.
       Have the changelog agent (or you) add a '## ${NEW}' section at the top, then re-run."
  fi
  info "CHANGELOG header matches ${NEW}"

  # -- 4. preflight ----------------------------------------------------------
  if [ "$RUN_PREFLIGHT" = "1" ]; then
    info "running preflight (fmt, clippy, test, scip)"
    run bash "${REPO_ROOT}/scripts/preflight.sh"
  else
    info "preflight skipped (--no-preflight)"
  fi

  # -- 5. bump Cargo.toml + refresh Cargo.lock -------------------------------
  # Replace the first exact `version = "<current>"` line via awk -- portable,
  # unlike `sed '0,/re/'` whose line-address 0 is a GNU extension BSD sed rejects.
  if [ "$DRY_RUN" = "1" ]; then
    printf '[dry-run] set Cargo.toml version %s -> %s\n' "$CURRENT" "$NEW" >&2
  else
    awk -v old="version = \"${CURRENT}\"" -v new="version = \"${NEW}\"" '
      !done && $0 == old { print new; done = 1; next } { print }
    ' "$CARGO_TOML" > "${CARGO_TOML}.tmp" && mv "${CARGO_TOML}.tmp" "$CARGO_TOML"
  fi
  run cargo build --quiet   # refreshes Cargo.lock's legion entry to ${NEW}

  # -- 6. sync manifests + commit --------------------------------------------
  # Run sync-version explicitly so the manifests are correct even if the
  # pre-commit hook is not installed in this clone (the #656 failure mode).
  run bash "${REPO_ROOT}/scripts/sync-version.sh"
  run git add Cargo.toml Cargo.lock plugin/CHANGELOG.md \
    plugin/.claude-plugin/plugin.json .claude-plugin/marketplace.json
  run git commit -m "chore(release): ${NEW}

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

  # -- 7. tag + atomic push --------------------------------------------------
  # --atomic so main and the tag land together: a half-push that left the commit
  # without its tag would leave release.yml unfired.
  run git tag -a "v${NEW}" -m "legion ${NEW}"
  run git push --atomic origin main "v${NEW}"
  info "pushed v${NEW} -- release.yml will build + publish the platform binaries"

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
