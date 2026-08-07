#!/usr/bin/env bash
# test-release.sh: unit tests for the pure helpers in scripts/release.sh -- the
# version computation and validation that turn into a pushed git tag, so a
# regression here ("a typo becomes a pushed tag") is exactly what must be caught.
#
# Sources release.sh; main() is BASH_SOURCE-guarded, so sourcing defines the
# helpers without triggering a release.
set -u

DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=release.sh disable=SC1091
source "${DIR}/release.sh"
set +e   # release.sh enables errexit on source; tests manage their own codes.

PASS=0
FAIL=0

eq() { # eq <label> <expected> <actual>
  if [ "$2" = "$3" ]; then PASS=$((PASS + 1)); else
    FAIL=$((FAIL + 1)); printf 'FAIL: %s -- expected [%s] got [%s]\n' "$1" "$2" "$3" >&2
  fi
}
ok() { # ok <label> <cmd...>   (expect exit 0)
  local label="$1"; shift
  if "$@"; then PASS=$((PASS + 1)); else FAIL=$((FAIL + 1)); printf 'FAIL: %s -- expected success\n' "$label" >&2; fi
}
no() { # no <label> <cmd...>   (expect non-zero)
  local label="$1"; shift
  if "$@"; then FAIL=$((FAIL + 1)); printf 'FAIL: %s -- expected failure\n' "$label" >&2; else PASS=$((PASS + 1)); fi
}

# -- THE FIXTURE PERIMETER (#861) --------------------------------------------
# Everything below this banner exists because this suite has twice driven git
# against the REAL repository it was running from, and the second time force-fed
# a one-line Cargo.toml and a `chore(release): 0.25.0` commit onto origin/main.
#
# THE DELIVERY CHANNEL, stated plainly, because it is not obvious from this
# file: scripts/release.sh runs scripts/preflight.sh, and preflight.sh does
# `cd "$(git rev-parse --show-toplevel)"` and then `bash scripts/test-release.sh`.
# So during a real release this suite executes with its CWD SET TO THE REAL
# CHECKOUT, inside a repo whose `origin` is the real GitHub remote. Every
# fallback-to-cwd in here is therefore a fallback to production.
#
# `require_dir` was the previous guard and it is DEMONSTRABLY NOT ENOUGH:
#   * `git -C ""` does not fail. Verified: inside a repo it runs in the CURRENT
#     repo and exits 0, exactly the silent no-op `cd ""` gives you. The
#     primitive protects nothing.
#   * `require_dir` accepts "." -- `[ -n "." ] && [ -d "." ]` is true. A path
#     helper that degrades to a RELATIVE path sails straight through it, and
#     then `git -C .` and `rm -rf "$(dirname .)"` both address the runner.
#   * `dirname ""` is ".", so a single empty variable turns the EXIT cleanup
#     into `rm -rf .` at the root of the real checkout.
#
# So the guard cannot be a check on the value. It is a check on the RESOLVED
# TARGET: every fixture git call goes through `fixture_git`, which refuses any
# directory that is not absolute, not under this suite's own temp root, or that
# resolves to the same git repository the suite is running from -- and refuses
# it by killing the suite, not by returning non-zero (most calls here sit inside
# `$( )` or `( )`, where a `return`/`exit` is swallowed by the subshell and the
# run marches on into the next block).
SUITE_PID=$$

# phys <path>: the physical, symlink-free form of an EXISTING directory. Path
# STRINGS are not comparable identities here: /Users/seansilvius/projects/... and
# /Volumes/store/projects/... are the same checkout through a symlink, and
# mktemp hands out /var/folders/... for /private/var/folders/....
phys() {
  local p="$1"
  [ -n "$p" ] || return 1
  (cd "$p" 2>/dev/null && pwd -P) || return 1
}

