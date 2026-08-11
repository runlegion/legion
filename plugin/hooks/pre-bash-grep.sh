#!/bin/bash
# Legion PreToolUse hook (#438, #876): when a Bash command starts with a
# search binary (grep|rg|ag|ack|find|fd), or a search spelled as a git
# subcommand (#829), apply this ladder:
#
#   0. REWRITE -- the command is a content search (grep/rg/`git grep`) that
#                 `legion sym etc find-content` can answer LOSSLESSLY:
#                 single (non-compound) invocation, no flag find-content
#                 cannot express, no subdirectory path scoping. Replace the
#                 command via updatedInput (#876) instead of denying --
#                 `git grep` was the live bypass (permissions.deny matches
#                 the first token, which for `git grep` is `git`) and the
#                 old ladder below only advises on symbol-shaped patterns,
#                 silently passing free-text/regex searches straight
#                 through. This tier runs after the bypass tier (state 3
#                 below) so an explicit operator escape still always wins,
#                 and only for grep/rg/`git grep` -- `ag`/`ack` are left
#                 alone (their default gitignore/hidden-file handling isn't
#                 confidently known here) and `find`/`fd` search FILE
#                 NAMES, not content, so they stay on the ladder below.
#                 Anything not confidently classified (unrecognized flag,
#                 subdirectory path argument, the two independent pattern
#                 extractors disagreeing) falls through to the ladder
#                 unchanged rather than guess -- see
#                 `_legion_bashgrep_classify`'s doc comment.
#   1. INJECT  -- repo not indexed or no high-confidence sym hit.
#                 Emit additionalContext with whatever sym found.
#   2. BLOCK   -- repo indexed AND `legion sym def` returned >=1 result.
#                 Block the Bash call; the agent should call sym instead.
#   3. SOFT BYPASS -- LEGION_BYPASS_GREP=1 env or `# legion-bypass:`
#                 sentinel, for free-text searches. REFUSED for
#                 symbol-shaped patterns with a local hit. There is NO
#                 hard escape (#560): mandatory shell-grep blocking is the
#                 operator's permissions.deny, not this hook.
#
# Sibling of pre-grep.sh which covers the Grep and Glob tools -- those
# guard a TOOL, and `updatedInput` rewrites a tool's arguments, not its
# type, so a Grep/Glob call cannot become a Bash call and pre-grep.sh must
# keep denying rather than rewriting. This hook covers Bash, so a rewrite
# is possible. Beyond the REWRITE tier, this hook is SOFT FALLBACK
# GUIDANCE: the mandatory shell-grep block is the operator's settings.json
# permissions.deny (Bash(grep:*)/Bash(rg:*)/...), which is evaluated
# before this hook runs and inherits to subagents. See
# docs/decisions/2026-06-02-grep-blocking-is-operator-permissions.md.
# This hook still injects sym context and refuses the soft bypass on a
# local symbol hit, for repos/operators that have not set the deny rule.
#
# Skip discipline:
# - LEGION_SKIP_PRE_BASH_GREP=1: skip this hook specifically.
# - command does not start with a covered search binary: pass through.
# - repo not legion-covered: pass through (universal hook gate).
# - extracted pattern not symbol-shaped: pass through (regex/free text
#   queries aren't sym lookups).
# - missing legion binary: pass through.
#
# Error handling: every legion invocation appends stderr to
# /tmp/legion-hook-errors.log; the hook always exits 0.

if [ "${LEGION_SKIP_PRE_BASH_GREP:-}" = "1" ]; then
  echo "[legion-pre-bash-grep] skipped (LEGION_SKIP_PRE_BASH_GREP=1)" >&2
  exit 0
fi

# shellcheck source=lib/prelude.sh
source "${CLAUDE_PLUGIN_ROOT:-}/hooks/lib/prelude.sh" 2>/dev/null || exit 0

legion_hook_parse || exit 0

COMMAND=$(legion_hook_field '.tool_input.command')

