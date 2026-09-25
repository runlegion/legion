#!/bin/bash
# legion-cmd no-go permissions mirror (#1237, FR-CMD-025).
#
# The binary's built-in no-go entries are mirrored into the harness
# permissions.deny as a second layer: `legion cmd-check --deny-patterns`
# prints them as one JSON array, and legion_merge_deny_patterns merges that
# array into a Claude Code settings file. The layer is a backstop and is not
# complete; legion-cmd's argument match is the check.
#
# The merge only ever adds: a pattern already present is skipped, existing
# entries keep their order, and every other key is preserved. The write is
# atomic (temp file in the same directory, then rename). A settings file
# that is not a JSON object, or whose permissions/deny are not an object and
# an array, is left untouched and the reason goes to stderr. When nothing is
# missing the file is not rewritten.

# legion_resolve_symlink PATH -- print the file PATH finally names, following
# every symlink hop (relative targets resolve against the link's directory).
# Portable: no `readlink -f`. Fails after 40 hops, the usual loop limit.
legion_resolve_symlink() {
  local path="$1" link hops=0
  while [ -L "$path" ]; do
    hops=$((hops + 1))
    if [ "$hops" -gt 40 ]; then
      echo "[legion] too many symlink hops resolving $1" >&2
      return 1
    fi
    link=$(readlink "$path") || return 1
    case "$link" in
      /*) path="$link" ;;
      *) path="$(dirname "$path")/$link" ;;
    esac
  done
  printf '%s' "$path"
}

# legion_merge_deny_patterns SETTINGS_FILE PATTERNS_JSON
#   Returns 0 when the file holds every pattern afterwards (or was left
#   untouched on purpose), non-zero only when the write itself failed.
legion_merge_deny_patterns() {
  local settings patterns="$2"
  # A symlinked settings file is written through to its target: the temp
  # file and the rename happen beside the target, so the link survives.
  settings=$(legion_resolve_symlink "$1") || return 1
  if ! command -v jq >/dev/null 2>&1; then
    echo "[legion] jq not found; permissions.deny mirror skipped" >&2
    return 0
  fi
  if ! printf '%s' "$patterns" | jq -e 'type == "array" and all(type == "string")' >/dev/null 2>&1; then
    echo "[legion] deny patterns are not a JSON string array; permissions.deny mirror skipped" >&2
    return 0
  fi

  local current
  if [ -f "$settings" ]; then
    current=$(cat "$settings")
  else
    current='{}'
  fi
  if ! printf '%s' "$current" | jq -e '
      type == "object"
      and ((.permissions // {}) | type == "object")
      and ((.permissions.deny // []) | type == "array")' >/dev/null 2>&1; then
    echo "[legion] $settings is not a settings object with a permissions.deny array; left untouched" >&2
    return 0
  fi

  local missing
  missing=$(printf '%s' "$current" | jq --argjson p "$patterns" '$p - (.permissions.deny // []) | length')
  if [ "$missing" = "0" ]; then
    return 0
  fi

  local dir tmp
  dir=$(dirname "$settings")
  mkdir -p "$dir" || return 1
  tmp=$(mktemp "$dir/.settings.json.XXXXXX") || return 1
  if ! printf '%s' "$current" | jq --argjson p "$patterns" '
      .permissions = ((.permissions // {})
        | .deny = ((.deny // []) + ($p - (.deny // []))))' > "$tmp"; then
    rm -f "$tmp"
    return 1
  fi
  if [ -f "$settings" ]; then
    chmod "$(stat -f '%Lp' "$settings" 2>/dev/null || stat -c '%a' "$settings")" "$tmp" 2>/dev/null || true
  fi
  mv "$tmp" "$settings"
}
