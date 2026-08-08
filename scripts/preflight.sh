#!/usr/bin/env bash
# preflight.sh: the quality bar that must pass before a release (or any push
# the developer wants gated). Reusable unit shared by scripts/release.sh.
#
#   1. cargo fmt -- --check          (formatting is committed-clean)
#   2. cargo clippy --all-targets -D warnings  (CI parity -- bare clippy misses
#                                               test/example/bench code)
#   3. cargo test                    (the suite is green)
#   4. scripts/test-*.sh             (the shell suites, IN A DISPOSABLE SANDBOX)
#   5. legion index <repo>           (SCIP index regenerated so sym queries
#                                     answer against current code)
#
# Exits non-zero on the FIRST failing gate, naming it. Safe to run standalone:
#
#   scripts/preflight.sh             # full bar against the legion repo
#   SKIP_SCIP=1 scripts/preflight.sh # skip the index regen (faster local loop)
#
# The SCIP step is best-effort by default: a missing `legion` binary or indexer
# is a warning, not a hard failure, so the script still gates code quality on a
# machine without the plugin installed. Set REQUIRE_SCIP=1 to make it mandatory.
#
# -- WHY THE SHELL SUITES RUN SOMEWHERE ELSE (#867) ---------------------------
# This file used to do `cd "$(git rev-parse --show-toplevel)"` and then
# `bash "${REPO_ROOT}/scripts/test-release.sh"`. release.sh runs preflight as its
# step 4, so during a REAL RELEASE the fixture suites executed with their cwd
# inside the live checkout, in a repo whose `origin` is the real remote. Every
# cwd fallback in a fixture was therefore a write to production, and that is the
# delivery channel behind three main-branch corruptions: fixture commits pushed
# to origin/main, a release-shaped commit that reduced Cargo.toml to one line,
# and `core.bare=true` set on the live checkout twice.
#
# A LINKED WORKTREE IS NOT A SANDBOX and must never be used as the fix. From a
# linked worktree `git rev-parse --git-common-dir` resolves to the PARENT repo's
# .git, so non-worktree-scoped config writes land in the shared config and refs
# are visible repo-wide -- and git IGNORES core.bare for linked worktrees, so a
# fixture that sets it breaks the MAIN checkout while the worktree it was run
# from keeps answering normally. Invisible from the worktree, from the suite, and
# from every gate.
#
# So the suites run in a THROWAWAY REPOSITORY: scripts/ is copied into a temp
# directory which is `git init`ed as a repo of its own, with no remote, no
# ambient git config, and no relationship to the checkout being released. A copy
# rather than a clone because the suites need no parent history -- verified: both
# build their own fixtures from scratch and pass identically from a bare copy --
# and because a clone carries the origin URL that has to be removed again anyway.
# The copy is also what makes #861's in-suite perimeter resolve SUITE_SELF_REPO
# to the throwaway instead of production; running the suites from a worktree
# would have keyed that guard on the real repo.
#
# Three gates surround the run, and each fails closed:
#   * before each suite -- the sandbox must resolve to a DIFFERENT repository
#     than the one being released, must contain no remote, and must not overlap
#     the release checkout. An unresolvable check is a failure, not a pass.
#   * after each suite  -- the sandbox must be byte-identical. The suites build
#     their fixtures in their own temp roots and never touch the repo they run
#     in, so any change means a suite reached for its host repository -- which
#     under the old arrangement WAS the live checkout. Named and fatal.
#   * around the phase   -- the release checkout must be byte-identical: refs,
#     HEAD, reflog, status, core.bare, the worktree registry and the local
#     config. This is the one that catches an escape by absolute path, which no
#     amount of cwd discipline can prevent. It is compared on EVERY exit from
#     the phase, not only the clean one -- a suite can corrupt the checkout and
#     then fail, and if the comparison were skipped on the failure path the
#     corruption would become the baseline the next run measures against.
set -euo pipefail

# The suites that MUST exist. Previously the runner was `if [ -f ... ]`, so
# renaming or deleting a suite silently stopped gating on it; a missing required
# suite is now a failure.
#
# test-preflight-isolation.sh is on this list for the same reason the other two
# are, and the reason is sharper for it: it is the committed proof of #867, and
# the discovery glob below would still match the other two if it were renamed
# away. Discovery would pass, the release would proceed, and the only thing that
# checks THIS FILE's guarantees would have stopped running silently.
PREFLIGHT_REQUIRED_SUITES=(test-release.sh test-sync-version.sh test-preflight-isolation.sh)

