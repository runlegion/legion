#!/usr/bin/env bash
# test-preflight-isolation.sh: the committed proof of #867 -- that a fixture
# suite run by scripts/preflight.sh cannot reach the repository being released.
#
# THE THING BEING PROVEN, and why it needs a test of its own: preflight is run by
# scripts/release.sh as step 4, and it used to `cd "$(git rev-parse
# --show-toplevel)"` before running the shell suites. So during a real release
# the fixtures executed inside the live checkout, in a repo whose origin is the
# real remote, and every fallback-to-cwd in a fixture was a write to production.
# Three main-branch corruptions came down that channel. The suites themselves
# were hardened twice (#861); this file asserts the property the CALLER is
# responsible for, which is the half nothing covered.
#
# Sources preflight.sh -- main() is BASH_SOURCE-guarded, so sourcing defines the
# sandbox machinery without running cargo -- and drives pf_run_shell_suites
# against SACRIFICIAL repositories that stand in for the release checkout. Each
# carries a deliberately hostile suite. Nothing here touches a real repo: every
# victim is a fresh `git init` under this file's own temp root, and the suites
# under test never learn a path outside it.
set -u

DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=preflight.sh disable=SC1091
source "${DIR}/preflight.sh"
set +e   # preflight.sh enables errexit on source; this file manages its own codes.

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
has() { # has <label> <haystack> <needle>
  if [ -z "${2##*"$3"*}" ]; then PASS=$((PASS + 1)); else
    FAIL=$((FAIL + 1))
    printf 'FAIL: %s -- output did not contain [%s]. Output:\n%s\n' "$1" "$3" "$2" >&2
  fi
}

# One temp root for every victim, removed through preflight's OWN guarded
# remover rather than a bare `rm -rf`: it refuses relative paths, unresolvable
# paths, and anything that does not land strictly beneath the system temp root.
#
# It is deliberately NOT put on PREFLIGHT_SANDBOX_ROOTS. That list is inherited
# by the subshells `iso_run` uses, and a cleanup running there would delete this
# file's own victims out from under it mid-run.
ISO_TMP="$(pf_phys "$(mktemp -d)")" || {
  printf '[test-preflight-isolation] could not create the temp root\n' >&2
  exit 90
}
iso_cleanup() {
  pf_sandbox_cleanup
  pf_sandbox_rm "$ISO_TMP" || true
}
trap iso_cleanup EXIT

# iso_victim <name>: a directory that will become a sacrificial "release
# checkout". It carries a copy of preflight.sh so it looks like the real thing.
iso_victim() {
  local v="${ISO_TMP}/$1"
  mkdir -p "${v}/scripts" || return 1
  cp "${DIR}/preflight.sh" "${v}/scripts/preflight.sh" || return 1
  printf '%s' "$v"
}

# iso_commit <victim>: turn it into a real repository, with NO remote.
iso_commit() {
  local v="$1"
  pf_git -c init.defaultBranch=main init --quiet "$v" >/dev/null || return 1
  pf_git -C "$v" config user.email t@t || return 1
  pf_git -C "$v" config user.name t || return 1
  pf_git -C "$v" add -A || return 1
  pf_git -C "$v" -c commit.gpgsign=false commit --quiet --no-verify -m init >/dev/null
}

# iso_run <repo-root>: drive the phase under test in a subshell, since every
# refusal in preflight is an `exit`. Leaves ISO_RC and ISO_OUT set.
ISO_RC=0
ISO_OUT=""
iso_run() {
  ISO_RC=0
  ISO_OUT="$( (pf_run_shell_suites "$1") 2>&1 )" || ISO_RC=$?
}

# iso_guard <sandbox> <repo-root>: the pre-run gate alone, same subshell reason.
iso_guard() {
  ISO_RC=0
  ISO_OUT="$( (pf_assert_sandbox_isolated "$1" "$2" "unit") 2>&1 )" || ISO_RC=$?
}

bare_of() { pf_git -C "$1" config --default '<unset>' --get core.bare; }

# iso_sandbox_path: the sandbox the last iso_run announced. Its removal is an
# assertion of its own -- the sandbox is a disposable, and a disposable that
# survives is a leak into the system temp dir, which is where the release is cut.
iso_sandbox_path() {
  printf '%s\n' "$ISO_OUT" | sed -n 's/^.*(sandbox: \(.*\)) ---$/\1/p' | sed -n '1p'
}

# -- 1. THE NEGATIVE CONTROL -------------------------------------------------
# Without it, a preflight that refused everything would satisfy every refusal
# below and this file would be measuring nothing.
V_OK="$(iso_victim control)"
cat >"${V_OK}/scripts/test-benign.sh" <<'EOF'
#!/usr/bin/env bash
# A suite that behaves: builds its fixtures elsewhere, leaves its host alone.
set -euo pipefail
printf '[benign] this suite ran\n' >&2
EOF
ok "control: victim built" iso_commit "$V_OK"
FP_OK="$(pf_fingerprint "$V_OK")"
iso_run "$V_OK"
eq "control: a well-behaved suite passes"          "0" "$ISO_RC"
has "control: and it really ran"                   "$ISO_OUT" "[benign] this suite ran"
eq "control: the release checkout is byte-identical" "$FP_OK" "$(pf_fingerprint "$V_OK")"
SB_SEEN="$(iso_sandbox_path)"
ok "control: the sandbox was announced by path" test -n "$SB_SEEN"
no "control: and it is cleaned up on the success path" test -d "$SB_SEEN"

