#!/usr/bin/env bash
# release.sh: one-command release, generalized to any repo via release.toml
# (schema documented there; see #741). The version-of-record file (default
# Cargo.toml) is the source of truth for the version; everything else --
# changelog path, propagation targets, tag format -- is read from config,
# derived, and validated.
#
# Usage:
#   scripts/release.sh patch            # 0.18.2 -> 0.18.3
#   scripts/release.sh minor            # 0.18.2 -> 0.19.0
#   scripts/release.sh major            # 0.18.2 -> 1.0.0
#   scripts/release.sh 0.20.0           # explicit version
#   scripts/release.sh patch --dry-run  # print every step, mutate nothing
#   scripts/release.sh patch --activate # also build+install the binary and
#                                        # restart the local daemon (legion-
#                                        # specific; a no-op knob elsewhere)
#   scripts/release.sh patch --no-preflight  # skip the preflight gate (CI re-runs it)
#   scripts/release.sh --docs-worktree       # print an isolated worktree for the
#                                            # cross-repo docs step (#845)
#   scripts/release.sh --docs-worktree-done=<path>  # tear that worktree down
#
# Entry contract: the CHANGELOG entry for the new version must already be written
# (by the `changelog` agent, or by hand) and left UNCOMMITTED in the working tree.
# release.sh validates it, bumps the version, syncs the manifests, and commits the
# whole release ONTO A RELEASE BRANCH. The only file allowed to be dirty at entry
# is the configured changelog path; everything else must be committed. (Note: even
# --dry-run requires the "## <new>" CHANGELOG header to exist -- the header
# check is part of what a dry-run verifies.)
#
# What it does, in order (#844 split the old one-shot "commit + tag + atomic
# push to main" step into phases 6-8: only the TAG is pushed to the release
# branch now, and the commit earns its way there through the merge queue):
#   1. Guards: on the configured branch, tree clean except the changelog,
#      in sync with origin.
#   2. Computes the new version from the bump level (or takes it explicitly).
#   3. Requires the changelog's "## <new>" top header to exist.
#   4. Runs the configured preflight commands (default: scripts/preflight.sh).
#   5. Bumps the version-of-record file, refreshes Cargo.lock (Rust sources only).
#   6. Syncs every configured target (scripts/sync-version.sh), switches to
#      `release/<new>` and commits there -- never onto the release branch.
#   7. Pushes that branch via `legion push`, then STOPS. Nothing is tagged.
#
# The release is deliberately TWO invocations, with an agent's work between
# them. Earning the legion-simplify and legion-pr-write gates on the release
# commit and opening the PR cannot be done by a script: both gates are keyed to
# the commit's hash and a clean verdict only exists once their CHECK validators
# have run. So the orchestrator runs the gates, `legion pr create` and `legion
# pr merge` (see plugin/commands/legion-release.md step 4), and then calls:
#
#   scripts/release.sh <X.Y.Z> --finish=<pr-number>
#
#   8. Polls, bounded, until the merge lands -- `legion pr merge` enqueues and
#      returns, so the merge is asynchronous. An ejection or a timeout stops the
#      release with the reason named and tags nothing.
#   9. Re-reads origin/<branch>, verifies its tip really carries <new> (proving
#      the release landed and that no later release superseded it), resolves the
#      RELEASE COMMIT within that branch -- not the tip, which by then may be
#      several unrelated merges ahead -- tags THAT sha per the configured tag
#      format, and pushes ONLY the tag, which fires release.yml.
#  10. With --activate: builds the release binary, installs it to the plugin data
#      dir atomically, and restarts the local daemon (verifying it comes back up).
#
# Atomicity note (#844): the old `git push --atomic origin main <tag>` guaranteed
# the commit and tag landed together. Separating them in time gives that up by
# construction, so the recovery is stated rather than assumed -- a tagging failure
# after a successful merge reports INCOMPLETE BUT RECOVERABLE and names the sha to
# tag, because the version bump is already on the release branch and only the tag
# step remains.
#
# The pure helpers below (compute_new_version, is_semver, is_strictly_greater,
# non_changelog_dirty, field_leaf, render_tag, bump_source_file,
# classify_release_merge, wait_for_release_merge, tag_target_matches,
# release_boundary_commit, watch_repo_root, docs_start_point,
# worktree_holding_branch, worktree_is_disposable) are unit-tested by
# scripts/test-release.sh, which sources this file. main() runs only when the
# script is executed, not when it is sourced. Implementation is kept
# POSIX-portable (no GNU-only sed/sort) because the release is cut on darwin,
# where BSD sed/sort lack the GNU extensions.
set -euo pipefail

info() { printf '[release] %s\n' "$1" >&2; }
fail() { printf '[release] ERROR: %s\n' "$1" >&2; exit 1; }

# compute_new_version <current> <bump>
#   bump in {patch,minor,major} -> semver-increment current; otherwise echo bump
#   verbatim (the explicit-version path). Pure: no I/O, no globals.
compute_new_version() {
  local current="$1" bump="$2" maj min pat
  case "$bump" in
    patch|minor|major)
      IFS='.' read -r maj min pat <<EOF
$current
EOF
      case "$bump" in
        patch) pat=$((pat + 1)) ;;
        minor) min=$((min + 1)); pat=0 ;;
        major) maj=$((maj + 1)); min=0; pat=0 ;;
      esac
      printf '%s.%s.%s\n' "$maj" "$min" "$pat"
      ;;
    *)
      printf '%s\n' "$bump"
      ;;
  esac
}

# is_semver <v> -> 0 iff v is strictly X.Y.Z (no prerelease/build/extra segments).
is_semver() { [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; }

# is_strictly_greater <new> <current> -> 0 iff new > current. Field-by-field
# numeric compare (both args must be strict semver) -- avoids `sort -V`, which is
# a GNU-ism absent from stock BSD sort.
is_strictly_greater() {
  local na nb nc ca cb cc
  IFS='.' read -r na nb nc <<EOF
$1
EOF
  IFS='.' read -r ca cb cc <<EOF
$2
EOF
  if [ "$na" -ne "$ca" ]; then [ "$na" -gt "$ca" ]; return; fi
  if [ "$nb" -ne "$cb" ]; then [ "$nb" -gt "$cb" ]; return; fi
  [ "$nc" -gt "$cc" ]
}

# non_changelog_dirty <changelog_path>: read `git status --porcelain` on
# stdin, print the lines whose changed path is NOT exactly <changelog_path>
# (repo-root-relative, forward-slash form -- the configured changelog.path).
# Parses the porcelain path field (handling rename/copy "ORIG -> DEST" by
# testing DEST) so the guard allowlists the file itself, not any path that
# merely ends in that suffix.
non_changelog_dirty() {
  local changelog_path="$1"
  local line path
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    path="${line:3}"                       # drop the 2-char status + space
    case "$path" in *" -> "*) path="${path##* -> }" ;; esac
    [ "$path" = "$changelog_path" ] || printf '%s\n' "$line"
  done
}

# field_leaf <field>: the last "."-separated segment of a dotted field path
# (e.g. "package.version" -> "version"). Used to find the literal `key =
# "value"` / `"key": "value"` line a flat-file bump rewrites -- release.toml's
# [version].field uses the same dotted-path convention as `legion sym etc
# extract`, but the bump step below only supports a top-level (or
# single-table-nested) leaf key, not a deep JSON walk.
field_leaf() {
  local field="$1"
  printf '%s' "${field##*.}"
}

# render_tag <format> <version>: substitute "{version}" in a template string.
# Used for both release.toml's [git].tag_format (e.g. "v{version}") and
# [git].tag_message (e.g. "legion {version}") -- same substitution either way.
render_tag() {
  local format="$1" version="$2"
  printf '%s' "${format//\{version\}/$version}"
}

# bump_source_file <file> <field> <current> <new>: rewrite the version-of-
# record file in place. Detects strategy from the file extension; both
# strategies rewrite only the FIRST matching line, so a second identical
# version string elsewhere in the file is left untouched:
#   .toml -- awk-based exact-line replace on `<leaf> = "<current>"` (the
#            existing Cargo.toml strategy; portable, no GNU sed/awk needed).
#   .json -- awk-based substring replace on `"<leaf>": "<current>"`, tolerant
#            of surrounding indentation (unlike the .toml branch, a json line
#            is rarely an exact match on its own).
# Any other extension is unsupported; returns 1 without mutating anything so
# the caller can name the failure (this function never calls exit, so it stays
# unit-testable, including its failure path).
bump_source_file() {
  local file="$1" field="$2" current="$3" new="$4" leaf
  leaf="$(field_leaf "$field")"
  case "$file" in
    *.toml)
      awk -v old="${leaf} = \"${current}\"" -v new="${leaf} = \"${new}\"" '
        !done && $0 == old { print new; done = 1; next } { print }
      ' "$file" >"${file}.tmp" && mv "${file}.tmp" "$file"
      ;;
    *.json)
      awk -v old="\"${leaf}\": \"${current}\"" -v new="\"${leaf}\": \"${new}\"" '
        !done && index($0, old) > 0 { sub(old, new); done = 1 } { print }
      ' "$file" >"${file}.tmp" && mv "${file}.tmp" "$file"
      ;;
    *)
      return 1
      ;;
  esac
}

