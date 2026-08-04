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
BUMP_DIR=$(mktemp -d)
trap 'rm -rf "$BUMP_DIR"' EXIT

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

# wait_for_release_merge: the bounded poll, driven entirely by injected command
# STRINGS -- no gh, no git, no network. Each observer is a shell one-liner; a
# counter file makes a sequence of observations across polls.
WAIT_DIR=$(mktemp -d)
# step <file>: print the call count so far, then increment it.
step() { local f="$1" n; n="$(cat "$f")"; printf '%s\n' "$((n + 1))" >"$f"; printf '%s\n' "$n"; }

eq "wait: merged on the first poll" "merged" \
  "$(wait_for_release_merge 5 0 'printf 1' 'printf OPEN' 'printf 0')"
eq "wait: merged rc" "0" \
  "$(wait_for_release_merge 5 0 'printf 1' 'printf OPEN' 'printf 0' >/dev/null; printf '%s' $?)"

eq "wait: closed pr stops the release" "closed" \
  "$(wait_for_release_merge 5 0 'printf 0' 'printf CLOSED' 'printf 0')"
eq "wait: closed rc" "3" \
  "$(wait_for_release_merge 5 0 'printf 0' 'printf CLOSED' 'printf 0' >/dev/null || printf '%s' $?)"

# Queued for two polls, then lands: the authoritative `landed` signal flips
# while the queue ref disappears in the same window. Must read as merged, not
# as an ejection.
printf '0\n' >"${WAIT_DIR}/land"
LANDED_SEQ="n=\$(step ${WAIT_DIR}/land); if [ \"\$n\" -ge 2 ]; then printf 1; else printf 0; fi"
QUEUE_SEQ="if [ \"\$(cat ${WAIT_DIR}/land)\" -lt 2 ]; then printf 1; else printf 0; fi"
eq "wait: queued then merged" "merged" \
  "$(wait_for_release_merge 6 0 "$LANDED_SEQ" 'printf OPEN' "$QUEUE_SEQ")"

# Seen in the queue, then gone, and main never carries the release: ejection --
# but only after the CONFIRMING second observation, since the queue ref also
# disappears at the moment of a successful merge.
printf '0\n' >"${WAIT_DIR}/ej"
QUEUE_ONCE="n=\$(step ${WAIT_DIR}/ej); if [ \"\$n\" = 0 ]; then printf 1; else printf 0; fi"
eq "wait: ejection reported" "ejected" \
  "$(wait_for_release_merge 6 0 'printf 0' 'printf OPEN' "$QUEUE_ONCE")"
printf '0\n' >"${WAIT_DIR}/ej2"
QUEUE_ONCE2="n=\$(step ${WAIT_DIR}/ej2); if [ \"\$n\" = 0 ]; then printf 1; else printf 0; fi"
eq "wait: ejection rc" "4" \
  "$(wait_for_release_merge 6 0 'printf 0' 'printf OPEN' "$QUEUE_ONCE2" >/dev/null || printf '%s' $?)"
# A single missing-ref observation immediately after a queued one is NOT an
# ejection: with a budget of exactly 2 polls the confirming observation is the
# last one, so a merge that lands on it still reads as merged.
printf '0\n' >"${WAIT_DIR}/race"
RACE_LANDED="n=\$(step ${WAIT_DIR}/race); if [ \"\$n\" -ge 1 ]; then printf 1; else printf 0; fi"
RACE_QUEUE="if [ \"\$(cat ${WAIT_DIR}/race)\" -lt 1 ]; then printf 1; else printf 0; fi"
eq "wait: merge-window ref drop is not an ejection" "merged" \
  "$(wait_for_release_merge 2 0 "$RACE_LANDED" 'printf OPEN' "$RACE_QUEUE")"

eq "wait: budget expiry is a timeout" "timeout" \
  "$(wait_for_release_merge 3 0 'printf 0' 'printf OPEN' 'printf 0')"
eq "wait: timeout rc" "5" \
  "$(wait_for_release_merge 3 0 'printf 0' 'printf OPEN' 'printf 0' >/dev/null || printf '%s' $?)"