# -- 2. THE ACCEPTANCE CRITERION ---------------------------------------------
# A suite that falls back to the cwd's repository -- the exact shape of every
# incident, since before #867 that cwd WAS the live checkout. The write must not
# land on the release checkout, and preflight must FAIL rather than carry on.
V_CWD="$(iso_victim cwd-write)"
cat >"${V_CWD}/scripts/test-cwd-write.sh" <<'EOF'
#!/usr/bin/env bash
# THE ATTACK: a path helper degraded to "" or ".", so the fixture's git call
# addresses whatever the runner's cwd happens to be.
set -euo pipefail
git commit --quiet --no-verify --allow-empty -m "fixture escape"
git branch fixture-escape
EOF
ok "cwd-write: victim built" iso_commit "$V_CWD"
FP_CWD="$(pf_fingerprint "$V_CWD")"
iso_run "$V_CWD"
no  "cwd-write: preflight FAILS"                    test "$ISO_RC" -eq 0
has "cwd-write: and it names the suite"             "$ISO_OUT" "scripts/test-cwd-write.sh"
has "cwd-write: and says what happened"             "$ISO_OUT" "MODIFIED THE REPOSITORY IT RAN IN"
eq  "cwd-write: the release checkout is byte-identical" "$FP_CWD" "$(pf_fingerprint "$V_CWD")"
no  "cwd-write: the escape ref never reached the release checkout" \
  pf_git -C "$V_CWD" show-ref --verify --quiet refs/heads/fixture-escape
SB_SEEN="$(iso_sandbox_path)"
ok "cwd-write: the sandbox was announced by path" test -n "$SB_SEEN"
no "cwd-write: and it is cleaned up on the FAILURE path too" test -d "$SB_SEEN"

# -- 3. THE core.bare CLASS --------------------------------------------------
# Incident #3, twice: `git config core.bare true` (equivalently `git init --bare`
# onto an existing .git) against the repo the suite is standing in. `git status`
# in the affected checkout then dies with "must be run in a work tree".
V_BARE="$(iso_victim core-bare)"
cat >"${V_BARE}/scripts/test-core-bare.sh" <<'EOF'
#!/usr/bin/env bash
# THE ATTACK: the config write that broke the operator's checkout twice.
set -euo pipefail
git config core.bare true
EOF
ok "core.bare: victim built" iso_commit "$V_BARE"
BARE_BEFORE="$(bare_of "$V_BARE")"
FP_BARE="$(pf_fingerprint "$V_BARE")"
iso_run "$V_BARE"
no  "core.bare: preflight FAILS"                     test "$ISO_RC" -eq 0
has "core.bare: and it names the suite"              "$ISO_OUT" "scripts/test-core-bare.sh"
eq  "core.bare: the release checkout's core.bare is untouched" "$BARE_BEFORE" "$(bare_of "$V_BARE")"
eq  "core.bare: and the checkout is byte-identical"  "$FP_BARE" "$(pf_fingerprint "$V_BARE")"
ok  "core.bare: the release checkout still has a work tree" \
  pf_git -C "$V_BARE" status --porcelain

# -- 4. THE LINKED-WORKTREE REGRESSION ---------------------------------------
# A LINKED WORKTREE IS NOT A SANDBOX. `git rev-parse --git-common-dir` from one
# resolves to the PARENT repo's .git, so a non-worktree-scoped config write from
# inside a worktree lands in the SHARED config -- and git ignores core.bare for
# linked worktrees, so the worktree keeps answering normally while the MAIN
# checkout breaks. That invisibility is why this recurred through two rounds of
# hardening aimed at paths, and it is why preflight must never "fix" #867 by
# running the suites in a worktree.
#
# Here preflight itself is run FROM a linked worktree, which is the ordinary
# case for an agent cutting a release. The suite's config write must reach
# neither the worktree nor the parent.
V_PAR="$(iso_victim worktree-parent)"
cat >"${V_PAR}/scripts/test-core-bare.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
git config core.bare true
EOF
ok "worktree: parent built" iso_commit "$V_PAR"
WT_LINKED="${ISO_TMP}/worktree-linked"
ok "worktree: linked worktree created" \
  pf_git -C "$V_PAR" worktree add --quiet -b linked "$WT_LINKED"
# The premise, asserted rather than assumed: the worktree and its parent ARE the
# same repository as far as config and refs are concerned.
eq "worktree: the worktree keys to the parent repository" \
  "$(pf_repo_key "$V_PAR")" "$(pf_repo_key "$WT_LINKED")"
