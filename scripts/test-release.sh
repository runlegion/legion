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

# -- git fixture preamble ----------------------------------------------------
# Shared by every block below that builds a real repo. Defined up here because
# the first of those blocks is the merge-observer section, well before the docs
# worktree fixtures.
#
# `require_dir` IS THE GUARD on fixture commands, and it is worth being exact
# about that, because the natural reading is that addressing repos with `git -C
# <dir>` instead of `cd` closed the incident class by itself. It did not:
# `git -C ""` does NOT fail -- inside a repo it runs in the CURRENT repo and
# exits 0, exactly the silent cwd fallback `cd ""` gives you. So a fixture
# command built on a helper that returned nothing is just as capable of
# operating on the test runner's own checkout either way. That is not
# hypothetical: during a mutation run that deliberately broke
# docs_worktree_setup, the fixture committed the test runner's own staged work
# onto a fixture branch. What prevents it is refusing to run the dependent
# assertions at all when the path is empty or missing -- so do not add an
# unguarded `git -C "$SOMETHING"` believing the primitive protects you.
require_dir() { # require_dir <label> <path>
  local label="$1" d="$2"
  if [ -n "$d" ] && [ -d "$d" ]; then PASS=$((PASS + 1)); return 0; fi
  FAIL=$((FAIL + 1))
  printf 'FAIL: %s -- no usable directory [%s]; dependent assertions skipped\n' "$label" "$d" >&2
  return 1
}

# The fixtures run against a PINNED git config, not the operator's. Ambient
# config silently degraded these assertions: a global `merge.ff = only` or
# `user.useConfigOnly = true` took the suite from 103 passed to 98, and a global
# `core.hooksPath` pointing at a failing pre-push took it to 87 -- all of it
# reported as ordinary failures with no hint that the machine, not the code, had
# changed. Identity is then set PER REPO (linked worktrees read the main
# checkout's config, so worktree commits inherit it) because /dev/null as a
# global config leaves git with no committer.
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
# gitp: a push with hooks pinned off, so a stray repo-level hook cannot turn a
# fixture push into a mystery failure.
gitp() { git -c core.hooksPath=/dev/null "$@"; }

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
# Every temp directory this file creates goes on one list with one EXIT trap, so
# a RED run cleans up as thoroughly as a green one. docs_worktree_setup makes its
# OWN `mktemp -d` parent, outside $DOCS_TMP, and only a successful teardown ever
# removed it -- so each failing run leaked a worktree parent into the system temp
# dir, which is where the release is cut. Registering the parent as soon as the
# path is known covers the failure paths too.
CLEANUP_PATHS=()
cleanup_temps() {
  [ "${#CLEANUP_PATHS[@]}" -eq 0 ] || rm -rf "${CLEANUP_PATHS[@]}"
}
trap cleanup_temps EXIT
# register_temp <path...>: add a directory to the EXIT cleanup list.
register_temp() {
  local p
  for p in "$@"; do
    [ -n "$p" ] || continue
    CLEANUP_PATHS+=("$p")
  done
}

BUMP_DIR=$(mktemp -d)
register_temp "$BUMP_DIR"

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
WAIT_DIR=$(mktemp -d)
register_temp "$WAIT_DIR"
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
# release_landed operates on the CURRENT directory (finish_release cd's to the
# repo root first), so these run through `in_dir`, which refuses an empty path.
# `cd ""` returns 0 in bash, so `cd "$X" || return` is NOT a guard on its own.
in_dir() { # in_dir <dir> <cmd...>
  local d="$1"; shift
  [ -n "$d" ] || return 90
  cd "$d" || return 91
  "$@"
}

