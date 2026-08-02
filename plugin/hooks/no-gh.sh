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

if [ "$FIRST_BIN" != "gh" ]; then
  exit 0
fi

# --- Translate, do not enumerate ---------------------------------------------
#
# This deny message is the only place the legion work-source surface gets
# named to an agent mid-task, and it used to print a fixed list of 8 verbs
# against a surface of roughly 40 -- with every READ verb missing
# (pr view, pr checks, pr comments, pr reviews, issue view). Those are
# exactly what an agent reaches for when reacting to review feedback or
# checking CI, so the list was thinnest precisely where it was most needed.
#
# `pre-bash-grep.sh` already established the better pattern: resolve the
# agent's actual query and hand back the specific answer instead of a
# catalog. An exact redirect also cannot drift into partiality the way an
# enumeration does -- a new legion verb does not silently make this message
# wrong.
#
# Parsing is argv-style, whole-token comparisons only. See the
# pre-whoami-rewrite scar (019e2a5a): a greedy regex over a command string
# matches content inside --body/--title values.

read -r -a GH_TOKENS <<<"$TRIMMED"

GROUP=""
VERB=""
NUMBER=""
for ((gi = 1; gi < ${#GH_TOKENS[@]}; gi++)); do
  TOK="${GH_TOKENS[$gi]}"
  case "$TOK" in
    -*) continue ;;
  esac
  if [ -z "$GROUP" ]; then
    GROUP="$TOK"
  elif [ -z "$VERB" ]; then
    VERB="$TOK"
  elif [ -z "$NUMBER" ]; then
    case "$TOK" in
      *[!0-9]*) : ;;
      *) NUMBER="$TOK" ;;
    esac
  fi
done

# `--number <n>` when the agent used the flag form rather than a positional.
if [ -z "$NUMBER" ]; then
  for ((gi = 1; gi < ${#GH_TOKENS[@]}; gi++)); do
    if [ "${GH_TOKENS[$gi]}" = "--number" ] && [ $((gi + 1)) -lt "${#GH_TOKENS[@]}" ]; then
      NUMBER="${GH_TOKENS[$((gi + 1))]}"
      break
    fi
  done
fi

NUM_ARG="--number ${NUMBER:-<n>}"

case "$GROUP $VERB" in
  "pr view")     SUGGESTION="legion pr view --repo ${REPO} ${NUM_ARG}" ;;
  "pr diff")     SUGGESTION="legion pr view --repo ${REPO} ${NUM_ARG}" ;;
  "pr checks")   SUGGESTION="legion pr checks --repo ${REPO} ${NUM_ARG}" ;;
  "pr list")     SUGGESTION="legion pr list --repo ${REPO}" ;;
  "pr create")   SUGGESTION="legion pr create --repo ${REPO} --title '...' --closes <issue>" ;;
  "pr merge")    SUGGESTION="legion pr merge --repo ${REPO} ${NUM_ARG}" ;;
  "pr close")    SUGGESTION="legion pr close --repo ${REPO} ${NUM_ARG}" ;;
  "pr edit")     SUGGESTION="legion pr edit --repo ${REPO} ${NUM_ARG} --title '...'" ;;
  "pr review")   SUGGESTION="legion pr review --repo ${REPO} ${NUM_ARG} --approve" ;;
  "pr comment")  SUGGESTION="legion comment --repo ${REPO} ${NUM_ARG} --body '...'" ;;
  "pr comments") SUGGESTION="legion pr comments --repo ${REPO} ${NUM_ARG}" ;;
  "issue view")    SUGGESTION="legion issue view --repo ${REPO} ${NUM_ARG}" ;;
  "issue list")    SUGGESTION="legion issue list --repo ${REPO}" ;;
  "issue create")  SUGGESTION="legion issue create --repo ${REPO} --title '...' --body '...'" ;;
  "issue close")   SUGGESTION="legion issue close --repo ${REPO} ${NUM_ARG}" ;;
  "issue reopen")  SUGGESTION="legion issue reopen --repo ${REPO} ${NUM_ARG}" ;;
  "issue edit")    SUGGESTION="legion issue edit --repo ${REPO} ${NUM_ARG} --title '...'" ;;
  "issue comment") SUGGESTION="legion comment --repo ${REPO} ${NUM_ARG} --body '...'" ;;
  "run list" | "run view" | "run watch")
    # CI status lives on the PR in legion's model, not on the run.
    SUGGESTION="legion pr checks --repo ${REPO} ${NUM_ARG}"
    ;;
  *)
    SUGGESTION=""
    ;;
esac

if [ -n "$SUGGESTION" ]; then
  emit_deny "Use legion, not gh:

    ${SUGGESTION}

Work-source actions go through legion so they land in the audit log (\`legion audit\`). Absolute-path invocations are blocked too -- legion resolves the binary by basename.

Fill in any \`...\` placeholder; \`legion ${GROUP} --help\` lists the rest of the surface, which is wider than this message shows."
  exit 0
fi

# No mapping. Point at the group help rather than inventing a translation
# -- a fabricated command is worse than an honest "look here", because the
# agent will run it and read the error as legion being broken.
#
# The pointer is deliberately to the GROUP help, never a leaf subcommand's
# help: a leaf documents its own options and hides its siblings, which is
# the one confusion this surface reproducibly causes (an agent that ran
# `legion pr list --help` concluded `legion pr checks` did not exist).
HELP_HINT="legion --help"
case "$GROUP" in
  pr | issue | run) HELP_HINT="legion ${GROUP/run/pr} --help" ;;
esac

emit_deny "Do not use gh directly -- work-source actions go through legion so they land in the audit log.

There is no direct legion equivalent for \`gh ${GROUP} ${VERB}\`. Run:

    ${HELP_HINT}

to see the surface (it is wider than you may expect -- \`legion pr\` alone covers view, checks, comments, reviews, edit, review, merge and close). Ask the group's help, not a single subcommand's: a leaf \`--help\` documents its own flags and hides its siblings."
