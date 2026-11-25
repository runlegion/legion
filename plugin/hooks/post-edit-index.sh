#!/bin/bash
# Legion PostToolUse hook (#281): re-index the SCIP blob for the repo
# containing the edited file so `legion sym` answers stay fresh.
#
# Fires on Edit, Write, MultiEdit. Reads tool_input.file_path, debounces
# 500ms per path via a lockfile, and runs `legion index --file <path>` in
# the background so the editing turn is never blocked.
#
# Skip conditions (any one is enough):
#   - file_path missing or empty
#   - repo not legion-covered (no watch.toml entry, no reflections)
#   - lockfile fresher than 500ms (debounce)
#
# Errors append to /tmp/legion-hook-errors.log; the hook always exits 0.

set -u

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0

LOG="$LEGION_HOOK_LOG"

legion_hook_parse || exit 0
FILE_PATH=$(legion_hook_field '.tool_input.file_path')

if [ -z "$FILE_PATH" ] || [ -z "$CWD" ]; then
  exit 0
fi

legion_hook_covered || exit 0

# Debounce per file path: skip if the lockfile mtime is newer than 500ms ago.
# GNU date supports %3N (millisecond precision); BSD date emits the literal
# "%3N" instead. Detect by checking whether the output contains a non-digit.
PATH_HASH=$(legion_hash_str "$FILE_PATH")
LOCK="/tmp/legion-index-${PATH_HASH}.lock"
NOW_MS=$(date +%s%3N 2>/dev/null || true)
case "$NOW_MS" in
  ''|*[!0-9]*)
    NOW_MS=$(python3 -c 'import time; print(int(time.time()*1000))' 2>/dev/null || echo "")
    ;;
esac
if [ -n "$NOW_MS" ] && [ -f "$LOCK" ]; then
  LOCK_MS=$(stat -f %m000 "$LOCK" 2>/dev/null || stat -c %Y000 "$LOCK" 2>/dev/null || echo 0)
  AGE_MS=$((NOW_MS - LOCK_MS))
  if [ "$AGE_MS" -lt 500 ]; then
    exit 0
  fi
fi
touch "$LOCK" 2>/dev/null

# Background re-index. nohup + & detaches so the editing turn is not blocked.
# Stderr appended to log so a missing indexer leaves a breadcrumb without
# interrupting the user.
nohup "$LEGION" index --file "$FILE_PATH" >>"$LOG" 2>&1 &
disown 2>/dev/null

exit 0
