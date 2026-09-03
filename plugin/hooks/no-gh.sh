#!/bin/bash
# Legion PreToolUse hook: translate or block direct gh usage.
#
# Three outcomes:
#
#   1. REWRITE -- the call is expressible losslessly as its legion
#      equivalent: `pr view`, `pr checks`, `pr list`, `issue view`,
#      `issue list`, each bare or carrying only --number. Replace the
#      command via updatedInput and announce the translation (#862).
#   2. DENY    -- everything else. Writes (merge, close, edit, review,
#      comment) carry free-text arguments this tokenizer cannot safely
#      round-trip -- `read -r -a` does no quote removal, so `--body "a b"`
#      splits into `"a` and `b"` on the space. Flags with no legion
#      equivalent (--json, --jq, --template, --web, -R/--repo) block a
#      would-be rewrite and name the flag rather than silently dropping
#      it. `gh pr diff` stays denied even though `pr view` exists: legion
#      pr view renders metadata and body, never diff content, so mapping
#      one to the other would silently swap what the agent asked for.
#      `gh api` has no legion counterpart at all. Any invocation composed
#      with a pipe, redirect, `&&`, `;`, or `$(...)` also denies --
#      updatedInput.command replaces the WHOLE string, so rewriting it
#      would silently drop everything after the gh call.
#   3. PASS    -- not a gh invocation, repo not legion-covered, or a
#      dependency missing. Fail open.
#
# All work source actions go through legion for audit logging and
# workflow tracking (`legion audit`).

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

# --- #886: compound commands deny BEFORE position-dependent detection -------
#
# `gh` does not have to be the FIRST word for this hook's guarantee to
# matter: `echo hi && gh pr merge 123` reaches the real `gh` binary just
# as surely as a bare `gh pr merge 123` does. The basename check below
# only ever looks at the first token, so it silently passed a leading
# unrelated command straight through -- checked here, independent of
# where in the command `gh` sits, before that position-dependent check
# ever runs. `updatedInput` replaces the WHOLE command string, so a
# composed command showing `gh` anywhere always denies; there is no
# segment of a chain this hook will classify or rewrite in isolation.
if legion_hook_compound "$COMMAND" && legion_hook_token_present "$COMMAND" gh; then
  emit_deny "Refusing -- this command is composed with something else (a pipe, redirect, \`&&\`, \`;\`, or \`\$(...)\`) and mentions \`gh\`, and legion's rewrite would replace the WHOLE command string.

