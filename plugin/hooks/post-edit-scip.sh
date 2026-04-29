#!/bin/bash
# Legion PostToolUse hook for Edit/Write: trigger an incremental
# `legion index --file <path>` re-index in the background so agents
# always query a fresh SCIP index without paying full-repo cost.
#
# Contract:
# - Reads hook JSON from stdin.
# - Never blocks the originating tool call (background subprocess).
# - Exits 0 in every failure mode; warnings to stderr only.
# - Honors LEGION_SKIP_POST_EDIT_SCIP=1 to bypass entirely.
# - Honors the legion-covered gate (#353): silent no-op in repos
#   legion does not manage.
# - Debounces per-file: a marker under XDG_CACHE_HOME/legion/scip-debounce/
#   suppresses spawns within LEGION_SCIP_DEBOUNCE_MS (default 500ms).

LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"

# Explicit skip override.
if [ "${LEGION_SKIP_POST_EDIT_SCIP:-}" = "1" ]; then
  exit 0
fi

INPUT=$(cat)
if [ -z "$INPUT" ]; then
  echo "[post-edit-scip] empty stdin; skipping" >&2
  exit 0
fi

# Tool detail extraction. Edit and Write both supply tool_input.file_path.
FILE_PATH=$(printf '%s' "$INPUT" | jq -r '.tool_input.file_path // empty' 2>/dev/null)
if [ -z "$FILE_PATH" ]; then
  echo "[post-edit-scip] missing file_path; skipping" >&2
  exit 0
fi

CWD=$(printf '%s' "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
SESSION_ID=$(printf '%s' "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)
REPO="${LEGION_REPO:-$(basename "${CWD:-$PWD}")}"

# Coverage gate (#353): no-op silently in uncovered repos.
if [ -f "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-covered.sh" ]; then
  # shellcheck source=/dev/null
  source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-covered.sh"
  if ! legion_covered "$SESSION_ID" "$REPO"; then
    exit 0
  fi
fi

# Cheap extension filter -- only Rust/TS/Python/etc. paths matter.
# Anything else (markdown, json, shell) wastes a fork+exec on legion.
case "$FILE_PATH" in
  *.rs|*.ts|*.tsx|*.py|*.go|*.js|*.jsx) ;;
  *)
    exit 0
    ;;
esac

if [ ! -x "$LEGION" ]; then
  echo "[post-edit-scip] legion binary not found at ${LEGION}; skipping" >&2
  exit 0
fi

# Per-path debounce. The marker name is a hash of the absolute path so
# pathological filenames (spaces, slashes after canonicalization) do
# not break the filesystem. Tag the marker with mtime; we compare against
# LEGION_SCIP_DEBOUNCE_MS (default 500).
DEBOUNCE_MS="${LEGION_SCIP_DEBOUNCE_MS:-500}"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/legion/scip-debounce"
if ! mkdir -p "$CACHE_DIR" 2>/dev/null; then
  echo "[post-edit-scip] cache dir unavailable; skipping debounce" >&2
fi

# Path hash via shasum (BSD/GNU) or openssl as fallback. Either gives
# 16+ hex chars; the leading 12 are plenty unique for this debounce.
hash_path() {
  if command -v shasum >/dev/null 2>&1; then
    printf '%s' "$1" | shasum -a 256 | cut -c1-12
  elif command -v openssl >/dev/null 2>&1; then
    printf '%s' "$1" | openssl dgst -sha256 | awk '{print substr($NF,1,12)}'
  else
    # Last resort: tr-strip non-alnum so the filename is safe.
    printf '%s' "$1" | tr -c 'a-zA-Z0-9' '_' | head -c 64
  fi
}

PATH_KEY=$(hash_path "$FILE_PATH")
MARKER="${CACHE_DIR}/${PATH_KEY}"

if [ -f "$MARKER" ]; then
  # mtime in seconds since epoch; convert debounce_ms to seconds.
  if command -v stat >/dev/null 2>&1; then
    NOW=$(date +%s)
    # macOS stat -f %m, GNU stat -c %Y.
    MTIME=$(stat -f %m "$MARKER" 2>/dev/null || stat -c %Y "$MARKER" 2>/dev/null || echo "$NOW")
    AGE_S=$((NOW - MTIME))
    DEBOUNCE_S=$((DEBOUNCE_MS / 1000))
    if [ "$DEBOUNCE_S" -lt 1 ]; then
      DEBOUNCE_S=1
    fi
    if [ "$AGE_S" -lt "$DEBOUNCE_S" ]; then
      exit 0
    fi
  fi
fi

touch "$MARKER" 2>/dev/null

# Spawn the indexer in the background, fully detached, with stdout and
# stderr pointed at the legion error log so a failure leaves a
# breadcrumb without contaminating the originating tool's output.
# Double-fork via subshell + & ensures the child reparents to init
# rather than holding the originating tool's process tree open.
LOG=/tmp/legion-hook-errors.log
("$LEGION" index --file "$FILE_PATH" >>"$LOG" 2>&1 </dev/null) &
disown 2>/dev/null || true

exit 0