# -- merge-queue phase helpers (#844) ---------------------------------------

# classify_release_merge <landed> <pr_state> <queue_ref_present> <seen_queued>
#   -> merged | closed | queued | ejected | pending
#
# One observation of the merge, reduced to a verdict. Pure: every input is
# already-collected text, so the whole state machine is unit-testable without
# gh, git or a network.
#
# The ORDER of the arms is the correctness point. `landed` -- "origin/<branch>
# already carries the new version" -- wins over everything, because the queue
# rewrites the sha and the PR's own `state` is the laggy signal, not the
# authoritative one. Only after landed and state are both ruled out does queue
# membership decide, and only a PR that was OBSERVED in the queue and is no
# longer there can be called ejected: absence from the queue refs is not
# evidence of ejection on its own (the queue admits a bounded group size, so a
# PR can sit un-enqueued for a while), which is why that case stays `pending`
# and expires into a timeout instead of a false ejection report.
#
# `landed` and `queue_present` may also be the literal string `unknown`, which
# means the OBSERVATION FAILED -- the fetch or the ls-remote did not complete,
# so nothing at all was learned this poll. That is not "no" and it must have its
# own arm: every comparison below is `= "1"`, so without this a third value
# falls straight through to the `ejected` branch and a network outage gets
# reported as an ejection from the merge queue. An unreadable observer is
# `pending`, which spends the budget and expires into a timeout -- a timeout
# says "I could not observe the merge", which is exactly what happened.
classify_release_merge() {
  local landed="$1" state="$2" queue_present="$3" seen_queued="$4"
  if [ "$landed" = "1" ]; then printf 'merged\n'; return 0; fi
  case "$state" in
    MERGED|merged) printf 'merged\n'; return 0 ;;
    CLOSED|closed) printf 'closed\n'; return 0 ;;
  esac
  # Placed AFTER the state arms deliberately: a PR state read from a different
  # source is still a positive signal when the git observers are blind.
  if [ "$landed" = "unknown" ] || [ "$queue_present" = "unknown" ]; then
    printf 'pending\n'; return 0
  fi
  if [ "$queue_present" = "1" ]; then
    printf 'queued\n'
  elif [ "$seen_queued" = "1" ]; then
    printf 'ejected\n'
  else
    printf 'pending\n'
  fi
}

# wait_for_release_merge <max_polls> <interval> <landed_fn> <state_fn> <queue_fn>
#   -> prints merged|closed|ejected|timeout; rc 0|3|4|5
#
# The bounded poll `legion pr merge` makes necessary: enqueuing returns
# immediately and the merge happens later, so the release has to wait for it.
# The three observers are injected as FUNCTION NAMES and called directly --
# never `eval`d, and never assembled from interpolated values. That is a
# security property, not a style choice: the old form built command strings out
# of the PR number and four release.toml fields, so a hostile (or merely
# malformed) `git.branch` or `--finish=` argument became shell. Taking a name
# and calling it removes the sink entirely while keeping the loop just as
# testable -- the tests pass their OWN function names, and never need gh, git or
# a network. In production the names are closures defined in finish_release,
# which bind the release_landed / release_pr_state / release_queue_ref_present
# arguments as ARGV. Being plain function calls, they resolve in this shell, so
# a sourced helper is visible without exporting it: a subshell that could not
# see release_landed would report landed=0 forever and time out a release that
# had actually merged.
#
# An observer that exits non-zero is `unknown` for the two git-backed signals
# (NOT `0`, which would assert "definitely not merged / definitely not in the
# queue" on the strength of a failure) and `UNKNOWN` for the advisory PR state.
#
# An `ejected` verdict must be seen TWICE in a row before it is reported: the
# queue ref also disappears at the moment of a successful merge, so a single
# observation taken in that window would report an ejection for a release that
# actually landed. The confirming poll re-reads `landed` first, which is what
# breaks the tie.
#
# The budget is expressed in polls, not wall-clock, so a timeout is exactly
# reproducible in a test and the failure message can state the budget it spent.
wait_for_release_merge() {
  local max_polls="$1" interval="$2" landed_fn="$3" state_fn="$4" queue_fn="$5"
  local poll=0 landed state queue_present obs seen_queued=0 ejected_streak=0
  while [ "$poll" -lt "$max_polls" ]; do
    poll=$((poll + 1))
    landed="$("$landed_fn" 2>/dev/null </dev/null || printf 'unknown')"
    state="$("$state_fn" 2>/dev/null </dev/null || printf 'UNKNOWN')"
    queue_present="$("$queue_fn" 2>/dev/null </dev/null || printf 'unknown')"
    obs="$(classify_release_merge "$landed" "$state" "$queue_present" "$seen_queued")"
    case "$obs" in
      merged) printf 'merged\n'; return 0 ;;
      closed) printf 'closed\n'; return 3 ;;
      queued) seen_queued=1; ejected_streak=0 ;;
      ejected)
        ejected_streak=$((ejected_streak + 1))
        if [ "$ejected_streak" -ge 2 ]; then printf 'ejected\n'; return 4; fi
        ;;
      *) ejected_streak=0 ;;
    esac
    if [ "$poll" -lt "$max_polls" ]; then sleep "$interval"; fi
  done
  printf 'timeout\n'
  return 5
}

# tag_target_matches <expected_version> <observed_version> -> 0 iff the commit
# about to be tagged really is the release. The merge queue may squash or
# rebase, so the sha the tag lands on is never assumed to be the branch tip --
# it is re-read from origin and its version-of-record checked. An empty
# observed version (file missing on the ref, extract failed) is a mismatch, not
# a pass: this guard fails closed.
tag_target_matches() {
  [ -n "$2" ] && [ "$1" = "$2" ]
}

# ref_version <ref> <source_file_rel> <field>: the version-of-record as it
# stands on <ref>, read WITHOUT checking anything out (git show into a scratch
# file, then the same `legion sym etc extract` the rest of the script uses).
# The scratch file keeps the source file's basename so extract can sniff the
# format from the extension.
#
#   rc 0 + the version on stdout -- the read succeeded.
#   rc 2 + nothing on stdout     -- THE QUESTION COULD NOT BE ASKED: the scratch
#                                   dir failed, the ref or the file is absent, or
#                                   extract failed or came back empty.
#
# The distinction is load-bearing and it was the second door into the false
# ejection. Fix 2 correctly keyed `unknown` on the FETCH's exit status, but a
# fetch that SUCCEEDS followed by a read that fails used to be indistinguishable
# from "the remote definitely does not carry this version": every failure path
# here was swallowed (`|| true`) into an empty string, and `release_landed` then
# printed a confident 0. Sustained past the point where the queue ref has been
# seen, a 0 is an EJECTED verdict -- reported for a release that landed, sending
# the operator to re-run gates on a merged PR. An empty read is not evidence of
# absence, so it must not be able to produce one.
ref_version() {
  local ref="$1" file_rel="$2" field="$3" tmpdir scratch got rc=0
  tmpdir="$(mktemp -d)" || return 2
  scratch="${tmpdir}/${file_rel##*/}"
  if git show "${ref}:${file_rel}" >"$scratch" 2>/dev/null; then
    got="$(legion sym etc extract "$scratch" --field "$field" 2>/dev/null)" || rc=2
  else
    rc=2
  fi
  rm -rf "$tmpdir"
  [ "$rc" -eq 0 ] || return "$rc"
  [ -n "$got" ] || return 2
  printf '%s\n' "$got"
}

# release_boundary_commit <version_reader> <want> <max_walk>
#   stdin: candidate shas, NEWEST FIRST
#   -> prints the OLDEST consecutive sha whose version is <want>; rc 0
#      rc 1: the newest candidate does not carry <want> at all
#      rc 2: the walk hit <max_walk> without finding the boundary
#      rc 3: the list ran out while every entry still carried <want>
#
# THE RELEASE COMMIT, not the branch tip. Tagging `origin/<branch>` tags whatever
# has landed since -- and later commits do not touch the version file, so the
# "does this tree carry <new>" check passes on a tip N commits past the release
# and the tag ships a tree nobody released. This repo's own history proves it:
# v0.16.2's tag object was created after 51b3304 had already landed on main.
#
# The boundary is found by walking commits that TOUCH THE VERSION FILE, newest
# first, and taking the last one that still reads <want>: the commit before it
# reads the previous version, which is what makes it the boundary. That list is
# `git rev-list <ref> -- <file>`, and the release commit is in it by definition
# (it is the commit that wrote the version). Two properties matter:
#
#   Cost. It is ONE subprocess for the list plus one version read per commit
#   that touched the file -- 1 read across v0.24.0..v0.25.0, where 8 commits
#   landed and exactly one touched Cargo.toml. Walking `<ref>^` parents instead
#   would be a subprocess per commit since the release, and `git rev-parse
#   <root>^` on a root commit fails FATALLY under `set -euo pipefail`.
#
#   Precision. Not `git log --grep "(#<pr>)"`: merge-commit subjects exist in
#   this repo, so that is merge-method dependent, and --grep is a basic regex
#   over the WHOLE message -- a body that merely mentions (#844) matches.
#
# The walk is capped and every exhausted path returns non-zero. Running off the
# end must never fall through to tagging something: an unresolved boundary is a
# reason to stop, and the caller names which of the three it hit.
release_boundary_commit() {
  local reader="$1" want="$2" max="$3"
  local sha candidate="" walked=0
  while IFS= read -r sha; do
    [ -n "$sha" ] || continue
    walked=$((walked + 1))
    if [ "$walked" -gt "$max" ]; then return 2; fi
    # </dev/null so a reader that reads stdin cannot eat the candidate list.
    if [ "$("$reader" "$sha" </dev/null)" = "$want" ]; then
      candidate="$sha"
    else
      [ -n "$candidate" ] || return 1
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  # Unreachable in production (the caller gates on the tip carrying <want>
  # first), but the contract holds standalone: nothing matched at all is rc 1.
  [ -n "$candidate" ] || return 1
  return 3
}

