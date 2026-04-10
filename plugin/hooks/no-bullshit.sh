#!/bin/bash
# Legion Stop hook: block responses that stop mid-task to "check in" on
# obvious continuation work.
#
# The pattern this catches:
#   - Agent is asked to do N things
#   - Agent does k of N (k < N)
#   - Agent stops and asks permission to do the remaining N-k
#   - User has to re-engage to approve work they already approved
#
# The block reason is injected as additionalContext so the agent re-reads it
# as "finish the job" and keeps going. Only fires when phrase patterns clearly
# indicate a mid-task check-in. Legitimate blockers (destructive action gates,
# undecidable ambiguity, external dependencies) phrase themselves differently
# and are not caught.
#
# To bypass intentionally: finish without any of the denied phrases. The hook
# does not read intent, it reads wording.

set -u

INPUT=$(cat)
TRANSCRIPT=$(echo "$INPUT" | jq -r '.transcript_path // empty' 2>/dev/null)

if [ -z "$TRANSCRIPT" ] || [ ! -f "$TRANSCRIPT" ]; then
  exit 0
fi

# Extract the most recent assistant text message from the transcript.
# Transcript is JSONL, one turn per line. Assistant messages have type="assistant"
# and message.content as an array of blocks. We concatenate text blocks only.
#
# Portable reverse: prefer `tail -r` (BSD/macOS), fall back to `tac` (GNU/Linux).
REVERSE_CMD=""
if command -v tail >/dev/null 2>&1 && tail -r /dev/null >/dev/null 2>&1; then
  REVERSE_CMD="tail -r"
elif command -v tac >/dev/null 2>&1; then
  REVERSE_CMD="tac"
else
  exit 0
fi

LAST_ASSISTANT=$($REVERSE_CMD "$TRANSCRIPT" 2>/dev/null | while IFS= read -r line; do
  TYPE=$(echo "$line" | jq -r '.type // empty' 2>/dev/null)
  if [ "$TYPE" = "assistant" ]; then
    echo "$line" | jq -r '
      .message.content
      | if type == "array"
        then map(select(.type == "text") | .text) | join("\n")
        else .
        end
      // empty
    ' 2>/dev/null
    break
  fi
done)

if [ -z "$LAST_ASSISTANT" ]; then
  exit 0
fi

# Denylist: phrases that signal "stopping mid-task to get approval for more
# of the same work." Case-insensitive. Add patterns as they surface.
PATTERNS=(
  'want me to (continue|keep going|write the rest|do the rest|build the rest|finish)'
  'should i (continue|keep going|write the rest|do the rest)'
  'pause here'
  'pausing here'
  'or we pause'
  'or pause here'
  'or (do you want|should i) (pause|stop)'
  'let me know (if you want|when you want|whether) (me|to)'
  'standing by'
  'ready for the next'
  'tell me (to proceed|when to|if you want)'
  'three down'
  '[0-9]+ down,? [0-9]+ to go'
  'half the (job|work) done'
  '(before|until) i (write|build|continue|proceed)'
  'awaiting (your )?(approval|direction|signal|greenlight)'
  'want me to (draft|write|author) (all|the rest|the other|the remaining)'
  'the pattern is set'
)

HIT=""
for pat in "${PATTERNS[@]}"; do
  if echo "$LAST_ASSISTANT" | grep -iqE "$pat"; then
    HIT="$pat"
    break
  fi
done

if [ -z "$HIT" ]; then
  exit 0
fi

# Emit a block decision so the Stop is refused and the agent keeps working.
# The reason is what the agent sees next, so it is phrased as a direct instruction.
jq -n --arg pat "$HIT" '
  {
    "decision": "block",
    "reason": (
      "You stopped mid-task to check in on obvious continuation work. " +
      "Finish the job you were asked to do. If the user asked for N items, " +
      "ship all N. Stop only for genuine blockers: destructive actions that " +
      "need explicit authorization, undecidable ambiguity (not aesthetic " +
      "preference), or an external dependency you cannot satisfy. Do NOT " +
      "stop to confirm work that was already authorized, to report progress, " +
      "or to ask permission to continue the pattern you already established. " +
      "Detected phrase pattern: " + $pat + ". Continue the task without " +
      "re-asking."
    )
  }
'
