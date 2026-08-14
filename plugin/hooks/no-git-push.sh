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

# POSITIONALS are [remote] [ref] in the common `git push origin foo` shape.
# A bare `git push` or `git push origin` lets `legion push` default to the
# CWD's branch, which is the behaviour we want anyway.
#
# When a ref IS named it cannot be classified lexically (#915): `v0.0.79` and
# `feat/thing` are both just words. This hook used to assume branch, so a tag
# push was rewritten into `legion push --branch v0.0.79` and failed -- which
# is what blocked rafters on a release tag. Ask the repository instead.
EXPLICIT_TAG=""
if [ "${#POSITIONALS[@]}" -ge 2 ]; then
  REF="${POSITIONALS[1]}"

  # Resolve in the payload's cwd, not the hook process's -- the ref belongs to
  # the repository the command targets.
  #
  # When the repo cannot be inspected at all, fall through to the pre-#915
  # behaviour (assume branch) rather than denying. Ref resolution is an
  # improvement where it is available, never a new gate: a hook that blocks a
  # legitimate push because it could not look at the repository has made
  # things worse than the bug it was added to fix.
  REF_IS_BRANCH=no
  REF_IS_TAG=no
  REF_RESOLVABLE=no
  if git -C "$CWD" rev-parse --git-dir >/dev/null 2>&1; then
    REF_RESOLVABLE=yes
    git -C "$CWD" rev-parse --verify --quiet "refs/heads/${REF}" >/dev/null 2>&1 && REF_IS_BRANCH=yes
    git -C "$CWD" rev-parse --verify --quiet "refs/tags/${REF}" >/dev/null 2>&1 && REF_IS_TAG=yes
  fi

  if [ "$REF_IS_BRANCH" = yes ] && [ "$REF_IS_TAG" = yes ]; then
    emit_deny "Refusing \`git push\` -- \`${REF}\` is BOTH a branch and a tag in this repository.

git would disambiguate this for you, and which one it picks is not something to rely on when the whole point of this path is knowing what reached origin. Say which you meant:

    legion push --repo ${REPO_ARG} --branch ${REF}
    legion push --repo ${REPO_ARG} --tag ${REF}"
    exit 0
  fi

  if [ "$REF_RESOLVABLE" = yes ] && [ "$REF_IS_BRANCH" = no ] && [ "$REF_IS_TAG" = no ]; then
    emit_deny "Refusing \`git push\` -- \`${REF}\` resolves to neither a branch nor a tag here.

Nothing by that name exists in this repository, so the push would either fail or create a ref you did not intend. Check the name, or create the branch/tag first."
    exit 0
  fi

  if [ "$REF_IS_TAG" = yes ]; then
    EXPLICIT_TAG="$REF"
  else
    EXPLICIT_BRANCH="$REF"
  fi
fi

if [ -n "$EXPLICIT_TAG" ]; then
  # The plugin's files and the binary they drive can be different versions --
  # bin/legion is a shim dispatching to the data dir, and that binary is
  # installed by a SessionStart hook, so a session started before an upgrade
  # runs new hooks against an old binary. Rewriting to a flag it does not have
  # would hand the agent a clap error instead of a push. Say what is actually
  # wrong instead.
  if [ -z "${LEGION:-}" ] || ! "$LEGION" push --help 2>/dev/null | grep -q -- '--tag'; then
    emit_deny "Refusing \`git push\` -- \`${EXPLICIT_TAG}\` is a tag, and this legion binary cannot push tags yet.

\`legion push --tag\` (#915) is the sanctioned path, but the installed binary predates it, so translating your command would produce an unknown-argument error rather than a push.

Start a new session to pick up the current binary, then re-run. If you need the tag out now and cannot wait, that is an operator action -- ask rather than routing around the guard."
    exit 0
  fi

  REWRITTEN="legion push --repo ${REPO_ARG} --tag ${EXPLICIT_TAG}"
  emit_rewrite "$REWRITTEN" "Translated your \`git push\` to \`${REWRITTEN}\`.

\`${EXPLICIT_TAG}\` is a tag, not a branch, so this routes to the tag path (#915). Until that path existed there was no sanctioned way to push a tag at all: this hook assumed every positional ref was a branch, and the only thing that worked was a release script shelling out past the guard -- which is exactly why release tags wrote no audit row.

\`legion push --tag\` pushes the fully-qualified \`refs/tags/\` refspec (never the ambiguous bare name), refuses a tag whose commit is not reachable from any branch on origin, and writes an audit row carrying the tag and the sha it points at." \
    "routed through legion push --tag for the audit trail (#915)"
  exit 0
fi

REWRITTEN="legion push --repo ${REPO_ARG}"
if [ -n "$EXPLICIT_BRANCH" ]; then
  REWRITTEN="${REWRITTEN} --branch ${EXPLICIT_BRANCH}"
fi

emit_rewrite "$REWRITTEN" "Translated your \`git push\` to \`${REWRITTEN}\`.

This is the audited push path (#791). It resolves the checkout that actually has the branch and pushes from there -- the pre-push hook reviews the CWD's checked-out branch, not the ref being pushed, so pushing from the wrong checkout silently reviews the wrong diff. It also refuses main/master and writes an audit row carrying the branch, the resolved checkout, and the head SHA.

That audit row is what binds a gate-verified commit to the artifact that actually reached origin. A raw \`git push\` writes none, so reach for \`legion push\` directly next time rather than relying on this translation." \
  "routed through legion push for the audit trail (#827)"