OBS_TMP=$(mktemp -d)
register_temp "$OBS_TMP"
OBS_CO="${OBS_TMP}/repo"
git init --bare --quiet "${OBS_TMP}/origin.git"
git -c init.defaultBranch=main init --quiet "$OBS_CO"
git -C "$OBS_CO" config user.email t@t
git -C "$OBS_CO" config user.name t
git -C "$OBS_CO" checkout --quiet -B main
printf 'version = "0.25.0"\n' >"${OBS_CO}/Cargo.toml"
git -C "$OBS_CO" add Cargo.toml
git -C "$OBS_CO" commit --quiet --no-verify -m "chore(release): 0.25.0"
git -C "$OBS_CO" remote add origin "${OBS_TMP}/origin.git"
gitp -C "$OBS_CO" push --quiet -u origin main

if require_dir "observers: fixture repo" "$OBS_CO"; then
  # A reachable remote that does not carry the version is a definite NO.
  eq "landed: reachable remote without the version is 0" "0" \
    "$( (in_dir "$OBS_CO" release_landed main Cargo.toml package.version 9.9.9) 2>/dev/null )"

  # THE REGRESSION. Break the remote AFTER a successful fetch, so the
  # remote-tracking ref is still perfectly readable locally -- which is exactly
  # what a network outage looks like: origin/main does not vanish, it just stops
  # being updated. A gate phrased as "did the ref read cleanly" answers a
  # confident 0 here, and the poll loop turns a sustained 0 into an EJECTED
  # verdict. Only the fetch's exit status can tell the difference.
  git -C "$OBS_CO" remote set-url origin "${OBS_TMP}/does-not-exist.git"
  ok "landed: the remote-tracking ref is still readable during the outage" \
    test -n "$(git -C "$OBS_CO" rev-parse --verify --quiet origin/main 2>/dev/null)"
  eq "landed: unreachable remote is unknown, not 0" "unknown" \
    "$( (in_dir "$OBS_CO" release_landed main Cargo.toml package.version 0.25.0) 2>/dev/null )"
  # Same failure, same requirement, for the queue observer: the rc must belong to
  # ls-remote, not to the grep that used to consume it.
  eq "queue: unreachable remote is unknown, not 0" "unknown" \
    "$( (in_dir "$OBS_CO" release_queue_ref_present 42) 2>/dev/null )"

  git -C "$OBS_CO" remote set-url origin "${OBS_TMP}/origin.git"
  eq "queue: reachable remote without the ref is 0" "0" \
    "$( (in_dir "$OBS_CO" release_queue_ref_present 42) 2>/dev/null )"
  # A real merge-queue ref: refs/heads/gh-readonly-queue/<base>/pr-<N>-<sha>.
  OBS_SHA="$(git -C "$OBS_CO" rev-parse HEAD)"
  git -C "${OBS_TMP}/origin.git" update-ref \
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
# Every fixture command below addresses its repo with `git -C <dir>` and writes
# through absolute paths, and `require_dir` (see the fixture preamble at the top
# of this file -- it, not `git -C`, is what makes that safe) gates every block on
# the helper under test having returned a real directory.
DOCS_TMP=$(mktemp -d)
register_temp "$DOCS_TMP"
DOCS_CO="${DOCS_TMP}/checkout"
DOCS_BRANCH="docs/fixture-current"
git init --bare --quiet "${DOCS_TMP}/origin.git"
git -c init.defaultBranch=main init --quiet "$DOCS_CO"
git -C "$DOCS_CO" config user.email t@t
git -C "$DOCS_CO" config user.name t
git -C "$DOCS_CO" checkout --quiet -B main
printf 'docs\n' >"${DOCS_CO}/index.md"
git -C "$DOCS_CO" add index.md
git -C "$DOCS_CO" commit --quiet --no-verify -m "init"
git -C "$DOCS_CO" remote add origin "${DOCS_TMP}/origin.git"
gitp -C "$DOCS_CO" push --quiet -u origin main
# Prove the pin took, rather than assuming it: every assertion below about
# config-independence rests on this.
eq "fixture: ambient global config is neutralised" "" \
  "$(git -C "$DOCS_CO" config --global --get merge.ff 2>/dev/null || true)"