# release_landed <branch> <source_file_rel> <field> <version>: print 1 when
# origin/<branch> already carries <version>, 0 when it demonstrably does not,
# and `unknown` when the question could not be asked. This is the AUTHORITATIVE
# merge signal -- the merge queue rewrites the sha, so "did my branch tip appear
# on main" is unanswerable, while "does main carry the release version" is exact.
#
# The unknown arm keys on the FETCH EXIT STATUS, and it has to. `ref_version`
# reads the LOCAL remote-tracking ref, which stays perfectly readable through a
# total network outage -- it just answers with whatever was true at the last
# successful fetch. So a gate phrased as "did the ref read cleanly" returns a
# confident 0 in precisely the scenario this exists to catch, and the caller
# then reports an ejection for a release nobody could see. The fetch is the only
# step that actually touches the remote, so its rc is the only honest signal.
release_landed() {
  local branch="$1" file_rel="$2" field="$3" want="$4" got
  if ! git fetch origin "$branch" --quiet >/dev/null 2>&1; then
    printf 'unknown\n'
    return 0
  fi
  # A successful fetch is necessary but NOT sufficient. If the version could not
  # be read off the ref that was just fetched, the honest answer is still
  # `unknown` -- printing 0 here is what turns an unreadable file into an
  # ejection verdict for a release that landed.
  if ! got="$(ref_version "origin/${branch}" "$file_rel" "$field")"; then
    printf 'unknown\n'
    return 0
  fi
  if [ "$got" = "$want" ]; then
    printf '1\n'
  else
    printf '0\n'
  fi
}

# release_pr_state <repo> <number>: the PR's state (OPEN/MERGED/CLOSED), or
# UNKNOWN when it cannot be read. Advisory only -- an unreadable state must not
# stall or fail the wait, it just leaves the decision to the other two signals.
release_pr_state() {
  local repo="$1" number="$2"
  legion pr view --repo "$repo" --number "$number" --json 2>/dev/null | python3 -c '
import json, sys
try:
    print(json.load(sys.stdin).get("state") or "UNKNOWN")
except Exception:
    print("UNKNOWN")
' 2>/dev/null || printf 'UNKNOWN\n'
}

