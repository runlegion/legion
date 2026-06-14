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
# plugin/CHANGELOG.md; everything else must be committed.
#
# What it does, in order:
#   1. Guards: on main, tree clean except plugin/CHANGELOG.md, in sync with origin.
#   2. Computes the new version from the bump level (or takes it explicitly).
#   3. Requires plugin/CHANGELOG.md's "## <new>" top header to exist.
#   4. Runs scripts/preflight.sh (fmt, clippy --all-targets, test, SCIP regen).
#   5. Bumps Cargo.toml, refreshes Cargo.lock.
#   6. Syncs plugin.json + marketplace.json (scripts/sync-version.sh) and commits
#      chore(release): <new>. Running sync-version explicitly means the release is
#      correct even in a clone where the pre-commit hook was never installed.
#   7. Tags v<new> and atomically pushes the commit + tag. The tag fires
#      .github/workflows/release.yml, which builds + publishes the binaries.
#   8. With --activate: builds the release binary, installs it to the plugin data
#      dir atomically, and restarts the local daemon.
#
# The pure helpers below (compute_new_version, is_semver, is_strictly_greater) are
# unit-tested by scripts/test-release.sh, which sources this file. main() runs only
# when the script is executed, not when it is sourced.
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

# is_strictly_greater <new> <current> -> 0 iff new > current by semver ordering.
is_strictly_greater() {
  [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -1)" = "$2" ]
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
  # must be clean. Allow plugin/CHANGELOG.md to be dirty, reject anything else.
  DIRTY_OTHER="$(git status --porcelain --untracked-files=no | { grep -v 'plugin/CHANGELOG\.md$' || true; })"
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
  if [ "$DRY_RUN" = "1" ]; then
    printf '[dry-run] set Cargo.toml version %s -> %s\n' "$CURRENT" "$NEW" >&2
  else
    sed -i.bak "0,/^version = \"${CURRENT}\"/s//version = \"${NEW}\"/" "$CARGO_TOML"
    rm -f "${CARGO_TOML}.bak"
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
  # --atomic so main and the tag land together: a half-push that left the
  # commit without its tag would leave release.yml unfired.
  run git tag -a "v${NEW}" -m "legion ${NEW}"
  run git push --atomic origin main "v${NEW}"
  info "pushed v${NEW} -- release.yml will build + publish the platform binaries"

  # -- 8. optional local activation ------------------------------------------
  if [ "$ACTIVATE" = "1" ]; then
    local DATA_DIR DATA_BIN OLDPID
    DATA_DIR="${CLAUDE_PLUGIN_DATA:-${HOME}/.claude/plugins/data/legion-legion}"
    DATA_BIN="${DATA_DIR}/legion"
    info "activating ${NEW} locally: build + install + daemon restart"
    run cargo build --release --quiet
    run mkdir -p "$DATA_DIR"
    if [ "$DRY_RUN" = "1" ]; then
      printf '[dry-run] install target/release/legion -> %s (atomic)\n' "$DATA_BIN" >&2
      printf '[dry-run] restart legion daemon on port 3131\n' >&2
    else
      cp "${REPO_ROOT}/target/release/legion" "${DATA_BIN}.new"
      chmod +x "${DATA_BIN}.new"
      mv -f "${DATA_BIN}.new" "$DATA_BIN"
      info "installed $("$DATA_BIN" --version) to ${DATA_BIN}"
      # Stop the old daemon and CONFIRM it died before starting a new one --
      # otherwise the replacement loses the port bind, dies, and the health
      # probe reads empty, masking a dead daemon.
      if OLDPID="$(pgrep -f 'legion daemon --port 3131' | head -1)" && [ -n "$OLDPID" ]; then
        kill "$OLDPID" || true
        local _i
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
      info "daemon restarted: $(curl -s http://127.0.0.1:3131/health 2>/dev/null | head -c 120)"
    fi
  fi

  info "done: ${NEW}"
}

# Run main only when executed directly, so scripts/test-release.sh can source
# this file to unit-test the pure helpers without triggering a release.
if [ "${BASH_SOURCE[0]:-$0}" = "${0}" ]; then
  main "$@"
fi