# path_under <path> <root>: <path> is <root> or lives beneath it.
path_under() {
  local p="$1" root="$2"
  [ -n "$p" ] && [ -n "$root" ] || return 1
  case "$p" in "$root" | "$root"/*) return 0 ;; esac
  return 1
}

# repo_key <dir>: a canonical identity for the git REPOSITORY <dir> belongs to.
# The common git dir, so a linked worktree keys to the repo it is linked into --
# a fixture pointed at a worktree of the runner's checkout must be caught too.
repo_key() {
  local d="$1" g
  [ -n "$d" ] || return 1
  g="$(cd "$d" 2>/dev/null && git rev-parse --git-common-dir 2>/dev/null)" || return 1
  [ -n "$g" ] || return 1
  case "$g" in /*) ;; *) g="${d}/${g}" ;; esac
  phys "$g"
}

# fixture_resolve <path>: the physical form of <path>, WHETHER OR NOT IT EXISTS.
#
# THIS IS THE FUNCTION THE GUARD'S CONTAINMENT TESTS ARE BUILT ON, and it exists
# because `case` is a glob matcher, not a path resolver. `path_under
# "${SUITE_TMP}/../../../x" "$SUITE_TMP"` is TRUE: the string starts with the
# root, so the test passes while the path lands wherever it likes. Every site
# that decides "is this mine?" about a path that may not exist yet -- the
# `git init` target, the remote-URL arm, the `rm -rf` cleanup vector -- resolves
# through here first, and a resolution FAILURE is a refusal, never a fallback to
# the raw string. The raw-string fallback is the same defect wearing a different
# hat: it is reached precisely when the path is exotic.
#
# An existing path resolves directly through `phys`. A path that does not exist
# yet -- the normal case for `git init` and for a `set-url` naming a bare repo
# not created yet -- resolves its PARENT and re-appends the leaf. A leaf of `.`
# or `..` is refused rather than re-appended: `${SUITE_TMP}/gone/..` would
# otherwise rejoin as a path that string-matches the root while leaving it.
fixture_resolve() {
  local d="$1" parent leaf rp
  [ -n "$d" ] || return 1
  if rp="$(phys "$d")"; then
    printf '%s' "$rp"
    return 0
  fi
  leaf="$(basename -- "$d")"
  case "$leaf" in "" | . | ..) return 1 ;; esac
  parent="$(phys "$(dirname -- "$d")")" || return 1
  printf '%s/%s' "$parent" "$leaf"
}

# The fixtures run against a PINNED git config, not the operator's. Ambient
# config silently degraded these assertions: a global `merge.ff = only` or
# `user.useConfigOnly = true` took the suite from 103 passed to 98, and a global
# `core.hooksPath` pointing at a failing pre-push took it to 87 -- all of it
# reported as ordinary failures with no hint that the machine, not the code, had
# changed. Identity is then set PER REPO (linked worktrees read the main
# checkout's config, so worktree commits inherit it) because /dev/null as a
# global config leaves git with no committer.
#
# The pin is established BEFORE the identity capture below, not after. Those two
# captures are `git rev-parse` calls, and ambient config that makes rev-parse
# fail (a broken `include.path`, an unreadable `core.hooksPath`) would leave both
# keys EMPTY -- and an empty key silently skips the identity comparison in
# `fixture_git`, turning the check that catches a planted worktree of the
# runner's repo into a no-op. Defense in depth, but it costs one line of
# ordering.
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null

# The forbidden repository, captured TWICE: the repo containing this script, and
# the repo at the cwd the suite started in. Under preflight they are the same;
# run standalone from elsewhere they need not be, and both are equally fatal.
#
# The git invocations in this file that do NOT go through `fixture_git` are, in
# full: the `git rev-parse --git-common-dir` inside `repo_key`, the `git
# rev-parse --show-toplevel` that fills SUITE_SELF_TOP, and the `git config
# --get-regexp` inside `fixture_assert_remote`. All three ARE the guard -- they
# are how it learns what to refuse, so routing them through it would recurse --
# and all three are reads. (The PERIMETER SELF-TEST block plants hostile fixtures
# with raw `git` too, but that block only ever runs in a child process whose own
# "runner" is a sacrificial copy; see its banner.)
SUITE_SELF_REPO="$(repo_key "$DIR" || printf '')"
SUITE_CWD_REPO="$(repo_key "." || printf '')"
SUITE_SELF_TOP="$( (cd "$DIR" 2>/dev/null && git rev-parse --show-toplevel 2>/dev/null) || printf '' )"
[ -z "$SUITE_SELF_TOP" ] || SUITE_SELF_TOP="$(phys "$SUITE_SELF_TOP" || printf '%s' "$SUITE_SELF_TOP")"

# ONE temp root for the whole suite, so "is this path mine?" is a containment
# test rather than a list of four unrelated mktemp results.
SUITE_TMP="$(phys "$(mktemp -d)")" || {
  printf '[test-release] could not create the suite temp root\n' >&2
  exit 90
}
SYSTEM_TMP="$(phys "${TMPDIR:-/tmp}" || printf '/tmp')"
SUITE_ABORT_NOTE="${SUITE_TMP}/ABORT"

# suite_abort <reason>: kill the RUN, from wherever it is called. `exit` alone
# is not enough (subshells), and `return 1` is not enough (`set +e` on line 13),
# so the signal goes to the top-level shell by pid and the local shell dies too.
suite_abort() {
  printf '\n[test-release] PERIMETER ABORT -- %s\n' "$1" >&2
  printf '[test-release] the fixture harness refuses to run git outside its own temp root.\n' >&2
  printf '%s\n' "$1" >"$SUITE_ABORT_NOTE" 2>/dev/null || true
  kill -TERM "$SUITE_PID" 2>/dev/null || true
  exit 97
}
suite_terminated() {
  printf '\n[test-release] SUITE ABORTED BY THE PERIMETER GUARD: %s\n' \
    "$(cat "$SUITE_ABORT_NOTE" 2>/dev/null || printf 'reason unavailable')" >&2
  exit 97
}
trap suite_terminated TERM

# Every temp directory this file creates goes on one list with one EXIT trap, so
# a RED run cleans up as thoroughly as a green one -- and an ABORTED run cleans
# up too. docs_worktree_setup makes its OWN `mktemp -d` parent, outside
# $SUITE_TMP, and only a successful teardown ever removed it, so each failing run
# leaked a worktree parent into the system temp dir, which is where the release
# is cut. Registering the parent as soon as the path is known covers the failure
# paths too.
CLEANUP_PATHS=("$SUITE_TMP")
cleanup_temps() {
  [ "${#CLEANUP_PATHS[@]}" -eq 0 ] || rm -rf "${CLEANUP_PATHS[@]}"
}
trap cleanup_temps EXIT

# register_temp <path...>: add a directory to the EXIT cleanup list. This list
# is an `rm -rf` argument vector, so it is guarded at least as hard as the git
# targets: `dirname ""` is ".", and a "." on this list is `rm -rf` at the root of
# the runner's checkout when the suite exits.
register_temp() {
  local p rp
  for p in "$@"; do
    [ -n "$p" ] || continue
    case "$p" in
      /*) ;;
      *) suite_abort "register_temp given the RELATIVE path [${p}] -- the EXIT 'rm -rf' would resolve it against the runner's checkout" ;;
    esac
    # Canonicalise before comparing. `mktemp -d` hands back /var/folders/... for
    # what is physically /private/var/folders/..., so a string test against a
    # physically-resolved root rejects a perfectly legitimate temp parent.
    #
    # A resolution failure is a REFUSAL. The previous `phys "$p" || printf '%s'
    # "$p"` fell back to the raw string on exactly the inputs worth worrying
    # about -- a path containing `..` that does not exist string-matches
    # "$SUITE_TMP"/* and lands outside it -- and this list is an `rm -rf`
    # argument vector.
    rp="$(fixture_resolve "$p")" \
      || suite_abort "register_temp given [${p}], which cannot be resolved to a real location -- refusing to put an unresolvable path on the EXIT 'rm -rf' list"
    if ! path_under "$rp" "$SUITE_TMP" && ! path_under "$rp" "$SYSTEM_TMP"; then
      suite_abort "register_temp given [${p}] (-> ${rp}), which is under neither ${SUITE_TMP} nor ${SYSTEM_TMP}"
    fi
    if [ -n "$SUITE_SELF_TOP" ] && { path_under "$rp" "$SUITE_SELF_TOP" || path_under "$SUITE_SELF_TOP" "$rp"; }; then
      suite_abort "register_temp given [${p}] (-> ${rp}), which contains or lives inside the runner's checkout ${SUITE_SELF_TOP}"
    fi
    CLEANUP_PATHS+=("$rp")
  done
}

# fixture_dir_allowed <physical-path>: under the suite temp root, or under a
# directory already registered for cleanup. The second arm is what admits the
# worktree parents docs_worktree_setup mints with its own `mktemp -d` -- those
# are outside $SUITE_TMP by construction, and release.sh is not in scope here.
fixture_dir_allowed() {
  local p="$1" reg
  path_under "$p" "$SUITE_TMP" && return 0
  for reg in "${CLEANUP_PATHS[@]}"; do
    path_under "$p" "$reg" && return 0
  done
  return 1
}

# fixture_git_argv <args...>: normalise a fixture git invocation into
#   line 1  -- the subcommand
#   line 2+ -- the non-flag OPERANDS that follow it, in order
# rc 1 when there is no subcommand at all (`git --version`).
#
# ONE walker, read by both consumers. The verb selects whether the remote
# assertion fires; the operands are where the DESTINATION of a network verb
# actually lives. Walking argv twice, in two functions, is how those two
# readings drift apart -- and `gitp` prepends `-c core.hooksPath=/dev/null` to
# every push, so both readings have to skip global options and their values
# identically or the guard reads a different command than git runs.
#
# `-C`, `--git-dir` and `--work-tree` are REFUSED rather than skipped. The
# target repository is the first argument to `fixture_git`, which is what the
# whole perimeter was applied to; an option in argv that repoints git at a
# different repo or work tree makes that check describe a directory git never
# touches. Nothing in this file has any reason to pass one.
fixture_git_argv() {
  local a skip=0 verb=""
  for a in "$@"; do
    if [ "$skip" = 1 ]; then
      skip=0
      continue
    fi
    if [ -z "$verb" ]; then
      case "$a" in
        -C | -C=* | --git-dir | --git-dir=* | --work-tree | --work-tree=*)
          suite_abort "fixture git argv carries the repository REDIRECT [${a}] -- the target repo is fixture_git's first argument and nothing in argv may repoint it: 'git $*'"
          ;;
        -c | --namespace | --exec-path | --super-prefix) skip=1 ;;
        -*) ;;
        *)
          verb="$a"
          printf '%s\n' "$a"
          ;;
      esac
      continue
    fi
    case "$a" in
      -*) ;;
      *) printf '%s\n' "$a" ;;
    esac
  done
  [ -n "$verb" ]
}

# fixture_git_verb <args...>: just the subcommand. Kept as its own name because
# it is THE SELECTOR -- if it ever returns empty, fixture_assert_remote silently
# never runs, in the one file whose failure mode is "push to production" -- and a
# selector that decides whether a guard fires deserves assertions of its own.
fixture_git_verb() {
  fixture_git_argv "$@" | sed -n '1p'
}

# fixture_assert_url <url-or-arg> <what>: a fixture's remote must be a bare repo
# inside this suite's own temp tree, or a plain remote NAME whose url is checked
# separately. Everything else is refused.
#
# THE LOGIC IS INVERTED ON PURPOSE, and the previous enumerate-the-hostile-forms
# version is why. It refused `*://*` and `*@*:*` and waved everything else
# through, so `github.com:runlegion/legion.git` -- scp-like ssh, which needs no
# `@` -- fell into the permissive arm and was a perfectly ordinary off-machine
# remote. Relative paths (`../../elsewhere.git`, resolved against the runner's
# cwd) sailed through the same arm. A blocklist of URL syntaxes is a losing
# game: git accepts more forms than anyone enumerates, so the only safe shape is
# an allowlist of the two things a fixture legitimately names.
fixture_assert_url() {
  local url="$1" what="$2" rp
  [ -n "$url" ] || return 0
  case "$url" in
    /*)
      # Resolved, never string-matched, and a resolution failure is a refusal:
      # `${SUITE_TMP}/../../../evil.git` string-matches the root.
      rp="$(fixture_resolve "$url")" \
        || suite_abort "fixture remote [${url}] cannot be resolved to a real location -- ${what}"
      fixture_dir_allowed "$rp" \
        || suite_abort "fixture remote [${url}] resolves to ${rp}, OUTSIDE the suite temp root ${SUITE_TMP} -- ${what}"
      return 0
      ;;
  esac
  # Not an absolute path, so the ONLY thing left that a fixture may name is a
  # plain remote name. Git remote names are `[A-Za-z0-9._-]`-ish and never begin
  # with a dot; anything outside that is addressing something -- a scheme, an
  # scp-like host:path, a relative path, a `~` expansion, a refspec.
  case "$url" in
    *[!A-Za-z0-9._-]* | .* | -*)
      suite_abort "fixture remote [${url}] is neither an absolute path under ${SUITE_TMP} nor a plain remote name -- anything else is addressable off this machine: ${what}"
      ;;
  esac
}

# fixture_assert_remote <dir> <what>: EVERY remote configured in <dir>, not just
# `origin`. A fixture that adds `upstream` and pushes to it was completely
# unexamined while the guard read `origin` and reported itself satisfied; the
# name a push addresses is chosen by the caller, so the guard cannot assume it.
# Push URLs are covered too -- `remote.<name>.pushurl` overrides `.url` for
# exactly the operation that matters here.
#
# The read is a DIRECT git call -- it is part of the guard, so routing it
# through fixture_git would recurse.
fixture_assert_remote() {
  local d="$1" what="$2" line key url
  while IFS= read -r line; do
    key="${line%% *}"
    url="${line#* }"
    case "$key" in
      remote.*.url | remote.*.pushurl) ;;
      *) continue ;;
    esac
    fixture_assert_url "$url" "${what} (${key} of ${d})"
  done <<<"$(git -C "$d" config --get-regexp '^remote\.' 2>/dev/null || true)"
}

# fixture_assert_destinations <verb> <what> <operands...>: the destination a
# network verb ACTUALLY addresses, which is not the same question as "what is
# this repo's configured origin".
#
# THE HOLE THIS CLOSES: `git push <url> HEAD:refs/heads/x` ignores the
# configured origin entirely. The old guard read origin, found a temp bare repo,
# approved -- and the push landed a release-shaped fixture commit in the
# runner's refs. Substituting a GitHub URL reaches GitHub.
#
# Position, not pattern-matching, is what makes this precise. Git's grammar for
# all five verbs is `<verb> [<options>] [<repository> [<refspec>...]]`, so the
# FIRST operand is the repository and the rest are refspecs -- and refspecs are
# full of `:` and `/`, so asserting them as URLs would refuse `HEAD:refs/heads/x`
# on correct input. `clone` takes a target directory as its second operand,
# which must be inside the perimeter for the same reason every other fixture
# path must.
#
# The trailing scan is the belt to that braces: an option this walker does not
# know consumes a value could displace the repository past operand 1, so any
# LATER operand that is unambiguously a location -- absolute, or carrying a
# `://` scheme, neither of which is ever a valid refspec -- is asserted too.
fixture_assert_destinations() {
  local verb="$1" what="$2"
  shift 2
  local first="${1:-}" a
  case "$verb" in
    clone)
      fixture_assert_url "$first" "${what} (clone source)"
      [ "$#" -lt 2 ] || fixture_assert_url "$2" "${what} (clone target directory)"
      ;;
    *)
      fixture_assert_url "$first" "${what} (destination operand)"
      ;;
  esac
  [ "$#" -gt 1 ] || return 0
  shift
  for a in "$@"; do
    case "$a" in
      /* | *://*) fixture_assert_url "$a" "${what} (location-shaped operand)" ;;
    esac
  done
}

# fixture_git <dir> <git-args...>: THE ONLY WAY this file addresses a git repo.
fixture_git() {
  local d="$1"
  shift
  local p key line verb="" a
  local -a operands=()
  [ -n "$d" ] || suite_abort "fixture git with an EMPTY target directory: 'git $*' -- 'git -C \"\"' silently runs in the CURRENT repo and exits 0"
  case "$d" in
    /*) ;;
    *) suite_abort "fixture git with the RELATIVE target directory [${d}]: 'git $*' -- it would resolve against the runner's cwd" ;;
  esac
  p="$(phys "$d")" || suite_abort "fixture git target does not exist [${d}]: 'git $*'"
  fixture_dir_allowed "$p" \
    || suite_abort "fixture git target [${p}] is outside the suite temp root ${SUITE_TMP}: 'git $*'"
  key="$(repo_key "$p" || printf '')"
  if [ -n "$key" ]; then
    [ "$key" != "$SUITE_SELF_REPO" ] \
      || suite_abort "fixture git target [${p}] IS the repository this suite is running from (${key}): 'git $*'"
    [ "$key" != "$SUITE_CWD_REPO" ] \
      || suite_abort "fixture git target [${p}] is the repository at the suite's startup cwd (${key}): 'git $*'"
  fi
  # One walk, two readings: which verb this is, and what it addresses.
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    if [ -z "$verb" ]; then verb="$line"; else operands+=("$line"); fi
  done <<<"$(fixture_git_argv "$@")"
  case "$verb" in
    push | fetch | pull | clone | ls-remote)
      # BOTH questions, because they are different questions: what the repo's
      # remotes point at, AND what this particular command names.
      fixture_assert_remote "$d" "git $*"
      fixture_assert_destinations "$verb" "git $*" ${operands[@]+"${operands[@]}"}
      ;;
    remote)
      fixture_assert_remote "$d" "git $*"
      # EVERY operand, not the last one. `git remote set-url [--push] <name>
      # <newurl> [<oldurl>]` takes an optional TRAILING oldurl, so checking the
      # last argument checks the url being replaced and leaves the new one --
      # the one about to become the fixture's remote -- unexamined. Proven
      # chained: set-url to an off-machine host passed, and the follow-up push
      # was handed to a real ssh attempt. Remote NAMES pass the name arm of
      # fixture_assert_url, so asserting all of them costs nothing.
      for a in ${operands[@]+"${operands[@]}"}; do
        fixture_assert_url "$a" "git $*"
      done
      ;;
  esac
  git -C "$d" "$@"
}

# fixture_git_init <dir> <git-args...>: <dir> is APPENDED, because `git init` and
# friends take the target as an argument rather than via -C, and -C requires the
# directory to already exist.
#
# THIS WAS A SECOND DOOR, and it was open. Its only check used to be
# `path_under "$d" "$SUITE_TMP"` on the UNRESOLVED string, and `case` resolves
# neither `..` nor symlinks -- so both walked straight out. Proven: a `..`
# traversal aimed at the runner's `.git` flipped `core.bare` false -> true
# through this function, which is incident #3, and a symlink inside $SUITE_TMP
# pointing at the runner passed the same way. `fixture_git` and `register_temp`
# both refuse all three shapes, so this was an asymmetry, not a design choice.
#
# It now runs the SAME pair `fixture_git` runs -- fixture_dir_allowed on the
# resolved path, then the repo-identity comparison -- against the resolution
# `fixture_resolve` gives a target that does not exist yet.
fixture_git_init() {
  local d="$1"
  shift
  local p key
  [ -n "$d" ] || suite_abort "fixture git init with an EMPTY target directory: 'git $*'"
  case "$d" in
    /*) ;;
    *) suite_abort "fixture git init with the RELATIVE target directory [${d}]: 'git $*'" ;;
  esac
  p="$(fixture_resolve "$d")" \
    || suite_abort "fixture git init target [${d}] cannot be resolved -- its parent directory does not exist: 'git $*'"
  fixture_dir_allowed "$p" \
    || suite_abort "fixture git init target [${d}] resolves to ${p}, OUTSIDE the suite temp root ${SUITE_TMP}: 'git $*'"
  # An init target that ALREADY belongs to a repo is the core.bare case: `git
  # init --bare` on an existing `.git` rewrites its config in place.
  key="$(repo_key "$p" || printf '')"
  if [ -n "$key" ]; then
    [ "$key" != "$SUITE_SELF_REPO" ] \
      || suite_abort "fixture git init target [${d}] resolves into the repository this suite is running from (${key}): 'git $*'"
    [ "$key" != "$SUITE_CWD_REPO" ] \
      || suite_abort "fixture git init target [${d}] resolves into the repository at the suite's startup cwd (${key}): 'git $*'"
  fi
  git "$@" "$d"
}

# gitp <dir> <git-args...>: a fixture git call with hooks pinned off, so a stray
# repo-level or inherited hook cannot turn a fixture push into a mystery failure.
gitp() {
  local d="$1"
  shift
  fixture_git "$d" -c core.hooksPath=/dev/null "$@"
}

# in_dir <dir> <cmd...>: THE SECOND DOOR into the runner's repo, and it has to be
# barred as hard as the first. It hands the cwd to a release.sh function whose
# own git calls cannot be routed through fixture_git, so whatever `cd` lands on
# is what that function operates on. `cd ""` returns 0 in bash, so
# `cd "$X" || return` is NOT a guard on its own. It therefore applies the same
# perimeter as fixture_git -- absolute, inside the suite temp root, and not the
# runner's repository -- before it moves anywhere. Refusing a relative path
# matters as much as refusing an empty one: "." is the value `require_dir` waves
# through.
in_dir() {
  local d="$1"; shift
  local p key
  [ -n "$d" ] || return 90
  case "$d" in
    /*) ;;
    *) suite_abort "in_dir given the RELATIVE directory [${d}] -- it would hand the runner's cwd to '$1'" ;;
  esac
  p="$(phys "$d")" || return 91
  fixture_dir_allowed "$p" \
    || suite_abort "in_dir target [${p}] is outside the suite temp root ${SUITE_TMP} -- refusing to run '$1' there"
  key="$(repo_key "$p" || printf '')"
  if [ -n "$key" ]; then
    [ "$key" != "$SUITE_SELF_REPO" ] \
      || suite_abort "in_dir target [${p}] IS the repository this suite is running from -- refusing to run '$1' there"
    [ "$key" != "$SUITE_CWD_REPO" ] \
      || suite_abort "in_dir target [${p}] is the repository at the suite's startup cwd -- refusing to run '$1' there"
  fi
  cd "$p" || return 91
  "$@"
}

# require_dir stays, one layer in from the perimeter: it turns "the helper under
# test returned nothing usable" into a NAMED test failure with its dependent
# assertions skipped, rather than a pile of confusing downstream failures. It is
# not a safety boundary -- it passes on "." -- and nothing below may rely on it
# as one.
require_dir() { # require_dir <label> <path>
  local label="$1" d="$2"
  if [ -n "$d" ] && [ -d "$d" ]; then
    PASS=$((PASS + 1))
    return 0
  fi
  FAIL=$((FAIL + 1))
  printf 'FAIL: %s -- no usable directory [%s]; dependent assertions skipped\n' "$label" "$d" >&2
  return 1
}

# -- THE PERIMETER SELF-TEST, CHILD SIDE (#861) ------------------------------
# The guard aborts by killing the run (`kill -TERM $$`), which is the only thing
# that works from inside `$( )` and `( )` -- and it means an abort CANNOT be
# observed from within the run it terminates. So each attack is executed by a
# CHILD PROCESS: the parent (further down, under PERIMETER SELF-TEST, PARENT
# SIDE) copies this script and release.sh into a sacrificial git repo, runs
# `bash <that copy>` with PERIMETER_ATTACK set, and asserts rc 97 plus the reason
# text. Because the copy lives in the sacrificial repo, THAT repo is what the
# child computes as SUITE_SELF_REPO -- so every attack aims at a throwaway, and a
# deleted guard damages a temp directory instead of the operator's checkout.
# That is what makes these safe to run as mutation checks.
#
# rc 98, never 97. 97 is what a real suite_abort produces and this block must not
# be able to counterfeit it -- and since the block runs BEFORE the first
# assertion and exits, an ambient PERIMETER_ATTACK in a preflight environment
# would otherwise turn a suite that asserted NOTHING into a green one. 98 makes
# that loud. (The parent always passes the variable as a per-command prefix,
# never an export, so no other child inherits it.)
if [ -n "${PERIMETER_ATTACK:-}" ]; then
  attack_bail() { printf '\n[test-release] SELF-TEST HARNESS ERROR: %s\n' "$1" >&2; exit 98; }
  VICTIM="${PERIMETER_VICTIM:-}"
  [ -n "$VICTIM" ] || attack_bail "PERIMETER_VICTIM (the sacrificial runner) is required"
  [ -d "$VICTIM" ] || attack_bail "PERIMETER_VICTIM [${VICTIM}] is not a directory"

  # A legitimate fixture: inside this child's own temp root, with an origin that
  # is a temp bare repo. Attacks that need a SOURCE repo use it; attacks that
  # need a destination outside the perimeter use $VICTIM.
  ATK="${SUITE_TMP}/atk"
  ATK_ORIGIN="${SUITE_TMP}/atk-origin.git"
  mkdir -p "$ATK"
  fixture_git_init "$ATK_ORIGIN" init --bare --quiet
  fixture_git_init "$ATK" -c init.defaultBranch=main init --quiet
  fixture_git "$ATK" config user.email t@t
  fixture_git "$ATK" config user.name t
  printf 'atk\n' >"${ATK}/f.txt"
  fixture_git "$ATK" add f.txt
  fixture_git "$ATK" commit --quiet --no-verify -m "atk"
  fixture_git "$ATK" remote add origin "$ATK_ORIGIN"
  gitp "$ATK" push --quiet -u origin main

  # A path that STRING-MATCHES "$SUITE_TMP"/* and yet resolves to $VICTIM: one
  # `..` per component of $SUITE_TMP walks back to `/`, and the victim's absolute
  # path is re-appended. This is the shape `path_under` cannot see and the reason
  # fixture_resolve exists -- no symlink involved, so `cd` resolves it the same
  # way lexically or physically.
  ATK_UP="$(printf '%s\n' "${SUITE_TMP#/}" | tr '/' '\n' | sed 's|.*|..|' | tr '\n' '/')"
  ATK_TRAVERSAL="${SUITE_TMP}/${ATK_UP}${VICTIM#/}"

  case "$PERIMETER_ATTACK" in
    control-legit)
      # THE NEGATIVE CONTROL, and it is not optional: without it a guard that
      # aborted on absolutely everything would pass every assertion below, and
      # the whole self-test would be measuring nothing.
      fixture_git "$ATK" status --porcelain >/dev/null
      gitp "$ATK" push --quiet origin main
      fixture_git "$ATK" remote set-url origin "$ATK_ORIGIN"
      fixture_git_init "${SUITE_TMP}/fresh.git" init --bare --quiet
      printf '[attack] control completed with no abort, as required\n' >&2
      exit 0
      ;;

    # -- fixture_git: the target directory ------------------------------------
    empty-target)    fixture_git "" status ;;
    relative-target) fixture_git "." status ;;
    outside-target)  fixture_git "$VICTIM" status ;;
    planted-worktree)
      # $PERIMETER_PLANTED is a linked worktree OF THE VICTIM that the parent
      # created. Admitting it to the allowed set is the point: containment must
      # PASS so the repo-IDENTITY comparison is the only thing left standing
      # between the fixture and the runner's object store. The write is real --
      # if the guard is gone, a commit lands on the victim's `planted` branch,
      # and the parent asserts that branch never moved.
      [ -n "${PERIMETER_PLANTED:-}" ] || attack_bail "PERIMETER_PLANTED is required for this attack"
      CLEANUP_PATHS+=("$PERIMETER_PLANTED")
      fixture_git "$PERIMETER_PLANTED" commit --quiet --no-verify --allow-empty -m "escape"
      ;;

    # -- fixture_git_init: the second door (#861 HIGH 1) ----------------------
    init-traversal)
      # Incident #3, exactly: `git init --bare` onto the runner's own .git flips
      # core.bare false -> true and `git status` starts failing with "must be run
      # in a work tree".
      fixture_git_init "${ATK_TRAVERSAL}/.git" init --bare --quiet
      ;;
    init-symlink)
      ln -s "$VICTIM" "${SUITE_TMP}/link"
      fixture_git_init "${SUITE_TMP}/link/.git" init --bare --quiet
      ;;

    # -- network verbs: the destination in argv (#861 HIGH 2) -----------------
    push-argv-destination)
      # The configured origin is a perfectly good temp bare repo. The push does
      # not use it.
      fixture_git "$ATK" push "$VICTIM" HEAD:refs/heads/fixture-escape
      ;;
    push-argv-url)
      fixture_git "$ATK" push "https://github.com/runlegion/legion.git" HEAD:refs/heads/fixture-escape
      ;;
    push-non-origin-remote)
      # The hostile remote is planted with a RAW git call -- fixture_git would
      # (correctly) refuse to create it, and the question here is what happens on
      # the PUSH when a remote other than `origin` already exists.
      git -C "$ATK" remote add upstream "https://github.com/runlegion/legion.git"
      gitp "$ATK" push --quiet upstream main
      ;;
    push-pushurl)
      git -C "$ATK" config remote.origin.pushurl "git@github.com:runlegion/legion.git"
      gitp "$ATK" push --quiet origin main
      ;;
    git-dir-redirect)
      fixture_git "$ATK" --git-dir="${VICTIM}/.git" status
      ;;

    # -- URL forms (#861 HIGH 3) and the set-url operand (#861 HIGH 4) --------
    remote-scp-like)
      # No `@`, so the old `*@*:*` arm never saw it, and it is an ordinary
      # off-machine ssh remote.
      fixture_git "$ATK" remote add ghost "github.com:runlegion/legion.git"
      ;;
    remote-relative)
      fixture_git "$ATK" remote add ghost "../../elsewhere.git"
      ;;
    remote-set-url-newurl)
      # The NEW url is not the last argument -- `set-url` takes an optional
      # trailing oldurl.
      fixture_git "$ATK" remote set-url origin "git@github.com:runlegion/legion.git" "$ATK_ORIGIN"
      ;;
    assert-url-traversal)
      fixture_assert_url "${ATK_TRAVERSAL}/evil.git" "self-test"
      ;;

    # -- the other two doors --------------------------------------------------
    in-dir-relative)    in_dir "." pwd ;;
    in-dir-outside)     in_dir "$VICTIM" pwd ;;
    register-relative)  register_temp "." ;;
    register-traversal) register_temp "$ATK_TRAVERSAL" ;;
    register-root)      register_temp "/" ;;
    register-traversal-unresolved)
      # The leaf does NOT exist, so `phys` fails on the whole path. That is the
      # input the old `phys ... || printf '%s' "$p"` fallback was reached by, and
      # under it the RAW string -- which still begins with $SUITE_TMP -- was what
      # got compared, so a path physically inside the runner passed every arm and
      # went onto the `rm -rf` list. A traversal whose components all exist does
      # NOT exercise this; it resolves and is caught elsewhere.
      register_temp "${ATK_TRAVERSAL}/not-created-yet"
      ;;

    *) attack_bail "unknown attack name [${PERIMETER_ATTACK}]" ;;
  esac

  printf '\n[test-release] SELF-TEST: THE PERIMETER LET [%s] THROUGH\n' "$PERIMETER_ATTACK" >&2
  exit 98
fi

# -- THE PERIMETER SELF-TEST, PARENT SIDE (#861) -----------------------------
# Before this section the perimeter had ZERO committed assertions. Six mutations
# were run by hand while it was being written, which is development activity, not
# coverage: nothing in the repository could tell whether the guard still worked.
# That is an unusually bad thing to leave untested, because the failure mode is
# not "a test goes red" -- it is a fixture pushing to production, silently, in a
# file that runs inside the live checkout during every release (preflight.sh cd's
# to the real toplevel and runs this suite).
#
# Two layers. First, unit assertions on the decidable core -- the pure functions
# whose answers the guard is built out of. Second, the attacks, each run in a
# child process against a sacrificial runner.

# path_under: a string test, and its LIMIT is the point of the last assertion.
ok "path_under: identity"            path_under /a/b /a/b
ok "path_under: child"               path_under /a/b/c /a/b
no "path_under: sibling with a shared prefix" path_under /a/bc /a/b
no "path_under: the parent is not under the child" path_under /a /a/b
no "path_under: empty path"          path_under "" /a
no "path_under: empty root"          path_under /a ""
# THE HOLE path_under CANNOT SEE. `case` is a glob matcher: this leaves the root
# and matches anyway. Every caller therefore resolves BEFORE it tests, and this
# assertion is what stops someone "simplifying" fixture_resolve back out again.
ok "path_under: a traversal out of the root still matches the string" path_under /a/b/../../x /a/b

# phys: resolution of an EXISTING directory only.
eq "phys: normalises an existing path" "$SUITE_TMP" "$(phys "${SUITE_TMP}/.")"
no "phys: empty"        phys ""
no "phys: nonexistent"  phys "${SUITE_TMP}/no-such-dir"

# fixture_resolve: resolution of a path that need not exist yet.
eq "resolve: an existing path" "$SUITE_TMP" "$(fixture_resolve "$SUITE_TMP")"
eq "resolve: a leaf that does not exist yet, under an existing parent" \
  "${SUITE_TMP}/not-yet.git" "$(fixture_resolve "${SUITE_TMP}/not-yet.git")"
eq "resolve: a traversal is RESOLVED, not string-matched" \
  "$(phys "${SUITE_TMP}/..")/x" "$(fixture_resolve "${SUITE_TMP}/../x")"
eq "resolve: '..' resolves out of the root rather than staying in it" \
  "$(phys "${SUITE_TMP}/..")" "$(fixture_resolve "${SUITE_TMP}/..")"
no "resolve: a missing parent is refused, never guessed" \
  fixture_resolve "${SUITE_TMP}/no/such/parent/x"
no "resolve: a '..' leaf under a missing parent is refused, not rejoined" \
  fixture_resolve "${SUITE_TMP}/gone/.."
no "resolve: empty" fixture_resolve ""

# fixture_dir_allowed
ok "allowed: the suite temp root itself"  fixture_dir_allowed "$SUITE_TMP"
ok "allowed: a child of the temp root"    fixture_dir_allowed "${SUITE_TMP}/x"
no "allowed: the system temp root itself" fixture_dir_allowed "$SYSTEM_TMP"
no "allowed: the runner's own checkout"   fixture_dir_allowed "$SUITE_SELF_TOP"

# repo_key: the identity a linked worktree resolves to is the repo it is linked
# INTO -- which is the whole reason the guard keys on --git-common-dir rather
# than on the path.
KEY_TMP="${SUITE_TMP}/key"
mkdir -p "$KEY_TMP"
fixture_git_init "${KEY_TMP}/repo" -c init.defaultBranch=main init --quiet
fixture_git "${KEY_TMP}/repo" config user.email t@t
fixture_git "${KEY_TMP}/repo" config user.name t
fixture_git "${KEY_TMP}/repo" commit --quiet --no-verify --allow-empty -m init
fixture_git "${KEY_TMP}/repo" worktree add --quiet -b linked "${KEY_TMP}/linked" >/dev/null 2>&1
eq "repo_key: a linked worktree keys to the repo it is linked into" \
  "$(repo_key "${KEY_TMP}/repo")" "$(repo_key "${KEY_TMP}/linked")"
no "repo_key: empty"       repo_key ""
no "repo_key: nonexistent" repo_key "${SUITE_TMP}/no-such-dir"

# fixture_git_verb / fixture_git_argv: THE SELECTOR. If the verb ever comes back
# empty, fixture_assert_remote never fires and the remote guard is gone without a
# symptom -- so `gitp`'s exact argv, which is the shape every fixture push takes,
# is asserted literally.
eq "verb: plain"                       "push"   "$(fixture_git_verb push origin main)"
eq "verb: gitp's exact argv"           "push"   "$(fixture_git_verb -c core.hooksPath=/dev/null push --quiet -u origin main)"
eq "verb: -c consumes its value"       "push"   "$(fixture_git_verb -c core.hooksPath=/dev/null push)"
eq "verb: after a valueless flag"      "status" "$(fixture_git_verb --no-pager status)"
eq "verb: a valueless flag after -c"   "remote" "$(fixture_git_verb -c a=b --no-pager remote set-url origin x)"
no "verb: no subcommand at all"        fixture_git_verb --version
eq "argv: the destination is the first operand after the verb" "origin" \
  "$(fixture_git_argv -c core.hooksPath=/dev/null push --quiet -u origin main | sed -n '2p')"
eq "argv: refspecs follow it in order" "HEAD:refs/heads/x" \
  "$(fixture_git_argv push --quiet origin HEAD:refs/heads/x | sed -n '3p')"

# fixture_assert_url: the ALLOWED forms, asserted here because every refused form
# aborts the run and can only be observed from a child process (below).
ok "assert_url: an absolute path inside the temp root"        fixture_assert_url "$SUITE_TMP" "unit"
ok "assert_url: a bare repo not created yet, inside the root" fixture_assert_url "${SUITE_TMP}/later.git" "unit"
ok "assert_url: a plain remote name"                          fixture_assert_url "origin" "unit"
ok "assert_url: no remote configured at all"                  fixture_assert_url "" "unit"

# SELF-LINT (#861 criterion 1): no unguarded `cd` in this file. The incident's
# shape was `(cd "$WT" && git ...)` with $WT empty -- `cd ""` is a silent no-op,
# so the fixture's git commands ran in the real checkout -- which makes an
# unguarded `cd` a defect class, not a style preference. Asserting the exact SET
# of targets means a new one fails here and has to be justified, and keeping that
# set to four is why the attacks copy this script into the sacrificial repo
# instead of `(cd "$victim" && ...)`.
#
# Comment lines are stripped first (several of them quote `cd ""` while
# explaining why it is fatal), and the pattern requires real whitespace after
# `cd`, so the regex literals below do not match themselves.
# ONE filtered stream, two readings -- the same discipline fixture_git_argv
# applies to argv. Two separately-written filters are two definitions of "what
# counts as a cd", and they drift.
SUITE_CD_LINES="$(grep -vE '^[[:space:]]*#' "$0" | grep -E '(^|[^[:alnum:]_])cd[[:space:]]')"
SUITE_CD_TARGETS="$(
  printf '%s\n' "$SUITE_CD_LINES" \
    | sed -E 's/.*[^[:alnum:]_]cd[[:space:]]+//; s/[[:space:]].*//' \
    | LC_ALL=C sort -u
)"
# `grep -c` over that same stream rather than `wc -l`: printf of an EMPTY
# variable still emits one newline, so `wc -l` would report 1 where the truth
# is 0 -- and a lint that reads 1 on "nothing matched" fails open.
SUITE_CD_COUNT="$(printf '%s\n' "$SUITE_CD_LINES" | grep -cE '(^|[^[:alnum:]_])cd[[:space:]]')"
# The expected value is the literal TEXT of the four targets as they appear in
# the source, so the single quotes are the point -- expanding them would compare
# this file against its own runtime values instead of against what it says.
# shellcheck disable=SC2016
eq "self-lint: every 'cd' in this file is a perimeter-resolved target" \
  '"$(dirname