# An observer that cannot be read must not stall or crash the wait -- it falls
# back to "no signal" and the budget still expires cleanly.
eq "wait: unreadable observers expire" "timeout" \
  "$(wait_for_release_merge 2 0 'exit 7' 'exit 7' 'exit 7')"
# The observers are eval'd in THIS shell, so a SOURCED helper resolves. If they
# were run in a subshell that could not see release.sh's own functions, every
# poll would read landed=0 and a merged release would time out.
sourced_probe() { printf '1\n'; }
eq "wait: injected command resolves a sourced helper" "merged" \
  "$(wait_for_release_merge 2 0 'sourced_probe' 'printf OPEN' 'printf 0')"

# tag_target_matches: the guard between "the queue merged something" and "the
# tag points at the release". Fails CLOSED on an unreadable version.
ok "tag target: match"          tag_target_matches 0.26.0 0.26.0
no "tag target: mismatch"       tag_target_matches 0.26.0 0.25.0
no "tag target: empty observed" tag_target_matches 0.26.0 ""

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

# docs_worktree_setup / docs_worktree_teardown against REAL git repos. These two
# are the mechanism #845 is actually buying -- the pure helpers above only decide
# what they should do -- so they are exercised end to end against a local bare
# remote. No network, no gh, no shingle. Both call `fail` (which exits), so every
# arm runs in a subshell.
#
# EVERY fixture command below addresses its repo with `git -C <dir>` and writes
# through absolute paths. Nothing here uses `cd`, and that is a hard rule, not a
# style preference: `cd ""` is a SILENT NO-OP in bash, so a `(cd "$WT" && git
# commit ...)` where the helper under test returned nothing runs the fixture's
# git commands in whatever repo is running the tests. That is not hypothetical --
# it happened while building this file, during a mutation run that deliberately
# broke docs_worktree_setup, and it committed the test runner's own staged work
# onto a fixture branch. `require_dir` below turns that class into a loud test
# failure instead of collateral damage.
require_dir() { # require_dir <label> <path>
  local label="$1" d="$2"
  if [ -n "$d" ] && [ -d "$d" ]; then PASS=$((PASS + 1)); return 0; fi
  FAIL=$((FAIL + 1))
  printf 'FAIL: %s -- no usable directory [%s]; dependent assertions skipped\n' "$label" "$d" >&2
  return 1
}
gitc() { git -c user.email=t@t -c user.name=t "$@"; }

DOCS_TMP=$(mktemp -d)
DOCS_CO="${DOCS_TMP}/checkout"
DOCS_BRANCH="docs/fixture-current"
git init --bare --quiet "${DOCS_TMP}/origin.git"
git -c init.defaultBranch=main init --quiet "$DOCS_CO"
git -C "$DOCS_CO" checkout --quiet -B main
printf 'docs\n' >"${DOCS_CO}/index.md"
git -C "$DOCS_CO" add index.md
gitc -C "$DOCS_CO" commit --quiet --no-verify -m "init"
git -C "$DOCS_CO" remote add origin "${DOCS_TMP}/origin.git"
git -C "$DOCS_CO" push --quiet -u origin main