eq "fixture: repo identity is set for worktree commits" "t@t" \
  "$(git -C "$DOCS_CO" config user.email 2>/dev/null)"

# Fresh arm: no such branch on the remote, so the worktree starts from
# origin/main -- NOT from whatever the shared checkout has checked out. Prove
# that by parking the shared checkout on an unrelated branch first.
git -C "$DOCS_CO" checkout --quiet -b someone-elses-work
printf 'wip\n' >"${DOCS_CO}/wip.md"
git -C "$DOCS_CO" add wip.md
git -C "$DOCS_CO" commit --quiet --no-verify -m "wip"
WT1="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>/dev/null )"
# Registered for cleanup the moment the path is known: docs_worktree_setup makes
# its own mktemp parent OUTSIDE $DOCS_TMP, and a red run never reaches teardown.
[ -z "$WT1" ] || register_temp "$(dirname "$WT1")"
if require_dir "docs wt: created" "$WT1"; then
  eq "docs wt: on the stable branch" "$DOCS_BRANCH" "$(git -C "$WT1" rev-parse --abbrev-ref HEAD 2>/dev/null)"
  # The load-bearing one: it must NOT have inherited the other agent's branch.
  eq "docs wt: starts from origin/main, not the shared checkout's branch" \
    "$(git -C "$DOCS_CO" rev-parse origin/main)" "$(git -C "$WT1" rev-parse HEAD 2>/dev/null)"
  ok "docs wt: the other agent's file is absent" test ! -f "${WT1}/wip.md"
  ok "docs wt: base sha recorded outside the worktree" test -f "$(dirname "$WT1")/base-sha"
  eq "docs wt: worktree left clean" "" "$(git -C "$WT1" status --porcelain 2>/dev/null)"
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
  git -C "$DOCS_CO" show-ref --verify --quiet "refs/heads/${DOCS_BRANCH}"

# Teardown, carrying unpushed commits: RETAINED, so a failed docs run is
# recoverable rather than discarded.
WT2="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>/dev/null )"
[ -z "$WT2" ] || register_temp "$(dirname "$WT2")"
if require_dir "docs teardown: second worktree created" "$WT2"; then
  printf 'draft\n' >"${WT2}/draft.md"
  git -C "$WT2" add draft.md
  git -C "$WT2" commit --quiet --no-verify -m "docs: draft"
  (docs_worktree_teardown "$WT2") >/dev/null 2>&1
  ok "docs teardown: unpushed work retained" test -d "$WT2"
  ok "docs teardown: retained worktree keeps its commit" test -f "${WT2}/draft.md"

  # Same worktree once the work is PUSHED: now disposable. Without this arm a
  # successful docs run would pin the worktree forever and collide next release.
  gitp -C "$WT2" push --quiet -u origin "$DOCS_BRANCH"
  (docs_worktree_teardown "$WT2") >/dev/null 2>&1
  ok "docs teardown: pushed work is disposable" test ! -d "$WT2"
fi

# Extend arm (#820): the stable branch now exists on the remote, so the next
# release starts from IT and merges origin/main in -- one docs PR that each
# release extends, not a new version-pinned branch stacked on the last.
git -C "$DOCS_CO" checkout --quiet main
printf 'more\n' >>"${DOCS_CO}/index.md"
git -C "$DOCS_CO" commit --quiet --no-verify -am "main moves on"
gitp -C "$DOCS_CO" push --quiet origin main