if [ -z "$CWD" ] || [ "$TOOL" != "Bash" ] || [ -z "$COMMAND" ]; then
  exit 0
fi

if [ -z "$REPO" ]; then
  exit 0
fi

# Source the shared ladder library. It sources _legion-indexed.sh and
# lib/emit.sh transitively.
# shellcheck source=_legion-prequery.sh
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-prequery.sh"

# _legion_bashgrep_safe_head CMD -- echo the portion of CMD up to (not
# including) the first shell chain/pipe/redirect operator (|, ;, >, <, &)
# OUTSIDE single/double quotes, or the whole CMD when no such operator
# exists. Quote-aware so a quoted pattern containing one of those
# characters (`grep -E 'foo|bar' .` -- regex alternation, not a pipe) is
# not truncated.
#
# THIS IS LOAD-BEARING (found via a real reproduction, #876 follow-up): the
# naive `${cmd%%|*}` idiom used elsewhere in this codebase's pattern
# extraction truncates on ANY literal `|`, quoted or not -- for
# `grep -E 'foo|bar' .` it silently produces "grep -E 'foo", and a pattern
# extracted from that truncated head is "foo", not "foo|bar". That is not a
# failure that reads as a failure: the rewritten command runs, returns a
# plausible non-empty result set, and the agent reads it as the answer to
# the question it asked. `_legion_bashgrep_classify` must never derive its
# own pattern this way.
_legion_bashgrep_safe_head() {
  local cmd="$1"
  local i=0 len=${#cmd}
  local in_single=0 in_double=0 c
  while [ "$i" -lt "$len" ]; do
    c="${cmd:$i:1}"
    if [ "$in_single" -eq 1 ]; then
      [ "$c" = "'" ] && in_single=0
    elif [ "$in_double" -eq 1 ]; then
      [ "$c" = '"' ] && in_double=0
    else
      case "$c" in
        "'") in_single=1 ;;
        '"') in_double=1 ;;
        '|' | ';' | '>' | '<' | '&')
          printf '%s' "${cmd:0:$i}"
          return 0
          ;;
      esac
    fi
    i=$((i + 1))
  done
  printf '%s' "$cmd"
}

# _legion_bashgrep_is_compound CMD -- true (0) when CMD contains a shell
# chain/pipe/redirect operator OUTSIDE single/double quotes. A wrong
# rewrite inside a pipeline is worse than no rewrite, so callers skip the
# REWRITE tier entirely when this returns true.
_legion_bashgrep_is_compound() {
  local cmd="$1"
  local head
  head="$(_legion_bashgrep_safe_head "$cmd")"
  [ "${#head}" -lt "${#cmd}" ]
}