"$DIR"
"$d"
"$p"' "$SUITE_CD_TARGETS"
# The SET alone is not enough, because `sort -u` dedupes: a new `cd "$p"` in an
# unguarded context adds no new member and would pass the assertion above --
# and `$p` is exactly the name a future author reuses. The count closes it, so
# the two together are total.
eq "self-lint: and there are exactly five of them" "5" "$SUITE_CD_COUNT"

# THE ATTACKS. The sacrificial runner: a real git repo carrying a COPY of this
# script and of release.sh, so a child launched from it computes that repo as its
# own SUITE_SELF_REPO/SUITE_SELF_TOP. Every attack below therefore aims at this
# throwaway; if a guard is deleted, a temp directory takes the damage and the
# assertions go red, which is what makes this safe as a mutation check.
ATTACK_TMP="${SUITE_TMP}/perimeter"
ATTACK_VICTIM="${ATTACK_TMP}/runner"
mkdir -p "${ATTACK_VICTIM}/scripts"
cp "${DIR}/${0##*/}" "${ATTACK_VICTIM}/scripts/${0##*/}"
cp "${DIR}/release.sh" "${ATTACK_VICTIM}/scripts/release.sh"
ATTACK_SCRIPT="${ATTACK_VICTIM}/scripts/${0##*/}"
fixture_git_init "$ATTACK_VICTIM" -c init.defaultBranch=main init --quiet
fixture_git "$ATTACK_VICTIM" config user.email t@t
fixture_git "$ATTACK_VICTIM" config user.name t
fixture_git "$ATTACK_VICTIM" add scripts
fixture_git "$ATTACK_VICTIM" commit --quiet --no-verify -m "sacrificial runner"