PAR_BARE_BEFORE="$(bare_of "$V_PAR")"
FP_PAR="$(pf_fingerprint "$V_PAR")"
iso_run "$WT_LINKED"
no  "worktree: preflight FAILS"                      test "$ISO_RC" -eq 0
has "worktree: and it names the suite"               "$ISO_OUT" "scripts/test-core-bare.sh"
eq  "worktree: the PARENT repo's core.bare is untouched" \
  "$PAR_BARE_BEFORE" "$(bare_of "$V_PAR")"
eq  "worktree: the PARENT repo is byte-identical"    "$FP_PAR" "$(pf_fingerprint "$V_PAR")"
ok  "worktree: the parent checkout still has a work tree" \
  pf_git -C "$V_PAR" status --porcelain
ok  "worktree: and so does the worktree"             pf_git -C "$WT_LINKED" status --porcelain

# -- 5. THE ESCAPE THE SANDBOX CANNOT BOUND ----------------------------------
# A suite that RESOLVED the release checkout's path and addressed it directly is
# not contained by moving the cwd -- nothing about a sandbox stops an absolute
# path. What must not happen is that the release proceeds unaware, so the phase
# is bracketed by a fingerprint of the release checkout itself.
V_ABS="$(iso_victim absolute-escape)"
cat >"${V_ABS}/scripts/test-absolute-escape.sh" <<EOF
#!/usr/bin/env bash
# THE ATTACK: the release checkout's path, baked in.
set -euo pipefail
git -C "${V_ABS}" branch absolute-escape
EOF
ok "absolute: victim built" iso_commit "$V_ABS"
iso_run "$V_ABS"
no  "absolute: preflight FAILS"                      test "$ISO_RC" -eq 0
has "absolute: and it reports the release checkout changed" \
  "$ISO_OUT" "THE RELEASE CHECKOUT"
ok  "absolute: the write really landed (so the gate, not luck, caught it)" \
  pf_git -C "$V_ABS" show-ref --verify --quiet refs/heads/absolute-escape

# -- 6. THE PRE-RUN GATE, DIRECTLY -------------------------------------------
# Everything above observes the gate through a whole phase. These pin the gate's
# own decisions, including the one arm the phase cannot reach: what happens when
# the "sandbox" handed to it is the release repository itself.
# Read through the GLOBAL, never `$(pf_make_sandbox ...)`: a command
# substitution runs the whole builder in a subshell, so the sandbox root never
# reaches the cleanup list and every call leaks a temp tree.
ok "gate: a sandbox can be built" pf_make_sandbox "$V_OK"
SB_OK="$PREFLIGHT_SANDBOX"
ok "gate: and it has a path" test -n "$SB_OK"
ok "gate: and its root is registered for cleanup" \
  test "${#PREFLIGHT_SANDBOX_ROOTS[@]}" -gt 0
ok "gate: a genuine sandbox is accepted" \
  pf_assert_sandbox_isolated "$SB_OK" "$V_OK" "unit"

# The trivial case -- the same directory -- is caught by containment, which runs
# first. Refusal is what matters; which arm names it does not.
iso_guard "$V_OK" "$V_OK"
eq  "gate: a sandbox that IS the release checkout is refused" "1" "$ISO_RC"
has "gate: and it says so by name" "$ISO_OUT" "overlaps the release checkout"

# The heart of #867's enlarged scope: a linked worktree of the release repo
# offered as a sandbox is refused, because the gate keys on the COMMON git dir
# rather than on the path. A path-based check would have waved this through.
iso_guard "$WT_LINKED" "$V_PAR"
eq  "gate: a linked worktree of the release repo is refused" "1" "$ISO_RC"
has "gate: and it is refused as the same repository" \
  "$ISO_OUT" "WOULD RUN AGAINST THE REPOSITORY BEING RELEASED"

# A sandbox that can reach a remote is refused before anything runs -- a fixture
# that reaches a real remote is the incident in its purest form.
pf_git -C "$SB_OK" remote add origin "${ISO_TMP}/nowhere.git"
iso_guard "$SB_OK" "$V_OK"
eq  "gate: a sandbox with a remote is refused" "1" "$ISO_RC"
has "gate: and it names the remote check" "$ISO_OUT" "has remotes configured"
pf_git -C "$SB_OK" remote remove origin

# Containment, both directions: a sandbox inside the release checkout is
# committable from it, and a release checkout inside the sandbox is inside the
# cleanup vector.
INSIDE="${V_OK}/nested-sandbox"
mkdir -p "$INSIDE"
iso_guard "$INSIDE" "$V_OK"
eq  "gate: a sandbox inside the release checkout is refused" "1" "$ISO_RC"
has "gate: and it says the two overlap" "$ISO_OUT" "overlaps the release checkout"
rmdir "$INSIDE"

# Fail closed: a path that does not resolve is a refusal, never a pass.
iso_guard "${ISO_TMP}/no-such-sandbox" "$V_OK"
eq  "gate: an unresolvable sandbox is refused" "1" "$ISO_RC"
has "gate: and refusal is the fail-CLOSED answer" "$ISO_OUT" "does not resolve"

printf '\n[test-preflight-isolation] %d passed, %d failed\n' "$PASS" "$FAIL" >&2
[ "$FAIL" -eq 0 ]