# _legion_bashgrep_classify CMD BINARY REPO -- attempt a lossless rewrite of
# a grep-shaped Bash command (grep/rg/`git grep`) to
# `legion sym etc find-content`. Sets, on return:
#
#   RW_CMD  -- non-empty: the rewritten command (success).
#   RW_DENY -- non-empty: a specific flag find-content cannot express, with
#              the reason. The caller denies rather than silently dropping
#              the flag and running something not asked for.
#
# BOTH EMPTY means "could not confidently classify" -- the caller falls
# through to the existing ladder rather than guess. This covers: an
# unrecognized flag, a subdirectory path argument (find-content has no
# path-scoping flag, only --repo/--ext -- narrowing would search more than
# asked, so this is treated the same as an unrecognized flag rather than a
# named deny, so the ladder's better BLOCK-tier answer for a genuinely
# local symbol -- `sym def`, which is not path-scoped at all -- still
# fires), and a mismatch between this function's own pattern extraction and
# the hook's sanctioned `legion_prequery_extract_pattern` /
# `legion_prequery_git_pattern` (the caller checks this; two independent
# extractors disagreeing is a signal to not guess which one is right).
_legion_bashgrep_classify() {
  local cmd="$1" binary="$2" repo="$3"
  RW_CMD=""
  RW_DENY=""
  RW_PATTERN=""

  local head
  head="$(_legion_bashgrep_safe_head "$cmd")"

  local toks=()
  IFS=' ' read -ra toks <<<"$head"

  local start=1
  if [ "$binary" = "git grep" ]; then
    local gi=1
    while [ "$gi" -lt "${#toks[@]}" ]; do
      case "${toks[$gi]}" in
        -C | -c | --git-dir | --work-tree | --namespace) gi=$((gi + 2)) ;;
        -*) gi=$((gi + 1)) ;;
        *) break ;;
      esac
    done
    start=$((gi + 1))
  fi

  local pattern="" pattern_found=0
  local path_args=()
  local fixed_strings=0 ignore_case=0 ext=""
  local seen_dashdash=0
  local i="$start" t val
  while [ "$i" -lt "${#toks[@]}" ]; do
    t="${toks[$i]}"
    if [ "$seen_dashdash" -eq 1 ]; then
      # `git grep` convention: `--` separates the search args from the
      # PATHSPEC that follows (pattern already found). Plain grep/rg
      # convention: `--` just ends flag parsing -- the first positional
      # after it is still the pattern if none was seen yet.
      if [ "$binary" = "git grep" ] || [ "$pattern_found" -eq 1 ]; then
        path_args+=("${t//[\"\']/}")
      else
        pattern="${t//[\"\']/}"
        pattern_found=1
      fi
      i=$((i + 1))
      continue
    fi
    case "$t" in
      --)
        seen_dashdash=1
        ;;
      -e | --regexp)
        i=$((i + 1))
        if [ "$i" -lt "${#toks[@]}" ]; then
          pattern="${toks[$i]//[\"\']/}"
          pattern_found=1
        fi
        ;;
      --regexp=*)
        pattern="${t#--regexp=}"
        pattern="${pattern//[\"\']/}"
        pattern_found=1
        ;;
      -r | -R | --recursive | -n | --line-number | -H | --with-filename | \
        -E | --extended-regexp | --color | --color=* | --colour | --colour=*)
        : # find-content already behaves this way -- no-op
        ;;
      -F | --fixed-strings | -Q | --literal)
        fixed_strings=1
        ;;
      -i | --ignore-case)
        ignore_case=1
        ;;
      --include=*)
        val="${t#--include=}"
        val="${val//[\"\']/}"
        case "$val" in
          '*.'*)
            local extval="${val#\*.}"
            case "$extval" in
              '' | *[!a-zA-Z0-9_]*)
                # Empty, or anything beyond a bare extension (another dot,
                # a directory separator, more glob syntax) -- not a single
                # simple extension. --ext takes exactly one extension.
                RW_DENY="\`$t\` -- find-content only supports a single-extension filter (--ext: one extension, no directory globs)"
                return 0
                ;;
              *)
                ext="$extval"
                ;;
            esac
            ;;
          *)
            RW_DENY="\`$t\` -- find-content only supports a single-extension filter (--ext: one extension, no directory globs)"
            return 0
            ;;
        esac
        ;;
      --exclude*)
        RW_DENY="\`$t\` -- find-content has no exclude filter"
        return 0
        ;;
      -A* | --after-context* | -B* | --before-context* | -C* | --context*)
        RW_DENY="\`$t\` (context lines) -- find-content returns matched lines only, no surrounding-context option"
        return 0
        ;;
      -l | --files-with-matches)
        RW_DENY="\`$t\` -- find-content returns per-line hits, not a files-only list"
        return 0
        ;;
      -c | --count)
        RW_DENY="\`$t\` -- find-content has no count mode"
        return 0
        ;;
      -o | --only-matching)
        RW_DENY="\`$t\` -- find-content prints the whole matching line, not just the matched substring"
        return 0
        ;;
      -v | --invert-match)
        RW_DENY="\`$t\` -- find-content has no invert-match mode"
        return 0
        ;;
      -w | --word-regexp | -x | --line-regexp)
        RW_DENY="\`$t\` -- find-content has no boundary-anchoring flag; silently rewriting the pattern would change what you asked for"
        return 0
        ;;
      -P | --perl-regexp)
        RW_DENY="\`$t\` -- find-content's regex engine (Rust regex) does not support PCRE lookaround/backreferences"
        return 0
        ;;
      -[a-zA-Z][a-zA-Z0-9]*)
        # Combined short-flag cluster (`-cE`, `-rn`, `-Fi`, `-A3`, ...).
        # Decompose and classify each letter; a digit is a numeric value
        # attached to a preceding A/B/C and carries no meaning of its own.
        local cluster="${t#-}" cj ch cluster_lossy="" cluster_unknown=0
        for ((cj = 0; cj < ${#cluster}; cj++)); do
          ch="${cluster:$cj:1}"
          case "$ch" in
            [0-9]) ;;
            r | R | n | H | E) ;;
            F | Q) fixed_strings=1 ;;
            i) ignore_case=1 ;;
            A | B | C) cluster_lossy="-$ch (context lines) -- find-content returns matched lines only, no surrounding-context option" ;;
            l) cluster_lossy="-l -- find-content returns per-line hits, not a files-only list" ;;
            c) cluster_lossy="-c -- find-content has no count mode" ;;
            o) cluster_lossy="-o -- find-content prints the whole matching line, not just the matched substring" ;;
            v) cluster_lossy="-v -- find-content has no invert-match mode" ;;
            w | x) cluster_lossy="-$ch -- find-content has no boundary-anchoring flag; silently rewriting the pattern would change what you asked for" ;;
            P) cluster_lossy="-P -- find-content's regex engine (Rust regex) does not support PCRE lookaround/backreferences" ;;
            *) cluster_unknown=1 ;;
          esac
        done
        if [ "$cluster_unknown" -eq 1 ]; then
          return 0
        fi
        if [ -n "$cluster_lossy" ]; then
          RW_DENY="\`$t\` (${cluster_lossy})"
          return 0
        fi
        ;;
      -*)
        # Unrecognized flag -- do not guess.
        return 0
        ;;
      *)
        if [ "$pattern_found" -eq 0 ]; then
          pattern="${t//[\"\']/}"
          pattern_found=1
        else
          path_args+=("${t//[\"\']/}")
        fi
        ;;
    esac
    i=$((i + 1))
  done

  [ -n "$pattern" ] || return 0

  # -i combined with -F/--fixed-strings has no expressible equivalent --
  # find-content's --fixed-strings has no case-insensitive companion flag.
  if [ "$fixed_strings" -eq 1 ] && [ "$ignore_case" -eq 1 ]; then
    RW_DENY="-i (combined with -F/--fixed-strings) -- find-content's --fixed-strings has no case-insensitive companion"
    return 0
  fi

  # A subdirectory path argument cannot be expressed -- find-content has
  # no path-scoping flag, only --repo (whole repo) and --ext (extension).
  # Not a named deny (see the function doc comment): fall through
  # unclassified so the BLOCK tier's `sym def` answer, which is not
  # path-scoped at all, still fires for a genuinely local symbol.
  local p
  for p in "${path_args[@]}"; do
    case "$p" in
      . | ./) ;;
      *) return 0 ;;
    esac
  done

  RW_PATTERN="$pattern"

  if [ "$ignore_case" -eq 1 ]; then
    pattern="(?i)${pattern}"
  fi

  local cmd_arr=(legion sym etc find-content "$pattern" --repo "$repo")
  if [ "$binary" = "git grep" ]; then
    # find-content excludes hidden paths by default; `git grep` sees
    # tracked dotfiles (.github/, .claude/, ...). Added for parity, never
    # --no-ignore -- that flag's own help text warns it can surface
    # gitignored secrets (.env), which `git grep` would never see either.
    cmd_arr+=(--hidden)
  fi
  if [ -n "$ext" ]; then
    cmd_arr+=(--ext "$ext")
  fi
  if [ "$fixed_strings" -eq 1 ]; then
    cmd_arr+=(--fixed-strings)
  fi

  local part rendered=""
  for part in "${cmd_arr[@]}"; do
    rendered="${rendered}$(printf '%q' "$part") "
  done
  RW_CMD="${rendered% }"
}