# release_queue_ref_present <number>: print 1 while the PR has a merge-queue ref
# on the remote, 0 when the remote answered and the ref is not there, and
# `unknown` when the remote could not be reached. GitHub exposes queue
# membership as refs/heads/gh-readonly-queue/<base>/pr-<N>-<base-sha>, which
# `git ls-remote` can read without gh and without auth beyond the push remote.
#
# The output is CAPTURED BEFORE it is matched, so the rc under test belongs to
# ls-remote. Piping straight into grep gives the pipeline grep's status, under
# which "the remote is unreachable" and "the PR is not in the queue" are the
# same observation -- and the second one, repeated, is what the caller calls an
# ejection.
release_queue_ref_present() {
  local number="$1" out
  out="$(git ls-remote origin "refs/heads/gh-readonly-queue/*" 2>/dev/null)" || {
    printf 'unknown\n'
    return 0
  }
  case "$out" in
    *"/pr-${number}-"*) printf '1\n' ;;
    *)                  printf '0\n' ;;
  esac
}

# -- cross-repo docs worktree helpers (#845) --------------------------------
# Operator rule: commits to a repo that is NOT yours go in a worktree. Your own
# repo you may work in directly; anyone else's you isolate, because that repo's
# agent may be in-process in its checkout and you cannot see them. Cutting
# v0.25.0 proved the cost -- another agent's uncommitted edits were swept into
# the docs commit through the shared working tree. An instruction an agent must
# remember is not a mechanism, so the script creates the worktree and hands over
# a path the agent cannot get wrong.

# watch_repo_root <name>: read `legion watch list` on stdin, print the working
# directory registered for <name>. The listing is "<name>\t<path>" with an
# optional " (agent: X)" suffix on the path, which is stripped. Returns 1 when
# the repo is not registered, so the caller can name that rather than handing
# out an empty path.
watch_repo_root() {
  local want="$1" line name path
  while IFS= read -r line; do
    name="${line%%$'\t'*}"
    [ "$name" = "$want" ] || continue
    path="${line#*$'\t'}"
    path="${path%% (agent: *}"
    [ -n "$path" ] || return 1
    printf '%s\n' "$path"
    return 0
  done
  return 1
}

# docs_start_point <branch> <base_ref>: read `git ls-remote --heads origin` on
# stdin and print the ref the docs worktree should start from.
#
# This is where #845 and #820 meet. #845 requires the worktree to branch from
# origin/<base>, NOT from whatever the shared checkout happens to have checked
# out -- inheriting another agent's in-flight branch is the same defect one step
# removed. #820 requires the docs branch to be a STABLE name that the next
# release EXTENDS rather than a version-pinned branch that stacks a new PR per
# release (five deep on 0.20->0.24 before it was collapsed). So: when the stable
# docs branch already exists on the remote, start from it and merge the base in
# (the caller does the merge -- never a rebase, since `legion push` has no force
# path); when it does not, start from origin/<base>. Either way the start point
# is a REMOTE ref, which is the guarantee #845 is actually asking for.
docs_start_point() {
  local branch="$1" base_ref="$2" line
  while IFS= read -r line; do
    case "$line" in
      *$'\t'"refs/heads/${branch}") printf 'origin/%s\n' "$branch"; return 0 ;;
    esac
  done
  printf '%s\n' "$base_ref"
}

# worktree_holding_branch <branch>: read `git worktree list --porcelain` on
# stdin, print the worktree path that currently has <branch> checked out.
# A stale worktree from a previous failed release is REPORTED, not clobbered --
# it may contain unpushed work -- and with a stable docs branch this is the
# ordinary collision, not an exotic one.
worktree_holding_branch() {
  local branch="$1" line current=""
  while IFS= read -r line; do
    case "$line" in
      "worktree "*) current="${line#worktree }" ;;
      "branch refs/heads/${branch}")
        [ -n "$current" ] || return 1
        printf '%s\n' "$current"
        return 0
        ;;
    esac
  done
  return 1
}

# worktree_is_disposable <head_sha> <base_sha> <porcelain_status> <pushed>
#   -> 0 iff removing the worktree throws nothing away.
#
# Disposable means the docs agent left nothing behind that only exists here: the
# tree is clean AND either the branch never moved past the sha the setup step
# recorded, or everything on it is already on the remote branch. That second arm
# is what keeps a SUCCESSFUL docs run from poisoning the next release -- a run
# that committed and pushed carries commits, and "carries commits" alone would
# pin the worktree forever and collide on the stable branch next time.
#
# Both shas must be READABLE. An empty base -- the marker missing or unreadable
# -- makes the first arm unfireable, which quietly reduces the whole decision to
# `pushed`, and "clean and fully pushed" describes almost every worktree in a
# healthy repo. Fail closed on it, the same way an empty head fails closed.
worktree_is_disposable() {
  local head="$1" base="$2" status="$3" pushed="$4"
  [ -n "$head" ] || return 1
  [ -n "$base" ] || return 1
  [ -z "$status" ] || return 1
  [ "$head" = "$base" ] || [ "$pushed" = "1" ]
}

main() {
  local REPO_ROOT CONFIG SOURCE_FILE CHANGELOG
  REPO_ROOT="$(git rev-parse --show-toplevel)"
  cd "$REPO_ROOT"
  CONFIG="${REPO_ROOT}/release.toml"

  command -v legion >/dev/null 2>&1 || fail "'legion' binary not found on PATH -- required to read release.toml"
  command -v python3 >/dev/null 2>&1 || fail "'python3' not found on PATH -- required to parse release.toml's json fields (preflight.commands, targets)"
  [ -f "$CONFIG" ] || fail "release.toml not found at repo root -- see scripts/release.sh header for the schema"

  # extract_cfg <field>: read one release.toml field, or fail loudly naming it.
  extract_cfg() {
    local field="$1" value
    value="$(legion sym etc extract "$CONFIG" --field "$field" 2>/dev/null)" \
      || fail "release.toml missing required field '${field}'"
    printf '%s' "$value"
  }
  # extract_cfg_default <field> <default>: like extract_cfg, but falls back
  # to <default> instead of failing when the field is absent.
  extract_cfg_default() {
    local field="$1" default="$2"
    legion sym etc extract "$CONFIG" --field "$field" 2>/dev/null || printf '%s' "$default"
  }

  local SOURCE_FILE_REL SOURCE_FIELD CHANGELOG_REL RELEASE_BRANCH TAG_FORMAT TAG_MESSAGE_FORMAT CO_AUTHORED_BY
  local WORK_REPO MERGE_WAIT_POLLS MERGE_WAIT_INTERVAL
  SOURCE_FILE_REL="$(extract_cfg version.file)"
  SOURCE_FIELD="$(extract_cfg version.field)"
  CHANGELOG_REL="$(extract_cfg changelog.path)"
  RELEASE_BRANCH="$(extract_cfg_default git.branch main)"
  TAG_FORMAT="$(extract_cfg_default git.tag_format 'v{version}')"
  TAG_MESSAGE_FORMAT="$(extract_cfg_default git.tag_message '{version}')"
  CO_AUTHORED_BY="$(extract_cfg_default git.co_authored_by 'Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
  # The name this repo answers to in watch.toml -- what `legion push` / `legion
  # pr *` resolve their work source from. Defaults to the checkout's directory
  # name, which is the convention every registered repo already follows.
  WORK_REPO="$(extract_cfg_default git.repo "${REPO_ROOT##*/}")"
  MERGE_WAIT_POLLS="$(extract_cfg_default git.merge_wait_polls 90)"
  MERGE_WAIT_INTERVAL="$(extract_cfg_default git.merge_wait_interval 20)"

  SOURCE_FILE="${REPO_ROOT}/${SOURCE_FILE_REL}"
  CHANGELOG="${REPO_ROOT}/${CHANGELOG_REL}"

  # -- arg parsing -----------------------------------------------------------
  local BUMP="" DRY_RUN=0 ACTIVATE=0 RUN_PREFLIGHT=1 MODE=stage PR_NUMBER="" DOCS_WT_DONE="" arg
  for arg in "$@"; do
    case "$arg" in
      patch|minor|major) BUMP="$arg" ;;
      [0-9]*.[0-9]*.[0-9]*) BUMP="$arg" ;;
      --dry-run) DRY_RUN=1 ;;
      --activate) ACTIVATE=1 ;;
      --no-preflight) RUN_PREFLIGHT=0 ;;
      --finish=*) MODE=finish; PR_NUMBER="${arg#*=}" ;;
      --docs-worktree) MODE=docs-setup ;;
      --docs-worktree-done=*) MODE=docs-teardown; DOCS_WT_DONE="${arg#*=}" ;;
      *) echo "[release] unknown argument: $arg" >&2; exit 2 ;;
    esac
  done

  # --dry-run is honoured by the STAGE mode only, and the other three modes must
  # say so rather than accept it and mutate anyway. `run()` -- the thing that
  # actually implements dry-run -- is defined further down and is not in scope
  # for the docs modes, which dispatch above it; and finish_release takes no
  # DRY_RUN argument at all. So `--finish=N --dry-run` really polled, tagged and
  # pushed, and `--docs-worktree-done=<path> --dry-run` really removed the
  # worktree. Refusing the combination is both cheaper and less ambiguous than
  # implementing a no-op for steps whose entire content is the mutation.
  # The message names the FLAG the operator typed, not the internal mode name:
  # "docs-setup mode" appears in neither the usage text nor legion-release.md.
  if [ "$DRY_RUN" = "1" ] && [ "$MODE" != "stage" ]; then
    local DRY_RUN_CONFLICT
    case "$MODE" in
      finish)        DRY_RUN_CONFLICT="--finish= (it polls the merge queue, tags the release commit and pushes the tag)" ;;
      docs-setup)    DRY_RUN_CONFLICT="--docs-worktree (it creates a branch and a worktree, and its whole output is that worktree's path)" ;;
      docs-teardown) DRY_RUN_CONFLICT="--docs-worktree-done= (it removes a worktree and deletes a branch)" ;;
      *)             DRY_RUN_CONFLICT="${MODE}" ;;
    esac
    fail "--dry-run cannot be combined with ${DRY_RUN_CONFLICT}. There is no meaningful no-op for a step whose entire content is the mutation, and this combination used to be accepted and then ignored. Re-run without --dry-run. (--dry-run applies to the staging run: 'release.sh <X.Y.Z> --dry-run'.)"
  fi

  # -- cross-repo docs worktree modes (#845) ---------------------------------
  # Dispatched before the release guards on purpose: these run AFTER a release,
  # from a repo that is no longer in the pre-release state those guards assert.
  if [ "$MODE" = "docs-teardown" ]; then
    docs_worktree_teardown "$DOCS_WT_DONE"
    return 0
  fi
  if [ "$MODE" = "docs-setup" ]; then
    local DOCS_REPO DOCS_BRANCH DOCS_BASE DOCS_ROOT DOCS_VERSION
    DOCS_REPO="$(extract_cfg_default docs.repo '')"
    [ -n "$DOCS_REPO" ] || fail "docs step: release.toml has no [docs] repo -- this repo declares no cross-repo docs target"
    # STABLE branch name (#820): the next release extends the open docs PR
    # instead of stacking a version-pinned branch on top of the previous one.
    DOCS_BRANCH="$(extract_cfg_default docs.branch "docs/${WORK_REPO}-current")"
    DOCS_BASE="$(extract_cfg_default docs.base main)"
    DOCS_ROOT="$(legion watch list | watch_repo_root "$DOCS_REPO")" \
      || fail "docs step: '${DOCS_REPO}' is not registered in watch.toml -- cannot resolve its checkout"
    DOCS_VERSION="$(legion sym etc extract "$SOURCE_FILE" --field "$SOURCE_FIELD" 2>/dev/null || printf 'current')"
    docs_worktree_setup "$DOCS_ROOT" "$DOCS_BRANCH" "$DOCS_BASE" "${DOCS_REPO}-docs-${DOCS_VERSION}"
    return 0
  fi

  [ -n "$BUMP" ] || {
    echo "[release] usage: release.sh <patch|minor|major|X.Y.Z> [--dry-run] [--activate] [--no-preflight]" >&2
    echo "                 release.sh <X.Y.Z> --finish=<pr-number> [--activate]" >&2
    echo "                 release.sh --docs-worktree | --docs-worktree-done=<path>" >&2
    exit 2
  }

  # run: execute a mutating command, or just print it under --dry-run.
  run() {
    if [ "$DRY_RUN" = "1" ]; then
      printf '[dry-run] %s\n' "$*" >&2
    else
      "$@"
    fi
  }

  # -- finish mode: phases 7b-9 of an already-staged release -----------------
  # Split from the staging run because the step between them -- earning the
  # legion-simplify and legion-pr-write gates on the release commit and opening
  # the PR -- is an AGENT's job, not a script's: both gates are keyed to the
  # commit hash, and a clean verdict can only be recorded through their CHECK
  # validators. That is the point of #844, not a workaround for it: the release
  # commit passes the same gates every other change passes.
  if [ "$MODE" = "finish" ]; then
    local F_TAG F_TAG_MESSAGE
    [ -n "$PR_NUMBER" ] || fail "--finish requires the PR number (--finish=<pr-number>)"
    # Validated for the same reason $BUMP is: it is a bare operator-supplied
    # string that flows into a `git ls-remote` match and a `legion pr view`
    # argument. The realistic failure is not adversarial -- `--finish=#12` or a
    # pasted PR URL simply never matches the queue ref, so the release burns the
    # entire 30-minute budget and reports a timeout instead of a typo.
    [[ "$PR_NUMBER" =~ ^[0-9]+$ ]] \
      || fail "--finish takes a bare PR NUMBER, got '${PR_NUMBER}' (no '#', no URL): release.sh ${BUMP} --finish=123"
    is_semver "$BUMP" || fail "--finish requires the explicit release version (release.sh X.Y.Z --finish=N), got '${BUMP}'"
    F_TAG="$(render_tag "$TAG_FORMAT" "$BUMP")"
    F_TAG_MESSAGE="$(render_tag "$TAG_MESSAGE_FORMAT" "$BUMP")"
    finish_release "$REPO_ROOT" "$WORK_REPO" "$RELEASE_BRANCH" "$SOURCE_FILE_REL" \
      "$SOURCE_FIELD" "$BUMP" "$F_TAG" "$F_TAG_MESSAGE" "$PR_NUMBER" \
      "$MERGE_WAIT_POLLS" "$MERGE_WAIT_INTERVAL" "$ACTIVATE"
    return 0
  fi

  # -- 1. guards -------------------------------------------------------------
  local BRANCH LOCAL REMOTE DIRTY_OTHER
  BRANCH="$(git rev-parse --abbrev-ref HEAD)"
  [ "$BRANCH" = "$RELEASE_BRANCH" ] || fail "must be on ${RELEASE_BRANCH} to release (on '$BRANCH')"

  # The CHANGELOG entry is written-but-uncommitted at entry; everything else
  # must be clean. Allow only the configured changelog path to be dirty.
  DIRTY_OTHER="$(git status --porcelain --untracked-files=no | non_changelog_dirty "$CHANGELOG_REL")"
  [ -z "$DIRTY_OTHER" ] || fail "working tree has changes other than ${CHANGELOG_REL} -- commit or stash first:
$DIRTY_OTHER"

  git fetch origin "$RELEASE_BRANCH" --quiet || fail "git fetch failed"
  LOCAL="$(git rev-parse @)"
  REMOTE="$(git rev-parse '@{u}' 2>/dev/null || echo "")"
  [ -n "$REMOTE" ] || fail "no upstream for ${RELEASE_BRANCH}"
  [ "$LOCAL" = "$REMOTE" ] || fail "local ${RELEASE_BRANCH} is not in sync with origin/${RELEASE_BRANCH} -- pull/push first"

  # -- 2. compute + validate the new version ---------------------------------
  local CURRENT NEW TAG TAG_MESSAGE
  CURRENT="$(legion sym etc extract "$SOURCE_FILE" --field "$SOURCE_FIELD" 2>/dev/null || true)"
  [ -n "$CURRENT" ] || fail "could not read '${SOURCE_FIELD}' from ${SOURCE_FILE_REL}"
  is_semver "$CURRENT" || fail "${SOURCE_FILE_REL} version '$CURRENT' is not strict X.Y.Z"

  NEW="$(compute_new_version "$CURRENT" "$BUMP")"
  is_semver "$NEW" || fail "invalid version '$NEW' (expected X.Y.Z)"
  is_strictly_greater "$NEW" "$CURRENT" || fail "refusing non-increment $CURRENT -> $NEW"

  TAG="$(render_tag "$TAG_FORMAT" "$NEW")"
  TAG_MESSAGE="$(render_tag "$TAG_MESSAGE_FORMAT" "$NEW")"

  # Refuse a tag that already exists (a re-run after a partial failure, or a typo
  # colliding with history) before we mutate anything. Note this guard is
  # STAGING-only: `--finish` deliberately tolerates a pre-existing tag that
  # already points at the verified merged sha, because re-running just the tag
  # step is the documented recovery from a failed tag push.
  ! git rev-parse "$TAG" >/dev/null 2>&1 || fail "tag ${TAG} already exists"

  # The release commit is made on its own branch (#844), so a leftover one from
  # an abandoned attempt is a collision worth naming before anything mutates.
  local RELEASE_PR_BRANCH
  RELEASE_PR_BRANCH="release/${NEW}"
  ! git rev-parse --verify --quiet "refs/heads/${RELEASE_PR_BRANCH}" >/dev/null 2>&1 \
    || fail "branch ${RELEASE_PR_BRANCH} already exists -- an earlier attempt at this release did not finish. Inspect it, then delete it or run 'release.sh ${NEW} --finish=<pr-number>' if its PR is already open."

  info "releasing ${CURRENT} -> ${NEW}"

  # -- 3. require the CHANGELOG entry (human/agent-authored, never generated) -
  local TOP_HEADER
  TOP_HEADER="$(grep -E '^## [0-9]' "$CHANGELOG" | head -1 | sed -E 's/^## //' | awk '{print $1}')"
  if [ "$TOP_HEADER" != "$NEW" ]; then
    fail "${CHANGELOG_REL} top header is '## ${TOP_HEADER:-<none>}', expected '## ${NEW}'.
       Have the changelog agent (or you) add a '## ${NEW}' section at the top, then re-run."
  fi
  info "CHANGELOG header matches ${NEW}"

  # -- 4. preflight ----------------------------------------------------------
  if [ "$RUN_PREFLIGHT" = "1" ]; then
    local PREFLIGHT_JSON PREFLIGHT_CMDS
    PREFLIGHT_JSON="$(legion sym etc extract "$CONFIG" --field preflight.commands --json 2>/dev/null || printf '["bash scripts/preflight.sh"]')"
    PREFLIGHT_CMDS="$(printf '%s' "$PREFLIGHT_JSON" | python3 -c '
import json, sys
for c in json.load(sys.stdin):
    print(c)
')"
    info "running preflight: $(printf '%s' "$PREFLIGHT_CMDS" | tr '\n' ',' | sed 's/,$//')"
    while IFS= read -r cmd; do
      [ -n "$cmd" ] || continue
      run bash -c "$cmd"
    done <<<"$PREFLIGHT_CMDS"
  else
    info "preflight skipped (--no-preflight)"
  fi

  # -- 5. bump the version-of-record file + refresh Cargo.lock ---------------
  if [ "$DRY_RUN" = "1" ]; then
    printf '[dry-run] set %s %s -> %s\n' "$SOURCE_FILE_REL" "$CURRENT" "$NEW" >&2
  else
    bump_source_file "$SOURCE_FILE" "$SOURCE_FIELD" "$CURRENT" "$NEW" \
      || fail "unsupported version.file format '${SOURCE_FILE_REL}' (bump supports .toml and .json)"
  fi
  # Cargo.lock only exists (and only needs refreshing) for a Rust source file.
  if [ "${SOURCE_FILE_REL##*/}" = "Cargo.toml" ]; then
    run cargo build --quiet   # refreshes Cargo.lock's own version entry to ${NEW}
  fi

  # -- 6. sync manifests + commit --------------------------------------------
  # Run sync-version explicitly so the manifests are correct even if the
  # pre-commit hook is not installed in this clone (the #656 failure mode).
  run bash "${REPO_ROOT}/scripts/sync-version.sh"
  local ADD_FILES=("$SOURCE_FILE" "$CHANGELOG")
  if [ "${SOURCE_FILE_REL##*/}" = "Cargo.toml" ] && [ -f "${REPO_ROOT}/Cargo.lock" ]; then
    ADD_FILES+=("${REPO_ROOT}/Cargo.lock")
  fi
  local TARGETS_JSON TARGET_FILES_REL
  TARGETS_JSON="$(legion sym etc extract "$CONFIG" --field targets --json 2>/dev/null || printf '[]')"
  TARGET_FILES_REL="$(printf '%s' "$TARGETS_JSON" | python3 -c '
import json, sys
for t in json.load(sys.stdin):
    print(t["file"])
')"
  if [ -n "$TARGET_FILES_REL" ]; then
    while IFS= read -r t; do
      [ -n "$t" ] || continue
      ADD_FILES+=("${REPO_ROOT}/${t}")
    done <<<"$TARGET_FILES_REL"
  fi
  # The release commit is made on `release/<new>`, never on ${RELEASE_BRANCH}.
  # `git switch -c` carries the staged and unstaged changes across, so the
  # CHANGELOG entry written under the entry contract lands in this commit.
  run git switch -c "$RELEASE_PR_BRANCH"
  run git add "${ADD_FILES[@]}"
  run git commit -m "chore(release): ${NEW}

Co-Authored-By: ${CO_AUTHORED_BY}"

  # -- 7a. push the release branch through the audited path ------------------
  # `legion push` only -- there is no `git push` of a commit anywhere in this
  # script any more. The old `git push --atomic origin main <tag>` is what
  # #844 removed: it bypassed the pull-request, merge-queue and status-check
  # rules on the one commit that reaches every agent in the cluster.
  local FINISH_CMD
  FINISH_CMD="scripts/release.sh ${NEW} --finish=<pr-number>"
  if [ "$ACTIVATE" = "1" ]; then FINISH_CMD="${FINISH_CMD} --activate"; fi

  if [ "$DRY_RUN" = "1" ]; then
    printf '[dry-run] legion push --repo %s --branch %s\n' "$WORK_REPO" "$RELEASE_PR_BRANCH" >&2
    printf '[dry-run] then: simplify + pr-write gates, legion pr create, legion pr merge\n' >&2
    printf '[dry-run] then: %s\n' "$FINISH_CMD" >&2
    printf '[dry-run] which waits for the queue, verifies origin/%s carries %s, tags that sha as %s and pushes ONLY the tag\n' \
      "$RELEASE_BRANCH" "$NEW" "$TAG" >&2
    if [ "$ACTIVATE" = "1" ]; then
      activate_local "$REPO_ROOT" "$NEW" 1
    fi
    info "dry-run complete: ${CURRENT} -> ${NEW} (nothing was mutated, pushed or tagged)"
    return 0
  fi

  legion push --repo "$WORK_REPO" --branch "$RELEASE_PR_BRANCH" \
    || fail "'legion push' failed for ${RELEASE_PR_BRANCH} -- the release commit exists locally but is not on the remote. Fix the push, then re-run it; nothing has been tagged."

  # -- 7b. hand off to the gates, then come back via --finish ----------------
  # Deliberately NOT done here: the legion-simplify and legion-pr-write gates
  # are keyed to the release commit's hash and can only be earned through their
  # CHECK validators, which is an agent's job. That is exactly the point -- the
  # release commit passes the same gates every other change passes.
  info "release ${NEW} is STAGED, not shipped: ${RELEASE_PR_BRANCH} is pushed and nothing is tagged."
  info "next: run the legion-simplify and legion-pr-write gates on this branch, then"
  info "      legion pr create --repo ${WORK_REPO} --title 'chore(release): ${NEW}' --head ${RELEASE_PR_BRANCH}"
  info "      legion pr merge --repo ${WORK_REPO} --number <pr-number>"
  info "      ${FINISH_CMD}"
}