# The fingerprint the four proven attacks were measured against by hand: refs,
# HEAD, reflog, working-tree status, core.bare (incident #3 flipped exactly
# this), the worktree registry (a planted worktree is invisible to all of the
# above) and the local config.
attack_fingerprint() {
  local r="$1"
  fixture_git "$r" for-each-ref --format='%(refname) %(objectname)' 2>/dev/null
  fixture_git "$r" rev-parse HEAD 2>/dev/null
  fixture_git "$r" reflog --format='%H %gs' 2>/dev/null
  fixture_git "$r" status --porcelain 2>/dev/null
  fixture_git "$r" config --get core.bare 2>/dev/null || true
  fixture_git "$r" worktree list --porcelain 2>/dev/null
  fixture_git "$r" config --list --local 2>/dev/null
}
ATTACK_FP="$(attack_fingerprint "$ATTACK_VICTIM")"

# attack <label> <name> <reason-substring>: run one attack in a child and assert
# both that it was refused (rc 97, which only suite_abort produces -- the
# self-test block's own bail is 98) and WHY. Asserting the reason is what stops a
# guard from passing these by aborting indiscriminately.
attack() {
  local label="$1" name="$2" want="$3" out rc=0
  out="$(PERIMETER_ATTACK="$name" PERIMETER_VICTIM="$ATTACK_VICTIM" \
         PERIMETER_PLANTED="${ATTACK_PLANTED:-}" bash "$ATTACK_SCRIPT" 2>&1)" || rc=$?
  eq "perimeter: ${label} is refused (rc 97)" "97" "$rc"
  if [ -z "${out##*"$want"*}" ]; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    printf 'FAIL: perimeter: %s -- the abort did not name [%s]. Output:\n%s\n' "$label" "$want" "$out" >&2
  fi
}

# The negative control runs FIRST. If it does not pass, every refusal below is
# uninterpretable -- a guard that aborts on everything would satisfy them all.
CONTROL_RC=0
CONTROL_OUT="$(PERIMETER_ATTACK=control-legit PERIMETER_VICTIM="$ATTACK_VICTIM" \
               bash "$ATTACK_SCRIPT" 2>&1)" || CONTROL_RC=$?
eq "perimeter: the negative control is NOT refused" "0" "$CONTROL_RC"
ok "perimeter: the negative control really ran" \
  test -z "${CONTROL_OUT##*control completed*}"

# The identity check, which is the only thing standing between a fixture and the
# runner's object store once containment has been satisfied. The worktree is
# planted by the PARENT so the fingerprint above is not disturbed by the setup,
# and the attack's write is real: a working guard leaves `planted` where it was.
ATTACK_PLANTED="${ATTACK_TMP}/planted"
fixture_git "$ATTACK_VICTIM" worktree add --quiet -b planted "$ATTACK_PLANTED" >/dev/null 2>&1
PLANTED_SHA="$(fixture_git "$ATTACK_VICTIM" rev-parse planted)"
attack "a planted worktree of the runner, inside the temp root" planted-worktree \
  "IS the repository this suite is running from"
eq "perimeter: the planted branch was never moved" "$PLANTED_SHA" \
  "$(fixture_git "$ATTACK_VICTIM" rev-parse planted)"
ATTACK_PLANTED=""
fixture_git "$ATTACK_VICTIM" worktree remove --force "${ATTACK_TMP}/planted" >/dev/null 2>&1
fixture_git "$ATTACK_VICTIM" worktree prune >/dev/null 2>&1
fixture_git "$ATTACK_VICTIM" branch -D planted >/dev/null 2>&1

# fixture_git's target directory.
attack "an empty target directory"    empty-target    "EMPTY target directory"
attack "a relative target directory"  relative-target "RELATIVE target directory"
attack "a target outside the temp root" outside-target "outside the suite temp root"

# fixture_git_init, the second door (HIGH 1). Both shapes `case` cannot see.
attack "a '..' traversal through git init" init-traversal "OUTSIDE the suite temp root"
attack "a symlink out of the temp root through git init" init-symlink "OUTSIDE the suite temp root"

# Network verbs: what the command ADDRESSES, not what origin happens to be
# (HIGH 2). The fixture in every one of these has a perfectly legitimate origin.
attack "a push whose destination operand is outside the perimeter" \
  push-argv-destination "OUTSIDE the suite temp root"
attack "a push to a forge URL in argv" push-argv-url "plain remote name"
attack "a push to a remote that is not origin" push-non-origin-remote "plain remote name"
attack "a push through a hostile pushurl" push-pushurl "plain remote name"
attack "a --git-dir redirect in argv" git-dir-redirect "repository REDIRECT"

# URL forms (HIGH 3) and the set-url operand (HIGH 4).
attack "an scp-like ssh remote with no '@'" remote-scp-like "plain remote name"
attack "a relative-path remote"             remote-relative "plain remote name"
attack "set-url's NEW url, which is not the last argument" \
  remote-set-url-newurl "plain remote name"
attack "a traversal in a remote URL" assert-url-traversal "OUTSIDE the suite temp root"

# The other two doors.
attack "in_dir given a relative directory" in-dir-relative "RELATIVE directory"
attack "in_dir given the runner"           in-dir-outside  "outside the suite temp root"
attack "register_temp given a relative path" register-relative "RELATIVE path"
# Both arms of the `rm -rf` vector's guard. A traversal that lands ON the runner
# is caught by the checkout arm; `/` reaches neither temp root and is caught by
# the containment arm. `dirname ""` is "." and this list is an `rm -rf` argument
# vector, so neither arm is theoretical.
attack "register_temp given a traversal onto the runner" register-traversal \
  "contains or lives inside the runner's checkout"
attack "register_temp given a path under no temp root at all" register-root "under neither"
attack "register_temp given a traversal that does not resolve to an existing path" \
  register-traversal-unresolved "contains or lives inside the runner's checkout"

# THE PROOF, committed: every attack above ran, and the sacrificial runner is
# byte-identical afterwards. Refusal and inertness are different claims, and this
# is the one that would have caught incident #1 (a fixture commit in the runner's
# refs) and incident #3 (core.bare flipped to true).
eq "perimeter: the sacrificial runner is byte-identical after every attack" \
  "$ATTACK_FP" "$(attack_fingerprint "$ATTACK_VICTIM")"

# compute_new_version
eq "patch"         0.18.3 "$(compute_new_version 0.18.2 patch)"
eq "minor"         0.19.0 "$(compute_new_version 0.18.2 minor)"
eq "major"         1.0.0  "$(compute_new_version 0.18.2 major)"
eq "patch carry"   1.4.10 "$(compute_new_version 1.4.9 patch)"
eq "minor resets"  2.1.0  "$(compute_new_version 2.0.7 minor)"
eq "major resets"  3.0.0  "$(compute_new_version 2.9.9 major)"
eq "explicit"      0.20.0 "$(compute_new_version 0.18.2 0.20.0)"

# is_semver
ok "semver ok"          is_semver 0.18.3
no "semver prerelease"  is_semver 1.2.3-beta
no "semver short"       is_semver 1.2
no "semver long"        is_semver 1.2.3.4
no "semver nonnum"      is_semver 1.2.x
no "semver empty"       is_semver ""

# is_strictly_greater (numeric, not lexical -- 0.19.0 > 0.18.9, 0.18.10 > 0.18.9)
ok "gt patch"   is_strictly_greater 0.18.3 0.18.2
no "gt noop"    is_strictly_greater 0.18.2 0.18.2
no "gt down"    is_strictly_greater 0.18.1 0.18.2
ok "gt numeric" is_strictly_greater 0.19.0 0.18.9
ok "gt twodigit" is_strictly_greater 0.18.10 0.18.9
ok "gt major"   is_strictly_greater 1.0.0 0.18.2
no "gt majdown" is_strictly_greater 0.18.2 1.0.0

# non_changelog_dirty: only the configured changelog path is allowed dirty;
# anything else (including a rename whose destination is not the changelog)
# is "other dirty".
eq "dirty: changelog only allowed" "" \
  "$(printf ' M plugin/CHANGELOG.md\n' | non_changelog_dirty 'plugin/CHANGELOG.md')"
eq "dirty: other file flagged" " M src/main.rs" \
  "$(printf ' M plugin/CHANGELOG.md\n M src/main.rs\n' | non_changelog_dirty 'plugin/CHANGELOG.md')"
eq "dirty: suffix lookalike flagged" " M docs/plugin/CHANGELOG.md" \
  "$(printf ' M docs/plugin/CHANGELOG.md\n' | non_changelog_dirty 'plugin/CHANGELOG.md')"
eq "dirty: rename dest to changelog allowed" "" \
  "$(printf 'R  old.md -> plugin/CHANGELOG.md\n' | non_changelog_dirty 'plugin/CHANGELOG.md')"