# Universal gate: skip uncovered repos.
legion_hook_covered || exit 0

# Detect leading search binary; pass through if none.
BINARY=$(legion_prequery_bash_binary "$COMMAND")
if [ -z "$BINARY" ]; then
  exit 0
fi

# Extract the pattern. Empty extraction means we couldn't isolate one;
# pass through rather than guessing. Git-spelled searches (#829) put the
# pattern in a different argv position per shape, so they use their own
# extractor.
if legion_prequery_is_git_shape "$BINARY"; then
  PATTERN=$(legion_prequery_git_pattern "$COMMAND" "$BINARY")
else
  PATTERN=$(legion_prequery_extract_pattern "$COMMAND" "$BINARY")
fi
if [ -z "$PATTERN" ]; then
  exit 0
fi

# `git log --grep` searches COMMIT MESSAGES, which sym does not index.
# Recognizing it matters -- an unrecognized shape is invisible to the
# bypass telemetry that grades whether the sanctioned surface answers
# what agents actually ask (#713/#704). But blocking it, or redirecting
# it to a sym command that structurally cannot serve it, is the exact
# failure this ladder exists to avoid: refusing a query the sanctioned
# surface has no answer for. Record the shape, then pass through.
if [ "$BINARY" = "git log --grep" ]; then
  legion_prequery_record_bypass \
    "$REPO" "$SESSION_ID" "Bash" "$PATTERN" "git-log-grep: commit messages are not sym-indexed" \
    "false" "false"
  exit 0
