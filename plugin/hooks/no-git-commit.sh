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
[ "$FIRST_BIN" = "git" ] || exit 0

# Walk past git's own global options to find the subcommand, capturing `-C`
# specially: it is the one global option this hook has to act on, since it
# retargets the checkout `legion commit` must run in (see header). A `-C`
# whose value itself contains spaces is a known, accepted gap shared with
# no-git-push.sh's identical walk -- the resulting `cd` fails loudly rather
# than silently targeting the wrong tree, which is the safe failure mode.
GIT_C_PATH=""
SUBCOMMAND=""
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

[ "$SUBCOMMAND" = "commit" ] || exit 0

# --- Resolve the target repo and check coverage ------------------------------
#
# LEGION_REPO wins over everything, in every hook (lib/prelude.sh:20-24).
# Absent that, a `-C <path>` retargets the repo identity the same way it
# retargets the checkout; with no `-C`, this is exactly the session repo
# legion_hook_parse already resolved. Coverage has to be checked against
# THIS resolved repo -- an agent running `git -C ~/other-repo commit` should
# not be gated by whether the session's OWN repo happens to be covered.
if [ -n "$GIT_C_PATH" ] && [ -z "${LEGION_REPO:-}" ]; then
  REPO="$(basename "$GIT_C_PATH")"
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
    while [ "$BI" -lt "$BLEN" ]; do
      C="${BODY:$BI:1}"
      if [ "$Q" = '"' ] && [ "$C" = "\\" ] && [ $((BI + 1)) -lt "$BLEN" ]; then
        NC="${BODY:$((BI + 1)):1}"
        case "$NC" in
          '"' | '\' | '$' | '`')
            VAL="${VAL}${NC}"
            BI=$((BI + 2))
            continue
            ;;
        esac
      fi
      if [ "$C" = "$Q" ]; then
        CLOSED=1
        BI=$((BI + 1))
        break
      fi
      VAL="${VAL}${C}"
      BI=$((BI + 1))
    done
    if [ "$CLOSED" -ne 1 ]; then
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
      EXTRACTED_VALUE="$WORD"
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
