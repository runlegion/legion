#!/bin/bash
# Legion PreToolUse hook (#856): route `git commit` through `legion commit`.
#
# `legion commit` (#854) closes the last unaudited hop in the mutation path
# (index -> commit -> push -> PR -> merge): it preflights the signer, refuses
# message-convention violations by name, and writes an audit row for every
# attempt. Adoption of that verb was left to agent discipline -- nothing
# intercepted a raw `git commit`, so the audit trail was only as complete as
# every agent remembering to reach for the verb. This hook closes that, the
# same way no-git-push.sh closed it for pushes.
#
# Three outcomes, same vocabulary as no-git-push.sh:
#
#   1. REWRITE -- the commit is expressible as `legion commit`. Replace the
#      command via updatedInput and announce the translation.
#   2. DENY    -- the command carries semantics `legion commit` cannot
#      express (-a/-am, --amend, -n/--no-verify, --allow-empty-message,
#      commit-reuse -C/-c, a bare commit with no message, ...). Refuse and
#      name the flag. Rewriting would silently drop it and run something the
#      agent did not ask for.
#   3. PASS    -- not a git commit, repo not legion-covered, or any
#      dependency missing. Fail open; a PreToolUse hook that fails closed
#      can wedge every session.
#
# One structural difference from no-git-push.sh, worth stating up front:
# `legion commit` does NOT do cross-checkout resolution the way `legion
# push` does (src/cli/commit.rs:252-260) -- it resolves the checkout from
# its OWN process cwd via `git rev-parse --show-toplevel`. `git push`
# tolerates `-C <path>` pointing somewhere else because `legion push` takes
# a `--branch` and finds the checkout that has it; `legion commit` operates
# on a staged index, which is per-checkout state only the actual target
# checkout has. So when the command carries a GLOBAL `git -C <path> commit`,
# the rewrite must `cd` there first, or it would silently commit the
# session's checkout instead of the one the agent named -- and `--repo` must
# resolve against THAT path's basename, not the session cwd's, with
# LEGION_REPO still taking precedence per lib/prelude.sh's documented rule.
# Coverage is checked against that same resolved repo, not the session one,
# which is why this hook does the git/commit detection BEFORE calling
# legion_hook_covered rather than after, unlike no-git-push.sh.
#
# Message extraction is the other real hazard. A commit message is
# arbitrary text that can contain anything -- including substrings that
# look like flags. Naive whitespace tokenization of the WHOLE command (as
# no-git-push.sh does, safely, because push arguments are simple tokens)
# would let message content leak into flag classification here, which is
# exactly the class of bug that retired pre-whoami-rewrite.sh (a greedy
# match reading INTO a quoted value, 019e2a5a). The fix: whitespace
# tokenization is only trusted up to and including the `-m`/`-F`/`--message`/
# `--file` token itself -- every token before that point is a plain,
# unquoted keyword (git subcommands and flags never contain spaces), so
# splitting is safe there. The VALUE past that point is re-extracted from
# the raw command string with a small quote-aware lexer (single string,
# double string with `\"\\$\`` escapes, or one bare unquoted word) that
# requires the value to consume the rest of the command with nothing
# trailing. Anything that does not parse unambiguously -- unterminated
# quotes, multiple -m flags, trailing pathspecs -- is a DENY, never a
# guessed rewrite.
#
# That lexer has to solve TWO separate re-quoting hazards, not one. The
# OUTBOUND half is: don't let the rewritten command's shell re-interpret
# `$`/backtick a second time (solved by writing to a tempfile and using
# --message-file, see below). The INBOUND half, easy to miss because
# nothing about it looks wrong until you reason about WHEN evaluation was
# supposed to happen: any `$(...)`, backtick, or bare `$VAR` inside the
# ORIGINAL command was supposed to be evaluated by the shell running that
# ORIGINAL command, before this hook ever saw the string. This hook only
# ever sees source text -- it cannot evaluate a live substitution on the
# agent's behalf -- so capturing one as-is either commits unevaluated
# syntax verbatim (a heredoc that never ran, permanently in history) or
# silently drops whatever the expansion would have produced. Same failure
# mode applies to an UNQUOTED value containing `;`, `&`, or `|`: those are
# command separators/pipes to the ORIGINAL shell, not message bytes, so
# `git commit -m done&&true` glues "done&&true" into the whole message and
# `true` silently never runs. Every one of these is a DENY, not a rewrite:
# there is no lossless way to carry a live evaluation through a
# translation that replaces the shell that was going to do the evaluating.
# Escaped forms (`\$`, `` \` ``) are already resolved to a literal
# character before the check runs, so they are unaffected; single-quoted
# values never reach the check at all, because POSIX makes single quotes
# fully inert -- `$(...)` inside them was always literal, nothing to miss.
#
# The extracted message is written to a fresh tempfile and passed via
# `legion commit --message-file`, not re-quoted into `--message "..."` on
# the rewritten command line: re-quoting risks re-interpreting embedded `$`,
# backtick, or quote characters differently the second time through the
# shell. Reading the bytes back out of a file sidesteps that entirely. The
# tempfile is intentionally NOT cleaned up here -- it has to outlive this
# hook, since the rewritten command is what actually runs.
#
# Bash compatibility note: this repo's default `bash` resolves to the
# macOS system copy (3.2.57), which lacks readarray, `${var@Q}`, and other
# 4.x-only features used freely elsewhere. Nothing here relies on them.
#
# No recursion guard needed, for the same reason as no-git-push.sh: hooks
# fire on the agent's own Bash tool calls, not on child processes, so
# `legion commit`'s internal `git commit` never re-enters here.
#
# Skip via LEGION_SKIP_GIT_COMMIT=1.