fi

# Probe sym before any decision so the bypass row carries the
# `had_sym_hits` signal that #440's summary will need.
HITS=$(legion_prequery_sym_def "$PATTERN")
HAD_SYM="false"
if [ -n "$HITS" ]; then
  HAD_SYM="true"
fi

# Pre-compute LOCAL_HITS so both the bypass refusal path and the
# State 2 BLOCK path can use them.
LOCAL_HITS=""
if [ "$HAD_SYM" = "true" ] && legion_indexed "$SESSION_ID" "$REPO"; then
  LOCAL_HITS=$(legion_prequery_filter_hits_local "$HITS" "$REPO")
fi

# State 3: bypass. Two tiers.
#
# Soft bypass (`# legion-bypass: <reason>` or LEGION_BYPASS_GREP=1) is
# REFUSED when the pattern resolves to a real symbol in THIS repo's
# SCIP index. The bypass sentinel exists for free-text searches; it
# cannot route around sym for symbol queries dressed up as text. The
# refusal points the agent at sym. There is NO env-var hard escape: a
# frictionless LEGION_BYPASS_GREP_HARD only made enforcement optional
# (#560). Mandatory shell-grep blocking is the operator's permissions.deny
# (docs/decisions/2026-06-02-grep-blocking-is-operator-permissions.md); this
# hook is soft fallback guidance for operators who have not set the deny rule.
BYPASS_REASON=$(legion_prequery_bypass_reason "$COMMAND")

if [ -n "$BYPASS_REASON" ]; then
  # Refuse the soft bypass if the pattern matches a real local symbol.
  if [ -n "$LOCAL_HITS" ] && [ "$LOCAL_HITS" != "[]" ]; then
    REASON="Soft bypass refused: \`${PATTERN}\` resolves to a symbol in this repo's SCIP index. Use \`legion sym def ${PATTERN} --repo ${REPO}\` (or \`sym refs\` / \`sym hover\`) instead. The \`# legion-bypass:\` sentinel exists for free-text searches; it cannot route around sym for symbol queries.

