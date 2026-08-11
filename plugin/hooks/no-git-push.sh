#!/bin/bash
# Legion PreToolUse hook (#827): route `git push` through `legion push`.
#
# `legion push` (#791) exists so the push-from-own-checkout doctrine is
# enforced by the tool rather than by agent discipline: the pre-push hook
# reviews the CWD's checked-out branch, not the ref being pushed, so a
# push issued from the wrong checkout silently reviews the wrong diff.
# It also refuses main/master, refuses force, and audit-logs every attempt
# with branch, resolved checkout, and head SHA.
#
# Adoption of that command was itself left to agent discipline. Nothing
# intercepted a raw `git push`, which writes no audit row at all, so the
# audit trail was only as complete as every agent remembering. This hook
# closes that: the sanctioned path becomes the one that happens.
#
# Three outcomes:
#
#   1. REWRITE -- the push is expressible as `legion push`. Replace the
#      command via updatedInput and announce the translation.
#   2. DENY    -- the command carries semantics `legion push` cannot
#      express (force, refspec, delete, tags, mirror, prune), OR is
#      composed with something else via a pipe, `&`/`&&`, `;`, a
#      redirect, `$(...)`, a backtick, or an embedded newline
#      (legion_hook_compound, lib/prelude.sh -- shared with no-gh.sh so
#      the two hooks cannot drift). Refuse and name the reason. Rewriting
#      would silently drop it and run something the agent did not ask
#      for -- measured before this guard existed: `git push && echo done`
#      rewrote to `legion push --branch echo`, and `git push | tee
#      /tmp/log` rewrote to `... --branch tee`. The classify loop below
#      has no notion of shell composition, so it read the NEXT command's
#      name as a branch argument (#883). The compound check also has to
#      run independent of WHERE `push` sits: `git status && git push`
#      silently passed through entirely, because the subcommand walk
#      only ever looks at the first git invocation and never re-consults
#      the compound flag once that walk finds something other than
#      `push` (#886; legion_hook_token_present, lib/prelude.sh).
#   3. PASS    -- not a git push, repo not legion-covered, or any
#      dependency missing. Fail open; a PreToolUse hook that fails closed
#      can wedge every session.
#
# No recursion guard is needed: hooks fire on the AGENT's Bash tool calls,
# not on child processes, so `legion push`'s own internal `git push` never
# re-enters here. That is precisely why this is a hook and not a PATH shim
# or a symlink -- legion is itself a git consumer, and a shim would recurse.
#
# Parsing is argv-style by deliberate choice. `pre-whoami-rewrite.sh` was
# retired after its greedy `sed` matched the LAST `--repo` in a command,
# so an agent writing `--repo` inside `--text` content shifted the
# extracted value to garbage and the hook silently allowed a bad rewrite.
# Iterate tokens, compare whole words, never regex across the string.
#
# Skip via LEGION_SKIP_GIT_PUSH=1.

set -u

if [ "${LEGION_SKIP_GIT_PUSH:-}" = "1" ]; then
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

# Universal gate: skip repos legion does not cover (#353).
legion_hook_covered || exit 0

# --- Detect `git push` -------------------------------------------------------
#
# Tokenise on whitespace. The first token's BASENAME must be `git` so an
# absolute-path invocation (/opt/homebrew/bin/git) resolves the same way,
# matching the hardening in no-gh.sh. The next non-flag token must be
# `push`; `git -C /path push` and `git --no-pager push` are real shapes.

read -r -a TOKENS <<<"$COMMAND"
[ "${#TOKENS[@]}" -ge 2 ] || exit 0

FIRST_BIN="${TOKENS[0]##*/}"

# Walk past git's own global options to find the subcommand. Options that
# take a value (-C, -c, --git-dir, --work-tree, --namespace) consume the
# next token too. Only meaningful when the FIRST token is git -- for
# anything else (including a leading unrelated command, see below) this
# never runs and SUBCOMMAND stays empty.
SUBCOMMAND=""
if [ "$FIRST_BIN" = "git" ]; then
  IDX=1
  while [ "$IDX" -lt "${#TOKENS[@]}" ]; do
    TOK="${TOKENS[$IDX]}"
    case "$TOK" in
      -C | -c | --git-dir | --work-tree | --namespace)
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

