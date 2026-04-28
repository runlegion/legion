#!/bin/bash
# Legion PreToolUse Write/Edit hook: block agents from writing to the
# Claude Code auto-memory directory (~/.claude/projects/*/memory/).
#
# Legion is the memory layer. Reflections live in the legion DB so they are
# searchable across sessions, repos, and agents via `legion recall` and
# `legion consult`. The auto-memory directory is invisible to other agents
# and other repos, so anything written there is dead weight the moment the
# session ends.
#
# The Claude Code system prompt actively encourages writing to that path
# every turn, so a passive memory entry telling the agent "use legion
# instead" loses to an active prompt telling it to write the file. This
# hook is the enforcement layer.
#
# What this hook blocks:
#   - Write/Edit/MultiEdit on any file_path containing
#     `.claude/projects/` and `/memory/`
#
# What this hook does NOT block:
#   - Reading those files (the auto-memory system reads MEMORY.md on its own)
#   - Writes elsewhere
#
# Bypass: none. If you genuinely need a file there, do it from your own
# terminal outside Claude Code.

INPUT=$(cat)

FILE_PATH=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty' 2>/dev/null)

if [ -z "$FILE_PATH" ]; then
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

if echo "$FILE_PATH" | grep -qE '\.claude/projects/.*/memory/'; then
  jq -n --arg reason "Blocked: this file path is in the Claude Code auto-memory directory. Legion is the memory layer for this project -- use \`legion reflect --repo <name> --text '...'\` instead. Reflections stored via legion are searchable across sessions, repos, and agents via \`legion recall\` and \`legion consult\`; files in ~/.claude/projects/*/memory/ are invisible outside this single session/agent. If the content is project-wide guidance (not a personal reflection), it belongs in CLAUDE.md, not auto-memory." '
    {
      "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": $reason
      }
    }
  '
  exit 0
fi

exit 0