# The extend arm runs under a HOSTILE-BUT-ORDINARY config, pinned on the repo so
# the assertion does not depend on the operator's machine. `merge.ff = only` is a
# common global setting, and under it the plain `git merge` this used to run dies
# with "fatal: Not possible to fast-forward" -- on entirely CORRECT input, since a
# diverged docs branch is the whole reason the merge exists. It then told the
# operator to go and resolve it in the shared checkout, which is the fallback
# #845 exists to close. `user.useConfigOnly` is pinned alongside it because it
# breaks the same step a different way.
git -C "$DOCS_CO" config merge.ff only
git -C "$DOCS_CO" config user.useConfigOnly true
WT3="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>/dev/null )"
[ -z "$WT3" ] || register_temp "$(dirname "$WT3")"
if require_dir "docs extend: worktree created under merge.ff=only" "$WT3"; then
  ok "docs extend: the earlier release's docs commit is still there" test -f "${WT3}/draft.md"
  ok "docs extend: origin/main was merged in" \
    git -C "$WT3" merge-base --is-ancestor origin/main HEAD
  # --no-ff, so the merge-in is a real merge commit (two parents) regardless of
  # whether it could have fast-forwarded.
  eq "docs extend: merge-in is a merge commit" "2" \
    "$(git -C "$WT3" rev-list --parents -n 1 HEAD 2>/dev/null | awk '{print NF-1}')"
  # The merge is setup's own work, so it must not count as agent work at teardown.
  eq "docs extend: base sha recorded after the merge-in" \
    "$(git -C "$WT3" rev-parse HEAD 2>/dev/null)" "$(cat "$(dirname "$WT3")/base-sha" 2>/dev/null)"
  (docs_worktree_teardown "$WT3") >/dev/null 2>&1
  ok "docs extend: unchanged extend-arm worktree removed" test ! -d "$WT3"
fi
git -C "$DOCS_CO" config --unset merge.ff
git -C "$DOCS_CO" config --unset user.useConfigOnly

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
    test -z "$(git -C "$DOCS_CO" worktree list --porcelain | worktree_holding_branch "$DOCS_BRANCH" || true)"
  # And the named recovery genuinely unblocks it: delete the branch, run again.
  git -C "$DOCS_CO" branch -D "$DOCS_BRANCH" >/dev/null 2>&1
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
git -C "$DOCS_CO" worktree add --quiet -b hand-made "${SHARED}/hand-made" main >/dev/null 2>&1
# Pushed, so it satisfies the DISPOSABLE arm on content. It must survive anyway:
# "looks disposable" is not an ownership claim, and every clean fully-pushed
# worktree in the docs repo looks exactly like this one.
gitp -C "${SHARED}/hand-made" push --quiet -u origin hand-made
FOREIGN_RC=0
(docs_worktree_teardown "${SHARED}/hand-made") >/dev/null 2>&1 || FOREIGN_RC=$?
eq "docs teardown: foreign worktree teardown refuses" "1" "$FOREIGN_RC"
ok "docs teardown: foreign worktree RETAINED" test -d "${SHARED}/hand-made"
ok "docs teardown: foreign worktree's branch survives" \
  git -C "$DOCS_CO" show-ref --verify --quiet "refs/heads/hand-made"
ok "docs teardown: foreign worktree keeps its registration" \
  test -n "$(git -C "$DOCS_CO" worktree list --porcelain | worktree_holding_branch hand-made || true)"
ok "docs teardown: an unowned parent directory survives" test -d "$SHARED"
ok "docs teardown: its siblings survive" test -f "${SHARED}/sibling.txt"

# A marker naming a DIFFERENT worktree is not a claim to this one either -- the
# marker has to match, or "some sibling directory has a base-sha in it" would be
# enough to authorise the removal.
MISMARK="${DOCS_TMP}/mismarked"
mkdir -p "$MISMARK"
git -C "$DOCS_CO" worktree add --quiet -b mis-marked "${MISMARK}/wt" main >/dev/null 2>&1
gitp -C "${MISMARK}/wt" push --quiet -u origin mis-marked
printf '%s\n' "$(git -C "${MISMARK}/wt" rev-parse HEAD)" >"${MISMARK}/base-sha"
printf '%s\n' "${MISMARK}/some-other-worktree" >"${MISMARK}/worktree-path"
MISMARK_RC=0
(docs_worktree_teardown "${MISMARK}/wt") >/dev/null 2>&1 || MISMARK_RC=$?
eq "docs teardown: marker naming another worktree refuses" "1" "$MISMARK_RC"
ok "docs teardown: mismarked worktree retained" test -d "${MISMARK}/wt"

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
BND_TMP=$(mktemp -d)
register_temp "$BND_TMP"
BND="${BND_TMP}/repo"
git -c init.defaultBranch=main init --quiet "$BND"
git -C "$BND" config user.email t@t
git -C "$BND" config user.name t
bnd_commit() { # bnd_commit <file> <content> <subject>
  printf '%s\n' "$2" >"${BND}/$1"
  git -C "$BND" add "$1"
  git -C "$BND" commit --quiet --no-verify -m "$3"
}
# The reader under test's production twin is ref_version; here it is git show
# plus a sed, so the fixture stays hermetic.
bnd_version() { git -C "$BND" show "${1}:Cargo.toml" 2>/dev/null | sed -n 's/^version = "\(.*\)"$/\1/p'; }