# Fresh arm: no such branch on the remote, so the worktree starts from
# origin/main -- NOT from whatever the shared checkout has checked out. Prove
# that by parking the shared checkout on an unrelated branch first.
git -C "$DOCS_CO" checkout --quiet -b someone-elses-work
printf 'wip\n' >"${DOCS_CO}/wip.md"
git -C "$DOCS_CO" add wip.md
gitc -C "$DOCS_CO" commit --quiet --no-verify -m "wip"
WT1="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>/dev/null )"
if require_dir "docs wt: created" "$WT1"; then
  eq "docs wt: on the stable branch" "$DOCS_BRANCH" "$(git -C "$WT1" rev-parse --abbrev-ref HEAD 2>/dev/null)"
  # The load-bearing one: it must NOT have inherited the other agent's branch.
  eq "docs wt: starts from origin/main, not the shared checkout's branch" \
    "$(git -C "$DOCS_CO" rev-parse origin/main)" "$(git -C "$WT1" rev-parse HEAD 2>/dev/null)"
  ok "docs wt: the other agent's file is absent" test ! -f "${WT1}/wip.md"
  ok "docs wt: base sha recorded outside the worktree" test -f "$(dirname "$WT1")/base-sha"
  eq "docs wt: worktree left clean" "" "$(git -C "$WT1" status --porcelain 2>/dev/null)"
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
if require_dir "docs teardown: second worktree created" "$WT2"; then
  printf 'draft\n' >"${WT2}/draft.md"
  git -C "$WT2" add draft.md
  gitc -C "$WT2" commit --quiet --no-verify -m "docs: draft"
  (docs_worktree_teardown "$WT2") >/dev/null 2>&1
  ok "docs teardown: unpushed work retained" test -d "$WT2"
  ok "docs teardown: retained worktree keeps its commit" test -f "${WT2}/draft.md"

  # Same worktree once the work is PUSHED: now disposable. Without this arm a
  # successful docs run would pin the worktree forever and collide next release.
  git -C "$WT2" push --quiet -u origin "$DOCS_BRANCH"
  (docs_worktree_teardown "$WT2") >/dev/null 2>&1
  ok "docs teardown: pushed work is disposable" test ! -d "$WT2"
fi

# Extend arm (#820): the stable branch now exists on the remote, so the next
# release starts from IT and merges origin/main in -- one docs PR that each
# release extends, not a new version-pinned branch stacked on the last.
git -C "$DOCS_CO" checkout --quiet main
printf 'more\n' >>"${DOCS_CO}/index.md"
gitc -C "$DOCS_CO" commit --quiet --no-verify -am "main moves on"
git -C "$DOCS_CO" push --quiet origin main
WT3="$( (docs_worktree_setup "$DOCS_CO" "$DOCS_BRANCH" main fixture-docs) 2>/dev/null )"
if require_dir "docs extend: worktree created on the existing stable branch" "$WT3"; then
  ok "docs extend: the earlier release's docs commit is still there" test -f "${WT3}/draft.md"
  ok "docs extend: origin/main was merged in" \
    git -C "$WT3" merge-base --is-ancestor origin/main HEAD
  # The merge is setup's own work, so it must not count as agent work at teardown.
  eq "docs extend: base sha recorded after the merge-in" \
    "$(git -C "$WT3" rev-parse HEAD 2>/dev/null)" "$(cat "$(dirname "$WT3")/base-sha" 2>/dev/null)"
  (docs_worktree_teardown "$WT3") >/dev/null 2>&1
  ok "docs extend: unchanged extend-arm worktree removed" test ! -d "$WT3"
fi

# --docs-worktree-done= takes an OPERATOR-supplied path. A worktree this script
# did not create lives in a directory it does not own, so teardown must remove
# the worktree and nothing else -- deleting the parent would take the siblings
# with it, which is the collateral damage this whole feature exists to prevent.
SHARED="${DOCS_TMP}/shared"
mkdir -p "$SHARED"
printf 'do not delete me\n' >"${SHARED}/sibling.txt"
git -C "$DOCS_CO" worktree add --quiet -b hand-made "${SHARED}/hand-made" main >/dev/null 2>&1
# Push it so it reaches the DISPOSABLE arm -- the guard only matters there, and
# a retained worktree would never have reached the `rm -rf` in the first place.
git -C "${SHARED}/hand-made" push --quiet -u origin hand-made
(docs_worktree_teardown "${SHARED}/hand-made") >/dev/null 2>&1
ok "docs teardown: foreign worktree still removed" test ! -d "${SHARED}/hand-made"
ok "docs teardown: an unowned parent directory survives" test -d "$SHARED"
ok "docs teardown: its siblings survive" test -f "${SHARED}/sibling.txt"

rm -rf "$WAIT_DIR" "$DOCS_TMP"

printf '\n[test-release] %d passed, %d failed\n' "$PASS" "$FAIL" >&2
[ "$FAIL" -eq 0 ]