eq "dirty: empty tree is clean" "" "$(printf '' | non_changelog_dirty 'plugin/CHANGELOG.md')"
eq "dirty: honors a different configured path" "" \
  "$(printf ' M CHANGELOG.md\n' | non_changelog_dirty 'CHANGELOG.md')"

# field_leaf: last "."-separated segment.
eq "leaf: nested"    "version" "$(field_leaf package.version)"
eq "leaf: flat"       "version" "$(field_leaf version)"
eq "leaf: array index" "version" "$(field_leaf plugins.0.version)"

# render_tag: "{version}" substitution.
eq "tag: default format"  "v0.20.0"        "$(render_tag 'v{version}' 0.20.0)"
eq "tag: no placeholder"  "release"        "$(render_tag release 0.20.0)"
eq "tag: custom format"   "ledger@0.2.0"   "$(render_tag '{version}' ledger@0.2.0)"

# bump_source_file: rewrites the version-of-record file in place per its
# extension, and never mutates + returns 1 on an unsupported one -- this is
# #741's proof that the bump step generalizes beyond Cargo.toml (a package.json
# source, e.g. a JS repo's `[version] file = "package.json"`, bumps the same way
# a flat json target does in scripts/sync-version.sh).
#
# Every fixture directory below is a CHILD of $SUITE_TMP rather than its own
# `mktemp -d`, so "is this path the suite's own?" -- which is what the perimeter
# guard asks on every git call -- is a single containment test.
BUMP_DIR="${SUITE_TMP}/bump"
mkdir -p "$BUMP_DIR"

cat >"${BUMP_DIR}/Cargo.toml" <<'EOF'
[package]
name = "fixture"
version = "0.6.5"
edition = "2024"
EOF
ok "bump: toml source" bump_source_file "${BUMP_DIR}/Cargo.toml" package.version 0.6.5 0.7.0
eq "bump: toml result" 'version = "0.7.0"' "$(grep '^version' "${BUMP_DIR}/Cargo.toml")"

cat >"${BUMP_DIR}/package.json" <<'EOF'
{
  "name": "@rafters/fixture",
  "version": "0.2.0"
}
EOF
ok "bump: json source" bump_source_file "${BUMP_DIR}/package.json" version 0.2.0 0.3.0
eq "bump: json result" '  "version": "0.3.0"' "$(grep '"version"' "${BUMP_DIR}/package.json")"
eq "bump: json rest untouched" '  "name": "@rafters/fixture",' "$(sed -n '2p' "${BUMP_DIR}/package.json")"

echo "unsupported" >"${BUMP_DIR}/version.yaml"
no "bump: unsupported format refused" bump_source_file "${BUMP_DIR}/version.yaml" version 0.1.0 0.2.0
eq "bump: unsupported format untouched" "unsupported" "$(cat "${BUMP_DIR}/version.yaml")"

# -- merge-queue phase helpers (#844) ---------------------------------------

# Stdin-reading helpers are wrapped so `ok`/`no` can exercise their exit status
# in THIS shell -- a `bash -c` subshell would not see the sourced functions at
# all, and would "fail" for the wrong reason.
feed_watch_root() { printf '%s' "$1" | watch_repo_root "$2"; }
feed_wt_holder()  { printf '%s' "$1" | worktree_holding_branch "$2"; }
feed_boundary()   { local input="$1"; shift; release_boundary_commit "$@" <<<"$input"; }

# classify_release_merge <landed> <state> <queue_present> <seen_queued>.
# The ORDER of the arms is the correctness property: "origin/main carries the
# new version" beats the PR's own state, which lags and which the queue's
# squash/rebase makes unreliable as a sha-level signal.
eq "merge: landed wins over open pr"    "merged"  "$(classify_release_merge 1 OPEN 1 1)"
eq "merge: landed wins over closed pr"  "merged"  "$(classify_release_merge 1 CLOSED 0 1)"
eq "merge: state merged"                "merged"  "$(classify_release_merge 0 MERGED 0 1)"
eq "merge: state closed"                "closed"  "$(classify_release_merge 0 CLOSED 0 1)"
eq "merge: in the queue"                "queued"  "$(classify_release_merge 0 OPEN 1 0)"
# Was in the queue, is no longer, and main does not carry the release: ejection.
eq "merge: ejected"                     "ejected" "$(classify_release_merge 0 OPEN 0 1)"
# Never seen in the queue is NOT evidence of ejection -- the queue admits a
# bounded group size, so a PR can sit un-enqueued for a while. Stays pending and
# expires into a timeout rather than reporting a false ejection.
eq "merge: never queued stays pending"  "pending" "$(classify_release_merge 0 OPEN 0 0)"
eq "merge: unknown state stays pending" "pending" "$(classify_release_merge 0 UNKNOWN 0 0)"

# `unknown` = the observation FAILED. Every comparison in the classifier is
# `= "1"`, so without an explicit arm a third value falls through to `ejected` --
# and these two are the exact shape of a sustained network outage: seen in the
# queue once, then nothing readable. Reporting that as "EJECTED from the merge
# queue" sends the operator to re-run gates on a PR that may have merged fine.
eq "merge: unknown landed is not an ejection" "pending" "$(classify_release_merge unknown OPEN 0 1)"
eq "merge: unknown queue is not an ejection"  "pending" "$(classify_release_merge 0 OPEN unknown 1)"
eq "merge: both observers unknown stays pending" "pending" "$(classify_release_merge unknown OPEN unknown 1)"
# ...but an unknown git observer must not veto a POSITIVE read from the other
# source: the PR state comes from the work-source API, not from the git remote.
eq "merge: pr state still decides under unknown landed" "merged" \
  "$(classify_release_merge unknown MERGED unknown 1)"
eq "merge: closed still decides under unknown landed"   "closed" \
  "$(classify_release_merge unknown CLOSED unknown 1)"
# A definite landed=1 outranks everything, unknowns included.
eq "merge: landed wins over unknown queue" "merged" "$(classify_release_merge 1 OPEN unknown 1)"

# wait_for_release_merge: the bounded poll, driven entirely by injected observer
# FUNCTIONS -- no gh, no git, no network, and no eval. The observers are passed
# by NAME and called directly, so nothing the tests (or a release.toml, or a
# --finish= argument) supply is ever parsed as shell. Sequenced observers keep
# their state in a counter FILE because each observer runs in a command
# substitution, i.e. a subshell: a shell variable would not survive the poll.
WAIT_DIR="${SUITE_TMP}/wait"
mkdir -p "$WAIT_DIR"
# step <file>: print the call count so far, then increment it.
step() { local f="$1" n; n="$(cat "$f")"; printf '%s\n' "$((n + 1))" >"$f"; printf '%s\n' "$n"; }

obs_landed_yes()  { printf '1\n'; }
obs_landed_no()   { printf '0\n'; }
obs_state_open()  { printf 'OPEN\n'; }
obs_state_closed(){ printf 'CLOSED\n'; }
obs_queue_yes()   { printf '1\n'; }
obs_queue_no()    { printf '0\n'; }
obs_unreadable()  { return 7; }

eq "wait: merged on the first poll" "merged" \
  "$(wait_for_release_merge 5 0 obs_landed_yes obs_state_open obs_queue_no)"
eq "wait: merged rc" "0" \
  "$(wait_for_release_merge 5 0 obs_landed_yes obs_state_open obs_queue_no >/dev/null; printf '%s' $?)"

eq "wait: closed pr stops the release" "closed" \
  "$(wait_for_release_merge 5 0 obs_landed_no obs_state_closed obs_queue_no)"
eq "wait: closed rc" "3" \
  "$(wait_for_release_merge 5 0 obs_landed_no obs_state_closed obs_queue_no >/dev/null || printf '%s' $?)"

# Queued for two polls, then lands: the authoritative `landed` signal flips
# while the queue ref disappears in the same window. Must read as merged, not
# as an ejection.
printf '0\n' >"${WAIT_DIR}/land"
seq_landed_on_3() { local n; n="$(step "${WAIT_DIR}/land")"; if [ "$n" -ge 2 ]; then printf '1\n'; else printf '0\n'; fi; }
seq_queue_till_3() { if [ "$(cat "${WAIT_DIR}/land")" -lt 2 ]; then printf '1\n'; else printf '0\n'; fi; }
eq "wait: queued then merged" "merged" \
  "$(wait_for_release_merge 6 0 seq_landed_on_3 obs_state_open seq_queue_till_3)"

# Seen in the queue, then gone, and main never carries the release: ejection --
# but only after the CONFIRMING second observation, since the queue ref also
# disappears at the moment of a successful merge.
printf '0\n' >"${WAIT_DIR}/ej"
seq_queue_once() { local n; n="$(step "${WAIT_DIR}/ej")"; if [ "$n" = 0 ]; then printf '1\n'; else printf '0\n'; fi; }
eq "wait: ejection reported" "ejected" \
  "$(wait_for_release_merge 6 0 obs_landed_no obs_state_open seq_queue_once)"
printf '0\n' >"${WAIT_DIR}/ej2"
seq_queue_once2() { local n; n="$(step "${WAIT_DIR}/ej2")"; if [ "$n" = 0 ]; then printf '1\n'; else printf '0\n'; fi; }
eq "wait: ejection rc" "4" \
  "$(wait_for_release_merge 6 0 obs_landed_no obs_state_open seq_queue_once2 >/dev/null || printf '%s' $?)"

# THE MERGE WINDOW, and the reason the ejection verdict needs a streak of two.
# The sequence is a real one: poll 1 the PR is in the queue; poll 2 the queue ref
# is GONE but origin/main is not yet observed to carry the version -- which is
# indistinguishable from an ejection in a single observation; poll 3 it lands.
# Correct code holds its verdict at poll 2 and reports merged. The counters are
# INDEPENDENT on purpose: driving the queue observer off the landed counter made
# poll 1 read "not queued", so seen_queued never became 1, the ejected arm was
# never reached, and the test passed just as happily with the streak guard
# mutated to -ge 1.
printf '0\n' >"${WAIT_DIR}/race"
printf '0\n' >"${WAIT_DIR}/raceq"
race_landed() { local n; n="$(step "${WAIT_DIR}/race")"; if [ "$n" -ge 2 ]; then printf '1\n'; else printf '0\n'; fi; }
race_queue()  { local n; n="$(step "${WAIT_DIR}/raceq")"; if [ "$n" = 0 ]; then printf '1\n'; else printf '0\n'; fi; }
eq "wait: merge-window ref drop is not an ejection" "merged" \
  "$(wait_for_release_merge 4 0 race_landed obs_state_open race_queue)"

eq "wait: budget expiry is a timeout" "timeout" \
  "$(wait_for_release_merge 3 0 obs_landed_no obs_state_open obs_queue_no)"
eq "wait: timeout rc" "5" \
  "$(wait_for_release_merge 3 0 obs_landed_no obs_state_open obs_queue_no >/dev/null || printf '%s' $?)"
# An observer that cannot be read must not stall or crash the wait -- and must
# not be counted as a NEGATIVE either. It becomes `unknown`, the budget expires,
# and the release reports a timeout ("I could not observe the merge"), which is
# the truth. Under the old `|| printf 0` fallback the same outage read as
# "definitely not in the queue" and was reported as an ejection.
eq "wait: unreadable observers expire" "timeout" \
  "$(wait_for_release_merge 2 0 obs_unreadable obs_unreadable obs_unreadable)"
# The sustained-outage shape end to end: queued once, then every observer blind.
printf '0\n' >"${WAIT_DIR}/out"
outage_queue() { local n; n="$(step "${WAIT_DIR}/out")"; if [ "$n" = 0 ]; then printf '1\n'; else return 7; fi; }
eq "wait: outage after a queued poll is a timeout, not an ejection" "timeout" \
  "$(wait_for_release_merge 4 0 obs_unreadable obs_state_open outage_queue)"
eq "wait: outage rc is timeout not ejection" "5" \
  "$(printf '0\n' >"${WAIT_DIR}/out"; wait_for_release_merge 4 0 obs_unreadable obs_state_open outage_queue >/dev/null || printf '%s' $?)"
# The observers are called in THIS shell, so a SOURCED helper resolves. Run in a
# subshell that could not see release.sh's own functions, every poll would read
# landed=unknown and a merged release would time out.
sourced_probe() { printf '1\n'; }
eq "wait: injected observer resolves a sourced function" "merged" \
  "$(wait_for_release_merge 2 0 sourced_probe obs_state_open obs_queue_no)"