set -u

if [ "${LEGION_SKIP_GIT_COMMIT:-}" = "1" ]; then
  exit 0
fi

if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
# shellcheck source=lib/emit.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/emit.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

COMMAND=$(legion_hook_field '.tool_input.command')

if [ -z "$CWD" ] || [ "$TOOL" != "Bash" ] || [ -z "$COMMAND" ] || [ -z "$REPO" ]; then
  exit 0
fi

# --- Detect `git commit` -----------------------------------------------------
#
# Tokenise on whitespace, exactly as no-git-push.sh does for the same prefix
# shape. Everything up to and including the subcommand and any global
# options is safe to split naively: none of it is ever a quoted, multi-word
# value in a real invocation.

read -r -a TOKENS <<<"$COMMAND"
[ "${#TOKENS[@]}" -ge 2 ] || exit 0

FIRST_BIN="${TOKENS[0]##*/}"

# Walk past git's own global options to find the subcommand, capturing `-C`
# specially: it is the one global option this hook has to act on, since it
# retargets the checkout `legion commit` must run in (see header). A `-C`
# whose value itself contains spaces is a known, accepted gap shared with
# no-git-push.sh's identical walk -- the resulting `cd` fails loudly rather
# than silently targeting the wrong tree, which is the safe failure mode.
# Only meaningful when the FIRST token is git -- for anything else
# (including a leading unrelated command, see #886 below) this never runs
# and SUBCOMMAND stays empty.
GIT_C_PATH=""
SUBCOMMAND=""
if [ "$FIRST_BIN" = "git" ]; then
  IDX=1
  while [ "$IDX" -lt "${#TOKENS[@]}" ]; do
    TOK="${TOKENS[$IDX]}"
    case "$TOK" in
      -C)
        GIT_C_PATH="${TOKENS[$((IDX + 1))]:-}"
        IDX=$((IDX + 2))
        continue
        ;;
      -c | --git-dir | --work-tree | --namespace)
        IDX=$((IDX + 2))
        continue
        ;;
      -*)
        IDX=$((IDX + 1))
        continue
        ;;
      *)
        SUBCOMMAND="$TOK"
        break
        ;;
    esac
  done
fi

# --- Resolve the target repo -------------------------------------------------
#
# LEGION_REPO wins over everything, in every hook (lib/prelude.sh:20-24).
# Absent that, a `-C <path>` retargets the repo identity the same way it
# retargets the checkout; with no `-C`, this is exactly the session repo
# legion_hook_parse already resolved. Resolved here, before the #886
# compound fallback below, so both branches (a clean `git commit` and a
# composed command where `commit` sits deeper in a chain) check coverage
# against the same repo identity.
if [ -n "$GIT_C_PATH" ] && [ -z "${LEGION_REPO:-}" ]; then
  REPO="$(basename "$GIT_C_PATH")"