# Sandbox roots created by this process, for the EXIT cleanup.
PREFLIGHT_SANDBOX_ROOTS=()

# Repo name as known to watch.toml (for the SCIP index step). Override with
# LEGION_REPO=<name> if this clone is registered under a different name.
LEGION_REPO="${LEGION_REPO:-legion}"

step() { printf '\n[preflight] === %s ===\n' "$1" >&2; }
fail() { printf '\n[preflight] FAILED at: %s\n' "$1" >&2; exit 1; }

# pf_env_run <cmd...>: run <cmd> with the ambient git environment SCRUBBED.
#
# ONE definition, used by both the guard's own reads and the suite runs, because
# two separately-written scrubs are two definitions of "isolated" and they drift.
#
# The unsets matter as much as the config pins: an inherited GIT_DIR or
# GIT_WORK_TREE repoints EVERY git call at the release repo while every path this
# script compares stays innocent -- the leak is through the environment, not
# through the argument. And the pins apply to the guard's reads too, because
# ambient config that makes `rev-parse` fail leaves the repository key EMPTY, and
# an empty key compares equal to nothing, which silently turns the identity gate
# into a no-op. Pin first, then capture.
#
# THE PINS ARE NOT ENOUGH ON THEIR OWN, measured 2026-08-07 rather than reasoned
# about: env-injected config OUTRANKS a repository's own `--local` values, and
# GIT_CONFIG_GLOBAL=/dev/null does not touch it. With GIT_CONFIG_COUNT=1
# GIT_CONFIG_KEY_0=core.hooksPath ambient, the sandbox's core.hooksPath pin reads
# back as the attacker's directory and a planted hook FIRES inside the sandbox.
# GIT_TEMPLATE_DIR is worse still, because it reaches the `git init` in
# pf_make_sandbox and plants files into .git/hooks BEFORE any pin exists.
# So the numbered pairs are unset individually. Clearing GIT_CONFIG_COUNT alone
# does hold the pin for git's next read -- measured -- but the subshell below is
# where the SUITE runs, and a suite that exports GIT_CONFIG_COUNT itself re-arms
# whatever KEY_n/VALUE_n are still lying in the environment. With the pairs
# actually gone that suite gets "missing config key GIT_CONFIG_KEY_0" and dies
# loudly instead of quietly reading an attacker's value. The rest of the list is
# the same class -- values that redirect where git finds its config, its
# templates, its helper programs, or the commands it will shell out to.
#
# The subshell scoping is the point, not an accident, hence the disable.
# shellcheck disable=SC2030,SC2031
pf_env_run() {
  (
    unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_NAMESPACE \
      GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES \
      GIT_CONFIG GIT_CONFIG_COUNT GIT_TEMPLATE_DIR GIT_EXEC_PATH \
      GIT_CEILING_DIRECTORIES GIT_SSH_COMMAND GIT_ASKPASS GIT_PROXY_COMMAND \
      GIT_EDITOR GIT_SEQUENCE_EDITOR GIT_ALLOW_PROTOCOL GIT_ATTR_NOSYSTEM
    # The numbered config pairs, by name. Unquoted on purpose: this is bash's
    # prefix expansion over VARIABLE NAMES, which cannot contain whitespace, and
    # it expands to nothing when none are set.
    # shellcheck disable=SC2086
    local pf_k
    for pf_k in ${!GIT_CONFIG_KEY_@} ${!GIT_CONFIG_VALUE_@}; do
      unset "$pf_k"
    done
    export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
      GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
    "$@"
  )
}

# pf_git <args...>: git, scrubbed.
pf_git() { pf_env_run git "$@"; }

# pf_phys <dir>: the physical, symlink-free form of an EXISTING directory. Path
# strings are not comparable identities here -- /Users/... and /Volumes/... are
# the same checkout through a symlink on this machine, and mktemp hands back
# /var/... for what is physically /private/var/....
pf_phys() {
  local p="${1:-}"
  [ -n "$p" ] || return 1
  (cd "$p" 2>/dev/null && pwd -P) || return 1
}