bnd_commit Cargo.toml 'version = "0.24.0"' "chore(release): 0.24.0"
bnd_commit src.txt 'work' "feat: something"
bnd_commit Cargo.toml 'version = "0.25.0"' "chore(release): 0.25.0"
BND_RELEASE="$(git -C "$BND" rev-parse HEAD)"
# What lands between `legion pr merge` and `--finish`: ordinary merges that do
# not touch the version file at all.
bnd_commit a.txt 'a' "feat(#1): a"
bnd_commit b.txt 'b' "fix(#2): b"
BND_TIP="$(git -C "$BND" rev-parse HEAD)"

BND_LIST="$(git -C "$BND" rev-list HEAD -- Cargo.toml)"
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
  "$(git -C "$BND" rev-list HEAD | wc -l | tr -d ' ')"

# IDEMPOTENCE, which is what release.sh:767 and legion-release.md:88 promise when
# they say re-running --finish is safe. Re-resolving must return the SAME sha
# however far the branch has moved on: that is what makes the second run match
# the existing tag and re-push it, instead of deriving a new sha and dying on
# "tag already exists and points at <other>".
bnd_commit c.txt 'c' "fix(#3): c"
BND_AGAIN="$(git -C "$BND" rev-list HEAD -- Cargo.toml | release_boundary_commit bnd_version 0.25.0 200)"
eq "release commit: stable across re-runs as the branch moves" "$BND_RELEASE" "$BND_AGAIN"
# And the next release's boundary is its own commit, not the previous one.
bnd_commit Cargo.toml 'version = "0.26.0"' "chore(release): 0.26.0"
BND_NEXT="$(git -C "$BND" rev-parse HEAD)"
eq "release commit: the next release resolves to its own commit" "$BND_NEXT" \
  "$(git -C "$BND" rev-list HEAD -- Cargo.toml | release_boundary_commit bnd_version 0.26.0 200)"
# Once a NEWER release has landed, the older one no longer resolves at all
# (rc 1) rather than resolving to something plausible-looking. In production the
# branch-tip landed gate refuses first, for the same reason: --finish on a
# superseded release must stop, not tag.
eq "release commit: a superseded release refuses to resolve" "1" \
  "$(git -C "$BND" rev-list HEAD -- Cargo.toml | release_boundary_commit bnd_version 0.25.0 200 >/dev/null; printf '%s' $?)"
# A commit that touches the version file WITHOUT changing the version (a
# dependency edit) must not be mistaken for the release commit.
bnd_commit Cargo.toml 'version = "0.26.0"
serde = "1.0.1"' "chore: dep bump"
ok "release commit: the dep-bump commit really landed" \
  test "$(git -C "$BND" rev-parse HEAD)" != "$BND_NEXT"
eq "release commit: a non-version edit to the file is not the release" "$BND_NEXT" \
  "$(git -C "$BND" rev-list HEAD -- Cargo.toml | release_boundary_commit bnd_version 0.26.0 200)"

printf '\n[test-release] %d passed, %d failed\n' "$PASS" "$FAIL" >&2
[ "$FAIL" -eq 0 ]