# release_landed / release_queue_ref_present: the two GIT-BACKED observers, and
# the only place `unknown` is actually produced. Against a real local remote --
# no network, no gh -- because the whole finding is about what happens when the
# remote cannot be reached, which a stubbed observer cannot show.
#
# release_landed and release_queue_ref_present operate on the CURRENT directory
# (finish_release cd's to the repo root first), so the assertions below run them
# through `in_dir` -- defined up in THE FIXTURE PERIMETER, because it is part of
# the perimeter rather than a convenience for this section.
#
# THE FIXTURE WHOSE SHAPE IS THE INCIDENT: a `chore(release): 0.25.0` commit
# bumping the version-of-record, and a `push -u origin main`. Nothing here is
# exotic; the escape was entirely in WHERE those lines land, which is why every
# one of them now goes through the perimeter rather than a bare `git -C`.
#
# The Cargo.toml carries a REAL [package] table. It used to be the incident's
# literal one-liner (`version = "0.25.0"`, no table), and that made this whole
# block self-deceiving: every `--field package.version` read failed, so
# `ref_version` returned "" on every call here and the assertions -- which all
# expect 0 or `unknown` -- passed for the wrong reason. Mutating `release_landed`
# to print 0 unconditionally left the suite green at 159/159. Reproducing the
# incident's bytes is the perimeter's job, above; this block's job is to exercise
# the observers, and it cannot do that against a file the reader cannot parse.
OBS_TMP="${SUITE_TMP}/obs"
mkdir -p "$OBS_TMP"
OBS_CO="${OBS_TMP}/repo"
fixture_git_init "${OBS_TMP}/origin.git" init --bare --quiet
fixture_git_init "$OBS_CO" -c init.defaultBranch=main init --quiet
fixture_git "$OBS_CO" config user.email t@t
fixture_git "$OBS_CO" config user.name t
fixture_git "$OBS_CO" checkout --quiet -B main
cat >"${OBS_CO}/Cargo.toml" <<'EOF'
[package]
name = "observer-fixture"
version = "0.25.0"
edition = "2024"
EOF
fixture_git "$OBS_CO" add Cargo.toml
fixture_git "$OBS_CO" commit --quiet --no-verify -m "chore(release): 0.25.0"
fixture_git "$OBS_CO" remote add origin "${OBS_TMP}/origin.git"
gitp "$OBS_CO" push --quiet -u origin main

if require_dir "observers: fixture repo" "$OBS_CO"; then
  # THE PRECONDITION FOR EVERYTHING BELOW, and the assertion that would have
  # caught the broken fixture: if the version-of-record cannot be READ, every
  # observer answers "" and the negative assertions all pass vacuously.
  eq "landed: the fixture's version-of-record is actually readable" "0.25.0" \
    "$( (in_dir "$OBS_CO" ref_version HEAD Cargo.toml package.version) 2>/dev/null )"

  # THE POSITIVE ARM. Nothing anywhere in this suite covered release_landed = 1,
  # which is why mutating it to print 0 unconditionally changed no result.
  eq "landed: a reachable remote that carries the version is 1" "1" \
    "$( (in_dir "$OBS_CO" release_landed main Cargo.toml package.version 0.25.0) 2>/dev/null )"

  # A reachable remote that does not carry the version is a definite NO.
  eq "landed: reachable remote without the version is 0" "0" \
    "$( (in_dir "$OBS_CO" release_landed main Cargo.toml package.version 9.9.9) 2>/dev/null )"

  # THE SECOND DOOR INTO THE FALSE EJECTION. Fix 2 keyed `unknown` on the FETCH's
  # exit status, which is right as far as it goes -- but a fetch that SUCCEEDS
  # followed by a READ that fails was indistinguishable from "the remote
  # definitely does not carry this version". Every failure inside ref_version was
  # swallowed into an empty string, and release_landed printed a confident 0;
  # sustained past a queued observation, a 0 is an EJECTED verdict for a release
  # that landed. This fixture is reachable and fetchable and simply has no
  # version file on the ref.
  NOFILE_CO="${OBS_TMP}/nofile"
  fixture_git_init "${OBS_TMP}/nofile-origin.git" init --bare --quiet
  fixture_git_init "$NOFILE_CO" -c init.defaultBranch=main init --quiet
  fixture_git "$NOFILE_CO" config user.email t@t
  fixture_git "$NOFILE_CO" config user.name t
  fixture_git "$NOFILE_CO" checkout --quiet -B main
  printf 'no version file here\n' >"${NOFILE_CO}/README.md"
  fixture_git "$NOFILE_CO" add README.md
  fixture_git "$NOFILE_CO" commit --quiet --no-verify -m "no version file"
  fixture_git "$NOFILE_CO" remote add origin "${OBS_TMP}/nofile-origin.git"
  gitp "$NOFILE_CO" push --quiet -u origin main

  eq "ref_version: a file absent on the ref is rc 2, not an empty success" "2" \
    "$( (in_dir "$NOFILE_CO" ref_version HEAD Cargo.toml package.version) >/dev/null 2>&1; printf '%s' $? )"
  eq "ref_version: and it prints nothing at all" "" \
    "$( (in_dir "$NOFILE_CO" ref_version HEAD Cargo.toml package.version) 2>/dev/null )"
  eq "ref_version: a nonexistent ref is rc 2" "2" \
    "$( (in_dir "$NOFILE_CO" ref_version no-such-ref Cargo.toml package.version) >/dev/null 2>&1; printf '%s' $? )"
  # A field the file genuinely does not carry is a FAILED read, not an empty one:
  # `extract` returning nothing is exactly the state that used to read as 0.
  eq "ref_version: a field the file does not carry is rc 2" "2" \
    "$( (in_dir "$OBS_CO" ref_version HEAD Cargo.toml package.nosuchfield) >/dev/null 2>&1; printf '%s' $? )"
  eq "ref_version: a successful read is rc 0" "0" \
    "$( (in_dir "$OBS_CO" ref_version HEAD Cargo.toml package.version) >/dev/null 2>&1; printf '%s' $? )"
  eq "landed: a successful fetch with an unreadable version is unknown, not 0" "unknown" \
    "$( (in_dir "$NOFILE_CO" release_landed main Cargo.toml package.version 0.25.0) 2>/dev/null )"

  # THE REGRESSION. Break the remote AFTER a successful fetch, so the
  # remote-tracking ref is still perfectly readable locally -- which is exactly
  # what a network outage looks like: origin/main does not vanish, it just stops
  # being updated. A gate phrased as "did the ref read cleanly" answers a
  # confident 0 here, and the poll loop turns a sustained 0 into an EJECTED
  # verdict. Only the fetch's exit status can tell the difference.
  fixture_git "$OBS_CO" remote set-url origin "${OBS_TMP}/does-not-exist.git"
  ok "landed: the remote-tracking ref is still readable during the outage" \
    test -n "$(fixture_git "$OBS_CO" rev-parse --verify --quiet origin/main 2>/dev/null)"
  eq "landed: unreachable remote is unknown, not 0" "unknown" \
    "$( (in_dir "$OBS_CO" release_landed main Cargo.toml package.version 0.25.0) 2>/dev/null )"
  # Same failure, same requirement, for the queue observer: the rc must belong to
  # ls-remote, not to the grep that used to consume it.
  eq "queue: unreachable remote is unknown, not 0" "unknown" \
    "$( (in_dir "$OBS_CO" release_queue_ref_present 42) 2>/dev/null )"

  fixture_git "$OBS_CO" remote set-url origin "${OBS_TMP}/origin.git"
  eq "queue: reachable remote without the ref is 0" "0" \
    "$( (in_dir "$OBS_CO" release_queue_ref_present 42) 2>/dev/null )"
  # A real merge-queue ref: refs/heads/gh-readonly-queue/<base>/pr-<N>-<sha>.
  OBS_SHA="$(fixture_git "$OBS_CO" rev-parse HEAD)"
  # A BARE repo: fixture_git keys on the common git dir, which a bare repo has,
  # so this is guarded exactly like the checkouts are.
  fixture_git "${OBS_TMP}/origin.git" update-ref \
    "refs/heads/gh-readonly-queue/main/pr-42-${OBS_SHA}" "$OBS_SHA"
  eq "queue: the PR's queue ref is present" "1" \
    "$( (in_dir "$OBS_CO" release_queue_ref_present 42) 2>/dev/null )"
  # Another PR's queue ref is not this PR's.
  eq "queue: another PR's ref does not count" "0" \
    "$( (in_dir "$OBS_CO" release_queue_ref_present 7) 2>/dev/null )"
  # And a number that is a PREFIX of the queued one must not match either.
  eq "queue: a prefix of the queued PR number does not match" "0" \
    "$( (in_dir "$OBS_CO" release_queue_ref_present 4) 2>/dev/null )"
fi

# tag_target_matches: the guard between "the queue merged something" and "the
# tag points at the release". Fails CLOSED on an unreadable version.
ok "tag target: match"          tag_target_matches 0.26.0 0.26.0
no "tag target: mismatch"       tag_target_matches 0.26.0 0.25.0
no "tag target: empty observed" tag_target_matches 0.26.0 ""
# The fail-closed conjunct specifically. Without it the function is plain string
# equality, under which two empty strings MATCH -- "we could not read the
# expected version" and "we could not read what is on the branch" would agree
# with each other and authorise the tag. The mismatch case above still returns 1
# under that mutation, so this is the only assertion that pins the conjunct.
no "tag target: both empty fails closed" tag_target_matches "" ""

# release_boundary_commit: which commit the tag actually lands on. The tip of
# the release branch is the WRONG answer -- later commits do not touch the
# version file, so "this tree carries <new>" still passes N merges past the
# release and the tag would ship a tree nobody released.
#
# The reader is injected by NAME, so these cases are a pure table: a fake
# history where sha -> version is a lookup.
fake_version() {
  case "$1" in
    f1|f2|f3) printf '0.26.0\n' ;;   # f1 = tip-most, f3 = the release commit
    f4|f5)    printf '0.25.0\n' ;;
    *)        printf '\n' ;;
  esac
}
eq "boundary: oldest consecutive match wins" "f3" \
  "$(printf 'f1\nf2\nf3\nf4\nf5\n' | release_boundary_commit fake_version 0.26.0 50)"
eq "boundary: a single-commit release resolves to itself" "f3" \
  "$(printf 'f3\nf4\nf5\n' | release_boundary_commit fake_version 0.26.0 50)"
# The tip-most entry not carrying the version at all: rc 1. The caller gates on
# the branch tip before it ever walks, so production cannot reach this -- it is
# the standalone contract, and a rc that is never asserted is a rc that drifts.
no "boundary: newest does not carry the version" \
  feed_boundary "$(printf 'f4\nf5\n')" fake_version 0.26.0 50
eq "boundary: rc when nothing matches" "1" \
  "$(printf 'f4\nf5\n' | release_boundary_commit fake_version 0.26.0 50 >/dev/null; printf '%s' $?)"
# Running off the end of the list is NOT "tag the oldest thing you saw". Every
# exhausted path returns non-zero and the caller stops: an unresolved boundary
# must never fall through to tagging.
eq "boundary: list exhausted is rc 3" "3" \
  "$(printf 'f1\nf2\nf3\n' | release_boundary_commit fake_version 0.26.0 50 >/dev/null; printf '%s' $?)"
eq "boundary: exhausted prints nothing" "" \
  "$(printf 'f1\nf2\nf3\n' | release_boundary_commit fake_version 0.26.0 50 2>/dev/null)"
eq "boundary: cap exceeded is rc 2" "2" \
  "$(printf 'f1\nf2\nf3\nf4\n' | release_boundary_commit fake_version 0.26.0 2 >/dev/null; printf '%s' $?)"
eq "boundary: cap exceeded prints nothing" "" \
  "$(printf 'f1\nf2\nf3\nf4\n' | release_boundary_commit fake_version 0.26.0 2 2>/dev/null)"
# A cap that exactly covers the boundary still resolves it.
eq "boundary: cap exactly at the boundary" "f3" \
  "$(printf 'f1\nf2\nf3\nf4\n' | release_boundary_commit fake_version 0.26.0 4)"
# An unreadable version reads as a mismatch, which ends the walk -- it never
# widens it.
eq "boundary: unreadable version ends the walk" "f1" \
  "$(printf 'f1\nunknown-sha\nf3\n' | release_boundary_commit fake_version 0.26.0 50)"

# -- cross-repo docs worktree helpers (#845) --------------------------------

# watch_repo_root: the docs repo's checkout is resolved, never hardcoded (the
# path differs per machine). Tab-separated "<name>\t<path>", with an optional
# " (agent: X)" suffix on the path.
WATCH_FIXTURE="$(printf 'legion\t/p/legion\nshingle\t/p/shingle\nastro-consent\t/p/astro-consent (agent: shingle)\n')"
eq "watch: resolves a repo"        "/p/shingle"      "$(printf '%s\n' "$WATCH_FIXTURE" | watch_repo_root shingle)"
eq "watch: strips agent suffix"    "/p/astro-consent" "$(printf '%s\n' "$WATCH_FIXTURE" | watch_repo_root astro-consent)"
eq "watch: exact name match only"  ""                "$(printf '%s\n' "$WATCH_FIXTURE" | watch_repo_root shing || true)"
no "watch: unregistered repo refused" feed_watch_root "$WATCH_FIXTURE" nope

# docs_start_point: where #845 (start from a REMOTE ref, never the shared
# checkout's current branch) meets #820 (a STABLE branch the next release
# extends, not a version-pinned one that stacks a PR per release).
LSREMOTE="$(printf 'aaa\trefs/heads/main\nbbb\trefs/heads/docs/legion-current\nccc\trefs/heads/docs/legion-0.24.0\n')"
eq "docs start: extends the existing stable branch" "origin/docs/legion-current" \
  "$(printf '%s\n' "$LSREMOTE" | docs_start_point 'docs/legion-current' 'origin/main')"
eq "docs start: falls back to the base when absent" "origin/main" \
  "$(printf 'aaa\trefs/heads/main\n' | docs_start_point 'docs/legion-current' 'origin/main')"
eq "docs start: no remote heads at all" "origin/main" \
  "$(printf '' | docs_start_point 'docs/legion-current' 'origin/main')"
# A prefix lookalike must not be mistaken for the branch -- starting from the
# wrong remote branch is the same class of defect as starting from the shared
# checkout's branch.
eq "docs start: prefix lookalike ignored" "origin/main" \
  "$(printf 'ccc\trefs/heads/docs/legion-current-old\n' | docs_start_point 'docs/legion-current' 'origin/main')"

