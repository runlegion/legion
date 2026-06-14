#!/usr/bin/env bash
# preflight.sh: the quality bar that must pass before a release (or any push
# the developer wants gated). Reusable unit shared by scripts/release.sh.
#
#   1. cargo fmt -- --check          (formatting is committed-clean)
#   2. cargo clippy --all-targets -D warnings  (CI parity -- bare clippy misses
#                                               test/example/bench code)
#   3. cargo test                    (the suite is green)
#   4. legion index <repo>           (SCIP index regenerated so sym queries
#                                     answer against current code)
#
# Exits non-zero on the FIRST failing gate, naming it. Safe to run standalone:
#
#   scripts/preflight.sh             # full bar against the legion repo
#   SKIP_SCIP=1 scripts/preflight.sh # skip the index regen (faster local loop)
#
# The SCIP step is best-effort by default: a missing `legion` binary or indexer
# is a warning, not a hard failure, so the script still gates code quality on a
# machine without the plugin installed. Set REQUIRE_SCIP=1 to make it mandatory.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Repo name as known to watch.toml (for the SCIP index step). Override with
# LEGION_REPO=<name> if this clone is registered under a different name.
LEGION_REPO="${LEGION_REPO:-legion}"

step() { printf '\n[preflight] === %s ===\n' "$1" >&2; }
fail() { printf '\n[preflight] FAILED at: %s\n' "$1" >&2; exit 1; }

step "cargo fmt -- --check"
cargo fmt -- --check || fail "formatting (run: cargo fmt)"

step "cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings || fail "clippy"

step "cargo test"
cargo test || fail "tests"

step "shell-script tests"
for t in test-release.sh test-sync-version.sh; do
  if [ -f "${REPO_ROOT}/scripts/${t}" ]; then
    bash "${REPO_ROOT}/scripts/${t}" || fail "scripts/${t}"
  fi
done

if [ "${SKIP_SCIP:-0}" = "1" ]; then
  printf '\n[preflight] SCIP regen skipped (SKIP_SCIP=1)\n' >&2
else
  step "legion index ${LEGION_REPO} (SCIP regen)"
  if command -v legion >/dev/null 2>&1; then
    if ! legion index "$LEGION_REPO"; then
      if [ "${REQUIRE_SCIP:-0}" = "1" ]; then
        fail "SCIP index regen"
      fi
      printf '[preflight] WARNING: SCIP regen failed (non-fatal; set REQUIRE_SCIP=1 to enforce)\n' >&2
    fi
  elif [ "${REQUIRE_SCIP:-0}" = "1" ]; then
    fail "legion binary not found (REQUIRE_SCIP=1)"
  else
    printf '[preflight] legion binary not found -- skipping SCIP regen (non-fatal)\n' >&2
  fi
fi

printf '\n[preflight] all gates passed\n' >&2
