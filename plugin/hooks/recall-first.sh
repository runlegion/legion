#!/bin/bash
# Legion PreToolUse hook: nudge agent to check legion before searching
# Fires on every Grep, Glob, WebFetch, WebSearch call.
INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')
if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")

jq -n --arg ctx "[Legion] Before searching code, check legion memory first. Run: legion recall --repo ${REPO} --context '<what you are looking for>' or legion consult --context '<problem>' to search all agents. Code shows WHAT exists -- legion tells you WHY." '{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "legion recall-first nudge",
    "additionalContext": $ctx
  }
}'
