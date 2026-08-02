#!/bin/bash
# Legion PreToolUse hook (#837): when an agent reaches for a SCRIPT to do
# what sym already answers, point at the sym command -- at the moment it
# reaches, not after.
#
# Observed 2026-07-31: the rafters agent wrote a Python script to get a
# directory tree. `legion sym tree --repo rafters` answers that directly,
# rafters is legion-covered, and its index had been rebuilt 42 seconds
# earlier. The capability was live and adjacent; nothing connected the
# agent to it.
#
# Why no existing hook catches this: every other enforcement point
# classifies by the command's LEADING BINARY (grep|rg|ag|ack|find|fd, plus
# the git shapes from #829). `python tree.py` leads with `python`, and no
# first-token rule can ever catch it -- you cannot tell what a script does
# from its invocation. Writing the script only meets no-local-memory.sh.
#
# So this hook watches two moments instead of one:
#
#   Write/Edit -- the content being written carries a search primitive.
#                 This is the EARLIER and more useful catch: the nudge
#                 lands before the script exists.
#   Bash       -- an interpreter with inline `-c` / `-e` code carrying a
#                 search primitive.
#
# INJECT, NEVER DENY. A script that walks a tree may also do real work,
# and refusing real work to prevent a redundant listing is a worse trade
# than a missed nudge. Same posture #829 took for `git log --grep`: never
# refuse a query the sanctioned surface cannot fully serve. Detection is
# heuristic; the cost of a false positive is one injected paragraph.
#
# Every detection also writes a telemetry row. This class is currently a
# structural blind spot in `etc-summary` (#713) -- the primary metric for
# whether the sanctioned surface answers what agents actually ask -- and a
# metric that cannot see script-shaped searches reads their absence as
# success.
#
# Skip via LEGION_SKIP_SCRIPT_SEARCH=1.

set -u

if [ "${LEGION_SKIP_SCRIPT_SEARCH:-}" = "1" ]; then
  exit 0
fi

if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

if [ -z "$CWD" ] || [ -z "$REPO" ]; then
  exit 0
fi

# shellcheck source=_legion-prequery.sh
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-prequery.sh" 2>/dev/null || exit 0

legion_hook_covered || exit 0

# Pointing at sym in a repo with no index is advice the agent cannot take.
legion_indexed "$SESSION_ID" "$REPO" || exit 0

# --- Gather the text to inspect, per tool ------------------------------------

SUBJECT=""
ORIGIN=""

case "$TOOL" in
  Bash)
    COMMAND=$(legion_hook_field '.tool_input.command')
    [ -n "$COMMAND" ] || exit 0
    INTERP=$(legion_prequery_script_interpreter "$COMMAND")
    [ -n "$INTERP" ] || exit 0
    # Only inline code is inspectable. `python tree.py` carries no clue
    # about what the script does, and reading arbitrary script files at
    # hook time is slow and unbounded -- the Write-side trigger covers
    # the authorship moment for those instead.
    case "$COMMAND" in
      *\ -c\ *|*\ -e\ *) SUBJECT="$COMMAND" ;;
      *) exit 0 ;;
    esac
    ORIGIN="inline ${INTERP}"
    ;;
  Write)
    SUBJECT=$(legion_hook_field '.tool_input.content')
    ORIGIN="a script you are writing"
    ;;
  Edit)
    SUBJECT=$(legion_hook_field '.tool_input.new_string')
    ORIGIN="a script you are editing"
    ;;
  *)
    exit 0
    ;;
esac

[ -n "$SUBJECT" ] || exit 0

SHAPE=$(legion_prequery_script_primitive "$SUBJECT")
[ -n "$SHAPE" ] || exit 0

# --- One command per shape, never a menu (#828's lesson) ---------------------

case "$SHAPE" in
  tree)
    SUGGESTION="legion sym tree --repo ${REPO}"
    WHAT="a directory walk"
    ;;
  file)
    SUGGESTION="legion sym etc find-file '<name-or-glob>' --repo ${REPO}"
    WHAT="a filename or glob match"
    ;;
  content)
    SUGGESTION="legion sym etc find-content '<pattern>' --repo ${REPO}"
    WHAT="a text search inside files"
    ;;
esac

legion_prequery_record_bypass \
  "$REPO" "$SESSION_ID" "$TOOL" "script:${SHAPE}" \
  "script-shaped search (${ORIGIN})" "false" "false"

emit_allow "## legion sym answers this without the script

That looks like ${WHAT} in ${ORIGIN}. \`${REPO}\` is indexed, so:

    ${SUGGESTION}

returns it directly -- no file to write, no walk to run, no output to page through.

Not blocking you: a script that searches may also do real work this cannot replace, and if that is the case here, carry on. But if the search IS the point, sym already has the answer indexed. \`legion sym --help\` covers def/refs/impl/hover for symbols; \`legion sym etc\` covers content, filenames, and structure." \
  "legion sym covers this shape"
