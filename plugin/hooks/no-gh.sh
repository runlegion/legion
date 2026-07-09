#!/bin/bash
# Block direct gh usage -- agents should use legion issue/pr/comment instead.
# All work source actions go through legion for audit logging and workflow tracking.

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

COMMAND=$(legion_hook_field '.tool_input.command')
if [ -z "$COMMAND" ]; then
  exit 0
fi

# Skip enforcement in repos legion does not cover (#353).
legion_hook_covered || exit 0

# Check if the command invokes gh -- including by absolute path. Naive
# prefix matching on `gh ` leaves /opt/homebrew/bin/gh, /usr/bin/gh,
# ~/bin/gh as silent escape hatches. Take the basename of the first
# whitespace-separated token, then compare to `gh`.
TRIMMED="${COMMAND#"${COMMAND%%[![:space:]]*}"}"
FIRST_TOKEN="${TRIMMED%%[[:space:]]*}"
FIRST_BIN="${FIRST_TOKEN##*/}"

if [ "$FIRST_BIN" = "gh" ]; then
  emit_deny "Do not use gh directly. Use legion commands instead:
- legion issue create --repo <name> --title '...' --body '...'
- legion issue list --repo <name>
- legion pr create --repo <name> --title '...'
- legion pr list --repo <name>
- legion pr review --repo <name> --number <n> --approve
- legion pr merge --repo <name> --number <n>
- legion comment --repo <name> --number <n> --body '...'
- legion audit

These commands go through the work source plugin and are tracked in the audit log. Absolute-path invocations (e.g. /opt/homebrew/bin/gh) are also blocked -- legion sees through the path."
fi
