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

## 4. Ship it

Run `scripts/release.sh NEW` (pass the explicit version, plus `--activate` and/or
`--dry-run` if the operator gave them). The script reads `release.toml` for the
version source, changelog path, propagation targets, and tag format (`Cargo.toml`
/ `plugin/CHANGELOG.md` / `v{version}` for legion itself), runs preflight (fmt,
clippy, test, SCIP regen), bumps the version file, refreshes `Cargo.lock`, syncs
the manifests, commits `chore(release): NEW`, tags, and pushes -- which fires the
release CI that builds and publishes the platform binaries. The script validates
that the CHANGELOG header the agent wrote matches the bumped version; a mismatch
aborts the release, which is the safety net.

If `--dry-run` was passed, stop here and report what would have happened.

## 5. Docs review on shingle

Resolve the shingle repo path from watch.toml -- `legion watch list` maps `shingle`
to its working directory; the runlegion.dev docs live under
`<shingle>/sites/runlegion.dev/src/pages/docs/` (do not hardcode an absolute path,
it differs per machine). Spawn `writer-legion` (Task, subagent_type `writer-legion`)
with the new changelog entry. Brief it: review the release against the docs pages
under that dir (concepts, architecture, cli-reference, getting-started, plugin-guide
at time of writing -- read the directory rather than trusting this list to stay
complete). If the
release changes user-facing behavior -- a new/changed command, verb, flag, config
knob, or concept -- update the affected pages on a `docs/legion-NEW` branch in the
shingle repo, in the writer-legion voice. If nothing user-facing changed (internal
refactor, test-only, CI), it should say so and write nothing -- not every release
needs docs.

If writer-legion produced doc changes, open the shingle PR for them through the
normal gate flow (issue -> branch -> simplify gate -> pr-write -> `legion pr create
--repo shingle`), exactly as a runlegion.dev docs PR is opened. Report the PR URL.

## 6. Report

Summarize: the version shipped, the tag, the release CI status, whether docs were
updated (and the PR if so), and -- if `--activate` was used -- the local daemon's
new version from `/health`. Note any follow-up (e.g. "verify the GitHub Release
published" if the CI build had not finished).