# --- #883/#886: composed commands always deny, never rewrite or pass -------
#
# Two shapes reach this point with a composed command, and both must
# deny the same way:
#
#   SUBCOMMAND == "push" cleanly, but something follows it (#883): "git
#   push && echo done" and "git push | tee log" tokenize push as a clean
#   word -- the walk above finds it immediately -- yet the classify loop
#   further down has no notion of shell composition and would read the
#   NEXT command's name as a branch argument (measured: rewrote to
#   `legion push --branch echo` / `--branch tee`).
#
#   SUBCOMMAND != "push" (#886): either `push` is glued to a
#   metacharacter with no space (`git push;`, `git push&&echo` tokenize
#   as ONE word -- `read -a` splits on whitespace only), or `git push`
#   is not the first command at all (`git status && git push`, `npm
#   test && git push`). The walk above only ever looks at the FIRST git
#   invocation's immediate subcommand, so both of these left SUBCOMMAND
#   holding something other than exactly `push` and the old strict
#   equality check exited 0 on all of them -- a raw `git push` ran with
#   no audit row, no deny, no rewrite, nothing.
#
# Never attempt to classify or rewrite one segment of a chain --
# `updatedInput` replaces the whole command string, so a per-segment
# rewrite would drop the rest, or worse, misread another command's
# argument as this one's. A compound command that does not mention `git
# push` at all (`git status && echo hi`) is not this hook's concern and
# passes through untouched, same as any other non-push command always
# has.
deny_compound_push() {
  emit_deny "Refusing \`git push\` -- it's composed with something else (a pipe, redirect, \`&&\`, \`;\`, or \`\$(...)\`), and legion's rewrite would replace the WHOLE command string.

Translating it would silently drop everything else in it -- or worse, misread part of it: this hook used to read the NEXT command's name as if it were a branch argument. Run the push and the rest of your pipeline as separate steps:

    legion push --repo ${REPO}

Work-source actions go through legion so they land in the audit log (\`legion audit\`)."
}

if [ "$SUBCOMMAND" = "push" ]; then
  if legion_hook_compound "$COMMAND"; then
    deny_compound_push
    exit 0
  fi
else
  if legion_hook_compound "$COMMAND" \
    && legion_hook_token_present "$COMMAND" git \
    && legion_hook_token_present "$COMMAND" push; then
    deny_compound_push
  fi
  exit 0
fi

# --- Classify the push -------------------------------------------------------
#
# Anything `legion push` cannot express is a DENY, never a silent drop.
# `legion push` has no force path by design (#791), takes no refspec, and
# pushes exactly one branch from the checkout that has it.

BLOCKING_FLAG=""
EXPLICIT_BRANCH=""
POSITIONALS=()

IDX=$((IDX + 1))
while [ "$IDX" -lt "${#TOKENS[@]}" ]; do
  TOK="${TOKENS[$IDX]}"
  case "$TOK" in
    -f | --force | --force-with-lease | --force-if-includes | --force-with-lease=*)
      BLOCKING_FLAG="$TOK"
      break
      ;;
    -d | --delete | --mirror | --tags | --prune | --all)
      BLOCKING_FLAG="$TOK"
      break
      ;;
    *:*)
      # A refspec (src:dst) retargets the remote ref. `legion push` takes
      # a branch name, not a refspec, so this cannot round-trip.
      BLOCKING_FLAG="$TOK (refspec)"
      break
      ;;
    -*)
      # Benign flags legion push either implies or does not need:
      # -u/--set-upstream (always set), -q/-v, --progress.
      ;;
    *)
      POSITIONALS+=("$TOK")
      ;;
  esac
  IDX=$((IDX + 1))
done

REPO_ARG="$REPO"

if [ -n "$BLOCKING_FLAG" ]; then
  emit_deny "Refusing \`git push\` -- and this one cannot be translated.

\`${BLOCKING_FLAG}\` has no equivalent on \`legion push\`, which by design has no force path, takes no refspec, and pushes exactly one branch from the checkout that has it (#791). Rewriting the command for you would silently drop that flag and run something you did not ask for.

If you want a plain push of this branch:
    legion push --repo ${REPO_ARG}

If you reached for force because the branch diverged: an already-pushed branch cannot be rebased under this doctrine. Merge origin/main INTO the branch and re-run the gates on the new HEAD. If that is not workable, close the PR and recreate from a fresh branch -- never a force bypass."
  exit 0
fi

# POSITIONALS are [remote] [branch] in the common `git push origin foo`
# shape. Take the branch only when both are present; a bare `git push` or
# `git push origin` lets `legion push` default to the CWD's branch, which
# is the behaviour we want anyway.
if [ "${#POSITIONALS[@]}" -ge 2 ]; then
  EXPLICIT_BRANCH="${POSITIONALS[1]}"
fi

REWRITTEN="legion push --repo ${REPO_ARG}"
if [ -n "$EXPLICIT_BRANCH" ]; then
  REWRITTEN="${REWRITTEN} --branch ${EXPLICIT_BRANCH}"
fi

emit_rewrite "$REWRITTEN" "Translated your \`git push\` to \`${REWRITTEN}\`.

This is the audited push path (#791). It resolves the checkout that actually has the branch and pushes from there -- the pre-push hook reviews the CWD's checked-out branch, not the ref being pushed, so pushing from the wrong checkout silently reviews the wrong diff. It also refuses main/master and writes an audit row carrying the branch, the resolved checkout, and the head SHA.

That audit row is what binds a gate-verified commit to the artifact that actually reached origin. A raw \`git push\` writes none, so reach for \`legion push\` directly next time rather than relying on this translation." \
  "routed through legion push for the audit trail (#827)"
