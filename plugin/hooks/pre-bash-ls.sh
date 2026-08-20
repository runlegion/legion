#!/bin/bash
# Legion PreToolUse hook (#976): when a Bash command lists the tree of an
# indexed repo with `ls`, point at `legion sym tree` / `legion sym list` --
# the same directory-structure question answered from the SCIP index instead
# of a filesystem walk.
#
# `ls` is the Bash twin of the `tree` script primitive pre-script-search.sh
# already catches (#837): a directory listing. So it takes the identical
# posture --
#
#   INJECT, NEVER DENY (doctrine 019fb9a8, ratified across #829/#837).
#
# The reason `ls` injects rather than blocks (unlike pre-bash-grep.sh's grep
# BLOCK tier) is that `ls` is NOT a lossless equivalent of `sym tree`:
# `ls -l` shows sizes/perms/dates, and `ls` shows the non-indexed files
# (README, configs, dotfiles) that live outside the source tree sym knows.
# Refusing a listing that carries information sym cannot reproduce is the
# exact failure the ladder exists to avoid, so this hook only ever suggests
# and always allows the command -- mirroring how pre-script-search.sh handles
# the `tree` shape.
#
# GATE (only intervene when the listing targets THIS repo's indexed tree):
#   - $REPO (LEGION_REPO env, else basename($CWD) -- the prelude's single
#     decision point) must be legion-covered AND indexed. An unindexed repo
#     has no `sym tree` to redirect to, so the hook passes through.
#   - The `ls` TARGET must resolve INSIDE $CWD: bare `ls` (or `ls -flags`)
#     lists $CWD; `ls <relative>` resolves against $CWD; `ls <absolute>` is
#     taken as-is; with several paths, ANY one inside $CWD qualifies. A
#     target that resolves OUTSIDE the tree (`ls /tmp/scratch`, `ls ..`, a
#     config dir) passes through untouched -- sym does not index it.
#
# Repo identity is deliberately $REPO from the prelude, NOT a watch-list
# path->repo resolution: no required behaviour needs to know WHICH repo an
# out-of-tree path belongs to, only whether the target is inside this one's
# tree, and diverging from the prelude's single decision point (#614) would
# reintroduce the LEGION_REPO-vs-basename split that file exists to close.
# The cost is a false negative for `ls /abs/path/to/another/indexed/repo`
# from this cwd -- nothing, under inject-never-deny.
#
# BYPASS + TELEMETRY: honors the shared soft-bypass affordance
# (`# legion-bypass: <reason>` sentinel / LEGION_BYPASS_GREP=1). On a bypass
# the suggestion is suppressed and one `telemetry record-bypass` row is
# written (tool "Bash", pattern = the resolved ls target, reason
# "ls-structure: <reason>") so the sym-under-serving signal is captured for
# `ls` too -- same escape-capturing mechanism grep/find use. The plain
# (non-bypass) inject path writes NO telemetry row: an injected suggestion is
# not an escape, and `ls` fires far too often to record every one without
# flooding bypass.jsonl. See the PR body for the metric consequence.
#
# Skip via LEGION_SKIP_PRE_BASH_LS=1.
#
# Boundary verdict: ADVISORY (inject-only; a script-shaped `ls` escapes it,
# and that is an acceptable cost -- see plugin/hooks/README.md).

if [ "${LEGION_SKIP_PRE_BASH_LS:-}" = "1" ]; then
  echo "[legion-pre-bash-ls] skipped (LEGION_SKIP_PRE_BASH_LS=1)" >&2
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

# shellcheck source=_legion-prequery.sh
source "${CLAUDE_PLUGIN_ROOT}/hooks/_legion-prequery.sh"

# --- ls detection (cheap string check first, before any covered/indexed
# probe) -- pass through the vast majority of Bash calls that are not `ls`. --
LS_TRIMMED="${COMMAND#"${COMMAND%%[![:space:]]*}"}"
LS_TOKS=()
IFS=' ' read -ra LS_TOKS <<<"$LS_TRIMMED"
if [ "${#LS_TOKS[@]}" -eq 0 ]; then
  exit 0
fi
# Resolve by basename so `/bin/ls` is caught the way no-gh.sh resolves gh,
# and so `lsof`/`lsblk` are NOT (basename must equal `ls` exactly).
if [ "${LS_TOKS[0]##*/}" != "ls" ]; then
  exit 0
fi

