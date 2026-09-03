#!/bin/bash
# Shared hook preamble (#614). Every hook used to longhand-copy the
# stdin-read / jq-extract / repo-resolve / coverage-gate block, and the
# copies had diverged (9 hooks honored LEGION_REPO, 7 did not). This file
# decides those questions ONCE.
#
# Usage from a hook script:
#
#   # shellcheck source=lib/prelude.sh
#   source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0
#
#   legion_hook_parse || exit 0     # sets INPUT, CWD, TOOL, SESSION_ID, REPO
#   [ -n "$REPO" ] || exit 0
#   legion_hook_covered || exit 0   # universal enforcement gate (#353)
#
# The `|| exit 0` on the source line keeps hooks fail-open when the plugin
# root is missing or half-installed: a broken prelude must never block a
# tool call.
#
# Repo identity (the single decision point): the LEGION_REPO env var takes
# precedence over basename($CWD), in EVERY hook. An operator setting
# LEGION_REPO must get the same repo identity from enforcement, lifecycle,
# and memory hooks alike -- per-hook divergence here meant enforcement
# could apply to one repo set and memory to another (#614 audit).
#
# Binary resolution: ${CLAUDE_PLUGIN_ROOT}/bin/legion first, PATH lookup
# second. Hook subshells do not inherit the plugin bin dir on PATH -- only
# the Bash tool does (#204) -- so the plugin-root path must lead; the PATH
# fallback covers system-wide installs where the plugin copy is absent
# (the setup-binary.sh pattern). The LEGION_BIN env var overrides both
# (operator escape + test seam). $LEGION is empty when nothing resolves;
# callers check [ -x "$LEGION" ] and fail open.
#
# Skip/bypass env-var tiers (the closed set, documented once):
#   LEGION_SKIP_<HOOK>=1 -- disable one hook entirely, silently
#   LEGION_BYPASS_<X>=1  -- telemetried escape from a block tier
#   LEGION_NO_<FEATURE>=1 -- disable a feature (sync, daemon), not a hook
#
# ---------------------------------------------------------------------------
# THE BOUNDARY (#860). Read this before you write a hook that denies anything.
#
# Claude-layer hooks fire on the AGENT'S TOOL CALL ONLY. They never see child
# processes. A script, a Makefile, a test suite, a `bash -c` wrapper, or any
# tool that shells out executes unimpeded. The harness's own permissions.deny
# has the same reach: it is matched against the command the agent submits, not
# against what that command goes on to spawn.
#
# Verified 2026-08-04, twice. A shell script containing `gh --version` ran with
# no interception although a direct `gh` call is denied by no-gh.sh; and a test
# fixture's `git push -u origin main` reached GitHub and rewrote main with no
# Claude-layer interception at all (incident writeup: reflection 019fce1a).
#
# This is REQUIRED by design, not a bug awaiting a patch -- see no-git-push.sh's
# header. The alternative, a PATH shim over `git`, would recurse, because legion
# is itself a git consumer. Interception has to happen at the agent's tool call,
# and the agent's tool call is exactly one process deep.
#
# CONSEQUENCE: hooks SHAPE AGENT BEHAVIOUR. They are not an enforcement
# boundary. Anything that must be TOTAL lives at the git layer (.githooks/*),
# in the binary (e.g. REFUSED_BRANCHES in src/cli/push.rs), or in remote branch
# protection. A hook may front such a rule with a better error message; it can
# never be the rule.
#
# COROLLARY: bypass.jsonl cannot record what the hooks never saw, so a
# script-shaped escape writes no row. Absence of bypass rows is not absence of
# bypass.
#
# Every guard in this directory carries an ADVISORY / MUST-BE-TOTAL verdict, and
# each MUST-BE-TOTAL row names the layer that actually enforces it (or says that
# nothing does). New hook that denies something: add its row.
#   -> plugin/hooks/README.md
# ---------------------------------------------------------------------------

# Double-source guard.
if [ -n "${LEGION_PRELUDE_SOURCED:-}" ]; then
  return 0
fi
LEGION_PRELUDE_SOURCED=1

LEGION_HOOK_LOG=/tmp/legion-hook-errors.log
export LEGION_HOOK_LOG

# legion_resolve_bin -- echo the resolved legion binary path, empty when
# nothing resolves. LEGION_BIN (resolved through command -v so both bare
# names and absolute paths work) > plugin-root copy > PATH lookup.
legion_resolve_bin() {
  if [ -n "${LEGION_BIN:-}" ]; then
    command -v "$LEGION_BIN" 2>/dev/null || true
    return 0
  fi
  if [ -x "${CLAUDE_PLUGIN_ROOT:-}/bin/legion" ]; then
    printf '%s\n' "${CLAUDE_PLUGIN_ROOT:-}/bin/legion"
    return 0
  fi
  command -v legion 2>/dev/null || true
}

LEGION=$(legion_resolve_bin)

