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
# What it does, in order:
#   1. Guards: on main, clean working tree, in sync with origin/main.
#   2. Computes the new version from the bump level (or takes it explicitly).
#   3. Requires plugin/CHANGELOG.md to already carry a "## <new>" top header --
#      release prose is human-authored, never generated. Fails loudly if absent.
#   4. Runs scripts/preflight.sh (fmt, clippy --all-targets, test, SCIP regen).
#   5. Bumps Cargo.toml, refreshes Cargo.lock.
#   6. Syncs plugin.json + marketplace.json (scripts/sync-version.sh) and commits
#      chore(release): <new>. Running sync-version explicitly means the release
#      is correct even in a clone where the pre-commit hook was never installed
#      -- the exact failure that motivated #656.
#   7. Tags v<new> and pushes the commit + tag. The tag fires .github/workflows/
#      release.yml, which builds the platform binaries and publishes the Release.
#   8. With --activate: builds the release binary, installs it to the plugin data
#      dir atomically, and restarts the local daemon so the new code runs now.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

CARGO_TOML="${REPO_ROOT}/Cargo.toml"
CHANGELOG="${REPO_ROOT}/plugin/CHANGELOG.md"

# -- arg parsing -------------------------------------------------------------
BUMP=""
DRY_RUN=0
ACTIVATE=0
RUN_PREFLIGHT=1

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

if [ -z "$BUMP" ]; then
  echo "[release] usage: release.sh <patch|minor|major|X.Y.Z> [--dry-run] [--activate] [--no-preflight]" >&2
  exit 2
fi

info() { printf '[release] %s\n' "$1" >&2; }
fail() { printf '[release] ERROR: %s\n' "$1" >&2; exit 1; }

# run: execute a mutating command, or just print it under --dry-run.
run() {
  if [ "$DRY_RUN" = "1" ]; then
    printf '[dry-run] %s\n' "$*" >&2
  else
    "$@"
  fi
}

# -- 1. guards ---------------------------------------------------------------
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
[ "$BRANCH" = "main" ] || fail "must be on main to release (on '$BRANCH')"

[ -z "$(git status --porcelain)" ] || fail "working tree is dirty -- commit or stash first"

git fetch origin main --quiet || fail "git fetch failed"
LOCAL="$(git rev-parse @)"
REMOTE="$(git rev-parse '@{u}' 2>/dev/null || echo "")"
[ -n "$REMOTE" ] || fail "no upstream for main"
[ "$LOCAL" = "$REMOTE" ] || fail "local main is not in sync with origin/main -- pull/push first"

# -- 2. compute the new version ----------------------------------------------
# `|| true` so a missing version line surfaces as the friendly guard below
# rather than a bare pipefail abort under `set -e`.
CURRENT="$(grep -m1 '^version' "$CARGO_TOML" | sed 's/version = "\(.*\)"/\1/' || true)"
[ -n "$CURRENT" ] || fail "could not read version from Cargo.toml"

case "$BUMP" in
  patch|minor|major)
    IFS='.' read -r MAJ MIN PAT <<EOF
$CURRENT
EOF
    case "$BUMP" in
      patch) PAT=$((PAT + 1)) ;;
      minor) MIN=$((MIN + 1)); PAT=0 ;;
      major) MAJ=$((MAJ + 1)); MIN=0; PAT=0 ;;
    esac
    NEW="${MAJ}.${MIN}.${PAT}"
    ;;
  *)
    NEW="$BUMP"
    ;;
esac

# Strict X.Y.Z -- the explicit-version arg glob is loose (accepts 1.2.3-beta,
# 1.2.3.4), and a typo here becomes a pushed tag. Reject anything non-semver.
[[ "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "invalid version '$NEW' (expected X.Y.Z)"

# Refuse a no-op or a downgrade (sort -V puts the lower version first).
[ "$NEW" != "$CURRENT" ] || fail "new version equals current ($CURRENT)"
LOWER="$(printf '%s\n%s\n' "$CURRENT" "$NEW" | sort -V | head -1)"
[ "$LOWER" = "$CURRENT" ] || fail "refusing to downgrade $CURRENT -> $NEW"

info "releasing ${CURRENT} -> ${NEW}"

# -- 3. require the CHANGELOG entry (human-authored, never generated) ---------
TOP_HEADER="$(grep -E '^## [0-9]' "$CHANGELOG" | head -1 | sed -E 's/^## //' | awk '{print $1}')"
if [ "$TOP_HEADER" != "$NEW" ]; then
  fail "plugin/CHANGELOG.md top header is '## ${TOP_HEADER:-<none>}', expected '## ${NEW}'.
       Add a '## ${NEW}' section at the top describing what shipped, then re-run."
fi
info "CHANGELOG header matches ${NEW}"

# -- 4. preflight ------------------------------------------------------------
if [ "$RUN_PREFLIGHT" = "1" ]; then
  info "running preflight (fmt, clippy, test, scip)"
  run bash "${REPO_ROOT}/scripts/preflight.sh"
else
  info "preflight skipped (--no-preflight)"
fi

# -- 5. bump Cargo.toml + refresh Cargo.lock ---------------------------------
if [ "$DRY_RUN" = "1" ]; then
  printf '[dry-run] set Cargo.toml version %s -> %s\n' "$CURRENT" "$NEW" >&2
else
  # Match only the package version line (first `version = ` under [package]).
  sed -i.bak "0,/^version = \"${CURRENT}\"/s//version = \"${NEW}\"/" "$CARGO_TOML"
  rm -f "${CARGO_TOML}.bak"
fi
run cargo build --quiet   # refreshes Cargo.lock's legion entry to ${NEW}

# -- 6. sync manifests + commit ----------------------------------------------
# Run sync-version explicitly so the manifests are correct even if the
# pre-commit hook is not installed in this clone (the #656 failure mode).
run bash "${REPO_ROOT}/scripts/sync-version.sh"
run git add Cargo.toml Cargo.lock plugin/CHANGELOG.md \
  plugin/.claude-plugin/plugin.json .claude-plugin/marketplace.json
run git commit -m "chore(release): ${NEW}

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

# -- 7. tag + push -----------------------------------------------------------
run git tag -a "v${NEW}" -m "legion ${NEW}"
run git push origin main
run git push origin "v${NEW}"
info "pushed v${NEW} -- release.yml will build + publish the platform binaries"

# -- 8. optional local activation --------------------------------------------
if [ "$ACTIVATE" = "1" ]; then
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
    # Restart the daemon so it execs the new binary.
    if OLDPID="$(pgrep -f 'legion daemon --port 3131' | head -1)" && [ -n "$OLDPID" ]; then
      kill "$OLDPID" || true
      for _ in 1 2 3 4 5; do
        pgrep -f 'legion daemon --port 3131' >/dev/null 2>&1 || break
        sleep 1
      done
    fi
    nohup "$DATA_BIN" daemon --port 3131 >/tmp/legion-daemon.log 2>&1 &
    disown || true
    sleep 2
    info "daemon restarted: $(curl -s http://127.0.0.1:3131/health 2>/dev/null | head -c 120)"
  fi
fi

info "done: ${NEW}"
