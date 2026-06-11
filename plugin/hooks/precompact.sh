#!/bin/bash
# Legion PreCompact hook: block auto-compaction and surface a message to the
# user telling them to run /checkpoint then /clear. Per Claude Code's hook spec,
# PreCompact's `reason` is shown to the USER, not the model -- so the text
# below addresses the human operator, who decides what to do next.
# See: https://code.claude.com/docs/en/hooks.md (Decision control table)
# A safety-net checkpoint reflection is still written below in case the user
# overrides the block; losing the checkpoint silently caused #209.

# Hook subshells do not inherit the plugin bin dir on PATH -- only the Bash
# tool does. Invoke via full CLAUDE_PLUGIN_ROOT path (fixes #204).
LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')
TRANSCRIPT=$(echo "$INPUT" | jq -r '.transcript_path // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")

# Extract recent assistant text from transcript JSONL as a safety-net checkpoint
CONTEXT=""
if [ -n "$TRANSCRIPT" ] && [ -f "$TRANSCRIPT" ]; then
  # Grab last 200 lines, pull assistant text content, keep last ~2000 chars
  CONTEXT=$(tail -200 "$TRANSCRIPT" 2>/dev/null | \
    jq -r 'select(.type == "assistant") | .message.content[]? | select(.type == "text") | .text // empty' 2>/dev/null | \
    tail -c 2000)
fi

# Fallback: try alternate JSONL format
if [ -z "$CONTEXT" ]; then
  CONTEXT=$(tail -200 "$TRANSCRIPT" 2>/dev/null | \
    jq -r 'select(.role == "assistant") | .content // empty' 2>/dev/null | \
    tail -c 2000)
fi

if [ -z "$CONTEXT" ]; then
  CONTEXT="(no transcript context extracted)"
fi

# Safety-net checkpoint reflection. See #209 -- losing a checkpoint silently
# was the original incident this hook was built to prevent. A failed reflect
# leaves its breadcrumb in $LOG (the operator-facing diagnostic path, #383).
"$LEGION" reflect --repo "$REPO" --text "[COMPACT CHECKPOINT] Work in progress before compaction: ${CONTEXT}" --domain "checkpoint" --tags "auto,precompact" 2>>"$LOG" || true

# Block auto-compaction. PreCompact reason text goes to the user, not the
# model, so this is a static heredoc -- no jq dependency, no failure path
# that would silently let compaction proceed.
cat <<'EOF'
{"decision":"block","reason":"Auto-compaction would lose everything not in the transcript tail. Blocked. Run /checkpoint to consolidate this session into legion memory (boost reflections that helped, write a domain=checkpoint resume-anchor the next session will recall, cross-pollinate to the bullpen), then /clear to start fresh. A safety-net checkpoint reflection has been written as a fallback."}
EOF