# legion_hook_parse -- read the hook event JSON from stdin and extract the
# common fields into INPUT, CWD, TOOL, SESSION_ID, REPO. Returns 1 on
# empty stdin so callers can `legion_hook_parse || exit 0`. jq failures
# leave the fields empty (fail-open).
legion_hook_parse() {
  INPUT=$(cat)
  CWD=""
  TOOL=""
  SESSION_ID=""
  REPO=""
  if [ -z "$INPUT" ]; then
    return 1
  fi
  CWD=$(echo "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
  # TOOL is part of the hook-facing API (consumed by sourcing hooks).
  # shellcheck disable=SC2034
  TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
  SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)
  if [ -n "${LEGION_REPO:-}" ]; then
    REPO="$LEGION_REPO"
  elif [ -n "$CWD" ]; then
    REPO=$(basename "$CWD")
  fi
  return 0
}

# legion_hook_field JQ_PATH -- extract one field from the parsed event,
# e.g. legion_hook_field '.tool_input.command'. Empty on miss/jq failure.
legion_hook_field() {
  echo "$INPUT" | jq -r "${1} // empty" 2>/dev/null
}

# legion_hash_str STR -- portable md5 of a string (+ trailing newline, to
# stay byte-compatible with the historical `echo | md5` marker paths).
# Used for the /tmp marker-file protocol (work/reflected markers, debounce
# locks, dedup keys); writers and readers MUST hash identically.
legion_hash_str() {
  printf '%s\n' "$1" | md5 -q 2>/dev/null \
    || printf '%s\n' "$1" | md5sum 2>/dev/null | cut -d' ' -f1
}

# legion_hook_compound COMMAND -- true (0) when COMMAND is composed with
# something else: a pipe, `&`/`&&`, `;`, a redirect, `$(...)`, a backtick,
# or an embedded newline. Shared by every hook that can emit a REWRITE
# (no-gh.sh, no-git-push.sh) so the check cannot drift between them.
#
# updatedInput.command replaces the tool's ENTIRE command string, and the
# tokenizer each of those hooks uses to find its subcommand has no notion
# of shell composition. Measured against no-git-push.sh before this
# existed: `git push && echo done` rewrote to `legion push --branch echo`
# and `git push | tee /tmp/log` rewrote to `... --branch tee` -- the
# classify loop read the NEXT command's name as a branch argument (#883).
# A composed command must always refuse the rewrite path, never attempt
# to translate part of it.
legion_hook_compound() {
  # shellcheck disable=SC2016 # case glob patterns, not expansions -- the
  # literal chars are what we're matching, single/ANSI-C quoting is correct.
  case "$1" in
    *'|'* | *'&'* | *';'* | *'>'* | *'<'* | *'$('* | *'`'* | *$'\n'*)
      return 0
      ;;
  esac
  return 1
}

