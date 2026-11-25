#!/bin/bash
# Install the tracked .githooks/ directory as the active git hooks path.
#
# Git hooks default to .git/hooks/, which is not tracked by the repo, so new
# contributors who clone never get the hooks unless they run this script.
# Setting core.hooksPath to .githooks/ makes every hook in that directory
# active immediately, and because .githooks/ IS tracked, every commit ships
# the current hook definitions.
#
# Idempotent: re-running this script against an already-configured repo is
# safe and prints what is already in place. Run it once per fresh clone.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"

if [ ! -d "${REPO_ROOT}/.githooks" ]; then
  echo "[install-hooks] ERROR: .githooks directory missing at ${REPO_ROOT}/.githooks" >&2
  exit 1
fi

# Make every hook in .githooks/ executable. Idempotent -- git tracks the
# executable bit, but contributors who checked out on a filesystem that does
# not honor it (certain Windows + WSL setups) may arrive here without it set.
find "${REPO_ROOT}/.githooks" -type f ! -name '.*' -exec chmod +x {} \;

# Point git at the tracked hooks directory. core.hooksPath is a per-repo
# setting that overrides the default .git/hooks location.
git -C "${REPO_ROOT}" config core.hooksPath .githooks

echo "[install-hooks] core.hooksPath -> .githooks"
echo "[install-hooks] hooks installed:"
find "${REPO_ROOT}/.githooks" -maxdepth 1 -type f ! -name '.*' -exec basename {} \; | sort | sed 's/^/  - /'