# finish_release <repo_root> <work_repo> <release_branch> <source_file_rel>
#   <field> <new> <tag> <tag_message> <pr_number> <max_polls> <interval> <activate>
#
# Phases 7b-9: wait for the merge queue, verify what actually landed, tag THAT
# commit, and push only the tag.
#
# The ordering is the whole point of #844 and is not rearrangeable:
#   1. wait  -- `legion pr merge` enqueues and returns; the merge is
#               asynchronous, so a bounded poll stands between enqueue and tag.
#   2. read  -- re-fetch origin/<branch>, confirm the release actually landed on
#               it, then resolve the RELEASE COMMIT within it. The queue may
#               squash or rebase, so the local branch tip is not the merged
#               commit; and by the time --finish runs, other PRs may have landed
#               on top, so the branch TIP is not the release commit either.
#   3. verify -- confirm the commit about to be tagged really carries <new>.
#   4. tag + push the TAG ONLY.
finish_release() {
  local REPO_ROOT="$1" WORK_REPO="$2" RELEASE_BRANCH="$3" SOURCE_FILE_REL="$4"
  local SOURCE_FIELD="$5" NEW="$6" TAG="$7" TAG_MESSAGE="$8" PR_NUMBER="$9"
  local MAX_POLLS="${10}" INTERVAL="${11}" ACTIVATE="${12}"
  local OUTCOME RC=0 TIP_SHA TIP_VERSION MERGED_SHA MERGED_VERSION EXISTING_TAG_SHA
  local BOUNDARY_LIST BOUNDARY_RC=0 TIP_VERSION_RC=0 MERGED_VERSION_RC=0
  # How far back the release-commit walk may go, counted in commits that TOUCH
  # the version file -- so this is "releases", not "commits", and 200 of them is
  # already far past any sane re-run.
  local MAX_BOUNDARY_WALK=200

  cd "$REPO_ROOT"

  # The three merge observers, as closures. Their arguments are bound as ARGV
  # here rather than interpolated into a command string for wait_for_release_merge
  # to eval: that is what keeps a PR number or a release.toml field from ever
  # being parsed as shell. They read the enclosing locals dynamically, so they
  # must stay defined here, inside the function whose scope they capture.
  release_landed_obs() { release_landed "$RELEASE_BRANCH" "$SOURCE_FILE_REL" "$SOURCE_FIELD" "$NEW"; }
  release_state_obs()  { release_pr_state "$WORK_REPO" "$PR_NUMBER"; }
  release_queue_obs()  { release_queue_ref_present "$PR_NUMBER"; }
  # The version-of-record at <sha>, for the release-commit walk below. Same
  # reader the landed-gate uses, so the walk cannot disagree with the gate.
  merged_version_at() { ref_version "$1" "$SOURCE_FILE_REL" "$SOURCE_FIELD"; }

  info "waiting for PR #${PR_NUMBER} to merge (up to ${MAX_POLLS} polls, ${INTERVAL}s apart)"
  OUTCOME="$(wait_for_release_merge "$MAX_POLLS" "$INTERVAL" \
    release_landed_obs release_state_obs release_queue_obs)" || RC=$?

  case "$OUTCOME" in
    merged) info "PR #${PR_NUMBER} merged" ;;
    ejected)
      fail "PR #${PR_NUMBER} was EJECTED from the merge queue -- it was in the queue, it is no longer, and origin/${RELEASE_BRANCH} does not carry ${NEW}. Nothing was tagged. Merge origin/${RELEASE_BRANCH} into the branch (never rebase -- 'legion push' has no force path), re-run the gates, push, re-enqueue, then re-run this --finish."
      ;;
    closed)
      fail "PR #${PR_NUMBER} is closed without ${NEW} reaching origin/${RELEASE_BRANCH}. Nothing was tagged."
      ;;
    timeout)
      fail "timed out after ${MAX_POLLS} polls (${INTERVAL}s apart) waiting for PR #${PR_NUMBER}. This is a failure to OBSERVE the merge, not evidence the release commit failed -- check PR #${PR_NUMBER} yourself, and re-run 'scripts/release.sh ${NEW} --finish=${PR_NUMBER}' once it has landed. Nothing was tagged."
      ;;
    *)
      fail "merge wait returned an unrecognized outcome '${OUTCOME}' (rc ${RC}). Nothing was tagged."
      ;;
  esac

  git fetch origin "$RELEASE_BRANCH" --quiet || fail "git fetch failed after the merge -- cannot resolve the sha to tag. Nothing was tagged."
  TIP_SHA="$(git rev-parse "origin/${RELEASE_BRANCH}")" \
    || fail "could not resolve origin/${RELEASE_BRANCH}. Nothing was tagged."

  # LANDED GATE. Fails CLOSED: an unreadable version is a mismatch, not a pass.
  # This asks of the branch TIP, and it is the right question to ask of the tip:
  # it proves the release is on the branch AND that no LATER release superseded
  # it. It is not, however, the sha to tag -- see below.
  # `|| TIP_VERSION=""` is required, not defensive: ref_version now returns rc 2
  # when the read fails, and a bare assignment carries that rc under `set -e`,
  # which would kill the run with no message at all. Empty flows into
  # tag_target_matches, which fails closed -- and the rc is kept so the operator
  # is told WHICH failure this is, since "the branch carries the wrong version"
  # and "the version could not be read" need different responses.
  TIP_VERSION="$(merged_version_at "origin/${RELEASE_BRANCH}")" || TIP_VERSION_RC=$?
  if [ "$TIP_VERSION_RC" -ne 0 ]; then
    fail "could not READ ${SOURCE_FILE_REL} at ${SOURCE_FIELD} on origin/${RELEASE_BRANCH} (${TIP_SHA}) -- the file may be absent on that ref, or 'legion sym etc extract' may have failed. This is not evidence the release did not land; it is a failure to observe. Nothing was tagged."
  fi
  tag_target_matches "$NEW" "$TIP_VERSION" \
    || fail "tag-target mismatch: origin/${RELEASE_BRANCH} is ${TIP_SHA}, whose ${SOURCE_FILE_REL} says '${TIP_VERSION:-<none>}', not ${NEW}. Refusing to tag a commit that is not the release. Nothing was tagged."

  # THE SHA TO TAG is the release commit, not the tip. Between `legion pr merge`
  # and this --finish, other PRs land; none of them touch the version file, so
  # the gate above still passes on a tip several commits past the release, and
  # tagging it would ship a tree that was never released. Stored ruling
  # 019e8440: tags sit on the chore(release) commit, not on later HEAD.
  #
  # This is also what makes the idempotency promised below (and in
  # plugin/commands/legion-release.md) TRUE rather than aspirational: the
  # boundary is a property of the history, so a second --finish run resolves the
  # same sha, matches the existing tag and re-pushes it, however far the branch
  # has moved on in between. Tip-resolution re-derived a DIFFERENT sha on every
  # re-run and hit the "tag already exists and points elsewhere" refusal.
  BOUNDARY_LIST="$(git rev-list "origin/${RELEASE_BRANCH}" -- "$SOURCE_FILE_REL")" \
    || fail "could not list the commits touching ${SOURCE_FILE_REL} on origin/${RELEASE_BRANCH} -- cannot resolve the release commit. Nothing was tagged."
  MERGED_SHA="$(printf '%s\n' "$BOUNDARY_LIST" \
    | release_boundary_commit merged_version_at "$NEW" "$MAX_BOUNDARY_WALK")" || BOUNDARY_RC=$?
  case "$BOUNDARY_RC" in
    0) ;;
    1) fail "could not find the ${NEW} release commit: no commit touching ${SOURCE_FILE_REL} on origin/${RELEASE_BRANCH} carries ${NEW}, even though its tip does. Nothing was tagged." ;;
    2) fail "could not find the ${NEW} release commit: walked ${MAX_BOUNDARY_WALK} commits touching ${SOURCE_FILE_REL} on origin/${RELEASE_BRANCH} and every one still read ${NEW}. Refusing to guess which commit is the release. Nothing was tagged." ;;
    3) fail "could not find the ${NEW} release commit: every commit that ever touched ${SOURCE_FILE_REL} on origin/${RELEASE_BRANCH} reads ${NEW}, so there is no boundary to tag. Nothing was tagged." ;;
    *) fail "release-commit resolution failed with rc ${BOUNDARY_RC}. Nothing was tagged." ;;
  esac
  [ -n "$MERGED_SHA" ] || fail "release-commit resolution returned an empty sha. Nothing was tagged."

  # Re-assert on the sha actually about to be tagged. The walk only ever keeps a
  # commit whose version matched, so this is redundant BY CONSTRUCTION -- which
  # is exactly why it is cheap to keep: it makes the tag step self-verifying no
  # matter how the sha was resolved, and the resolution above reaches it through
  # a dynamically-scoped closure that a later refactor could quietly repoint.
  MERGED_VERSION="$(merged_version_at "$MERGED_SHA")" || MERGED_VERSION_RC=$?
  if [ "$MERGED_VERSION_RC" -ne 0 ]; then
    fail "could not READ ${SOURCE_FILE_REL} at ${SOURCE_FIELD} on the resolved release commit ${MERGED_SHA} -- a failure to observe, not a mismatch. Nothing was tagged."
  fi
  tag_target_matches "$NEW" "$MERGED_VERSION" \
    || fail "tag-target mismatch: resolved the ${NEW} release commit as ${MERGED_SHA}, but its ${SOURCE_FILE_REL} says '${MERGED_VERSION:-<none>}'. Nothing was tagged."
  info "release commit for ${NEW} is ${MERGED_SHA} (origin/${RELEASE_BRANCH} tip is ${TIP_SHA})"

  # A tag left over from a failed push is the documented recovery path, so it is
  # reused when it already points at the verified sha and fatal otherwise.
  if EXISTING_TAG_SHA="$(git rev-parse --verify --quiet "refs/tags/${TAG}^{commit}" 2>/dev/null)" && [ -n "$EXISTING_TAG_SHA" ]; then
    [ "$EXISTING_TAG_SHA" = "$MERGED_SHA" ] \
      || fail "tag ${TAG} already exists and points at ${EXISTING_TAG_SHA}, not the merged commit ${MERGED_SHA}. Refusing to move it. Nothing was pushed."
    info "tag ${TAG} already exists on the merged commit -- pushing it"
  else
    git tag -a "$TAG" -m "$TAG_MESSAGE" "$MERGED_SHA" \
      || fail "release INCOMPLETE BUT RECOVERABLE: ${NEW} IS merged on origin/${RELEASE_BRANCH} as ${MERGED_SHA}, but creating ${TAG} failed, so release.yml has NOT fired and no binaries are building. Finish it with:
       git tag -a ${TAG} -m '${TAG_MESSAGE}' ${MERGED_SHA} && git push origin refs/tags/${TAG}"
  fi

  git push origin "refs/tags/${TAG}" \
    || fail "release INCOMPLETE BUT RECOVERABLE: ${NEW} IS merged on origin/${RELEASE_BRANCH} as ${MERGED_SHA} and ${TAG} exists locally on it, but pushing the tag failed, so release.yml has NOT fired and no binaries are building. Re-run only the tag step:
       git push origin refs/tags/${TAG}
       (or re-run 'scripts/release.sh ${NEW} --finish=${PR_NUMBER}', which is idempotent from here)"

  info "pushed ${TAG} -> ${MERGED_SHA} -- release.yml will build + publish the platform binaries"

  # Bring the local release branch up to what was just tagged, so an --activate
  # build (and the next release's in-sync guard) sees the merged tree.
  if git switch "$RELEASE_BRANCH" --quiet; then
    git merge --ff-only "origin/${RELEASE_BRANCH}" --quiet \
      || info "WARNING: local ${RELEASE_BRANCH} could not fast-forward to origin/${RELEASE_BRANCH} -- reconcile it before the next release"
  else
    info "WARNING: could not switch back to ${RELEASE_BRANCH} -- the tag is pushed and the release is complete, but reconcile the local checkout before the next one"
  fi

  if [ "$ACTIVATE" = "1" ]; then
    activate_local "$REPO_ROOT" "$NEW" 0
  fi

  info "done: ${NEW}"
}