# legion_hook_token_present COMMAND WORD -- true (0) when WORD appears
# anywhere in COMMAND as a whitespace-delimited token, after stripping
# (a) any leading/trailing run of shell metacharacters glued onto that
# token with no space (";git", "push;", "&&push" all still match "git"/
# "push") and (b) the token's directory prefix, so an absolute-path
# invocation matches the same way a bare command does.
#
# Used for compound commands, where the single-command "walk to the next
# subcommand and classify it" approach these hooks use for a plain
# invocation cannot safely apply: `updatedInput` replaces the WHOLE
# command string, so a hook that can rewrite or deny must be able to
# notice the guarded binary/verb ANYWHERE in a chain, not only as the
# very first word. Before this existed, `git status && git push` and
# `echo hi && gh pr merge 123` both silently passed through their guard
# hooks -- each hook's detection walk looked only at the first token, so
# a leading unrelated command (or the guarded verb glued to an operator)
# made the whole command invisible to it (#886).
#
# Deliberately coarse: it does not verify the matched token's ROLE in
# the command (e.g. that "commit" immediately follows a "git" token,
# rather than sitting inside some unrelated quoted string). A
# false-positive costs the agent one extra manual step; a false-negative
# silently skips the audit trail these hooks exist to guarantee, which is
# the worse failure -- the same reasoning documented on
# legion_hook_compound above and on every DENY branch in no-git-push.sh.
legion_hook_token_present() {
  local command="$1" word="$2" tok stripped
  local -a toks
  read -r -a toks <<<"$command"
  for tok in "${toks[@]}"; do
    stripped="$tok"
    # Strip a leading run of glued metacharacters (";git" -> "git"), and
    # a leading backslash escape (see legion_hook_strip_escape below --
    # `\gh` inside a compound chain is the same escape class, same fix).
    while :; do
      case "$stripped" in
        [\;\&\|\<\>\$\`\\]*) stripped="${stripped:1}" ;;
        *) break ;;
      esac
    done
    # Strip a trailing run of glued metacharacters ("push;" -> "push").
    stripped="${stripped%%[\;\&\|\<\>\$\`]*}"
    if [ "${stripped##*/}" = "$word" ]; then
      return 0
    fi
  done
  return 1
}

# legion_hook_strip_escape WORD -- echo WORD with a single leading
# backslash removed. `\gh` is the ordinary "skip alias/function lookup,
# run the literal command" shell escape -- once bash tokenizes it, the
# backslash is not part of the argv0 that actually execs, so a guard
# comparing raw first-token bytes silently misses it the same way an
# absolute path was silently missed before basename-stripping was added
# (#1117). Safe to normalize away unconditionally: it is a spelling
# variant of the SAME invocation, not a wrapper that drops information,
# so callers may treat the normalized form exactly like the bare word
# (rewrite-eligible, not deny-only -- contrast legion_hook_wrapped_call).
legion_hook_strip_escape() {
  case "$1" in
    \\*) printf '%s' "${1#\\}" ;;
    *) printf '%s' "$1" ;;
  esac
}

# legion_hook_first_bin COMMAND -- echo the basename of COMMAND's first
# whitespace-separated token, after stripping a leading backslash escape.
# Shared so no-gh.sh / no-git-commit.sh / no-git-push.sh compute their
# FIRST_BIN identically -- they used to each inline this (#1117: `\gh`
# bypassed all three, because none of them stripped the escape before
# comparing basenames).
legion_hook_first_bin() {
  local trimmed token
  trimmed="${1#"${1%%[![:space:]]*}"}"
  token="${trimmed%%[[:space:]]*}"
  token="$(legion_hook_strip_escape "$token")"
  printf '%s' "${token##*/}"
}

# legion_hook_wrapper_marker TOKEN -- true (0) when TOKEN is a wrapper
# prefix: a bare VAR=val assignment (identifier before the first `=`), or
# one of the wrapper binaries that exec their remaining argv as a child
# process (env, sudo, timeout, nice, xargs, command, exec, time).
# Matched by basename after stripping a leading backslash escape, same
# as legion_hook_first_bin, so `\env gh ...` counts too. Used only to
# decide whether a command's FIRST token puts it in the wrapper shape --
# see legion_hook_wrapped_call.
legion_hook_wrapper_marker() {
  local tok base name
  tok="$(legion_hook_strip_escape "$1")"
  base="${tok##*/}"
  case "$base" in
    *=*)
      name="${base%%=*}"
      case "$name" in
        '' | [!A-Za-z_]* | *[!A-Za-z0-9_]*) return 1 ;;
        *) return 0 ;;
      esac
      ;;
    env | sudo | timeout | nice | xargs | command | exec | time)
      return 0
      ;;
  esac
  return 1
}

# legion_hook_wrapped_call COMMAND BINARY -- true (0) when COMMAND is a
# SIMPLE (non-compound) command that reaches BINARY through a wrapper
# prefix rather than calling it directly: `env X=1 gh ...`, `sudo -u x gh
# ...`, `timeout 5 gh ...`, chained (`env X=1 timeout 5 gh ...`), or a
# bare `VAR=val` assignment prefix (`X=1 gh ...`). None of these are
# shell metacharacters, so legion_hook_compound -- which only fires on
# `| & ; > < $( backtick newline` -- never sees them, and a guard's
# first-token basename check walks straight past: FIRST_BIN is `env` or
# `sudo` or `X=1`, never `gh` (#1117; no-gh.sh:48 named this exact gap as
# the motivation for the compound check and it went uncovered anyway).
#
# Deliberately requires the FIRST token to be a wrapper marker -- that is
# what distinguishes this from legion_hook_token_present (which answers
# "does BINARY appear anywhere" and is used for compound chains, already
# denied wholesale). Once the wrapper shape is confirmed, every remaining
# token is scanned for BINARY by basename (after stripping a backslash
# escape too), deliberately coarse the same way legion_hook_token_present
# is: it does not attempt to parse the wrapper's own flags (`sudo -u
# nobody`, `timeout 5`, `nice -n 5`), because a false positive costs the
# agent one extra manual step and a false negative silently skips the
# audit trail this whole file exists to guarantee.
#
# Callers MUST deny on a match here, never rewrite: rewriting `env X=1 gh
# pr view 1` into `legion pr view 1` would silently drop the environment
# assignment the agent explicitly asked to run under, and a hook cannot
# know whether that assignment mattered.
legion_hook_wrapped_call() {
  local command="$1" binary="$2" tok base
  local -a toks
  read -r -a toks <<<"$command"
  [ "${#toks[@]}" -ge 1 ] || return 1
  legion_hook_wrapper_marker "${toks[0]}" || return 1
  for tok in "${toks[@]}"; do
    base="$(legion_hook_strip_escape "$tok")"
    base="${base##*/}"
    if [ "$base" = "$binary" ]; then
      return 0
    fi
  done
  return 1
}

# Coverage gate (#353). Sourced here so every hook shares one probe; the
# declare -F guard tolerates hooks that sourced it themselves.
if ! declare -F legion_covered >/dev/null 2>&1; then
  # shellcheck source=../_legion-covered.sh
  source "${CLAUDE_PLUGIN_ROOT:-}/hooks/_legion-covered.sh"
fi

# legion_hook_covered -- the universal enforcement gate over the parsed
# event: exit-0 passthrough territory when the repo is not legion-covered.
# Hooks opt IN by calling this; repo-resolution-only hooks (markers,
# task-state mirrors) simply don't call it.
legion_hook_covered() {
  legion_covered "$SESSION_ID" "$REPO"
}
