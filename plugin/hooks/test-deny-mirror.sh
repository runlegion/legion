#!/bin/bash
# The JSON literals in this file hold $HOME as text, not an expansion.
# shellcheck disable=SC2016
# Test runner for lib/deny-mirror.sh (#1237): the merge plugin setup runs to
# mirror the built-in no-go entries into the harness permissions.deny.
#
# Covers: existing entries and every other key kept, patterns added once and
# appended after the operator's own, a second run is a no-op, a settings file
# that is not valid JSON (or has a non-array deny) is left untouched, and an
# absent settings file is created.
#
# Run from anywhere:
#   bash plugin/hooks/test-deny-mirror.sh

set -u

# shellcheck source=tests/testutil.sh
source "$(dirname "${BASH_SOURCE[0]}")/tests/testutil.sh"
# shellcheck source=lib/deny-mirror.sh
source "$HOOKS_SRC_DIR/lib/deny-mirror.sh"

WORK=$(mktemp -d)
# shellcheck disable=SC2064
trap "rm -rf '$WORK'" EXIT

inode() { stat -f %i "$1" 2>/dev/null || stat -c %i "$1"; }

PATTERNS='["Bash(rm -rf /)","Bash(rm -rf \"$HOME\")","Bash(mkfs *)"]'

echo "==> existing entries and other keys are kept; patterns are appended once"
SETTINGS="$WORK/settings.json"
cat > "$SETTINGS" <<'JSON'
{
  "model": "opus",
  "permissions": {
    "allow": ["Bash(ls *)"],
    "deny": ["Bash(grep *)", "Bash(mkfs *)"]
  },
  "hooks": {"Stop": []}
}
JSON
legion_merge_deny_patterns "$SETTINGS" "$PATTERNS" 2>/dev/null
assert_rc "merge succeeds" 0 "$?"
assert_eq "deny keeps the operator's entries first, adds only the missing" \
  "$(jq -c '.permissions.deny' "$SETTINGS")" \
  '["Bash(grep *)","Bash(mkfs *)","Bash(rm -rf /)","Bash(rm -rf \"$HOME\")"]'
assert_eq "other keys preserved" "$(jq -c '{model, allow: .permissions.allow, hooks}' "$SETTINGS")" \
  '{"model":"opus","allow":["Bash(ls *)"],"hooks":{"Stop":[]}}'
assert_eq "key order preserved" "$(jq -c 'keys_unsorted' "$SETTINGS")" '["model","permissions","hooks"]'

echo "==> a second run is a no-op"
before=$(cat "$SETTINGS")
before_inode=$(inode "$SETTINGS")
legion_merge_deny_patterns "$SETTINGS" "$PATTERNS" 2>/dev/null
assert_eq "content unchanged" "$(cat "$SETTINGS")" "$before"
assert_eq "file not rewritten" "$(inode "$SETTINGS")" "$before_inode"

echo "==> invalid JSON is left untouched"
BAD="$WORK/bad.json"
printf '{ "permissions": { "deny": [ ' > "$BAD"
bad_before=$(cat "$BAD")
err=$(legion_merge_deny_patterns "$BAD" "$PATTERNS" 2>&1 >/dev/null)
assert_eq "invalid file unchanged" "$(cat "$BAD")" "$bad_before"
assert_contains "the reason reaches stderr" "$err" "left untouched"

echo "==> a deny that is not an array is left untouched"
ODD="$WORK/odd.json"
printf '{"permissions":{"deny":"Bash(rm *)"}}' > "$ODD"
odd_before=$(cat "$ODD")
legion_merge_deny_patterns "$ODD" "$PATTERNS" 2>/dev/null
assert_eq "non-array deny unchanged" "$(cat "$ODD")" "$odd_before"

echo "==> an absent settings file is created with the patterns"
NEW="$WORK/sub/settings.json"
legion_merge_deny_patterns "$NEW" "$PATTERNS" 2>/dev/null
assert_eq "created with every pattern" "$(jq -c '.permissions.deny' "$NEW")" \
  '["Bash(rm -rf /)","Bash(rm -rf \"$HOME\")","Bash(mkfs *)"]'

echo "==> no temp file is left behind"
assert_eq "only the settings files remain" \
  "$(find "$WORK" -name '.settings.json.*' | wc -l | tr -d ' ')" "0"

finish_tests