fi

# --- #886: compound-chain fallback, ONLY when commit is not the FIRST -------
# --- subcommand found --------------------------------------------------------
#
# The walk above only ever looks at the FIRST git invocation's immediate
# subcommand. `commit` glued to a metacharacter (`git commit;`) or not
# the first command in the chain at all (`git add -A && git commit -m
# x`, `npm test && git commit -m x`) both leave SUBCOMMAND holding
# something other than exactly `commit`, and the old strict equality
# check exited 0 on all of them -- a raw `git commit` ran with no audit
# row, no deny, no rewrite, nothing.
#
# Deliberately NOT applied when SUBCOMMAND == "commit" cleanly, unlike
# no-git-push.sh's equivalent guard. A commit MESSAGE is free text that
# can legitimately contain `&&`, `;`, `$(...)` etc. as ordinary prose or
# as an inert single-quoted literal (POSIX makes single quotes fully
# inert -- `'$(date) is not evaluated'` is exactly that, and denying it
# outright was tried here and measured as a false-positive: it broke a
# genuinely safe rewrite). `legion_hook_compound` scans the RAW string,
# not tokens, so it cannot tell "composed with another real command"
# apart from "these characters happen to sit inside a quoted message,"
# and would silently deny messages this hook is fully equipped to handle
# correctly.
#
# That correctness is not lost by skipping the check here: when `commit`
# genuinely IS the first subcommand, this hook's own message-value lexer
# (below) already denies real trailing composition on its own terms --
# a `&&`/`;` AFTER a properly closed quote fails the "nothing may follow
# the value" rule (EXTRACT_ERROR: unexpected content after the quoted
# value), and a live, UNQUOTED or double-quoted `$`/backtick/`;`/`&`/`|`
# inside the value denies as a live-substitution/live-metacharacter
# hazard the translator cannot evaluate on the agent's behalf. Only the
# "commit is not the first subcommand" shape below still needs a
# dedicated guard, because for THAT shape the message-extraction
# machinery never runs at all -- there is no lexer downstream to catch
# it, which is exactly why the whole-command scan is safe there: it is
# reached only when this command was never going to touch a real commit
# message in the first place.
if [ "$SUBCOMMAND" != "commit" ]; then
  if legion_hook_compound "$COMMAND" \
    && legion_hook_token_present "$COMMAND" git \
    && legion_hook_token_present "$COMMAND" commit; then
    legion_hook_covered || exit 0
    emit_deny "Refusing \`git commit\` -- it's composed with something else (a pipe, redirect, \`&&\`, \`;\`, or \`\$(...)\`), and legion's rewrite would replace the WHOLE command string.

Translating it would silently drop everything else in it, or misread part of one command as belonging to another. Run the commit and the rest of your pipeline as separate steps:

    legion commit --repo ${REPO} --message '<type>(<scope>): <summary>

Co-Authored-By: <name> <email>'