# docs_worktree_setup <docs_root> <branch> <base_branch> <label>: create the
# isolated worktree the cross-repo docs step runs in and print ITS PATH on
# stdout -- the only thing on stdout, so the caller can capture it directly.
# The shared checkout path is never printed and never handed onward.
#
# Every failure arm below ends in `fail`. There is deliberately no fallback to
# the shared checkout: that fallback IS the defect this exists to close, and a
# docs step that silently degrades to the shared tree is worse than one that
# stops with a reason.
docs_worktree_setup() {
  local root="$1" branch="$2" base_branch="$3" label="$4"
  local holder start_point parent wt head

  [ -n "$root" ] || fail "docs step: no checkout path resolved for the docs repo"
  git -C "$root" rev-parse --git-dir >/dev/null 2>&1 \
    || fail "docs step: '${root}' is not a git checkout -- cannot create the docs worktree"
  git -C "$root" fetch origin --quiet \
    || fail "docs step: 'git fetch origin' failed in ${root} -- the worktree must start from a remote ref, so this is fatal"

  # Deregister worktrees whose directory no longer exists BEFORE asking who
  # holds the branch. A retained docs worktree that the operator simply deleted
  # stays registered as "prunable", and that registration is invisible to
  # `worktree_holding_branch` (the porcelain still lists it) -- so every later
  # --docs-worktree failed naming a path that no longer existed, with no hint
  # that `git worktree prune` was the way out. Pruning is safe precisely here:
  # it drops bookkeeping for directories that are already gone, and touches no
  # branch and no commit.
  git -C "$root" worktree prune >/dev/null 2>&1 || true

  # A stale worktree or branch from an earlier release is reported, never
  # reused and never clobbered: it may hold unpushed work.
  holder="$(git -C "$root" worktree list --porcelain | worktree_holding_branch "$branch")" || holder=""
  [ -z "$holder" ] || fail "docs step: ${branch} is already checked out at ${holder} -- it may hold unpushed work from an earlier release. Finish or remove it there; this run will not clobber it and will not fall back to ${root}."
  ! git -C "$root" show-ref --verify --quiet "refs/heads/${branch}" \
    || fail "docs step: local branch ${branch} exists but no worktree holds it -- it may hold unpushed work from an earlier release. Inspect it, and delete it only once you have confirmed it is on the remote:
       git -C ${root} log origin/${branch}..${branch}    # what is on it that origin does not have
       git -C ${root} branch -D ${branch}                # only if that is empty
       This run will not reuse or clobber it. (Deleting a branch does not touch the shared checkout's working tree.)"

  start_point="$(git -C "$root" ls-remote --heads origin | docs_start_point "$branch" "origin/${base_branch}")"

  parent="$(mktemp -d)" || fail "docs step: could not create a temp dir for the docs worktree"
  wt="${parent}/${label}"

  git -C "$root" worktree add --quiet -b "$branch" "$wt" "$start_point" \
    || fail "docs step: 'git worktree add -b ${branch} ${wt} ${start_point}' failed in ${root} -- not falling back to the shared checkout"

  # abandon_setup <message>: every failure AFTER the worktree exists unwinds the
  # whole of setup's own work -- worktree, branch and temp parent -- before it
  # reports. A half-built worktree left behind would hold the stable branch and
  # collide with the next release, which is the state this function refuses to
  # create in the first place. Nothing here can lose agent work: the docs agent
  # has not been handed the path yet.
  abandon_setup() {
    git -C "$wt" merge --abort >/dev/null 2>&1 || true
    git -C "$root" worktree remove --force "$wt" >/dev/null 2>&1 || true
    git -C "$root" branch -D "$branch" >/dev/null 2>&1 || true
    rm -rf "$parent"
    fail "$1"
  }

  # Extend arm (#820): the stable docs branch already exists on the remote, so
  # bring it up to the base rather than opening a second stacked docs PR. MERGE,
  # never rebase -- `legion push` has no force path, so rewritten history would
  # strand the branch.
  #
  # `-c merge.ff=false --no-ff` pins the merge against the OPERATOR's git config
  # rather than inheriting it. A global `[merge] ff = only` is an ordinary
  # setting, and under it this merge dies with "fatal: Not possible to
  # fast-forward" on entirely correct input -- a diverged docs branch is the
  # whole reason the merge exists, so it fires in the NORMAL case, not an exotic
  # one. Forcing a merge commit also keeps the shape stable, which is what
  # teardown's base-sha comparison assumes.
  if [ "$start_point" != "origin/${base_branch}" ]; then
    if ! git -C "$wt" -c merge.ff=false merge --no-ff --quiet "origin/${base_branch}" \
      -m "Merge origin/${base_branch} into ${branch}"; then
      # A real content conflict leaves unmerged paths in the index; a merge
      # refused by config or a hook leaves none. Same rc, different fix, so the
      # message must not guess -- and neither answer is "go and do it in the
      # shared checkout", which is the fallback #845 exists to close.
      if [ -n "$(git -C "$wt" ls-files -u 2>/dev/null)" ]; then
        abandon_setup "docs step: merging origin/${base_branch} into the existing ${branch} CONFLICTED (unmerged paths). This worktree has been removed and the shared checkout was not touched. Reconcile ${branch} with origin/${base_branch} in a worktree of your OWN ('git worktree add'), push it, then re-run --docs-worktree."
      fi
      abandon_setup "docs step: merging origin/${base_branch} into ${branch} FAILED WITHOUT CONFLICT (no unmerged paths) -- that is a merge refused by git config or a hook, not by content: typically 'merge.ff = only', 'user.useConfigOnly = true' with no identity, or a failing commit hook. Fix the config, then re-run --docs-worktree. Not falling back to the shared checkout."
    fi
  fi

  # The base sha is recorded AFTER any merge-in, so teardown measures what the
  # docs agent added rather than counting setup's own merge as agent work. It
  # lives in the temp parent, NOT inside the worktree, which would dirty the
  # very tree teardown inspects for cleanliness.
  head="$(git -C "$wt" rev-parse HEAD)" || abandon_setup "docs step: could not read HEAD of the new worktree at ${wt}"
  printf '%s\n' "$head" >"${parent}/base-sha" \
    || abandon_setup "docs step: could not record the base sha for ${wt}"
  # The OWNERSHIP marker, and it records the worktree PATH, not just the fact
  # that some marker exists. Teardown destroys three things -- the worktree, its
  # branch, and this directory -- and "the parent happens to contain a base-sha
  # file" is a claim about the directory, not about the worktree inside it. This
  # is what lets teardown answer "did I create THIS worktree", which is the
  # question a destructive verb actually has to answer.
  printf '%s\n' "$wt" >"${parent}/worktree-path" \
    || abandon_setup "docs step: could not record the ownership marker for ${wt}"

  info "docs worktree ready: ${wt} (branch ${branch}, from ${start_point})"
  printf '%s\n' "$wt"
}

# docs_worktree_teardown <worktree_path>: remove the docs worktree when this
# script created it AND it carries nothing; retain it (naming the path) in every
# other case. A failed or partial docs run is recoverable rather than discarded;
# a clean or fully-pushed one leaves no residue to collide with the next
# release's stable branch.
#
# OWNERSHIP IS CHECKED BEFORE ANYTHING IS DESTROYED, and it gates all three
# destructive acts -- `worktree remove --force`, `branch -D`, and `rm -rf` --
# not just the last one. `--docs-worktree-done=` takes an operator-supplied
# path, and force-removing a worktree and deleting its branch are exactly as
# destructive as deleting a directory. Nor is "does the tree look disposable"
# an ownership claim: without the marker the base sha is empty, so the
# `head = base` arm can never fire and disposability collapses onto `pushed`
# alone -- under which every clean, fully-pushed worktree in the docs repo,
# including one an agent is working in right now, reads as disposable.
docs_worktree_teardown() {
  local wt="$1" parent base head status branch root pushed=0

  [ -n "$wt" ] || fail "--docs-worktree-done requires the worktree path (--docs-worktree-done=<path>)"
  [ -d "$wt" ] || fail "docs step: no worktree at ${wt} -- nothing to tear down"
  git -C "$wt" rev-parse --git-dir >/dev/null 2>&1 \
    || fail "docs step: ${wt} is not a git worktree"

  parent="$(dirname "$wt")"
  base="$(cat "${parent}/base-sha" 2>/dev/null || true)"
  # The marker must exist AND name THIS worktree. String equality, deliberately:
  # an ownership check is not the place for tolerance, and a path that does not
  # match the one setup printed is a case for retain-and-report, which loses
  # nothing. (Comparing inodes instead would silently re-approve a directory
  # that was removed and recreated by something else in the meantime.)
  local wt_is_ours=0 marked
  marked="$(cat "${parent}/worktree-path" 2>/dev/null || true)"
  if [ -f "${parent}/base-sha" ] && [ -n "$marked" ] && [ "$marked" = "$wt" ]; then
    wt_is_ours=1
  fi
  head="$(git -C "$wt" rev-parse HEAD 2>/dev/null || true)"
  status="$(git -C "$wt" status --porcelain 2>/dev/null || printf 'unknown')"
  branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
  root="$(git -C "$wt" worktree list --porcelain 2>/dev/null | head -1)"
  root="${root#worktree }"
  [ -n "$root" ] || fail "docs step: could not resolve the main checkout backing ${wt}"

  # "Already on the remote" is the second disposal arm: a docs run that
  # committed AND pushed carries commits but loses nothing when removed.
  git -C "$wt" fetch origin "$branch" --quiet >/dev/null 2>&1 || true
  if [ -n "$branch" ] && git -C "$wt" merge-base --is-ancestor HEAD "origin/${branch}" >/dev/null 2>&1; then
    pushed=1
  fi

  if [ "$wt_is_ours" != "1" ]; then
    fail "docs step: refusing to tear down ${wt} -- this script cannot prove it created it. ${parent} has no 'worktree-path' marker naming that worktree, so it was made by hand or by another tool, and removing a worktree and deleting its branch are not acts to perform on someone else's checkout. Nothing was removed. If it really is disposable, remove it yourself:
       git -C <its main checkout> worktree remove ${wt}"
  fi

  if worktree_is_disposable "$head" "$base" "$status" "$pushed"; then
    git -C "$root" worktree remove --force "$wt" \
      || fail "docs step: could not remove the unchanged worktree at ${wt}"
    # Safe by construction: the branch is either untouched since setup or fully
    # present on origin, so the only thing dropped is setup's own local merge
    # commit, which the next release re-derives. Reported either way -- a
    # destructive act that silences its own outcome cannot be audited, and
    # `|| true` on a `branch -D` hides both the deletion and its failure.
    if git -C "$root" branch -D "$branch" >/dev/null 2>&1; then
      info "docs branch ${branch} deleted (it was fully on origin, or untouched since setup)"
    else
      info "docs branch ${branch} was NOT deleted -- it is checked out elsewhere or already gone. Harmless, but the next release will report it as a collision if it is still there."
    fi
    rm -rf "$parent"
    info "docs worktree removed -- ${branch} carried nothing that is not already on origin"
  else
    info "docs worktree RETAINED at ${wt} (branch ${branch}) -- it carries work that is not on origin. Finish or push it from there, then re-run: scripts/release.sh --docs-worktree-done=${wt}"
  fi
}

# activate_local <repo_root> <new> <dry_run>: build the release binary, install it
# to the plugin data dir atomically, and restart the daemon -- verifying both that
# the old one died and that the new one actually came back up on ${new}.
activate_local() {
  local REPO_ROOT="$1" NEW="$2" DRY_RUN="$3"
  local DATA_DIR DATA_BIN OLDPID HEALTH _i
  DATA_DIR="${CLAUDE_PLUGIN_DATA:-${HOME}/.claude/plugins/data/legion-legion}"
  DATA_BIN="${DATA_DIR}/legion"
  info "activating ${NEW} locally: build + install + daemon restart"

  if [ "$DRY_RUN" = "1" ]; then
    printf '[dry-run] cargo build --release\n' >&2
    printf '[dry-run] install target/release/legion -> %s (atomic)\n' "$DATA_BIN" >&2
    printf '[dry-run] restart legion daemon on port 3131 and verify /health\n' >&2
    return 0
  fi

  cargo build --release --quiet
  mkdir -p "$DATA_DIR"
  cp "${REPO_ROOT}/target/release/legion" "${DATA_BIN}.new"
  chmod +x "${DATA_BIN}.new"
  mv -f "${DATA_BIN}.new" "$DATA_BIN"
  info "installed $("$DATA_BIN" --version) to ${DATA_BIN}"

  # Stop the old daemon and CONFIRM it died before starting a new one -- a
  # replacement that races the old one loses the port bind and dies silently.
  if OLDPID="$(pgrep -f 'legion daemon --port 3131' | head -1)" && [ -n "$OLDPID" ]; then
    kill "$OLDPID" || true
    for _i in 1 2 3 4 5; do
      pgrep -f 'legion daemon --port 3131' >/dev/null 2>&1 || break
      sleep 1
    done
    if pgrep -f 'legion daemon --port 3131' >/dev/null 2>&1; then
      fail "old daemon (pid ${OLDPID}) did not exit -- refusing to start a competing daemon"
    fi
  fi

  nohup "$DATA_BIN" daemon --port 3131 >/tmp/legion-daemon.log 2>&1 &
  disown || true
  sleep 2

  # Verify the new daemon actually came up on the new version. An empty body
  # means it failed to bind/crashed -- do NOT report success on a dead daemon.
  HEALTH="$(curl -s http://127.0.0.1:3131/health 2>/dev/null || true)"
  case "$HEALTH" in
    "")          fail "daemon did not come back up (empty /health) -- see /tmp/legion-daemon.log" ;;
    *"${NEW}"*)  info "daemon restarted on ${NEW}: ${HEALTH}" ;;
    *)           fail "daemon /health does not report ${NEW}: ${HEALTH}" ;;
  esac
}

# Run main only when executed directly, so scripts/test-release.sh can source
# this file to unit-test the pure helpers without triggering a release.
if [ "${BASH_SOURCE[0]:-$0}" = "${0}" ]; then
  main "$@"
fi
