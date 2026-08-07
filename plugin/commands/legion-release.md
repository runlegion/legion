---
description: Cut a legion release -- changelog agent writes the entry, scripts/release.sh ships it, writer-legion updates runlegion.dev docs if the change warrants it
argument-hint: "<patch|minor|major|X.Y.Z> [--activate] [--dry-run]"
allowed-tools: ["Bash", "Task", "Read", "Edit"]
---

# /legion-release -- orchestrated release

You are cutting a legion release. The mechanical work lives in `scripts/release.sh`;
the prose work is delegated to agents. Your job is to run the pipeline in order and
stop on the first failure. Argument: the bump level (`patch`/`minor`/`major`) or an
explicit `X.Y.Z`, plus optional `--activate` (build+install the binary and restart
the local daemon) and `--dry-run`.

## 1. Preconditions

Confirm you are on `main`, the tree is clean, and local `main` is in sync with
`origin/main`. If not, stop and tell the operator -- do not release from a dirty or
behind tree. (`scripts/release.sh` re-checks these, but fail early and clearly.)

## 2. Compute the target version

Read the current version from `Cargo.toml`. From the bump argument, compute the
target `NEW` (patch/minor/major arithmetic, or use the explicit `X.Y.Z`). State
`CURRENT -> NEW` to the operator before proceeding.

## 3. Changelog agent writes the entry

Spawn the `changelog` agent (Task, subagent_type `changelog`) with the target
version `NEW`. It reads `release.toml`'s `changelog.path` (`plugin/CHANGELOG.md`
for legion; see #741) to find the file, diffs `<prevtag>..HEAD`, reads the merged
PRs and their issues, and prepends a `## NEW` section in the repo's voice. When
it returns, `Read` that same file and sanity-check the top entry yourself: right
version, real prose, every bullet cited, correct release-type rationale. If it is
thin or wrong, send the agent back with specifics. Do NOT hand-write the entry
yourself -- that is the agent's job; you are the editor.

## 4. Ship it -- through the queue, like every other change

The release commit is not special and no longer behaves as if it were. Until
#844 this step was one `scripts/release.sh NEW` invocation that committed,
tagged and pushed straight to `main`, bypassing the pull-request rule, the merge
queue and seven required status checks on the one commit that reaches every
agent in the cluster (the marketplace sets `autoUpdate: true`). It now runs as a
chain, and **only the tag is ever pushed to `main`**.

**4a. Stage.** Run `scripts/release.sh NEW` (explicit version, plus `--activate`
and/or `--dry-run` if the operator gave them). It reads `release.toml` for the
version source, changelog path, propagation targets, work-source repo name and
tag format, runs preflight (fmt, clippy, test, SCIP regen), bumps the version
file, refreshes `Cargo.lock`, syncs the manifests, commits `chore(release): NEW`
**onto `release/NEW`**, and pushes that branch with `legion push`. It validates
that the CHANGELOG header the agent wrote matches the bumped version; a mismatch
aborts before anything is committed. Nothing is tagged yet, and the script says
so.

If `--dry-run` was passed, stop here and report what would have happened.

**4b. Earn the gates.** On `release/NEW`, run `/legion:legion-simplify` and then
`/legion:legion-pr-write`. This is not ceremony to work around a tool -- it is
the point of the change. Both gates are keyed to the release commit's hash and
`legion pr create` refuses without a clean row for each, which is exactly what
every feature branch faces. Do not reach for `--skip-gates`.

**4c. Open and enqueue.** `legion pr create --repo legion --title "chore(release):
NEW" --head release/NEW --body-file <the body you validated>`, then `legion pr
merge --repo legion --number <N>`. On a queue-enabled base this enqueues and
returns immediately; the merge is asynchronous.

**4d. Tag the release commit.** Run `scripts/release.sh NEW --finish=<N>`
(re-pass `--activate` if the operator asked for it). It polls until the merge
lands, then does two separate things that are easy to run together and must not
be:

1. **Re-reads `origin/main` as the landed gate.** This proves the release
   actually reached the branch *and* that no later release superseded it. It is
   the right question to ask of the tip -- and it is not the sha to tag.
2. **Resolves the release commit, and tags that.** Between `legion pr merge` and
   `--finish`, unrelated PRs land on `main`. None of them touch the version file,
   so the "does this tree carry `NEW`" check still passes several commits past
   the release, and tagging the tip would ship a tree nobody released. The script
   therefore walks the commits that *touch the version file*, newest first, and
   takes the last one still reading `NEW` -- the commit before it reads the
   previous version, which is what makes it the boundary. The queue may squash or
   rebase, so this is derived from history, never assumed from the branch tip.

