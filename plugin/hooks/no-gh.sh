#!/bin/bash
# Block direct gh usage -- agents should use legion issue/pr/comment instead.
# All work source actions go through legion for audit logging and workflow tracking.
INPUT=$(cat)

COMMAND=$(echo "$INPUT" | jq -r '.tool_input.command // empty')
if [ -z "$COMMAND" ]; then
  exit 0
fi

# Skip enforcement in repos legion does not cover (#353).
CWD=$(echo "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)
REPO="${LEGION_REPO:-$(basename "${CWD:-$PWD}")}"
if [ -f "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-covered.sh" ]; then
  # shellcheck source=_legion-covered.sh
  source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-covered.sh"
  if ! legion_covered "$SESSION_ID" "$REPO"; then
    exit 0
  fi
fi

# Check if the command invokes gh -- including by absolute path. Naive
# prefix matching on `gh ` leaves /opt/homebrew/bin/gh, /usr/bin/gh,
# ~/bin/gh as silent escape hatches. Take the basename of the first
# whitespace-separated token, then compare to `gh`.
TRIMMED="${COMMAND#"${COMMAND%%[![:space:]]*}"}"
FIRST_TOKEN="${TRIMMED%%[[:space:]]*}"
FIRST_BIN="${FIRST_TOKEN##*/}"

if [ "$FIRST_BIN" = "gh" ]; then
  jq -n --arg reason "Do not use gh directly. Use legion commands instead:
- legion issue create --repo <name> --title '...' --body '...'
- legion pr create --repo <name> --title '...'
- legion pr list --repo <name>
- legion pr review --repo <name> --number <n> --approve
- legion pr merge --repo <name> --number <n>
- legion comment --repo <name> --number <n> --body '...'
- legion audit

These commands go through the work source plugin and are tracked in the audit log. Absolute-path invocations (e.g. /opt/homebrew/bin/gh) are also blocked -- legion sees through the path." '{
    "decision": "block",
    "reason": $reason
  }'
fi