# worktree_holding_branch: a stale worktree on the stable docs branch is
# REPORTED, not clobbered -- it may hold unpushed work.
WTLIST="$(printf 'worktree /p/shingle\nHEAD aaa\nbranch refs/heads/main\n\nworktree /tmp/x/shingle-docs\nHEAD bbb\nbranch refs/heads/docs/legion-current\n')"
eq "worktree: finds the holder" "/tmp/x/shingle-docs" \
  "$(printf '%s\n' "$WTLIST" | worktree_holding_branch 'docs/legion-current')"
eq "worktree: finds the main checkout" "/p/shingle" \
  "$(printf '%s\n' "$WTLIST" | worktree_holding_branch 'main')"
no "worktree: nobody holds it" \
  feed_wt_holder "$(printf 'worktree /p/shingle\nHEAD aaa\nbranch refs/heads/main\n')" 'docs/legion-current'
# A detached worktree has no `branch` line at all and must not match.
no "worktree: detached entry does not match" \
  feed_wt_holder "$(printf 'worktree /p/shingle\nHEAD aaa\ndetached\n')" 'docs/legion-current'

# worktree_is_disposable <head> <base> <status> <pushed>: removing it must
# throw nothing away.
ok "disposable: untouched since setup"      worktree_is_disposable aaa aaa "" 0
no "disposable: uncommitted changes kept"   worktree_is_disposable aaa aaa " M docs/x.mdx" 0
no "disposable: unpushed commits kept"      worktree_is_disposable bbb aaa "" 0
# The second arm: a docs run that committed AND pushed carries commits but
# loses nothing when removed. Without it a SUCCESSFUL run would pin the
# worktree forever and collide on the stable branch at the next release.
ok "disposable: committed but pushed"       worktree_is_disposable bbb aaa "" 1
no "disposable: pushed but tree dirty"      worktree_is_disposable bbb aaa " M docs/x.mdx" 1
no "disposable: unreadable head fails closed" worktree_is_disposable "" aaa "" 1
# A missing base sha fails closed too. With base empty the `head = base` arm can
# never fire, so the whole decision silently collapses onto `pushed` -- and
# "clean and fully pushed" describes nearly every worktree in a healthy repo,
# including one another agent is working in.
no "disposable: missing base sha fails closed" worktree_is_disposable bbb "" "" 1
no "disposable: missing base sha even when untouched" worktree_is_disposable bbb "" "" 0

# docs_worktree_setup / docs_worktree_teardown against REAL git repos. These two
# are the mechanism #845 is actually buying -- the pure helpers above only decide
# what they should do -- so they are exercised end to end against a local bare
# remote. No network, no gh, no shingle. Both call `fail` (which exits), so every
# arm runs in a subshell.
#
# Every fixture command below goes through `fixture_git`, which resolves the
# target repo and refuses anything that is not this suite's own temp tree (see
# THE FIXTURE PERIMETER at the top of this file). `docs_worktree_setup` and
# `docs_worktree_teardown` are the functions UNDER TEST, so their internal git
# calls cannot be routed -- what bounds them is `in_dir`/`require_dir` on the
# way in, the fact that the only checkout they are ever handed is $DOCS_CO, and
# the perimeter on every assertion made about their output afterwards.
DOCS_TMP="${SUITE_TMP}/docs"
mkdir -p "$DOCS_TMP"
DOCS_CO="${DOCS_TMP}/checkout"
DOCS_BRANCH="docs/fixture-current"
fixture_git_init "${DOCS_TMP}/origin.git" init --bare --quiet
fixture_git_init "$DOCS_CO" -c init.defaultBranch=main init --quiet
fixture_git "$DOCS_CO" config user.email t@t
fixture_git "$DOCS_CO" config user.name t
fixture_git "$DOCS_CO" checkout --quiet -B main
printf 'docs\n' >"${DOCS_CO}/index.md"
fixture_git "$DOCS_CO" add index.md
fixture_git "$DOCS_CO" commit --quiet --no-verify -m "init"
fixture_git "$DOCS_CO" remote add origin "${DOCS_TMP}/origin.git"
gitp "$DOCS_CO" push --quiet -u origin main
# Prove the pin took, rather than assuming it: every assertion below about
# config-independence rests on this.
eq "fixture: ambient global config is neutralised" "" \
  "$(fixture_git "$DOCS_CO" config --global --get merge.ff 2>/dev/null || true)"
eq "fixture: repo identity is set for worktree commits" "t@t" \
  "$(fixture_git "$DOCS_CO" config user.email 2>/dev/null)"
# The remote guard, asserted rather than assumed: the fixture's origin resolves
# inside the suite temp root. A fixture whose origin is a real forge is the
# entire incident, so this must be a checked property of the fixture, not a
# property of the line that happened to create it.
ok "fixture: origin is a temp bare repo inside the suite root" \
  path_under "$(phys "$(fixture_git "$DOCS_CO" remote get-url origin)")" "$SUITE_TMP"

# Fresh arm: no such branch on the remote, so the worktree starts from
# origin/main -- NOT from whatever the shared checkout has checked out. Prove
# that by parking the shared checkout on an unrelated branch first.
fixture_git "$DOCS_CO" checkout --quiet -b someone-elses-work
printf 'wip\n' >"${DOCS_CO}/wip.md"
fixture_git "$DOCS_CO" add wip.md
fixture_git "$DOCS_CO" commit --quiet --no-verify -m "wip"
WT1="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>/dev/null )"
# Registered for cleanup the moment the path is known: docs_worktree_setup makes
# its own mktemp parent OUTSIDE $DOCS_TMP, and a red run never reaches teardown.
[ -z "$WT1" ] || register_temp "$(dirname "$WT1")"
if require_dir "docs wt: created" "$WT1"; then
  eq "docs wt: on the stable branch" "$DOCS_BRANCH" "$(fixture_git "$WT1" rev-parse --abbrev-ref HEAD 2>/dev/null)"
  # The load-bearing one: it must NOT have inherited the other agent's branch.
  eq "docs wt: starts from origin/main, not the shared checkout's branch" \
    "$(fixture_git "$DOCS_CO" rev-parse origin/main)" "$(fixture_git "$WT1" rev-parse HEAD 2>/dev/null)"
  ok "docs wt: the other agent's file is absent" test ! -f "${WT1}/wip.md"
  ok "docs wt: base sha recorded outside the worktree" test -f "$(dirname "$WT1")/base-sha"
  eq "docs wt: worktree left clean" "" "$(fixture_git "$WT1" status --porcelain 2>/dev/null)"
  # The OWNERSHIP marker, and it must name this worktree by path -- teardown
  # refuses to destroy anything it cannot match against this file.
  ok "docs wt: ownership marker written" test -f "$(dirname "$WT1")/worktree-path"
  eq "docs wt: ownership marker names this worktree" "$WT1" \
    "$(cat "$(dirname "$WT1")/worktree-path" 2>/dev/null)"
fi

# Collision: the stable branch is already checked out. Report it, never clobber
# it, and above all never emit the shared checkout path as the answer.
COLLIDE_RC=0
COLLIDE_OUT="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>/dev/null )" || COLLIDE_RC=$?
eq "docs wt: collision fails" "1" "$COLLIDE_RC"
eq "docs wt: collision emits no path at all" "" "$COLLIDE_OUT"
ok "docs wt: collision never yields the shared checkout" test "$COLLIDE_OUT" != "$DOCS_CO"

# Teardown, unchanged: removed, and the local branch with it, so the next
# release does not collide on the stable name.
(docs_worktree_teardown "$WT1") >/dev/null 2>&1
ok "docs teardown: unchanged worktree removed" test ! -d "$WT1"
no "docs teardown: unchanged branch removed too" \
  fixture_git "$DOCS_CO" show-ref --verify --quiet "refs/heads/${DOCS_BRANCH}"

# Teardown, carrying unpushed commits: RETAINED, so a failed docs run is
# recoverable rather than discarded.
WT2="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>/dev/null )"
[ -z "$WT2" ] || register_temp "$(dirname "$WT2")"
if require_dir "docs teardown: second worktree created" "$WT2"; then
  printf 'draft\n' >"${WT2}/draft.md"
  fixture_git "$WT2" add draft.md
  fixture_git "$WT2" commit --quiet --no-verify -m "docs: draft"
  (docs_worktree_teardown "$WT2") >/dev/null 2>&1
  ok "docs teardown: unpushed work retained" test -d "$WT2"
  ok "docs teardown: retained worktree keeps its commit" test -f "${WT2}/draft.md"

  # Same worktree once the work is PUSHED: now disposable. Without this arm a
  # successful docs run would pin the worktree forever and collide next release.
  gitp "$WT2" push --quiet -u origin "$DOCS_BRANCH"
  (docs_worktree_teardown "$WT2") >/dev/null 2>&1
  ok "docs teardown: pushed work is disposable" test ! -d "$WT2"
fi

# Extend arm (#820): the stable branch now exists on the remote, so the next
# release starts from IT and merges origin/main in -- one docs PR that each
# release extends, not a new version-pinned branch stacked on the last.
fixture_git "$DOCS_CO" checkout --quiet main
printf 'more\n' >>"${DOCS_CO}/index.md"
fixture_git "$DOCS_CO" commit --quiet --no-verify -am "main moves on"
gitp "$DOCS_CO" push --quiet origin main

# The extend arm runs under a HOSTILE-BUT-ORDINARY config, pinned on the repo so
# the assertion does not depend on the operator's machine. `merge.ff = only` is a
# common global setting, and under it the plain `git merge` this used to run dies
# with "fatal: Not possible to fast-forward" -- on entirely CORRECT input, since a
# diverged docs branch is the whole reason the merge exists. It then told the
# operator to go and resolve it in the shared checkout, which is the fallback
# #845 exists to close. `user.useConfigOnly` is pinned alongside it because it
# breaks the same step a different way.
fixture_git "$DOCS_CO" config merge.ff only
fixture_git "$DOCS_CO" config user.useConfigOnly true
WT3="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>/dev/null )"
[ -z "$WT3" ] || register_temp "$(dirname "$WT3")"
if require_dir "docs extend: worktree created under merge.ff=only" "$WT3"; then
  ok "docs extend: the earlier release's docs commit is still there" test -f "${WT3}/draft.md"
  ok "docs extend: origin/main was merged in" \
    fixture_git "$WT3" merge-base --is-ancestor origin/main HEAD
  # --no-ff, so the merge-in is a real merge commit (two parents) regardless of
  # whether it could have fast-forwarded.
  eq "docs extend: merge-in is a merge commit" "2" \
    "$(fixture_git "$WT3" rev-list --parents -n 1 HEAD 2>/dev/null | awk '{print NF-1}')"
  # The merge is setup's own work, so it must not count as agent work at teardown.
  eq "docs extend: base sha recorded after the merge-in" \
    "$(fixture_git "$WT3" rev-parse HEAD 2>/dev/null)" "$(cat "$(dirname "$WT3")/base-sha" 2>/dev/null)"
  (docs_worktree_teardown "$WT3") >/dev/null 2>&1
  ok "docs extend: unchanged extend-arm worktree removed" test ! -d "$WT3"
fi
fixture_git "$DOCS_CO" config --unset merge.ff
fixture_git "$DOCS_CO" config --unset user.useConfigOnly

# A retained worktree whose DIRECTORY was deleted by hand stays registered as
# "prunable". That registration is invisible to worktree_holding_branch, so every
# later --docs-worktree failed naming a path that no longer existed, with no hint
# that `git worktree prune` was the way out. Setup prunes first now, so the
# stale registration resolves itself and what remains is the local BRANCH -- a
# real, inspectable object the message can name a recovery for.
WT4="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>/dev/null )"
[ -z "$WT4" ] || register_temp "$(dirname "$WT4")"
if require_dir "docs prune: worktree created" "$WT4"; then
  rm -rf "$WT4"
  PRUNE_ERR="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>&1 >/dev/null )"
  ok "docs prune: vanished worktree no longer reported as the holder" \
    test -z "${PRUNE_ERR##*local branch*}"
  ok "docs prune: message does not name the vanished path" \
    test "${PRUNE_ERR##*"$WT4"*}" = "$PRUNE_ERR"
  ok "docs prune: registration was actually pruned" \
    test -z "$(fixture_git "$DOCS_CO" worktree list --porcelain | worktree_holding_branch "$DOCS_BRANCH" || true)"
  # And the named recovery genuinely unblocks it: delete the branch, run again.
  fixture_git "$DOCS_CO" branch -D "$DOCS_BRANCH" >/dev/null 2>&1
  WT5="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>/dev/null )"
  [ -z "$WT5" ] || register_temp "$(dirname "$WT5")"
  ok "docs prune: docs step is unblocked after the named recovery" test -n "$WT5"
  [ -z "$WT5" ] || (docs_worktree_teardown "$WT5") >/dev/null 2>&1
fi