# Universal coverage gate + index gate. An unindexed repo has no `sym tree`
# to point at, so this stays pass-through there.
legion_hook_covered || exit 0
legion_indexed "$SESSION_ID" "$REPO" || exit 0

# _legion_ls_normalize PATH -- echo PATH with `.`/empty segments dropped and
# `..` segments resolved logically (no filesystem access, so it works for a
# target that may or may not exist). Absolute paths only; the caller joins
# relative targets onto $CWD first.
_legion_ls_normalize() {
  local path="$1"
  local segs=() stack=()
  local oldifs="$IFS"
  IFS=/ read -ra segs <<<"$path"
  IFS="$oldifs"
  local seg
  for seg in "${segs[@]}"; do
    case "$seg" in
      '' | .) ;;
      ..) [ "${#stack[@]}" -gt 0 ] && stack=("${stack[@]:0:${#stack[@]} - 1}") ;;
      *) stack+=("$seg") ;;
    esac
  done
  if [ "${#stack[@]}" -eq 0 ]; then
    printf '/'
    return 0
  fi
  local out="" s
  for s in "${stack[@]}"; do
    out="${out}/${s}"
  done
  printf '%s' "$out"
}

# Collect the ls PATH arguments: every non-flag token after `ls`, stopping at
# the first shell operator or comment. `--` ends flag parsing; tokens after
# it are paths. No path arg at all means a bare listing of $CWD.
LS_TARGETS=()
LS_SEEN_DD=0
LS_I=1
while [ "$LS_I" -lt "${#LS_TOKS[@]}" ]; do
  LS_T="${LS_TOKS[$LS_I]}"
  if [ "$LS_SEEN_DD" -eq 1 ]; then
    LS_TARGETS+=("$LS_T")
    LS_I=$((LS_I + 1))
    continue
  fi
  case "$LS_T" in
    '|'* | '&'* | ';'* | '<'* | '>'* | '#'*)
      # Shell operator or comment start -- args end here.
      break
      ;;
    --)
      LS_SEEN_DD=1
      ;;
    -*)
      : # flag
      ;;
    *)
      LS_TARGETS+=("$LS_T")
      ;;
  esac
  LS_I=$((LS_I + 1))
done

NCWD=$(_legion_ls_normalize "$CWD")

# Find the first target that resolves inside $CWD's tree. Bare `ls` (no path
# arg) lists $CWD itself, which trivially qualifies.
QUALIFIED=""
if [ "${#LS_TARGETS[@]}" -eq 0 ]; then
  QUALIFIED="$NCWD"
else
  for LS_T in "${LS_TARGETS[@]}"; do
    # Strip surrounding quotes a token may still carry (`ls "src dir"`).
    LS_ARG="${LS_T//\"/}"
    LS_ARG="${LS_ARG//\'/}"
    case "$LS_ARG" in
      /*) LS_RESOLVED="$LS_ARG" ;;
      *) LS_RESOLVED="${CWD}/${LS_ARG}" ;;
    esac
    LS_NORM=$(_legion_ls_normalize "$LS_RESOLVED")
    case "$LS_NORM" in
      "$NCWD" | "$NCWD"/*)
        QUALIFIED="$LS_NORM"
        break
        ;;
    esac
  done
fi

# No target inside this repo's tree -- scratch dir, config dir, a parent
# outside $CWD. Pass through untouched.
if [ -z "$QUALIFIED" ]; then
  exit 0
fi

# Soft bypass: the operator explicitly opted out of the nudge. Suppress the
# suggestion and record one telemetry row so the escape is still counted.
BYPASS_REASON=$(legion_prequery_bypass_reason "$COMMAND")
if [ -n "$BYPASS_REASON" ]; then
  legion_prequery_record_bypass \
    "$REPO" "$SESSION_ID" "Bash" "$QUALIFIED" "ls-structure: ${BYPASS_REASON}" \
    "false" "false"
  exit 0
fi

# INJECT: suggest sym, note that ls still ran.
emit_allow "## legion sym maps this tree from the index

\`${REPO}\` is indexed, so its structure is already answered without a directory walk:

    legion sym tree --repo ${REPO}     # directories and file layout
    legion sym list --repo ${REPO}     # modules and their contents

Your \`ls\` still ran -- not blocking you. \`ls\` shows what sym does not (metadata via \`ls -l\`, and non-source files like READMEs, configs, and dotfiles that are not in the index), so reach for whichever fits the question: sym for \"what is the shape of this code,\" \`ls\` for \"what is literally on disk here.\"" \
  "legion sym tree/list covers this listing"