Work-source actions go through legion so they land in the audit log (\`legion audit\`)."
  fi
  exit 0
fi

legion_hook_covered || exit 0

# --- Classify the commit ------------------------------------------------------
#
# Anything `legion commit` cannot express is a DENY, never a silent drop.
# `legion commit` (src/cli/commit.rs) commits only the staged index (no -a),
# has no --amend, no --no-verify, no --allow-empty-message, and no
# commit-reuse forms (-C/-c as commit flags, not to be confused with the
# GLOBAL -C handled above). It always requires an explicit message.
#
# The scan stops at the first `-m`/`--message` or `-F`/`--file` token: past
# that point the rest of the command is the message/file VALUE, not more
# argv, and must not be token-scanned (see header).

DENY_FLAG=""
MSG_TOKEN_IDX=""
FILE_TOKEN_IDX=""

IDX=$((IDX + 1))
while [ "$IDX" -lt "${#TOKENS[@]}" ]; do
  TOK="${TOKENS[$IDX]}"
  case "$TOK" in
    -m)
      MSG_TOKEN_IDX=$IDX
      break
      ;;
    --message)
      MSG_TOKEN_IDX=$IDX
      break
      ;;
    --message=*)
      DENY_FLAG="$TOK (the inline --message=... form is not supported by this translator -- use --message '...' with a separate argument, or -m '...')"
      break
      ;;
    -F)
      FILE_TOKEN_IDX=$IDX
      break
      ;;
    --file)
      FILE_TOKEN_IDX=$IDX
      break
      ;;
    --file=*)
      DENY_FLAG="$TOK (the inline --file=... form is not supported by this translator -- use --file <path> with a separate argument)"
      break
      ;;
    --amend | --amend=*)
      DENY_FLAG="--amend has no legion commit equivalent"
      break
      ;;
    --all)
      DENY_FLAG="--all (legion commit only commits what is already staged -- run \`git add\` on the files you want, then re-run)"
      break
      ;;
    --no-verify)
      DENY_FLAG="--no-verify has no legion commit equivalent -- gates cannot be bypassed"
      break
      ;;
    --allow-empty-message)
      DENY_FLAG="--allow-empty-message has no legion commit equivalent -- a message is always required"
      break
      ;;
    --edit | --interactive | --patch)
      DENY_FLAG="$TOK (legion commit needs an explicit message up front -- it cannot open an editor or an interactive staging prompt)"
      break
      ;;
    --*)
      DENY_FLAG="$TOK (not translatable -- unrecognized flag)"
      break
      ;;
    -a | -am | -ma)
      DENY_FLAG="$TOK (-a/--all has no legion commit equivalent -- legion commit only commits what is already staged; \`git add\` the files you want, then re-run)"
      break
      ;;
    -n)
      DENY_FLAG="-n (--no-verify has no legion commit equivalent -- gates cannot be bypassed)"
      break
      ;;
    -e)
      DENY_FLAG="-e (legion commit needs an explicit message up front -- it cannot open an editor)"
      break
      ;;
    -C)
      DENY_FLAG="-C <commit-ish> (reuse the message from another commit -- no legion commit equivalent)"
      break
      ;;
    -c)
      DENY_FLAG="-c <commit-ish> (reuse and re-edit the message from another commit -- no legion commit equivalent)"
      break
      ;;
    -*)
      DENY_FLAG="$TOK (not translatable -- bundled or unrecognized short flag)"
      break
      ;;
    *)
      DENY_FLAG="$TOK (legion commit takes no positional pathspec)"
      break
      ;;
  esac
done

REPO_ARG="$REPO"

if [ -n "$DENY_FLAG" ]; then
  emit_deny "Refusing \`git commit\` -- \`${DENY_FLAG}\`.

This cannot be translated to \`legion commit\`, the audited commit verb (#854): it preflights the signer, refuses message-convention violations by name, and writes an audit row for every attempt (agent, branch, pre/post SHA, gate state). Rewriting the command for you would silently drop the flag and run something you did not ask for.

If you meant a plain commit of the staged index:
    legion commit --repo ${REPO_ARG} --message '<type>(<scope>): <summary>

Co-Authored-By: <name> <email>'
or with the message in a file:
    legion commit --repo ${REPO_ARG} --message-file <path>"
  exit 0
fi

if [ -z "$MSG_TOKEN_IDX" ] && [ -z "$FILE_TOKEN_IDX" ]; then
  emit_deny "Refusing bare \`git commit\` -- it opens an editor, and \`legion commit\` needs an explicit message up front.

Run:
    legion commit --repo ${REPO_ARG} --message '<type>(<scope>): <summary>

Co-Authored-By: <name> <email>'
or with the message in a file:
    legion commit --repo ${REPO_ARG} --message-file <path>"
  exit 0
fi

# --- Extract the message/file value from the RAW command --------------------
#
# TOKENS past the flag we stopped on are not trustworthy (see header); strip
# the flag and everything before it off the ORIGINAL command string as
# literal text instead, then lex exactly one value: a single-quoted string
# (no escapes, POSIX rule), a double-quoted string (`\"\\$\`` escapes only),
# or one bare unquoted word. Anything left over after that value is a
# translation this hook will not guess at -- multiple -m flags, trailing
# pathspecs, unterminated quotes all DENY.

VALUE_TOKEN_IDX="$MSG_TOKEN_IDX"
[ -n "$VALUE_TOKEN_IDX" ] || VALUE_TOKEN_IDX="$FILE_TOKEN_IDX"

REMAINDER="$COMMAND"
STRIP_UPTO=$((VALUE_TOKEN_IDX + 1))
SI=0
while [ "$SI" -lt "$STRIP_UPTO" ]; do
  REMAINDER="${REMAINDER#"${REMAINDER%%[![:space:]]*}"}"
  REMAINDER="${REMAINDER#"${TOKENS[$SI]}"}"
  SI=$((SI + 1))
done
REMAINDER="${REMAINDER#"${REMAINDER%%[![:space:]]*}"}"

EXTRACTED_VALUE=""
EXTRACT_ERROR=""

if [ -z "$REMAINDER" ]; then
  EXTRACT_ERROR="missing value"
else
  FIRST_CHAR="${REMAINDER:0:1}"
  if [ "$FIRST_CHAR" = "'" ] || [ "$FIRST_CHAR" = '"' ]; then
    Q="$FIRST_CHAR"
    BODY="${REMAINDER:1}"
    BLEN=${#BODY}
    BI=0
    VAL=""
    CLOSED=0
    LIVE_SUBST=0
    while [ "$BI" -lt "$BLEN" ]; do
      C="${BODY:$BI:1}"
      if [ "$Q" = '"' ] && [ "$C" = "\\" ] && [ $((BI + 1)) -lt "$BLEN" ]; then
        NC="${BODY:$((BI + 1)):1}"
        # shellcheck disable=SC1003 # '\' here is the one-char literal
        # backslash pattern, not an attempt to escape the closing quote.
        case "$NC" in
          '"' | '\' | '$' | '`')
            VAL="${VAL}${NC}"
            BI=$((BI + 2))
            continue
            ;;
        esac
      fi
      # An UNESCAPED $ or ` inside a double-quoted value is live: the
      # ORIGINAL shell would have evaluated $(...), a backtick command
      # substitution, or a bare $VAR/${VAR} expansion right here. This
      # hook cannot evaluate that on the agent's behalf -- it can only
      # capture source text -- so capturing it silently would either
      # commit the raw unevaluated syntax verbatim (a heredoc that never
      # ran, literally in history) or drop a variable expansion the
      # message depended on. Escaped forms (\$, \`, handled above) are
      # already literal by the time they reach here and never trigger
      # this. Single-quoted values (`$Q` = "'") never reach this branch
      # at all -- POSIX makes single quotes fully inert, so `$(...)`
      # inside them was always meant literally, no live substitution to
      # miss.
      if [ "$Q" = '"' ] && { [ "$C" = '$' ] || [ "$C" = '`' ]; }; then
        LIVE_SUBST=1
        break
      fi
      if [ "$C" = "$Q" ]; then
        CLOSED=1
        BI=$((BI + 1))
        break
      fi
      VAL="${VAL}${C}"
      BI=$((BI + 1))
    done
    if [ "$LIVE_SUBST" -eq 1 ]; then
      EXTRACT_ERROR="the message contains an unescaped \$ or \` -- the ORIGINAL command's shell would have evaluated that (command substitution or a variable expansion), and this translator cannot evaluate it on your behalf. Escape it (\\\$, \\\`) if you meant it literally, quote the message with single quotes instead (POSIX single quotes never evaluate anything), or write the already-evaluated text to a file and use --file"
    elif [ "$CLOSED" -ne 1 ]; then
      EXTRACT_ERROR="unterminated quote"
    else
      TRAILING="${BODY:$BI}"
      TRAILING="${TRAILING#"${TRAILING%%[![:space:]]*}"}"
      if [ -n "$TRAILING" ]; then
        EXTRACT_ERROR="unexpected content after the quoted value: ${TRAILING}"
      else
        EXTRACTED_VALUE="$VAL"
      fi
    fi
  else
    WORD="${REMAINDER%%[[:space:]]*}"
    REST="${REMAINDER#"$WORD"}"
    REST="${REST#"${REST%%[![:space:]]*}"}"
    if [ -n "$REST" ]; then
      EXTRACT_ERROR="unexpected content after the value: ${REST}"
    else
      # An unquoted value is not just a word -- it is argv text the shell
      # itself would have parsed. $, `, ;, &, and | are all live there
      # too (command substitution, command separators, background jobs,
      # pipes): `git commit -m done&&true` never runs `true`, it glues
      # "done&&true" into ONE argv word that becomes the whole message,
      # silently absorbing a command this hook cannot know was supposed
      # to run. Same posture as the quoted branch above: deny rather than
      # guess.
      case "$WORD" in
        *'$'* | *'`'* | *';'* | *'&'* | *'|'*)
          EXTRACT_ERROR="an unquoted value containing \$, \`, ;, &, or | would have been evaluated -- or would have ended the command and started another -- when the ORIGINAL command ran. This translator cannot replicate that safely. Quote the message (single or double quotes) and re-run"
          ;;
        *)
          EXTRACTED_VALUE="$WORD"
          ;;
      esac
    fi
  fi