# pf_path_under <path> <root>: <path> is <root> or lives beneath it. Both
# arguments must already be physically resolved -- `case` is a glob matcher, not
# a path resolver, and a `..` traversal string-matches a root it has left.
pf_path_under() {
  local p="${1:-}" root="${2:-}"
  [ -n "$p" ] && [ -n "$root" ] || return 1
  case "$p" in "$root" | "$root"/*) return 0 ;; esac
  return 1
}

# pf_repo_key <dir>: a canonical identity for the git REPOSITORY <dir> belongs
# to. The COMMON git dir, so a linked worktree keys to the repo it is linked
# into -- which is the entire point here, since a worktree of the release
# checkout shares its config and refs and would otherwise look like a sandbox.
pf_repo_key() {
  local d="${1:-}" g
  [ -n "$d" ] || return 1
  g="$(cd "$d" 2>/dev/null && pf_git rev-parse --git-common-dir 2>/dev/null)" || return 1
  [ -n "$g" ] || return 1
  case "$g" in /*) ;; *) g="${d}/${g}" ;; esac
  pf_phys "$g"
}

# pf_fingerprint <repo-dir>: the byte-identity of a checkout, and the exact set
# of things the three incidents landed in -- refs (a fixture commit), HEAD, the
# reflog, the working tree, core.bare (flipped twice), the worktree registry (a
# planted worktree is invisible to all of the above) and the local config
# (config OUTLIVES refs, so a stale branch stanza is the longest-lived
# fingerprint of an escape).
#
# Every command must SUCCEED. A fingerprint that silently degrades to empty
# compares equal to another empty one and the whole check passes on nothing, so
# an unreadable repository is a failure rather than a clean bill of health.
# `--abbrev-ref` rather than `symbolic-ref` because a detached HEAD prints
# "HEAD" and exits 0 instead of being an error; `--default` on core.bare because
# an unset key is rc 1.
pf_fingerprint() {
  local d="${1:-}"
  [ -n "$d" ] || return 1
  pf_git -C "$d" for-each-ref --format='%(refname) %(objectname)' || return 1
  pf_git -C "$d" rev-parse HEAD || return 1
  pf_git -C "$d" rev-parse --abbrev-ref HEAD || return 1
  pf_git -C "$d" reflog --format='%H %gs' || return 1
  pf_git -C "$d" status --porcelain || return 1
  pf_git -C "$d" config --default '<unset>' --get core.bare || return 1
  pf_git -C "$d" worktree list --porcelain || return 1
  pf_git -C "$d" config --list --local || return 1
}

# pf_sandbox_rm <path>: ONE guarded removal. This is an `rm -rf` argument vector,
# so it is guarded at least as hard as the git targets are: a relative path is
# refused outright (`dirname ""` is ".", which would be `rm -rf` at the root of
# whatever the cwd happens to be), an unresolvable path is refused rather than
# guessed at, and so is anything that does not land strictly beneath the system
# temp root. Refusal is reported and never fatal -- this runs on the failure path
# too, and a cleanup that exits non-zero would mask the failure being reported.
pf_sandbox_rm() {
  local p="${1:-}" rp tmp
  [ -n "$p" ] || return 0
  case "$p" in
    /*) ;;
    *)
      printf '[preflight] refusing to clean up the RELATIVE path [%s]\n' "$p" >&2
      return 1
      ;;
  esac
  rp="$(pf_phys "$p")" || return 0
  tmp="$(pf_phys "${TMPDIR:-/tmp}" || printf '/tmp')"
  if ! pf_path_under "$rp" "$tmp" || [ "$rp" = "$tmp" ] || [ "$rp" = "/" ]; then
    printf '[preflight] refusing to clean up [%s] -- not strictly under %s\n' "$rp" "$tmp" >&2
    return 1
  fi
  rm -rf "$rp"
}

# pf_sandbox_cleanup: the EXIT handler -- every sandbox root THIS shell created.
# The list is deliberately not consulted by pf_make_sandbox's caller: a subshell
# inherits a copy, so a cleanup that ran there would otherwise delete roots the
# parent still owns.
pf_sandbox_cleanup() {
  local p
  for p in ${PREFLIGHT_SANDBOX_ROOTS[@]+"${PREFLIGHT_SANDBOX_ROOTS[@]}"}; do
    pf_sandbox_rm "$p" || true
  done
  PREFLIGHT_SANDBOX_ROOTS=()
}

# pf_make_sandbox <repo-root>: build the throwaway repository the shell suites
# run in. Only scripts/ is copied -- the suites are unit tests OF those scripts
# and build every other fixture themselves -- so the sandbox carries none of the
# release checkout's history, remotes, hooks or config.
#
# It gets one commit at creation because a repo with no HEAD makes half the
# fingerprint commands error, and a fingerprint that errors is a fingerprint
# that cannot prove anything.
#
# THE RESULT IS A GLOBAL, NOT STDOUT, and that is load-bearing: read through
# `$(pf_make_sandbox ...)` the whole body runs in a command-substitution
# SUBSHELL, so the cleanup registration below is discarded the moment it returns
# and every run leaks its sandbox into the system temp dir -- silently, since
# nothing else changes. Callers read $PREFLIGHT_SANDBOX.
PREFLIGHT_SANDBOX=""
pf_make_sandbox() {
  local repo_root="$1" root sandbox
  PREFLIGHT_SANDBOX=""
  root="$(mktemp -d)" || return 1
  root="$(pf_phys "$root")" || return 1
  # Registered, not trapped: installing the EXIT trap here would clobber a trap
  # the caller set (scripts/test-preflight-isolation.sh owns one), and traps are
  # global. The caller arranges for pf_sandbox_cleanup to run.
  PREFLIGHT_SANDBOX_ROOTS+=("$root")

  sandbox="${root}/sandbox"
  mkdir -p "$sandbox" || return 1
  cp -R "${repo_root}/scripts" "${sandbox}/scripts" || return 1

  pf_git -c init.defaultBranch=main init --quiet "$sandbox" >/dev/null || return 1
  pf_git -C "$sandbox" config user.email preflight@localhost || return 1
  pf_git -C "$sandbox" config user.name preflight || return 1
  # A repo-level hooks pin. What it covers: this repository's own hooks, and any
  # hook a suite installs into .git/hooks afterwards. What it does NOT cover, and
  # the comment here used to claim it did -- an ambient GIT_CONFIG_COUNT/KEY_n
  # pair, which outranks `--local` and would make this line read back as the
  # attacker's directory; and GIT_TEMPLATE_DIR, which planted its files during
  # the `git init` above, before this line existed to be outranked. Neither is
  # closed here. Both are closed by the scrub in pf_env_run, which is why every
  # git call in this file goes through it.
  pf_git -C "$sandbox" config core.hooksPath /dev/null || return 1
  pf_git -C "$sandbox" add -A || return 1
  pf_git -C "$sandbox" -c commit.gpgsign=false commit --quiet --no-verify \
    -m "preflight sandbox" >/dev/null || return 1

  PREFLIGHT_SANDBOX="$sandbox"
}

# pf_assert_sandbox_isolated <sandbox> <repo-root> <what>: the pre-run gate.
# Every arm fails CLOSED -- a check that cannot be resolved is a refusal.
pf_assert_sandbox_isolated() {
  local sandbox="$1" repo_root="$2" what="$3"
  local sandbox_phys release_phys sandbox_key release_key remotes

  sandbox_phys="$(pf_phys "$sandbox")" \
    || fail "${what}: the sandbox [${sandbox}] does not resolve to a real directory"
  release_phys="$(pf_phys "$repo_root")" \
    || fail "${what}: the release checkout [${repo_root}] does not resolve to a real directory"

  # Containment, both ways. A sandbox inside the checkout would be committable
  # from it; a checkout inside the sandbox would be inside the cleanup vector.
  if pf_path_under "$sandbox_phys" "$release_phys" || pf_path_under "$release_phys" "$sandbox_phys"; then
    fail "${what}: the sandbox [${sandbox_phys}] overlaps the release checkout [${release_phys}]"
  fi

  # Repository IDENTITY, keyed on the common git dir so a linked worktree of the
  # release repo cannot pose as a sandbox. THIS is the check the issue asks for:
  # a suite whose resolved repo would be the repository being released is named
  # and refused, never run.
  sandbox_key="$(pf_repo_key "$sandbox_phys")" \
    || fail "${what}: cannot resolve the sandbox's git repository -- refusing to run it"
  release_key="$(pf_repo_key "$release_phys")" \
    || fail "${what}: cannot resolve the release checkout's git repository -- refusing to run anything against it"
  if [ "$sandbox_key" = "$release_key" ]; then
    fail "${what}: WOULD RUN AGAINST THE REPOSITORY BEING RELEASED (${release_key}) -- refusing"
  fi

  # No remote, asserted immediately before the run rather than assumed from the
  # way the sandbox was built. A fixture that reaches a real remote is the whole
  # incident, so this is a checked property, and a read that FAILS is a refusal.
  remotes="$(pf_git -C "$sandbox_phys" remote)" \
    || fail "${what}: cannot read the sandbox's remotes -- refusing to run it"
  if [ -n "$remotes" ]; then
    fail "${what}: the sandbox has remotes configured [$(printf '%s' "$remotes" | tr '\n' ' ')] -- refusing to run it"
  fi
}

# pf_run_shell_suites <repo-root>: every scripts/test-*.sh, each executed in the
# sandbox with the cwd, the git config and the git environment all pointed away
# from the release checkout.
pf_run_shell_suites() {
  local repo_root="$1"
  local sandbox t before after release_before release_after rc phase_rc
  local -a suites=()

  # The sandbox must be removed on the failure path too, and `fail` exits.
  trap pf_sandbox_cleanup EXIT

  local f
  for f in "${repo_root}"/scripts/test-*.sh; do
    [ -f "$f" ] || continue
    suites+=("${f##*/}")
  done
  # Fail closed: "the glob matched nothing" is indistinguishable from "the
  # suites were renamed", and both must stop a release rather than pass it.
  [ "${#suites[@]}" -gt 0 ] \
    || fail "shell-script tests -- no scripts/test-*.sh found under ${repo_root}"

  release_before="$(pf_fingerprint "$repo_root")" \
    || fail "shell-script tests -- cannot fingerprint the release checkout ${repo_root}"

  pf_make_sandbox "$repo_root" \
    || fail "shell-script tests -- could not build the disposable sandbox"
  sandbox="$PREFLIGHT_SANDBOX"
  [ -n "$sandbox" ] \
    || fail "shell-script tests -- the disposable sandbox has no path"

  # THE LOOP RUNS IN A SUBSHELL so that the release-checkout comparison below is
  # reached by EVERY exit from this phase rather than only by the clean one.
  #
  # `fail` exits, and there are FIVE refusals inside this loop -- the pre-run
  # gate, the before-fingerprint, the suite's own exit code, and the two
  # after-run arms -- every one of which used to jump straight over the
  # comparison. A suite that corrupted the release checkout AND then failed
  # reported only its own failure; the corruption was never mentioned. The
  # operator would then fix the suite and re-run, and run 2 would capture
  # `release_before` from the ALREADY-CORRUPTED checkout and pass. Silent, and
  # self-concealing, and the corruption becomes the next run's baseline.
  #
  # A subshell rather than a wrapper around each `fail`: a wrapper is a thing
  # somebody has to remember to call at every new exit, which is the same shape
  # as the defect being fixed. This makes the bracket structural. The suite's own
  # diagnosis is already on stderr from inside the subshell, so a run that both
  # corrupted the checkout and failed reports BOTH facts.
  phase_rc=0
  (
    for t in "${suites[@]}"; do
      pf_assert_sandbox_isolated "$sandbox" "$repo_root" "scripts/${t}"

      before="$(pf_fingerprint "$sandbox")" \
        || fail "scripts/${t} -- cannot fingerprint the sandbox before the run"

      # This exact format is PARSED by test-preflight-isolation.sh's
      # iso_sandbox_path, which asserts the sandbox is removed afterwards on both
      # the success and the failure path. Reformat both ends together.
      printf '\n[preflight] --- scripts/%s (sandbox: %s) ---\n' "$t" "$sandbox" >&2
      # The cwd the suite inherits is the sandbox, never the release checkout --
      # which is the whole of #867. $sandbox has just been resolved and gated, so
      # the `cd` is a perimeter-checked target rather than a bare one.
      rc=0
      (cd "$sandbox" && pf_env_run bash "${sandbox}/scripts/${t}") || rc=$?
      [ "$rc" -eq 0 ] || fail "scripts/${t} (exit ${rc})"

      # An unreadable sandbox AFTER the run is not an inconvenience, it is the
      # finding: `core.bare=true` is exactly what makes `git status` answer "this
      # operation must be run in a work tree", and that write landed on the
      # operator's live checkout twice.
      after="$(pf_fingerprint "$sandbox")" \
        || fail "scripts/${t} -- THE SANDBOX REPOSITORY IS UNREADABLE AFTER THE RUN.
       The suite broke the repository it ran in (core.bare=true is the known
       shape); under the pre-#867 runner that repository was ${repo_root}."
      if [ "$before" != "$after" ]; then
        fail "scripts/${t} -- THE SUITE MODIFIED THE REPOSITORY IT RAN IN. It was
       sandboxed, so the damage is a temp directory -- but the same write with
       the pre-#867 runner would have landed in ${repo_root}. Fix the suite's
       fallback-to-cwd rather than this gate."
      fi
    done
  ) || phase_rc=$?

  # THE BRACKET. Reached on the success path and on every failure path above.
  # An unreadable release checkout is reported on its own terms rather than
  # collapsed into "it changed": core.bare=true set on the checkout by absolute
  # path is exactly what makes it unreadable, and naming that is the finding.
  if ! release_after="$(pf_fingerprint "$repo_root")"; then
    fail "shell-script tests -- THE RELEASE CHECKOUT ${repo_root} IS UNREADABLE
       after the shell suites ran. A suite reached it and broke it (core.bare
       =true is the known shape). Do not release; repair the checkout and
       inspect refs and the local config before doing anything else."
  fi
  if [ "$release_before" != "$release_after" ]; then
    fail "shell-script tests -- THE RELEASE CHECKOUT ${repo_root} CHANGED while the
       shell suites ran. A suite reached it by a route the sandbox does not
       bound (an absolute path, or an inherited git environment). Do not
       release; inspect refs, core.bare and the local config.
       THIS IS REPORTED EVEN WHEN A SUITE ALSO FAILED ABOVE, and fixing that
       suite does NOT clear it: the next run would capture its baseline from the
       checkout as this run left it, and would pass."
  fi

  # The suite failure itself, passed through unchanged. `exit`, not `fail` --
  # the failing suite already named itself from inside the subshell and a second
  # "FAILED at:" line would only obscure which gate actually fired.
  [ "$phase_rc" -eq 0 ] || exit "$phase_rc"

  pf_sandbox_cleanup
}

