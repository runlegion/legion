#!/bin/bash
# Legion SessionStart hook: inject recall hits + compact status counts.
#
# Only injects what requires a DB read: recent reflections and a one-line
# status summary. Everything else (instructions, command reference) lives
# in the plugin's own documentation surfaces.
#
# Also warms the Tantivy index in the background so the first PreToolUse
# recall-first.sh hit is fast (cold ~2.2s, warm ~170ms).
#
# Error handling: legion invocations append stderr to /tmp/legion-hook-errors.log
# instead of dropping it so a silently-broken legion (missing binary, bad
# migration, DB schema mismatch) leaves a breadcrumb. The hook still exits 0
# and degrades to "no context" so Claude Code does not block on a legion bug.

# Hook subshells do not inherit the plugin bin dir on PATH -- only the Bash
# tool does. Invoke the plugin binary by full path via CLAUDE_PLUGIN_ROOT
# (exported to hook processes per plugins-reference). Fixes #204.
LEGION="${CLAUDE_PLUGIN_ROOT}/bin/legion"
LOG=/tmp/legion-hook-errors.log

# Shared warning helper -- tracks degraded legion calls and renders a
# visible block when any legion invocation exits nonzero. See #209.
# shellcheck source=_legion-warn.sh
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-warn.sh"

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')

if [ -z "$CWD" ]; then
  exit 0
fi

REPO=$(basename "$CWD")

# Clean up per-session markers from prior session
CWD_HASH=$(echo "$CWD" | md5 -q 2>/dev/null || echo "$CWD" | md5sum 2>/dev/null | cut -d' ' -f1)
rm -f "/tmp/legion-reflected-${CWD_HASH}" 2>/dev/null

# Mark session as having done work (used by Stop hook to decide if reflect prompt fires)
touch "/tmp/legion-work-${CWD_HASH}"

# Warm the Tantivy index in the background -- makes PreToolUse recall-first.sh
# fast on the first tool call instead of paying a 2s cold-start cost
("$LEGION" recall --repo "$REPO" --context warmup --limit 1 >/dev/null 2>&1 &)

# Sync GitHub issues into kanban before session starts (5-second timeout to avoid blocking).
# Opt-out via LEGION_NO_SYNC=1 (for environments without network access or to avoid latency).
# A timeout exit is treated as "no sync this session" not a legion breakage -- don't surface it.
if [ "${LEGION_NO_SYNC:-}" != "1" ]; then
  # Use gtimeout if available (macOS via homebrew), otherwise fall back to perl idiom.
  if command -v gtimeout >/dev/null 2>&1; then
    gtimeout 5 "$LEGION" sync --repo "$REPO" >/dev/null 2>>"$LOG"
  else
    perl -e 'alarm 5; exec @ARGV' -- "$LEGION" sync --repo "$REPO" >/dev/null 2>>"$LOG"
  fi
  SYNC_RC=$?
  # 124 = gtimeout/perl SIGALRM; treat as informational, not a failure.
  if [ "$SYNC_RC" -ne 0 ] && [ "$SYNC_RC" -ne 124 ] && [ "$SYNC_RC" -ne 142 ]; then
    legion_check "$SYNC_RC" "sync"
  fi
fi

# Branch-context recall first, fallback to latest if nothing matched.
# --preview 240 keeps each hit compact so the session start stays small.
BRANCH=$(cd "$CWD" && git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
OUTPUT=""
if [ -n "$BRANCH" ] && [ "$BRANCH" != "main" ] && [ "$BRANCH" != "master" ]; then
  OUTPUT=$("$LEGION" recall --repo "$REPO" --context "$BRANCH" --limit 2 --preview 240 2>>"$LOG")
  legion_check $? "recall (branch)"
fi
if [ -z "$OUTPUT" ]; then
  OUTPUT=$("$LEGION" recall --repo "$REPO" --latest --limit 2 --preview 240 2>>"$LOG")
  legion_check $? "recall (latest)"
fi

# Compact status counts from the JSON summary instead of full status text
STATUS_JSON=$("$LEGION" status --repo "$REPO" --json 2>>"$LOG")
legion_check $? "status --json"
if [ -n "$STATUS_JSON" ]; then
  TASKS=$(echo "$STATUS_JSON" | jq -r '.tasks // 0')
  BLOCKED=$(echo "$STATUS_JSON" | jq -r '.blocked // 0')
  TEAM_NEEDS=$(echo "$STATUS_JSON" | jq -r '.team_needs // 0')
  CHANGED=$(echo "$STATUS_JSON" | jq -r '.what_changed // 0')

  PARTS=()
  if [ "$TASKS" -gt 0 ]; then
    if [ "$BLOCKED" -gt 0 ]; then
      PARTS+=("$TASKS tasks ($BLOCKED blocked)")
    else
      PARTS+=("$TASKS tasks")
    fi
  fi
  if [ "$TEAM_NEEDS" -gt 0 ]; then
    PARTS+=("$TEAM_NEEDS unread @$REPO")
  fi
  if [ "$CHANGED" -gt 0 ]; then
    PARTS+=("$CHANGED updates")
  fi

  if [ ${#PARTS[@]} -gt 0 ]; then
    STATUS_LINE="[Legion] $(IFS=', '; echo "${PARTS[*]}") -- legion bullpen for details"
    if [ -n "$OUTPUT" ]; then
      OUTPUT="${OUTPUT}"$'\n\n'"${STATUS_LINE}"
    else
      OUTPUT="$STATUS_LINE"
    fi
  fi
fi

# Prepend a visible warning block if any legion call above failed.
# The block is load-bearing for agent awareness of legion degradation -- see #209.
WARN=$(legion_warnings_block)
if [ -n "$WARN" ]; then
  if [ -n "$OUTPUT" ]; then
    OUTPUT="${WARN}"$'\n\n'"${OUTPUT}"
  else
    OUTPUT="$WARN"
  fi
fi

if [ -n "$OUTPUT" ]; then
  jq -n --arg ctx "$OUTPUT" '{
    "hookSpecificOutput": {
      "hookEventName": "SessionStart",
      "additionalContext": $ctx
    }
  }'
fi
