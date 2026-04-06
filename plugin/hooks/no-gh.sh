#!/bin/bash
# Block direct gh usage -- agents should use legion issue/pr/comment instead.
# All work source actions go through legion for audit logging and workflow tracking.
INPUT=$(cat)

COMMAND=$(echo "$INPUT" | jq -r '.tool_input.command // empty')
if [ -z "$COMMAND" ]; then
  exit 0
fi

# Check if the command starts with gh (ignoring leading whitespace)
TRIMMED="${COMMAND#"${COMMAND%%[![:space:]]*}"}"
case "$TRIMMED" in
  gh\ *|gh)
    jq -n --arg reason "Do not use gh directly. Use legion commands instead:
- legion issue create --repo <name> --title '...' --body '...'
- legion pr create --repo <name> --title '...'
- legion pr list --repo <name>
- legion pr review --repo <name> --number <n> --approve
- legion pr merge --repo <name> --number <n>
- legion comment --repo <name> --number <n> --body '...'
- legion audit

These commands go through the work source plugin and are tracked in the audit log." '{
      "decision": "block",
      "reason": $reason
    }'
    ;;
esac