\`legion sym def ${PATTERN} --repo ${REPO}\` returned:

\`\`\`json
${LOCAL_HITS}
\`\`\`

For symbols, \`legion sym def ${PATTERN}\` / \`sym refs\` / \`sym list\` answer in bytes -- sym covers every indexed language, not just Rust. For non-symbol shapes: \`legion sym etc find-content '${PATTERN}' --repo ${REPO}\` (exact/regex content, the sanctioned grep), \`legion sym tree --repo ${REPO}\` (file/dir structure), \`legion sym etc extract <path> --field <field>\` (one config/frontmatter value without a full read), \`legion sym etc find-file '${PATTERN}' --repo ${REPO}\` (locate a file by name/role). Reach for the Grep tool only if none of those answer it. Your operator may block shell ${BINARY} outright via permissions.deny; that is the intended mandatory gate, and there is no env-var escape from it."
    emit_deny "$REASON"
    exit 0
  fi
  # Soft bypass allowed: pattern is free-text or has no local symbol hits.
  legion_prequery_record_bypass \
    "$REPO" "$SESSION_ID" "Bash" "$PATTERN" "$BYPASS_REASON" \
    "$HAD_SYM" "false"
  exit 0
fi

# --- REWRITE tier (#876) ----------------------------------------------------
#
# grep/rg/`git grep` are content-search shapes `legion sym etc find-content`
# can answer exactly (#707). Runs after the bypass tier above (an explicit
# operator escape always wins) and before the INJECT/BLOCK ladder below (so
# it covers BOTH the free-text patterns that ladder silently passes through
# at line ~148, AND the symbol-shaped patterns it would otherwise BLOCK --
# converting the deny into a working answer is the point of #876). Only a
# single, non-compound invocation is attempted -- a wrong rewrite inside a
# pipeline is worse than no rewrite.
case "$BINARY" in
  grep | rg | "git grep")
    if ! _legion_bashgrep_is_compound "$COMMAND"; then
      _legion_bashgrep_classify "$COMMAND" "$BINARY" "$REPO"
      # The hook's own sanctioned extractor and this classifier's
      # independent re-derivation must agree on the pattern. A mismatch
      # means the two parses diverged somewhere -- do not guess which one
      # is right, fall through to the existing ladder unchanged.
      if [ -n "$RW_CMD" ] && [ "$RW_PATTERN" != "$PATTERN" ]; then
        RW_CMD=""
      fi
      if [ -n "$RW_CMD" ]; then
        SCOPE_NOTE=""
        case "$BINARY" in
          grep)
            SCOPE_NOTE="

Note: unlike a raw \`grep -r\`, this does not see gitignored files or dotfiles (.github/, .env, etc.) by default -- legion will not silently widen a content search into gitignored territory. If you specifically need those, ask your operator."
            ;;
          "git grep")
            SCOPE_NOTE="

\`--hidden\` was added so tracked dotfiles (.github/, .claude/, etc.) that \`git grep\` sees are not silently dropped -- find-content excludes hidden paths by default. Gitignored files stay out of scope either way, same as \`git grep\`."
            ;;
        esac
        CTX="Translated your \`${BINARY}\` search to \`${RW_CMD}\`.

This is the sanctioned content-search path (#707) -- the same one the shell-grep block already points you to, run for you instead of denied.${SCOPE_NOTE}"
        emit_rewrite "$RW_CMD" "$CTX" "routed through legion sym etc find-content (#876)"
        exit 0
      fi
      if [ -n "$RW_DENY" ]; then
        REASON="Refusing to auto-translate this \`${BINARY}\` search to \`legion sym etc find-content\` -- ${RW_DENY}.

Rewriting anyway would silently drop that and hand you results that do not answer what you asked, which is worse than refusing. If the plain search answers your question: \`legion sym etc find-content '${PATTERN}' --repo ${REPO}\`. Otherwise this shape is not on the sanctioned surface yet -- ask your operator, or use \`LEGION_BYPASS_GREP=1\` / \`# legion-bypass: <reason>\` for a one-off (refused for symbol-shaped patterns that resolve locally in this repo's SCIP index)."
        emit_deny "$REASON"
        exit 0
      fi
      # Unclassifiable (unrecognized flag, subdirectory path, pattern
      # mismatch) -- fall through to the existing ladder rather than guess.
    fi
    ;;
esac

# No hits: state 1 INJECT path is empty -- nothing to inject. Pass through
# silently rather than emit a content-free additionalContext block (which
# would just bill cache_read for "no hits").
if [ "$HAD_SYM" != "true" ]; then
  exit 0
fi

# State 2 BLOCK: only when the repo actually has an index to redirect to.
# In an unindexed repo, the agent has no sym alternative, so we keep the
# softer State 1 (inject + nudge) shape.
if legion_indexed "$SESSION_ID" "$REPO"; then
  # LOCAL_HITS was pre-computed above for the bypass refusal path; reuse
  # the value here rather than calling the relevance gate twice.
  # Relevance gate (#458): cluster-wide sym hits in unrelated repos are
  # not a useful redirect for a grep targeting THIS repo. Common
  # dictionary words like `name`, `data`, `value`, `type`, `id` are
  # symbol-shaped and exist as identifiers in every codebase, but a
  # grep on the operator's TOML config in legion's repo should not be
  # blocked because huttspawn has a variable named `name`. The
  # pre-computed LOCAL_HITS reflects that filter.
  if [ -z "$LOCAL_HITS" ] || [ "$LOCAL_HITS" = "[]" ]; then
    # No relevant hits in this repo's index. Fall through to inject
    # below -- the cluster-wide hits may still be useful context but
    # don't justify blocking.
    :
  else
    REASON="Use \`legion sym def ${PATTERN} --repo ${REPO}\` -- it answered this in bytes from the SCIP index. Bash ${BINARY} on \`${PATTERN}\` would scan files and bill cache_read.

\`legion sym def ${PATTERN} --repo ${REPO}\` returned:

\`\`\`json
${LOCAL_HITS}
\`\`\`

The soft bypass (\`# legion-bypass: <reason>\` or LEGION_BYPASS_GREP=1) is REFUSED for symbol-shaped patterns that resolve in this repo's SCIP index -- the sentinel exists for free-text searches, not for symbol queries dressed up as text. For symbols use \`legion sym def ${PATTERN}\` / \`sym refs\` / \`sym list\` -- sym covers every indexed language, not just Rust. For non-symbol shapes: \`legion sym etc find-content '${PATTERN}' --repo ${REPO}\` (content), \`legion sym tree --repo ${REPO}\` (structure), \`legion sym etc extract <path> --field <field>\` (a config/frontmatter value), \`legion sym etc find-file '${PATTERN}' --repo ${REPO}\` (locate by name/role). Reach for the Grep tool only if none of those answer it. There is no env-var hard escape -- the mandatory shell-grep block is the operator's permissions.deny."
    emit_deny "$REASON"
    exit 0
  fi
fi

# State 1 INJECT: not indexed but we did find something via the cluster
# (sym pulls from every stored index, not just the current repo). Emit
# the hits as additionalContext so the agent can decide.
CTX="## Legion sym for \`${PATTERN}\` (Bash ${BINARY})

\`legion sym def ${PATTERN}\` returned (from other repos' indexes -- ${REPO} has no local SCIP index yet, so the block tier is disabled here):

\`\`\`json
${HITS}
\`\`\`

For ${REPO} itself, non-symbol shapes still have a sanctioned answer that needs no SCIP index: \`legion sym etc find-content '${PATTERN}' --repo ${REPO}\` (content), \`legion sym tree --repo ${REPO}\` (structure), \`legion sym etc find-file '${PATTERN}' --repo ${REPO}\` (locate by name). Run \`legion index ${REPO}\` to get a real local \`sym def\` answer too."
emit_allow "$CTX" "legion sym/recall results injected"