fi

if [ -n "$EXTRACT_ERROR" ]; then
  FLAG_NAME="-m/--message"
  [ -n "$FILE_TOKEN_IDX" ] && FLAG_NAME="-F/--file"
  emit_deny "Refusing \`git commit\` -- could not read the ${FLAG_NAME} value unambiguously (${EXTRACT_ERROR}).

This translator only handles a single quoted or bare-word value with nothing after it -- not multiple -m flags, not trailing pathspecs, not unterminated quotes. Re-run with the message quoted once, or write it to a file and pass it directly:
    legion commit --repo ${REPO_ARG} --message '<type>(<scope>): <summary>

Co-Authored-By: <name> <email>'
or:
    legion commit --repo ${REPO_ARG} --message-file <path>"
  exit 0
fi

# shell_quote -- POSIX-portable single-quote escaping (no bash 4 @Q here;
# the resolved `bash` on this repo's default setup is 3.2.57).
shell_quote() {
  local s="$1"
  s="${s//\'/\'\\\'\'}"
  printf "'%s'" "$s"
}

if [ -n "$MSG_TOKEN_IDX" ]; then
  MSG_FILE=$(mktemp "${TMPDIR:-/tmp}/legion-commit-msg.XXXXXX") || {
    emit_deny "Refusing \`git commit\` -- could not create a tempfile to carry the message to \`legion commit --message-file\`."
    exit 0
  }
  printf '%s' "$EXTRACTED_VALUE" >"$MSG_FILE"
  MESSAGE_FILE_ARG="$MSG_FILE"
  CAPTURE_NOTE=" Your message was captured to a tempfile and passed via \`--message-file\` rather than re-quoted inline on the rewritten command -- re-quoting risks re-interpreting \$ or backtick characters a second time through the shell; reading the bytes back out of a file does not."
else
  MESSAGE_FILE_ARG="$EXTRACTED_VALUE"
  CAPTURE_NOTE=""
fi

if [ -n "$GIT_C_PATH" ]; then
  REWRITTEN="cd $(shell_quote "$GIT_C_PATH") && legion commit --repo $(shell_quote "$REPO_ARG") --message-file $(shell_quote "$MESSAGE_FILE_ARG")"
else
  REWRITTEN="legion commit --repo $(shell_quote "$REPO_ARG") --message-file $(shell_quote "$MESSAGE_FILE_ARG")"
fi

emit_rewrite "$REWRITTEN" "Translated your \`git commit\` to \`legion commit\`.

This is the audited commit path (#854): it preflights the configured signer before touching anything (a locked or absent signer fails once, by name), refuses message-convention violations by name (subject shape, the Co-Authored-By trailer, no emoji), and writes an audit row for every attempt -- refusals included -- carrying the resolved checkout, pre/post HEAD, and gate state.${CAPTURE_NOTE}

A raw \`git commit\` writes none of that, so reach for \`legion commit\` directly next time rather than relying on this translation." \
  "routed through legion commit for the audit trail (#856)"
