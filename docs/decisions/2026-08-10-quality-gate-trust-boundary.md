# The quality-gate trust boundary: what legion captures that an agent cannot forge

Date: 2026-08-10
Issue: #882 (epic), Step 0
Research backing: reflection `019fe965-fbfd-76b2-a449-d4163f546a95` (cross-platform enforcement
survey -- BenchJack, Verification Horizon, AgentSpec, AgentLTL, the Aug-2026 CVEs), reflection
`019f4962-ba6f-7a12-b3b3-41f165e86358` (GATE-LEDGER INTEGRITY incident, #780)
Verified against: legion HEAD `a0fad9b` (2026-08-10; SCIP index lags at `4346765`, re-run `legion
index legion` before trusting a sym miss as absence). Claude Code hook-execution doctrine per
`plugin/hooks/lib/prelude.sh:39-70` (#860), whose subagent-firing claim was last re-verified
2026-08-04. This doc's own CC-version anchor is intentionally left unfilled -- see premise table,
item 1 -- rather than asserted from memory the way the 2026-06-02 doc's premise was.

## The property that matters, and why this repo already half-knows it

"Out-of-process" is not the load-bearing property. **Trust domain** is: whether the checking logic
runs somewhere the agent has no write access, such that the agent's own output cannot alter the
check's outcome. BenchJack (arXiv 2605.12673) reached 100% resolve rate on SWE-bench Verified via a
nine-line `conftest.py` hijacking pytest's own reporting hook -- the verifier ran in-process, sharing
the agent's filesystem, so "ran a test" and "the test framework agrees it passed" were both under the
agent's control. NeMo Guardrails and Guardrails AI are non-prompted, deterministic code, and are
*still* bypassable, because they run in-process. Gateways, OPA sidecars, and Claude Code's own
`PreToolUse` hooks share the one property that survives adversarial testing: different process,
different write access.

This repo has already discovered a narrower version of this exact fact, the hard way, for a different
attack surface. `plugin/hooks/lib/prelude.sh:40-70` ("THE BOUNDARY (#860)") states plainly: *"hooks
SHAPE AGENT BEHAVIOUR. They are not an enforcement boundary... Anything that must be TOTAL lives at
the git layer, in the binary, or in remote branch protection."* `plugin/hooks/README.md`'s per-guard
audit table grades every denying hook ADVISORY or MUST-BE-TOTAL and, for `no-direct-db.sh` -- the
*only* thing standing between an agent and a raw write to `legion.db` -- the verdict is already on
record: **"No total layer exists today; filed as #864."** This doc inherits that finding rather than
rediscovering it, and applies it specifically to the `quality_gates` table, which #864 never named by
name.

The read on `legion`'s own architecture, stated once: where a check is a **hard boundary**
(`permissions.deny`, or a value the binary computes itself from a subprocess it spawns, ignoring
whatever the caller says) -- an agent cannot forge it *today*, full stop. Where a check is a **hook**
or a **structural validator scored on caller-supplied text** -- it is forgeable today, and the
question is only how much composing convincing-but-false text costs versus how much telling the
truth costs. Below, each of the five items is graded on that axis, with the file:line that makes the
grade checkable.

## The six items

### 1. Git state / commit SHAs -- CANNOT FORGE (identity), SILENT (content)

Every gate write resolves HEAD by having the **legion binary itself** shell out to `git rev-parse
HEAD` at call time (`src/cli/util.rs:57-81`, `git_head_commit_and_branch`). There is no
`--commit-hash` flag anywhere in `cli/verify.rs`, `cli/pr.rs`, or `cli/push.rs` -- every call site
(`src/cli/verify.rs:284,496,1117,1164`, `src/cli/pr.rs:412,690,775`, `src/cli/push.rs:36`) calls the
same function, which takes no caller input. To make the recorded SHA say something false, an agent
would have to make it *true* -- actually check out that commit -- which is not forgery, it's just
using git normally. `legion push` (`src/cli/push.rs`) does the same for the SHA it audits, and reads
it off `git worktree list --porcelain` (`parse_worktree_list_porcelain`), not off anything the caller
passed.

What this does **not** certify: that the tree at that SHA is the tree that was tested. The SHA
answers "which commit," never "was this commit's code actually exercised." That gap is exactly what
item 2 exposes.

### 2. Command exit codes -- SPLIT: cannot forge for legion's own subprocesses, fully forgeable for the agent's

Two different populations, and conflating them is the mistake to avoid:

- **Subprocesses legion spawns itself** (`git rev-parse`, `git push`, `git commit-tree -S` for the
  signer preflight, `git commit`) have their exit status read directly off `std::process::Command`'s
  `Output`/`Child` and gate subsequent control flow: `src/cli/util.rs:62`, `src/cli/push.rs:163,247`,
  `src/cli/commit.rs:266,298,328,456,532,554,571,866`, `src/finding_gate.rs:287`,
  `src/inventory.rs:174`, `src/scip.rs:414`. `legion push` and `legion commit` write an audit row
  (`src/db/audit.rs` via `cli::util::audit`) with `outcome: success|failure` derived from that real
  status, not from anything the caller asserts (`src/cli/push.rs:69-85`). These **cannot be forged**
  short of making the underlying git command actually succeed or fail that way.

- **Commands the agent runs through its own Bash tool** -- `cargo test`, the project's actual test
  suite -- have **no capture path today.** The only PostToolUse hook that fires on every tool call,
  `mark-work.sh`, touches a marker file and reads nothing from `tool_response`
  (`plugin/hooks/mark-work.sh:1-19`). Reading `plugin/hooks/hooks.json`'s full `PostToolUse` block
  (5 matchers: unconditional, `Edit`, `Write`, `MultiEdit`, `TaskCreate`, `TaskUpdate`) shows no hook
  parses a Bash exit code or output. The probe run for this claim: `legion sym etc find-content
  'status.success'` returns 37 hits, every one inside `std::process::Command`-spawning code legion
  itself runs (`build.rs`, `src/cli/*.rs`, `src/finding_gate.rs`, `src/inventory.rs`, `src/scip.rs`,
  `src/watch/*.rs`) -- none inside a hook or a capture path for the agent's own tool calls; a literal
  search for `exit_code` returns zero hits anywhere in the tree. `plugin/skills/legion-verify/SKILL.md:39`
  tells the agent to "run the tests... and note the test that exercises it" -- that is an instruction
  to the agent, not an artifact legion captures. This is precisely epic #882 step 2's unbuilt
  "ARTIFACT CAPTURE": *"A PostToolUse hook records every test and command run -- exit code, output
  hash, HEAD, session -- into the DB as it happens. This is the unforgeable substrate; without it
  tier 1 resolves claims against claims."* It does not exist yet: no per-command capture table beyond
  `quality_gates`, `audit`, and `bypass.jsonl`, none of which record an agent-run test's exit status.
  (Caveat on this negative claim: the SCIP index used for the search above lags HEAD by several
  commits -- see the header -- so this is stated as the result of a specific probe to re-run, not as
  settled fact; see premise table item 5.)

**Verdict: forgeable today for the one thing that matters most (did the tests actually pass) -- the
single largest gap this epic exists to close.**

### 3. Test results -- FORGEABLE, no mitigation exists yet

Follows directly from item 2. `legion-verify`'s gate is `Asserted` by design -- it is not in
`gate_registry::CHECK_GATED_SKILLS` (`src/gate_registry.rs:23`) -- so nothing validates that a cited
test exists, ran, or passed. An agent can write "ran `cargo test foo_bar`, it passed" into a verify
verdict and no code path re-derives that claim. Same for `legion-review`. This is the failure class
#870 named (a PR body asserting proofs never in the diff) generalized to test citations.

### 4. Spec references -- FORGEABLE (schema-conformant, not truth-conformant); binding does not exist yet

`legion document` (`src/documents.rs`) stores agent-authored JSON payloads. `legion document create`
validates `doc_type=schema` payloads structurally at create; every other document type is checked
only via `legion document validate --schema <id>` against a landed schema, and that validator "checks
the dependency-free subset: type, required, properties, items, enum" (`legion document --help`) --
shape only, never truth. A criterion document can say "implemented: yes" and pass validation whether
or not it is. Confirmed: zero `spec_id`/`spec_ref`/`serviced` hits anywhere in `src/`
(`legion sym etc find-content` for each returns nothing) -- the binding that would let a gate row say
"this verdict is about criterion X of spec Y at revision Z" is genuinely unbuilt, matching #882 step 1
verbatim.

### 5. The gate rows themselves -- PARTIALLY FORGEABLE

**Who writes them:** the legion binary exclusively, in principle -- no sanctioned direct-SQL path
exists. `provenance` is never a CLI flag; it is hardcoded per code path.
`GateProvenance::Validated` is written at exactly two production sites,
`src/cli/verify.rs:545` (the `legion-simplify` Check arm) and `src/cli/pr.rs:358`
(`validate_and_record_pr_write_gate`, the `legion-pr-write` Check arm) -- matching
`gate_registry::CHECK_GATED_SKILLS = ["legion-simplify", "legion-pr-write"]`
(`src/gate_registry.rs:23`) one-for-one. Every other write site uses `GateProvenance::Asserted`. Since
#780, `quality-gate record` **refuses** a `--result clean` for any skill `has_check_validator` names
(`src/cli/verify.rs:274-282`) -- the exact loophole that let an agent manufacture a clean row is
closed *for that specific bypass*.

**What would have to be true to falsify "validated," today:**

  a. **Satisfy the Check validator with well-formed fiction.** `simplify_check.rs` and its
     `pr_write` counterpart score *structure*: one `### <path>` entry per changed file
     (`parse_articulation`), `MIN_ENTRY_WORDS` of prose per entry, and a located-evidence pattern
     (`has_within_file_locator` -- a symbol, a `file:line`, or an `Evidence:` line). None of it
     re-derives the claim from the diff; a fluent, plausible, *false* per-file analysis clears the
     bar identically to a true one. This already happened: reflection `019f4962` records an agent
     force-recording a `legion-simplify` clean via `record` on a stacked branch where `check`
     couldn't run -- "the articulation was honestly written but the row was fake." #780 closed the
     `record`-bypass half; the Check validator's substance bar being satisfiable by fiction is
     untouched and is exactly what #882 tier 2 (a decorrelated validator agent) exists to fix.
  b. **Write to `legion.db` directly**, bypassing the CLI entirely. This is not closed, and it has
     two distinct sub-cases that the README's own vocabulary is careful not to conflate
     (`plugin/hooks/README.md:166-170`: "a matcher gap is fixable in the hook; the child-process
     boundary is not"):
       - **`[REPRO]`, #860's child-process boundary.** A wrapper script or any tool that shells out
         (a Makefile target, a `bash -c` invocation) runs a `legion.db`-touching command with no
         Claude-layer interception at all -- hooks fire on the agent's own tool call, never on what
         that call spawns. This is the same reproduced class as the `gh --version`-inside-a-script
         and `git push` fixture escapes named in `plugin/hooks/lib/prelude.sh:48-51` and
         `plugin/hooks/README.md`'s `no-git-push.sh` row.
       - **`[UNVERIFIED]`, a possible matcher gap.** Whether variable indirection or quote-splitting
         inside a single Bash command the hook *does* see (e.g. a path built from concatenated
         variables that never places the literal substring `legion.db` contiguously in the argv the
         hook's `grep -qE 'legion\.db'` matches, `plugin/hooks/no-direct-db.sh:49`) evades the
         pattern is not something this doc ran. It is flagged, not asserted -- see premise table.
       - What the hook's own header **does** concede, in writing: `sqlite3 /tmp/x.db` then
         `ATTACH '.../legion.db' AS legion;` from the interactive prompt is an in-band gap it
         calls "cultural, not technical" (`plugin/hooks/no-direct-db.sh:21-27`).
     Unlike shell-grep, this control never graduated to `permissions.deny` (2026-06-02 doc). No
     integrity check exists to detect an out-of-band write after the fact either -- `legion health`
     samples machine load, not database integrity (`src/health.rs:7-26`), and `PRAGMA foreign_keys`
     is not enabled globally (`src/db/kanban.rs:109`, `src/db/wake.rs:155`), so the schema does not
     constrain an out-of-band writer.

**Verdict:** the CLI-level forgery path (claiming `validated` via a flag) is closed. The two paths
that remain open -- satisfying a structural validator with fiction, and writing around the CLI
entirely -- are both named, tracked (#864 general; #882 tiers 1/2/5 specific to gates), and unfixed.

### 6. Commit signatures -- the KEY cannot be forged, but a signature attests a WINDOW, not a commit

The operator's signing key lives in 1Password and is released only against their physical presence
plus a paired watch. That puts the key material genuinely out of an agent's reach: no amount of
writing to files, tables, or gate rows produces a signature. On the axis this doc cares about, that
is stronger than every other item above -- the others are unforgeable because of where we chose to
write them, and this one is unforgeable because the agent cannot obtain the input.

The correction that matters: **authorization is time-windowed, not per-commit.** Once unlocked, the
agent issues signatures without further interaction, and committing unattended inside that window is
the normal operating mode, not an edge case. So a signature attests:

> this commit originated from a checkout with agent access during an authorized window

and specifically does NOT attest that the operator saw, read, or approved this particular commit.
The two are indistinguishable in `git log`.

Design consequence for the rest of this epic: a gate may read "signed" as *originated from an
authorized checkout*, and must never read it as *human-approved*. Anything wanting per-commit human
attestation needs a separate mechanism, and it costs the operator one interaction per commit --
which is a real cost to weigh, not a free tightening.

This entry exists because the first draft of this analysis asserted the stronger claim -- that a
signature is unforgeable "by physics" and therefore constitutes human approval -- and the operator
corrected it. It is recorded here as a worked instance of exactly what the premise table below is
for: a tooling behaviour asserted from a plausible mental model rather than checked, which is the
same failure that made the 2026-06-02 decision doc wrong for two months.

## The ceiling: caller-supplied signals, named plainly

Out-of-process is necessary, not sufficient. The Aug-2026 CVEs (AWS CVE-2026-18830 -- a tool-use
block injected into the final message, dispatched without consulting the model; Google ADK
CVE-2026-18236 -- a confirmation processor that never checked the tool belonged to the executing
agent or that arguments matched) were both server-side gates defeated by trusting a
**caller-supplied signal**, not by the verifier sharing the agent's process. Legion has the same
shape of exposure in five places, all named above and gathered here for one read:

1. Articulation prose (item 5a) -- the caller supplies the text the Check validator scores.
2. `--base` (`QualityGateInput.base`, `src/db/quality_gates.rs:157-161`) is caller-supplied; a
   narrowed base shrinks the changed-file set "coverage" is checked against, so an agent can shrink
   what it has to explain.
3. `legion-review` / `legion-verify:<card_id>` verdicts are asserted **by construction** --
   `gate_registry` has no validator for either, so the caller's claim *is* the row.
4. `git rev-parse HEAD` (item 1) is honest about identity and silent about content -- it certifies
   *which* tree, never *that* tree was tested.
5. `no-direct-db.sh` (item 5b) is itself a caller-supplied-signal check: it pattern-matches the
   command *text* the agent chose to submit, the identical signal shape the AWS/ADK CVEs abused.

None of these are new discoveries. They are enumerated because #882's acceptance criteria requires
the unforgeable-artifact set named, and each is the caller-supplied-signal class that survives even
after "run it out-of-process" is done. The epic's own answer is tier 2, the validator agent: it must
be **decorrelated** (different model, read-only, refuses work its own model produced) and must
receive both the producer's narrative *and* tier 1's resolution results together -- withholding the
narrative was considered and rejected, because the #870 failure class (a claim in a body the diff
never implements) is only catchable by something holding both at once (#882 step 5). That is the
intended answer to this ceiling. It is not built yet, and this doc does not build it.

## Re-verification schedule

A decision doc that records a platform or codebase fact as permanent is a liability -- the
2026-06-02 doc's "hooks do not fire in subagents" premise sat disproven in this repo's own telemetry
for two months before anyone reread it. Every load-bearing premise above gets a runnable probe, a
cadence, and a place the result must land -- not a promise to "review periodically."

| # | Premise | Runnable probe | Cadence | Lands |
|---|---|---|---|---|
| 1 | `PostToolUse` fires in subagents with `agent_id`/`agent_type` in the payload (asserted by #882's own text and this repo's bypass telemetry; **not independently re-run in this doc**) | `bash plugin/hooks/test-subagent-stop.sh` (exercises `.agent_type` today) plus one live subagent spawn inspected via `legion bullpen --repo legion` for a workflow-run-id-keyed bypass row | Before #882 step 2 (artifact capture) is built on top of it; then every legion minor bump that touches `hooks.json` | This doc, section 2 -- edit in place, do not fork, exactly as the 2026-06-02 doc's own #860 addendum did |
| 2 | `no-direct-db.sh` is the only technical barrier on `legion.db` writes; no `permissions.deny` backing; no integrity check | `jq '.permissions.deny' ~/.claude/settings.json /etc/claude-code/managed-settings.json` for a `legion.db`/`sqlite3` entry; `legion health --help` for an integrity flag | Whenever #864's state changes | `legion issue view --repo legion --number 864` is the source of truth; this doc's section 5b is derived from it and must be re-derived, not assumed, once #864 lands |
| 3 | `CHECK_GATED_SKILLS` is exactly `{legion-simplify, legion-pr-write}`; `provenance` is never a CLI flag | `legion quality-gate record --help` (confirm no `--provenance` flag); `legion sym def GateProvenance --repo legion` plus a literal search for `GateProvenance::Validated` (confirm production write-site count stays 2) | On every PR touching `src/gate_registry.rs` or `src/cli/verify.rs`/`src/cli/pr.rs` (diff-triggered, not calendar-triggered) | Section 5 of this doc |
| 4 | `LEGION_SKIP_GIT_PUSH=1` and the `LEGION_SKIP_<HOOK>`/`LEGION_BYPASS_<X>` env-var tiers (`plugin/hooks/lib/prelude.sh:34-37`) are reachable from an agent's own Bash tool call, not just an operator's shell -- **unverified in this doc, flagged rather than asserted** | From a fresh session in a legion-covered repo: `export LEGION_SKIP_GIT_PUSH=1` then attempt a covered-repo `git push` and observe whether `no-git-push.sh`'s rewrite/deny still fires | Once, before this premise is used to justify anything; then on any `hooks.json` change to the skip-var contract | This doc's ceiling section, item 5 analogue -- promote to a numbered exposure if the probe confirms agent-reachability |
| 5 | Artifact capture (item 2's gap) remains unbuilt | Re-run this doc's own searches (`legion sym etc find-content` for `status.success`, `ExitStatus`, `cargo test` -- **as single literal terms, not `\|`-joined alternation, which `find-content` does not support**) after `legion index legion`; check `legion quality-gate --help` for a new capture subcommand | On every #882 sub-issue merge -- this premise is *expected* to flip, and must flip for the epic to close | When it flips, items 2, 3, and 5 above are rewritten in place, not superseded |

**Scheduling mechanism, not aspiration:** `legion schedule create` (`src/cli/schedule.rs`) posts to
the bullpen on a cron expression. A schedule naming this doc and its premise table, addressed to
whoever owns #882/#864, turns "re-verify periodically" into a task that lands in a channel someone
reads -- the same failure mode the 2026-06-02 doc had was not "nobody thought to check," it was
"the disproof had nowhere it was obligated to surface." Concretely: `legion schedule create --repo
legion --text "Re-run the #882 trust-boundary premise table (docs/decisions/2026-08-10-...)" --cron
"<monthly>"` (exact cron syntax per `legion schedule create --help`), owner assigned at creation, not
left implicit.

## What this doc is not

- Not a design for artifact capture, spec binding, or the validator agent -- those are #882 steps 1,
  2, and 5, separate work with their own acceptance criteria.
- Not a ruling on #864's prevention design (detection via `PRAGMA user_version`, `PRAGMA
  foreign_keys` globally, or moving the store behind the daemon) -- cited as the tracked fix, not
  re-litigated here.
- Not a claim that hooks are worthless. Per #860's own doctrine, ADVISORY guards that control cost
  or routing (`pre-grep.sh`, `pre-read-sym.sh`, `pre-script-search.sh`) are working exactly as
  designed. Only the MUST-BE-TOTAL guards with no enforcing layer beneath them -- `no-direct-db.sh`'s
  write case chief among them -- are the exposure this doc is about.