# --docs-worktree-done= takes an OPERATOR-supplied path, and a worktree this
# script did not create is one it has NO CLAIM TO DESTROY. All three destructive
# acts are gated on the ownership marker, not just the `rm -rf`: force-removing
# someone's worktree and deleting their branch are exactly as destructive as
# deleting a directory, and `branch -D ... >/dev/null 2>&1 || true` did not even
# report that it had happened.
#
# This assertion was previously INVERTED -- it asserted the foreign worktree was
# "still removed" and only checked that the parent survived. It encoded the
# defect as a requirement, which is why no mutation ever surfaced it: the guard
# it was protecting was the `rm -rf`, and the two genuinely destructive calls
# above it were never covered at all.
SHARED="${DOCS_TMP}/shared"
mkdir -p "$SHARED"
printf 'do not delete me\n' >"${SHARED}/sibling.txt"
fixture_git "$DOCS_CO" worktree add --quiet -b hand-made "${SHARED}/hand-made" main >/dev/null 2>&1
# Pushed, so it satisfies the DISPOSABLE arm on content. It must survive anyway:
# "looks disposable" is not an ownership claim, and every clean fully-pushed
# worktree in the docs repo looks exactly like this one.
gitp "${SHARED}/hand-made" push --quiet -u origin hand-made
FOREIGN_RC=0
(docs_worktree_teardown "${SHARED}/hand-made") >/dev/null 2>&1 || FOREIGN_RC=$?
eq "docs teardown: foreign worktree teardown refuses" "1" "$FOREIGN_RC"
ok "docs teardown: foreign worktree RETAINED" test -d "${SHARED}/hand-made"
ok "docs teardown: foreign worktree's branch survives" \
  fixture_git "$DOCS_CO" show-ref --verify --quiet "refs/heads/hand-made"
ok "docs teardown: foreign worktree keeps its registration" \
  test -n "$(fixture_git "$DOCS_CO" worktree list --porcelain | worktree_holding_branch hand-made || true)"
ok "docs teardown: an unowned parent directory survives" test -d "$SHARED"
ok "docs teardown: its siblings survive" test -f "${SHARED}/sibling.txt"

# A marker naming a DIFFERENT worktree is not a claim to this one either -- the
# marker has to match, or "some sibling directory has a base-sha in it" would be
# enough to authorise the removal.
MISMARK="${DOCS_TMP}/mismarked"
mkdir -p "$MISMARK"
fixture_git "$DOCS_CO" worktree add --quiet -b mis-marked "${MISMARK}/wt" main >/dev/null 2>&1
gitp "${MISMARK}/wt" push --quiet -u origin mis-marked
printf '%s\n' "$(fixture_git "${MISMARK}/wt" rev-parse HEAD)" >"${MISMARK}/base-sha"
printf '%s\n' "${MISMARK}/some-other-worktree" >"${MISMARK}/worktree-path"
MISMARK_RC=0
(docs_worktree_teardown "${MISMARK}/wt") >/dev/null 2>&1 || MISMARK_RC=$?
eq "docs teardown: marker naming another worktree refuses" "1" "$MISMARK_RC"
ok "docs teardown: mismarked worktree retained" test -d "${MISMARK}/wt"

# -- conflict vs. config: two operator-facing messages, one rc (#845) --------
#
# `git merge` exits 1 both when CONTENT conflicts and when a hook or a config
# setting refuses the merge, and the two need opposite responses -- reconcile the
# branches, versus fix your git config. The discrimination is therefore a
# behaviour (are there unmerged paths in the index?), and it was chosen by logic
# nothing exercised. Both arms must also leave the same wreckage behind: no
# worktree, no branch, and above all no edit to the shared checkout, which is the
# fallback #845 exists to close.

# ARM 1: REFUSED WITHOUT A CONFLICT. A failing `pre-merge-commit` hook is the
# honest reproduction -- the content merges cleanly, so the index has no unmerged
# paths, and it is the COMMIT git declines. The hook lives outside $DOCS_CO so
# the shared checkout stays clean, which is one of the things asserted below.
DOCS_HOOKS="${DOCS_TMP}/hooks"
mkdir -p "$DOCS_HOOKS"
printf '#!/bin/sh\nexit 1\n' >"${DOCS_HOOKS}/pre-merge-commit"
chmod +x "${DOCS_HOOKS}/pre-merge-commit"
DOCS_CO_HEAD="$(fixture_git "$DOCS_CO" rev-parse HEAD)"
fixture_git "$DOCS_CO" config core.hooksPath "$DOCS_HOOKS"
NOCONF_RC=0
NOCONF_ERR="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>&1 >/dev/null )" || NOCONF_RC=$?
fixture_git "$DOCS_CO" config --unset core.hooksPath
eq "docs config-refusal: setup fails" "1" "$NOCONF_RC"
ok "docs config-refusal: reports FAILED WITHOUT CONFLICT" \
  test -z "${NOCONF_ERR##*FAILED WITHOUT CONFLICT (no unmerged paths)*}"
ok "docs config-refusal: does NOT report a content conflict" \
  test "${NOCONF_ERR##*CONFLICTED*}" = "$NOCONF_ERR"
ok "docs config-refusal: names config and hooks as the thing to fix" \
  test -z "${NOCONF_ERR##*refused by git config or a hook*}"
ok "docs config-refusal: the worktree was removed" \
  test -z "$(fixture_git "$DOCS_CO" worktree list --porcelain | worktree_holding_branch "$DOCS_BRANCH" || true)"
no "docs config-refusal: the branch was removed with it" \
  fixture_git "$DOCS_CO" show-ref --verify --quiet "refs/heads/${DOCS_BRANCH}"
eq "docs config-refusal: the shared checkout is clean" "" \
  "$(fixture_git "$DOCS_CO" status --porcelain)"
eq "docs config-refusal: the shared checkout did not move" "$DOCS_CO_HEAD" \
  "$(fixture_git "$DOCS_CO" rev-parse HEAD)"

# ARM 2: A REAL CONTENT CONFLICT. Both sides rewrite the same line of the same
# file, which is the only way to make the merge fail on content rather than on
# policy.
fixture_git "$DOCS_CO" fetch --quiet origin
fixture_git "$DOCS_CO" checkout --quiet -B conflict-stage "origin/${DOCS_BRANCH}"
printf 'the docs branch rewrote this line\n' >"${DOCS_CO}/index.md"
fixture_git "$DOCS_CO" commit --quiet --no-verify -am "docs side"
gitp "$DOCS_CO" push --quiet origin "conflict-stage:${DOCS_BRANCH}"
fixture_git "$DOCS_CO" checkout --quiet main
printf 'main rewrote the same line\n' >"${DOCS_CO}/index.md"
fixture_git "$DOCS_CO" commit --quiet --no-verify -am "main side"
gitp "$DOCS_CO" push --quiet origin main
DOCS_CO_HEAD="$(fixture_git "$DOCS_CO" rev-parse HEAD)"
CONFLICT_RC=0
CONFLICT_ERR="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>&1 >/dev/null )" || CONFLICT_RC=$?
eq "docs conflict: setup fails" "1" "$CONFLICT_RC"
ok "docs conflict: reports CONFLICTED with unmerged paths" \
  test -z "${CONFLICT_ERR##*CONFLICTED (unmerged paths)*}"
ok "docs conflict: does NOT blame git config" \
  test "${CONFLICT_ERR##*FAILED WITHOUT CONFLICT*}" = "$CONFLICT_ERR"
ok "docs conflict: sends the operator to a worktree of their OWN" \
  test -z "${CONFLICT_ERR##*worktree of your OWN*}"
ok "docs conflict: the worktree was removed" \
  test -z "$(fixture_git "$DOCS_CO" worktree list --porcelain | worktree_holding_branch "$DOCS_BRANCH" || true)"
no "docs conflict: the branch was removed with it" \
  fixture_git "$DOCS_CO" show-ref --verify --quiet "refs/heads/${DOCS_BRANCH}"
eq "docs conflict: the shared checkout is clean" "" \
  "$(fixture_git "$DOCS_CO" status --porcelain)"
eq "docs conflict: the shared checkout did not move" "$DOCS_CO_HEAD" \
  "$(fixture_git "$DOCS_CO" rev-parse HEAD)"
# Neither arm may leave a half-merged state behind in the shared checkout.
no "docs conflict: no merge is in progress in the shared checkout" \
  test -f "${DOCS_CO}/.git/MERGE_HEAD"

# -- the release commit, against a REAL history (#844) -----------------------
#
# release_boundary_commit's table tests fix its logic; this fixes its INPUT. The
# whole finding is that `git rev-parse origin/main` answers a different question
# than "which commit is the release", and that the difference is invisible to
# every check the script made -- later commits do not touch the version file, so
# the version-carries-<new> gate passes on all of them.
#
# Deliberately no `legion`: the reader is injected by name, so the fixture reads
# the version with git+sed. CI never runs this file (only scripts/preflight.sh
# does), and a standalone preflight run is not guaranteed a legion binary.
BND_TMP="${SUITE_TMP}/bnd"
mkdir -p "$BND_TMP"
BND="${BND_TMP}/repo"
fixture_git_init "$BND" -c init.defaultBranch=main init --quiet
fixture_git "$BND" config user.email t@t
fixture_git "$BND" config user.name t
bnd_commit() { # bnd_commit <file> <content> <subject>
  printf '%s\n' "$2" >"${BND}/$1"
  fixture_git "$BND" add "$1"
  fixture_git "$BND" commit --quiet --no-verify -m "$3"
}
# The reader under test's production twin is ref_version; here it is git show
# plus a sed, so the fixture stays hermetic.
bnd_version() { fixture_git "$BND" show "${1}:Cargo.toml" 2>/dev/null | sed -n 's/^version = "\(.*\)"$/\1/p'; }

bnd_commit Cargo.toml 'version = "0.24.0"' "chore(release): 0.24.0"
bnd_commit src.txt 'work' "feat: something"
bnd_commit Cargo.toml 'version = "0.25.0"' "chore(release): 0.25.0"
BND_RELEASE="$(fixture_git "$BND" rev-parse HEAD)"
# What lands between `legion pr merge` and `--finish`: ordinary merges that do
# not touch the version file at all.
bnd_commit a.txt 'a' "feat(#1): a"
bnd_commit b.txt 'b' "fix(#2): b"
BND_TIP="$(fixture_git "$BND" rev-parse HEAD)"

BND_LIST="$(fixture_git "$BND" rev-list HEAD -- Cargo.toml)"
eq "release commit: resolved from a real history" "$BND_RELEASE" \
  "$(printf '%s\n' "$BND_LIST" | release_boundary_commit bnd_version 0.25.0 200)"
# The point of the finding, stated as an assertion: the tip is NOT the answer,
# and yet it passes the tag-target check that used to be the only guard.
ok "release commit: the branch tip is a different commit" test "$BND_RELEASE" != "$BND_TIP"
ok "release commit: the tip still passes the version gate (why tip-resolution looked correct)" \
  tag_target_matches 0.25.0 "$(bnd_version "$BND_TIP")"
# One version read per commit that TOUCHED the file -- 2 here, across a history
# of 5 commits. The `<ref>^` parent walk this replaced would have been a
# subprocess per commit since the release, and fatal on a root commit.
eq "release commit: walks only commits that touch the version file" "2" \
  "$(printf '%s\n' "$BND_LIST" | wc -l | tr -d ' ')"
eq "release commit: while the branch itself is longer" "5" \
  "$(fixture_git "$BND" rev-list HEAD | wc -l | tr -d ' ')"

# IDEMPOTENCE, which is what finish_release's release-commit comment and step 4d
# of legion-release.md both promise when they say re-running --finish is safe.
# (Named, not cited by line number: the previous version of this comment pointed
# at release.sh:767, which by then was the middle of the ADD_FILES array.)
# Re-resolving must return the SAME sha
# however far the branch has moved on: that is what makes the second run match
# the existing tag and re-push it, instead of deriving a new sha and dying on
# "tag already exists and points at <other>".
bnd_commit c.txt 'c' "fix(#3): c"
BND_AGAIN="$(fixture_git "$BND" rev-list HEAD -- Cargo.toml | release_boundary_commit bnd_version 0.25.0 200)"
eq "release commit: stable across re-runs as the branch moves" "$BND_RELEASE" "$BND_AGAIN"
# And the next release's boundary is its own commit, not the previous one.
bnd_commit Cargo.toml 'version = "0.26.0"' "chore(release): 0.26.0"
BND_NEXT="$(fixture_git "$BND" rev-parse HEAD)"
eq "release commit: the next release resolves to its own commit" "$BND_NEXT" \
  "$(fixture_git "$BND" rev-list HEAD -- Cargo.toml | release_boundary_commit bnd_version 0.26.0 200)"
# Once a NEWER release has landed, the older one no longer resolves at all
# (rc 1) rather than resolving to something plausible-looking. In production the
# branch-tip landed gate refuses first, for the same reason: --finish on a
# superseded release must stop, not tag.
eq "release commit: a superseded release refuses to resolve" "1" \
  "$(fixture_git "$BND" rev-list HEAD -- Cargo.toml | release_boundary_commit bnd_version 0.25.0 200 >/dev/null; printf '%s' $?)"
# A commit that touches the version file WITHOUT changing the version (a
# dependency edit) must not be mistaken for the release commit.
bnd_commit Cargo.toml 'version = "0.26.0"
serde = "1.0.1"' "chore: dep bump"
ok "release commit: the dep-bump commit really landed" \
  test "$(fixture_git "$BND" rev-parse HEAD)" != "$BND_NEXT"
eq "release commit: a non-version edit to the file is not the release" "$BND_NEXT" \
  "$(fixture_git "$BND" rev-list HEAD -- Cargo.toml | release_boundary_commit bnd_version 0.26.0 200)"

printf '\n[test-release] %d passed, %d failed\n' "$PASS" "$FAIL" >&2
[ "$FAIL" -eq 0 ]
