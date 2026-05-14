#!/bin/bash
# Legion PreToolUse Bash hook: guard against agents replacing their identity
# reflection without deliberate intent.
#
# Agents have been treating `legion reflect --whoami` like a CLAUDE.md edit
# -- stuffing architecture rules, file paths, and build commands into the
# identity domain. That bloats the SessionStart banner past its 2K budget
# and crowds out real identity (role, voice, doctrine).
#
# Two failure modes the hook fights:
# 1. Rewriting the whole banner as one blob instead of chaining beats via
#    --follows <prev-id> -- which preserves the history and lets the banner
#    show the latest beat while older context stays recall-findable.
# 2. Storing project knowledge (rules, paths, commands) as identity. That
#    content belongs as a regular reflection -- recall + consult will
#    surface it when relevant.
#
# When the agent runs `legion reflect --whoami` (or `--domain identity`)
# AND an identity already exists for the repo AND the command does NOT
# carry --force or --follows, the hook blocks with the current identity
# inline so the agent re-reads who they are before replacing it.
#
# Bypass paths (the only legitimate continuations):
# - --follows <prev-id>: chain a new identity beat onto the existing one.
# - --force: explicit acknowledgment that this is a full rewrite.

set -u

INPUT=$(cat)

if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

COMMAND=$(echo "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)
if [ -z "$COMMAND" ]; then
  exit 0
fi

# Trigger only on legion reflect calls that set the identity domain. Match
# the basename pattern (catches /opt/homebrew/bin/legion too) so the same
# bypass that hit no-gh cannot slip past here.
TRIMMED="${COMMAND#"${COMMAND%%[![:space:]]*}"}"
FIRST_TOKEN="${TRIMMED%%[[:space:]]*}"
FIRST_BIN="${FIRST_TOKEN##*/}"
if [ "$FIRST_BIN" != "legion" ]; then
  exit 0
fi

case "$COMMAND" in
  *"reflect"*"--whoami"*) ;;
  *"reflect"*"--domain"*"identity"*) ;;
  *) exit 0 ;;
esac

# Bypass: explicit --force or --follows is the right shape -- agent
# already signaled intent.
case "$COMMAND" in
  *"--force"*|*"--follows"*) exit 0 ;;
esac

# Extract --repo argument so we can ask the right repo's identity.
REPO=$(echo "$COMMAND" | sed -nE 's/.*--repo[= ]+([A-Za-z0-9_,.\/-]+).*/\1/p' | head -n1)
if [ -z "$REPO" ]; then
  exit 0
fi
# Compound repo (a,b,c) -- check the first one for an existing identity.
REPO="${REPO%%,*}"

LEGION_BIN="${LEGION_BIN:-legion}"
if ! command -v "$LEGION_BIN" >/dev/null 2>&1; then
  exit 0
fi

EXISTING=$("$LEGION_BIN" whoami --repo "$REPO" 2>/dev/null)
if [ -z "$EXISTING" ]; then
  exit 0
fi
# whoami output always carries the header. Count the body lines; if the
# only content is the header, no identity exists yet and the rewrite is
# the first-time setup -- allow it.
BODY_LINES=$(echo "$EXISTING" | grep -cvE '^=== WHO YOU ARE|^\[Legion\] Identity|^$')
if [ "$BODY_LINES" -lt 1 ]; then
  exit 0
fi

REASON=$(printf '%s\n%s\n\n%s\n\n%s\n\n%s\n' \
  "You're about to rewrite your identity reflection. Read who you are first:" \
  "" \
  "--- Current identity (would be replaced) ---" \
  "$EXISTING" \
  "--- End ---
Have you grown out of this?

- If yes -- you're genuinely evolving your role, voice, or doctrine -- pick the right shape:
  - Preferred: chain a new beat with --follows <prev-id>. The banner shows the latest beat; older history stays recall-findable. Get the prev-id with: legion recall --repo $REPO --domain identity --limit 1
  - Full rewrite: re-run with --force. Old identity becomes the previous link in the chain, banner replaced wholesale.
- If your text is project knowledge (architecture rules, file paths, build commands, naming conventions): drop --whoami. Store as a regular reflection:
    legion reflect --repo $REPO --text \"...\"
  Recall + consult will surface it when relevant. The 2K SessionStart banner only fits role/voice/doctrine; project rules crowd it out.")

jq -n --arg reason "$REASON" '{
  "decision": "block",
  "reason": $reason
}'