Translating it would either drop everything else in it or misread part of one command as an argument to another. Run the \`gh\` step (or its legion equivalent) and the rest of your pipeline as separate steps:

    legion --help

Work-source actions go through legion so they land in the audit log (\`legion audit\`)."
  exit 0
fi

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

# --- #862: rewrite the lossless read subset, deny everything else -----------
#
# `pr view`, `pr checks`, `pr list`, `issue view`, `issue list` are the
# only arms converted to emit_rewrite. Every other verb keeps the
# unmodified deny below -- see the file header for why.
#
# One guard gates every candidate before it is allowed to become a
# rewrite: NUMBER absent -- gh resolves an omitted PR/issue number from
# the current branch; legion has no equivalent and requires --number
# explicit. Rather than emit a command with a "<n>" placeholder (bash
# would read `--number <n>` as a redirect from a file named `n`), this
# simply skips the rewrite attempt and falls through to the existing
# deny below, which already renders the placeholder as text, never as
# something executed.
#
# A compound command never reaches this section at all -- the #886 guard
# near the top of the file denies before FIRST_BIN is even resolved, so
# by the time GROUP/VERB/NUMBER are known, the command is guaranteed
# uncomposed.

# first_flag_token WANT... -- echo the first GH_TOKENS entry (after the
# binary) matching one of the given flag spellings, exact token or
# --flag=value form. Nothing echoed / return 1 on no match.
first_flag_token() {
  local tok want gi
  for ((gi = 1; gi < ${#GH_TOKENS[@]}; gi++)); do
    tok="${GH_TOKENS[$gi]}"
    for want in "$@"; do
      if [ "$tok" = "$want" ] || [ "${tok%%=*}" = "$want" ]; then
        printf '%s\n' "$tok"
        return 0
      fi
    done
  done
  return 1
}

# any_flag_token -- echo the first GH_TOKENS entry (after the binary) that
# looks like a flag at all. Used for `pr list` / `issue list`, which take
# zero flags on the legion side -- any flag present means a filter would
# silently vanish, not merely a display option.
any_flag_token() {
  local tok gi
  for ((gi = 1; gi < ${#GH_TOKENS[@]}; gi++)); do
    tok="${GH_TOKENS[$gi]}"
    case "$tok" in
      -*)
        printf '%s\n' "$tok"
        return 0
        ;;
    esac
  done
  return 1
}

REWRITE_CMD=""
REWRITE_BLOCK_REASON=""
NUM_SUFFIX=""
case "$GROUP $VERB" in
  "pr view" | "issue view" | "pr checks") NUM_SUFFIX=" --number ${NUMBER}" ;;
esac

case "$GROUP $VERB" in
  "pr view")
    if [ -n "$NUMBER" ]; then
      BLOCKING=$(first_flag_token -c --comments -q --jq --json -t --template -w --web -R --repo) || true
      if [ -n "$BLOCKING" ]; then
        REWRITE_BLOCK_REASON="$BLOCKING"
      else
        REWRITE_CMD="legion pr view --repo ${REPO} --number ${NUMBER}"
      fi
    fi
    ;;
  "issue view")
    if [ -n "$NUMBER" ]; then
      BLOCKING=$(first_flag_token -c --comments -q --jq --json -t --template -w --web -R --repo) || true
      if [ -n "$BLOCKING" ]; then
        REWRITE_BLOCK_REASON="$BLOCKING"
      else
        REWRITE_CMD="legion issue view --repo ${REPO} --number ${NUMBER}"
      fi
    fi
    ;;
  "pr checks")
    if [ -n "$NUMBER" ]; then
      BLOCKING=$(first_flag_token -q --jq --json -t --template -w --web -R --repo --watch --fail-fast -i --interval --required) || true
      if [ -n "$BLOCKING" ]; then
        REWRITE_BLOCK_REASON="$BLOCKING"
      else
        REWRITE_CMD="legion pr checks --repo ${REPO} --number ${NUMBER}"
      fi
    fi
    ;;
  "pr list")
    BLOCKING=$(any_flag_token) || true
    if [ -n "$BLOCKING" ]; then
      REWRITE_BLOCK_REASON="$BLOCKING"
    else
      REWRITE_CMD="legion pr list --repo ${REPO}"
    fi
    ;;
  "issue list")
    BLOCKING=$(any_flag_token) || true
    if [ -n "$BLOCKING" ]; then
      REWRITE_BLOCK_REASON="$BLOCKING"
    else
      REWRITE_CMD="legion issue list --repo ${REPO}"
    fi
    ;;
esac

if [ -n "$REWRITE_CMD" ]; then
  emit_rewrite "$REWRITE_CMD" "Translated your \`gh ${GROUP} ${VERB}\` to \`${REWRITE_CMD}\`.

This is the audited work-source path: the command lands in \`legion audit\` the way a raw \`gh\` call never does. Reach for \`legion ${GROUP} ${VERB}\` directly next time rather than relying on this translation." \
    "routed through legion for the audit trail (#862)"
  exit 0
fi

if [ -n "$REWRITE_BLOCK_REASON" ]; then
  emit_deny "Use legion, not gh -- but \`${REWRITE_BLOCK_REASON}\` cannot be translated.

\`${REWRITE_BLOCK_REASON}\` has no equivalent on \`legion ${GROUP} ${VERB}\`. Rewriting anyway would silently drop it and hand you a narrower answer than the one you asked for.

Without that flag:
    legion ${GROUP} ${VERB} --repo ${REPO}${NUM_SUFFIX}

Work-source actions go through legion so they land in the audit log (\`legion audit\`)."
  exit 0
fi

# `pr diff` never becomes a rewrite: `pr_view.rs::render_pr` prints a PR's
# metadata and body only, never diff content, so mapping `gh pr diff` onto
# `legion pr view` would silently swap the code changes the agent asked
# for with something else. Denied on purpose, not merely unclassified.
if [ "$GROUP $VERB" = "pr diff" ]; then
  emit_deny "Use legion, not gh -- but there is no legion equivalent for the diff itself.

\`legion pr view --repo ${REPO} ${NUM_ARG}\` shows a PR's metadata and body -- never the diff content, so rewriting \`gh pr diff\` to it would silently hand you something other than what you asked for. No legion command renders a PR's diff.

Work-source actions go through legion so they land in the audit log (\`legion audit\`); \`legion pr --help\` lists the full surface."
  exit 0
fi

case "$GROUP $VERB" in
  "pr view")     SUGGESTION="legion pr view --repo ${REPO} ${NUM_ARG}" ;;
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