main() {
  local REPO_ROOT t
  REPO_ROOT="$(git rev-parse --show-toplevel)"
  cd "$REPO_ROOT" || fail "cannot enter the repository root ${REPO_ROOT}"

  # The Rust and lint gates legitimately need the real checkout -- they compile
  # and test the code under release -- and they are not #867's hazard, because
  # they are not the thing whose fixtures fall back to the cwd. The precise
  # claim, audited 2026-08-07, is narrower than "they never touch git":
  #   * build.rs runs `git rev-parse --short HEAD` and `git status --porcelain`
  #     against the repo root. Reads.
  #   * a few integration tests deliberately read the repo root through the
  #     inherited cwd -- current_git_head in tests/integration/worksource_pr.rs,
  #     query_config_value in tests/integration/common.rs. Reads.
  #   * ONE conditional write exists: tests/integration/common.rs sets
  #     `core.bare false` on the repo root. It is a REPAIR that fires only when a
  #     previous run already left core.bare=true, and it then panics naming what
  #     it found -- a symptom handler for this issue's own class, installed after
  #     #723/#740, not a channel of its own.
  # Leaving them on the real checkout is a decision, not an oversight.
  step "cargo fmt -- --check"
  cargo fmt -- --check || fail "formatting (run: cargo fmt)"

  step "cargo clippy --all-targets -- -D warnings"
  cargo clippy --all-targets -- -D warnings || fail "clippy"

  step "cargo test"
  cargo test || fail "tests"

  step "shell-script tests (disposable sandbox)"
  for t in "${PREFLIGHT_REQUIRED_SUITES[@]}"; do
    [ -f "${REPO_ROOT}/scripts/${t}" ] \
      || fail "shell-script tests -- required suite scripts/${t} is missing"
  done
  pf_run_shell_suites "$REPO_ROOT"

  if [ "${SKIP_SCIP:-0}" = "1" ]; then
    printf '\n[preflight] SCIP regen skipped (SKIP_SCIP=1)\n' >&2
  else
    step "legion index ${LEGION_REPO} (SCIP regen)"
    if command -v legion >/dev/null 2>&1; then
      if ! legion index "$LEGION_REPO"; then
        if [ "${REQUIRE_SCIP:-0}" = "1" ]; then
          fail "SCIP index regen"
        fi
        printf '[preflight] WARNING: SCIP regen failed (non-fatal; set REQUIRE_SCIP=1 to enforce)\n' >&2
      fi
    elif [ "${REQUIRE_SCIP:-0}" = "1" ]; then
      fail "legion binary not found (REQUIRE_SCIP=1)"
    else
      printf '[preflight] legion binary not found -- skipping SCIP regen (non-fatal)\n' >&2
    fi
  fi

  printf '\n[preflight] all gates passed\n' >&2
}

# Sourcing this file defines the sandbox machinery without running the gates --
# scripts/test-preflight-isolation.sh drives pf_run_shell_suites directly, the
# same way test-release.sh sources release.sh.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