It prints both shas (`release commit for NEW is <sha> (origin/main tip is
<other>)`); two different shas is the normal case, not a warning. Then it pushes
**only the tag**, which fires the release CI.

Resolving the boundary from history rather than from the tip is also what makes
re-running `--finish` idempotent: a second run derives the *same* sha however far
`main` has moved on, matches the tag that already exists on it, and re-pushes it.
Tip-resolution derived a different sha every time and died on "tag already exists
and points at <other>".

Four failures this step reports rather than papers over, all of which leave
nothing tagged:

- **Ejected from the queue.** It happens (it happened in the 0.25.0 batch).
  Merge `origin/main` into the branch -- never rebase, `legion push` has no force
  path -- re-run the gates, push, re-enqueue, then re-run `--finish`.
- **Timeout.** A failure to *observe* the merge, not evidence the release commit
  failed. Check the PR, then re-run `--finish` once it has landed.
- **The version could not be read.** Distinct from "the branch carries the wrong
  version", and reported as such: the file may be absent on the ref, or `legion
  sym etc extract` may have failed. Also a failure to observe -- it never counts
  as evidence that the release did not land, because a poll loop that treats an
  unreadable version as a definite "no" reports an *ejection* for a release that
  merged fine.
- **Tagging failed after a successful merge.** Reported as INCOMPLETE BUT
  RECOVERABLE, naming the merged sha. The version bump is already on `main`;
  re-running `--finish` completes it and is idempotent from that point.

## 5. Docs review on shingle -- in a worktree the script gives you

**Do not resolve shingle's checkout path and do not send an agent into it.** That
repo has its own agent who may be working in that tree right now, and you cannot
see them. Cutting v0.25.0 swept another agent's uncommitted `cli-reference.mdx`
edits into the docs commit through the shared working tree -- and note that a
`git add <file>` *by name*, not `-A`, still took their whole file.

Run `scripts/release.sh --docs-worktree`. It resolves shingle from `watch.toml`
itself, creates an isolated worktree off a **remote** ref on the stable docs
branch (`docs/legion-current`), and prints that worktree's path -- the only thing
on stdout. Hand `writer-legion` **that path and only that path**. If it fails, it
fails named (a stale worktree or branch from an earlier release is reported, not
clobbered) and the docs step stops; there is no fallback to the shared checkout,
because that fallback is the defect.

The branch is stable on purpose (#820). Docs describe the CURRENT state, not
per-version history, so each release EXTENDS the one open runlegion.dev docs PR
rather than stacking a `docs/legion-<ver>` branch per release -- five of those
stacked and conflict-cascaded across 0.20 through 0.24. When the branch already
exists on the remote, the script starts from it and merges `origin/main` in.

Spawn `writer-legion` (Task, subagent_type `writer-legion`) with the new
changelog entry and the worktree path. Brief it: the runlegion.dev docs live
under `<worktree>/sites/runlegion.dev/src/pages/docs/` -- read that directory
rather than trusting any list of pages to stay complete. If the release changes
user-facing behavior -- a new/changed command, verb, flag, config knob, or
concept -- update the affected pages there, in the writer-legion voice, describing
the current state rather than framing it as "new in X.Y". If nothing user-facing
changed (internal refactor, test-only, CI), it should say so and write nothing --
not every release needs docs.

If writer-legion produced doc changes, take them through shingle's normal gate
flow from inside the worktree (issue -> simplify gate -> pr-write -> `legion pr
create --repo shingle`), or extend the open docs PR if one is already on that
branch. `legion push` resolves the checkout holding the branch itself, so it
works from the worktree unchanged. Report the PR URL.

When the docs step is done, run `scripts/release.sh --docs-worktree-done=<path>`.
It removes the worktree when nothing would be lost -- unchanged, or already
pushed -- and RETAINS it, naming the path, when it carries work that is not on
the remote, so a failed or partial docs run is recoverable rather than discarded.

A docs step that fails does not fail the release: the tag is pushed and the
binaries are building by then. Report it as an incomplete follow-up naming the
worktree path so it can be resumed.

## 6. Report

Summarize: the version shipped, the release PR number and the sha the queue
actually produced, the tag and what it points at, the release CI status, whether
docs were updated (and the PR if so), and -- if `--activate` was used -- the local
daemon's new version from `/health`. Note any follow-up (e.g. "verify the GitHub
Release published" if the CI build had not finished, or a retained docs worktree
path if the docs step did not finish).
