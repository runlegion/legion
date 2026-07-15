# Legion Changelog

## 0.21.0

The sanctioned-path release. Twenty-five PRs since 0.20.0, with one through-line: guards
that used to live in agent discipline or text-matching hooks move into the tools
themselves. `legion push` retires raw `git push` from agent doctrine -- it resolves the
branch's own checkout and refuses main and force by construction, so the
push-from-own-checkout rule is enforced rather than remembered. The identity-root guard
moves from a defeatable Bash hook into the DB insert path. `pr merge` stops trusting
absence-of-red: it now refuses actively failing check-runs, and `pr checks` pins its
answer to the PR's actual head SHA instead of whatever `gh pr checks` felt like echoing.
`pr edit`, `issue list`, and `pr create --closes` close the remaining everyday reasons to
shell out to `gh`, and `sym imports`/`sym importers` finally wire a query surface onto the
0.19.0 module graph. Two new workflow states make previously informal conditions
machine-checked: `Delegated` (bound to live wake-attempt liveness, never a free
self-set label) and the spec-revision protocol (an unratified deviation from a card's
frozen acceptance criteria now hard-blocks `legion verify`). Minor release: two additive
schema migrations (the new `replan_records` table and a nullable `wake_attempts.card_id`
column -- no existing table's columns touched), a raft of net-new CLI subcommands, and no
wire-format break; where existing surfaces changed behavior they only got stricter
(`pr merge` refuses more, forced dashboard card moves fail loudly on document-sync
failure) -- deliberate gate-tightening, not format changes.

### New

- **`legion push` -- in-band audited git push** (PR #795, #791): the sanctioned push path
  for agents, retiring raw `git push` from doctrine. `push --repo <name> [--branch <b>]`
  resolves the checkout that has the target branch checked out via `git worktree list
  --porcelain` and pushes FROM that checkout, because the pre-push hook reviews the CWD's
  checked-out branch, not the ref being pushed -- pushing branch B from a checkout sitting
  on A silently reviews A's diff. Refuses `main`/`master` and any flag- or refspec-shaped
  `--branch` value by construction (no `--force` flag exists), sets upstream on every push
  (`-u origin <branch>`, a no-op after the first), and audit-logs every attempt -- success
  or hook-blocked failure -- with the branch, resolved checkout, and head SHA. The audit DB
  is opened before the push, not after, so a DB-open failure fails fast instead of masking
  the push result and losing the row.
- **`legion pr edit` -- correct a live PR's title/body in place** (PR #793, #776):
  `pr edit --repo <name> --number <n> [--title <t>] [--body-file <f>] [--issue <n>]`
  replaces the old close-and-recreate dance via a new `edit-pr` worksource verb. A
  `--body-file` body runs the same structural validation as `pr write-check` and
  re-records the legion-pr-write gate for local HEAD -- refused, not recorded, when local
  HEAD does not match the PR's own head SHA, so the gate never attests a commit this
  checkout was not on. The shared load-issue/validate/record-gate block is factored into
  one function used by both `write-check` and `edit`.
- **`sym imports` / `sym importers` -- the module graph gets its query surface** (PR #794,
  #772): thin readers over `Database::list_module_edges_from/_to`, the reader pair 0.19.0
  shipped dead-code with the `module_edges` table. Both share one implementation
  (repo-scope loop, freshness computation, human/`--json` output); `--json` wraps results
  in the 0.20.0 `{snapshots, entries}` freshness envelope, matching `sym tree` rather than
  the bare-array shape. Never-indexed detection keys off `file_inventory` rows for the
  repo -- not `module_edges` rowcount, which is js/ts-only and would false-error a cleanly
  indexed pure-Rust repo. Unresolved/external edges print as "unresolved" instead of being
  dropped (surfacing a dangling import is the point), suffix matching mirrors `sym list
  --file`, and an empty `file` argument is rejected up front -- it previously
  suffix-matched every edge in scope as a silent full-table dump.
- **`CardStatus::Delegated` -- delegation bound to wake-attempt liveness** (PR #797,
  #778): a new card state for work handed off to a live watch-spawned wake attempt, with
  `legion kanban delegate/undelegate/delegated-needs-attention`. Entry and exit share one
  fail-closed predicate (`delegated_card_is_live`: fresh watch heartbeat AND an in-flight
  linked `wake_attempts` row) so Delegated can never be a free self-set label: the watch
  tick auto-reverts the card to Accepted the moment either half goes dark, and a new
  stop.sh gate re-checks the same predicate directly against the DB as the backstop for
  the one case the daemon-driven sweep cannot reach -- the watch daemon itself being down.
  Schema: a `has_column`-guarded nullable `wake_attempts.card_id` column plus partial
  index, set when `delegate_card` links an Accepted card to a live attempt.
- **Spec-revision protocol -- `ReplanRequest`, `ReplanRecord`, and a verify deviation
  gate** (PR #766, #554): when an agent concludes a card's frozen acceptance criteria are
  wrong, incomplete, or unachievable as written, `legion kanban replan-request --id --reason`
  stops the work (Accepted -> NeedsInput, a distinct audit action from generic
  needs-input) instead of improvising around the AC; after a human ratifies the revision,
  `legion kanban replan-record` stores it in the new `replan_records` table. `legion
  verify --deviation <reason>` consults the card's latest record: an asserted deviation
  with no ratified record hard-blocks as improvisation, while a ratified one lets verify
  audit against the revised criteria as normal.
- **`legion uncertainty witness-gate` -- decorrelated ground truth for gate trust** (PR
  #770, #694): an operator who actually knows whether a recorded gate verdict was correct
  (pre-push diff read, post-merge bug report) can now witness the `legion.gate` prediction
  directly -- looked up by the same `(skill, commit)` fingerprint `quality-gate list`
  prints, never an opaque prediction id, erroring when no emitted prediction matches.
  Unlike the automatic legion-review witness (whose clean-on-approve positive direction
  skews optimistic), an external source is trustworthy in BOTH directions. The same PR
  fixed an outcome-direction inversion caught in review: `--correct` is scored relative to
  whichever verdict the gate recorded, so a correct issues catch (the diff was NOT clean)
  reprojects to Escalated/0.0 rather than being misfiled as Shipped/1.0 and corrupting the
  calibration signal. The known clean-verdict undercount is now bounded rather than
  hand-waved: every silently-missed prediction eventually surfaces in `uncertainty
  orphans --surface legion.gate`.
- **`pr merge` enqueues on merge-queue base branches** (PR #768, #630): `merge_pr` now
  reports whether the PR was merged or queued. A queued PR has not actually merged -- the
  queue completes it asynchronously, possibly after re-running CI -- so the kanban-done
  transition and issue-close side effects are skipped rather than fired prematurely, the
  audit row records `queued` and the *effective* branch-deletion outcome (the queue path
  never passes `--delete-branch`), and the output says "enqueued", not "merged". Non-queue
  repos behave exactly as before.
- **`legion issue list` -- enumerate work-source issues in-band** (PR #759, #750): backlog
  grooming no longer needs the `gh` the no-gh hook blocks. A new `list-issues` plugin verb
  (deliberately distinct from the `list` verb `sync` uses, so sync's behavior cannot shift
  as a side effect) serves `--state open|closed|all`, `--label`, and `--json`, audited
  like the other worksource verbs. Review hardening made it fail closed everywhere an
  empty answer could lie: a gh auth/rate-limit/repo failure surfaces as an error instead
  of an empty array, a blank repo config errors instead of printing `[]`, plugin stderr
  warnings are relayed on success, and hitting the row limit warns rather than silently
  truncating.
- **`pr create --closes` -- the closing keyword stops being manual** (PR #758, #751):
  repeatable `--closes <n | owner/repo#n>` appends an idempotent `Closes #N` line per
  issue unless a recognized closing keyword for that exact issue is already present --
  detection requires the keyword immediately adjacent to each reference, because GitHub
  only auto-closes the first issue of a comma-joined group ("Closes #1, #751" does not
  close #751). `pr write-check --issue N` now warns (v1: warn, not fail) when the body
  lacks a closing keyword. Fixes the wave-5 drift where shipped issues stayed open after
  merge because nothing injected the keyword.
- **Release toolchain generalized via `release.toml`** (PR #769, #741): `scripts/release.sh`
  and `scripts/sync-version.sh` read the version-of-record file and dotted field, changelog
  path, propagation targets, branch, and tag/commit templates from a per-repo `release.toml`
  (parsed generically with `legion sym etc extract`) instead of hardcoding legion's layout.
  Any repo with `legion` on PATH adopts the flow by dropping in its own config; legion's
  own release is byte-identical by default.
- **Stop-gate background-work + fix-loop doctrine design doc** (PR #790, #788):
  `docs/plans/2026-07-14-stop-gate-background-and-fixloop-doctrine.md`, the design
  groundwork for the wave-2 stop-gate work. Doc only; no behavior shipped.

### Fixed

- **`pr merge` refuses actively failing check-runs, not just absent ones** (PR #792,
  #761): the merge arm gated on zero-check-runs (#736) but let a present-and-failing
  check through unnoticed -- a red required check could merge via legion on repos with
  admin-bypass branch protection. It now classifies checks with the same
  `ExternalPRCheck::is_failing()` that `pr checks` uses and refuses, naming the failing
  checks. `--merge-despite-failures` is an audited operator override mirroring
  `--skip-gates`: it writes an audit row naming the failing checks before merging, and
  does not touch the zero-runs refusal, which still fires first.
- **`pr checks` pinned to the PR head SHA; zero runs fail closed** (PR #757, #736): `gh pr
  checks` was observed live echoing a parent commit's green suite when the head commit
  itself had zero check runs -- a false 7/7 SUCCESS for an untested commit that `pr merge`
  gates on. The worksource now resolves the PR's `headRefOid` and queries that exact SHA's
  check-runs and legacy status API directly (merged, so third-party CI on the old API
  still surfaces), paginates check-runs correctly, and wraps the response with the head
  SHA so an empty list names the commit it is empty for. `pr checks` and `pr merge` both
  refuse with the same "no runs for head <sha>" message instead of reading an empty list
  as nothing-failing.
- **Identity roots guarded at the DB layer; the pre-whoami-rewrite hook is deleted** (PR
  #796, #785): the only guard against a second, unparented `domain=identity` reflection
  was a Bash PreToolUse hook that pattern-matched command text and waved `--force`
  through -- exactly how a checkpoint-shaped reflection landed as a second orphan root and
  outranked the real identity in the boot banner. `insert_reflection_with_meta` now
  refuses a second live identity root unconditionally (`IdentityRootExists`; bootstrap and
  `--follows` chaining unaffected), and `swap_identity_root` is the one sanctioned replace
  path: a single transaction that deletes every live identity root for the repo --
  including previously leaked duplicates -- and inserts the new root plus optional chained
  children, rolling back atomically. Not yet wired to a CLI surface (#784 consumes it).
  The hook and its test are deleted: the DB guard makes its denial redundant, and its
  command-string matching was defeatable by any wrapper or alias.
- **One liveness predicate for lease list and release; reaper finalization decoupled** (PR
  #775, #679): the release paths checked only `deleted_at` while `leases list` also
  required `expires_at > now`, so an expired-but-undeleted lease was invisible to list yet
  still "releasable". A single shared `LIVE_LEASE_WHERE` predicate now backs list, the
  operator release, and release-by-host. Deliberately NOT applied to
  `release_persona_lease_if_owner`: that is the reaper's own-row finalization write, and
  gating it on `expires_at` (as the first commit did, caught in PR review) let an
  already-expired owned row survive reap and get its TTL pushed forward by the same tick's
  heartbeat -- resurrecting it as a permanent ghost in `leases list`, the exact display
  bug being fixed, via a different path.
- **`force_move_card` syncs the bound document in the same transaction** (PR #755, #753):
  the dashboard's drag-and-drop move was a bare non-transactional UPDATE that left a
  linked spec document's status wherever it was, silently drifting from the governed
  transition path. The shared doc-sync logic is extracted into `sync_bound_document`, and
  a forced move now reads the card's `document_id` and runs the identical sync inside one
  transaction -- a sync failure (dangling document id, unparseable payload) rolls back the
  card move too, matching the governed path's guarantee, where it previously succeeded
  silently.
- **Worksource deserialization errors name the operation** (PR #754, #720): a serde
  failure on a plugin response surfaced only the offending field ("missing field
  `labels`") with no clue which of a dozen verbs produced it. The op string every call
  site already passes as `args[0]` is threaded through a new `decode_plugin_output` --
  the sole JSON-decode boundary -- so every verb's failure reads "list-sub-issues: missing
  field `labels`".
- **Subagent-stop nudge restates the deliverable before checkpointing** (PR #760, #752):
  the SubagentStop hook's injected context continues the SUBAGENT's own turn -- its next
  message is what the Task tool hands the parent as the final result. The old prompt made
  the subagent answer the nudge instead of its task, stranding the real deliverable in a
  turn the parent could never see. The prompt now orders the subagent to restate its
  complete deliverable first and append the checkpoint note after, and the legion-explore
  and issue-writer agent definitions carry the matching "your final message is your only
  output channel" instruction.
- **Simplify validator refinements** (PR #767, #669): three review-driven fixes to the
  simplify articulation gate. The no-base-ref/no-parent vacuous pass now prints a loud
  stderr warning instead of passing invisibly; the changed-file check covers exact-path
  renames; and `simplify_check` no longer shares pr-write's `strip_evidence_lines` --
  in simplify articulation an `Evidence:` line IS the within-file locator, so stripping it
  silently discarded a legitimate entry's substance before the word count.

### Changed

- **`recall-first.sh` no longer decides Explore spawns** (PR #763, #672): its
  deny-the-spawn Explore branch overlapped the no-harness block and is retired --
  `no-harness-explore.sh` is the sole Explore decider, and `recall-first.sh` is unwired
  from the Agent matcher in hooks.json, keeping only its WebFetch/WebSearch
  recall-injection role.
- **Explicit 2s `busy_timeout` on every `Database::open` connection** (PR #756, #721):
  CLI connections previously leaned on rusqlite's undocumented bundled 5s default while
  `sync_actor` set its own explicit 2s; every connection is now pinned to the same
  explicit value, with a behavioral test proving a concurrent open+write retries instead
  of failing `SQLITE_BUSY` immediately.
- **`QualityGateInput` struct replaces `record_quality_gate`'s positional args** (PR #789,
  #787): pure refactor, zero behavior change -- rows written are byte-identical. Follows
  the `db::AuditInput` precedent so the three in-flight gate-ledger issues (#779, #780,
  #773) each add a field instead of extending a six-argument positional signature.
- **Test-infrastructure hardening** (PR #765, #740; PR #762, #675; PR #774, #773): fixture
  git invocations are fully isolated (`GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` pinned to
  empty files, `GIT_DIR`/`GIT_WORK_TREE` pinned to the fixture) and the integration suite
  opens with a config-corruption guard that repairs a damaged real `.git/config` and fails
  loud; the daemon kill orchestration gains a DI-seam smoke test; and the wave-5/6/7
  review-finding dispositions land as small cleanups across the github worksource,
  inventory, and test common code.

## 0.20.0

The field-report release. All three changes trace to a single bullpen post (019f355c): a
rafters agent exercised the 0.19.0 `sym`/`sym etc` surface hands-on within hours of release
and hit three real gaps -- CSS queries told to re-run an index that had already run,
gitignored generated workspaces invisible to the sanctioned grep replacement, and `find-file`
serving a six-day-stale file row with no hint it was stale. Each gap violated the #713
standard the surface was built to (every block or error routes to a command that actually
works, and the index answer is trustworthy enough that shell bypass has no excuse), and each
is now closed. Minor release: one additive schema migration (`inventory_snapshots`, one
freshness row per repo -- no existing table touched), and one deliberate wire-format break --
`sym tree --json` and `sym etc find-file --json` now emit a `{snapshots, entries}` envelope
instead of the bare array 0.19.0 shipped days earlier with no external contract.

### New

- **`--no-ignore` on `sym etc find-content` -- reach gitignored generated workspaces** (PR
  #747, #745): `ContentScope` gains a `no_ignore` toggle threaded into the walk's
  `parents`/`ignore`/`git_ignore`/`git_exclude`/`git_global` switches, mirroring ripgrep's
  `-u`. The field report's live case: rafters' generated-but-gitignored `.rafters/` workspace
  was invisible to the entire `etc` surface, recreating the exact shell-grep bypass incentive
  epic #704 exists to kill. `no_ignore` is deliberately independent of `--hidden` -- bypassing
  gitignore alone does not admit a dot-directory, so reaching a gitignored DOTDIR like
  `.rafters/` needs `--no-ignore` and `--hidden` together, pinned by both a unit test and a
  CLI end-to-end test (the field report itself misdiagnosed which flag was missing). `.git/`
  stays excluded regardless of either flag, and the help text carries a secret-exposure
  warning in the same register as `--hidden`'s.
- **Inventory freshness -- indexed-at snapshots and HEAD-drift warnings on `tree`/`find-file`**
  (PR #749, #746): the field report's live case was `find-file` returning a `CHANGELOG.md`
  size/mtime from six days before the file on disk -- legion's own 0.19.0 release edit,
  invisible to legion's own tool, with nothing in the output hinting the row might be stale.
  A new `inventory_snapshots` table (one row per repo: `indexed_at` plus repo HEAD at index
  time, captured by a best-effort `git rev-parse HEAD` that returns `None` rather than failing
  on a non-git workdir) is written on every `legion index` run. `sym tree --json` and `sym etc
  find-file --json` now wrap results in a `{snapshots, entries}` envelope carrying per-repo
  `indexed_at`, `head_at_index`, `current_head`, and `head_drift` -- computed with one live
  `rev-parse` per repo in the result, never a filesystem walk; human output prints a stderr
  freshness line per repo ("up to date", a loud HEAD-drift WARNING naming both SHAs, or a
  re-index hint when no snapshot exists). The issue's open design question was resolved
  against a live-walk `--verify` fallback: it would reintroduce the per-query walk cost the
  inventory-backed design exists to avoid, and #707 already redesigned the one shape that
  needed staleness-proofing (`find-content`) as a direct scan. A malformed `watch.toml`
  degrades HEAD comparison to a stderr note instead of hard-failing a previously-DB-only read
  path.

### Fixed

- **CSS symbols wired to the query surface -- and the no-index error stops lying** (PR #748,
  #744): 0.19.0's #711 shipped CSS extraction (the `css_symbols` table via `lightningcss`)
  but no reader path -- `sym list`/`def --lang css` fell through to the SCIP-only index
  lookup, always empty for CSS, so every query printed "no index found; run `legion index`"
  even one command after a successful index run: an error prescribing a fix that cannot fix
  it, precisely the misroute #713 exists to close. A new `Database::list_css_symbols` (the
  enumeration half of the existing `find_css_symbol` point lookup) now backs `sym list --lang
  css` (class and custom-property defs, text and `--json`, `--kind`/`--file` filters) and
  `sym def <name> --lang css`, cross-repo by default with each hit tagged by owning repo. The
  never-indexed check now gates on `file_inventory` rows with `ext = "css"` -- not `lang`,
  which is always `NULL` for CSS -- so "never indexed" and "indexed, empty result" print
  different, truthful messages, and `sym hover --lang css` fails loudly with a fixed
  no-hover-surface message routing to the commands that do work. A regression test reproduces
  the live 0.19.0 bug: `legion index` followed immediately by a CSS query.

## 0.19.0

The bypass-replacement release. Epic #704 set out to replace `grep`/`find`/`ls -R`/`os.walk`
shell escapes with structured queries answered from an index, and this release ships every
query shape the epic's bypass survey named: exact content search, a cross-repo build tree,
config/frontmatter field extraction, and cross-repo file lookup by name or role -- plus two
new symbol engines (a JS/TS module-import graph and CSS class/custom-property extraction) that
extend `sym`-style answers to file types SCIP never covered. The grep/find guard and the
`legion-explore` routing ladder are reworded so every blocked query names the exact `sym etc`
command to use instead of leaving symbol lookup as the only sanctioned alternative -- the guard
message and the explore agent's ladder are plugin surfaces, so this wording only reaches live
agent sessions once the plugin itself updates to 0.19.0; pinning an older plugin against a
0.19.0 daemon keeps the old, symbols-only guard text. Three hardening fixes round out the
release: a per-repo index lock closes a walk/upsert/prune race, integration test fixtures no
longer touch the real repo's `.git/config`, and `claude -p` review hooks are bounded by a
watchdog so they cannot wedge a commit or push. Minor release: three new tables
(`file_inventory`, `module_edges`, `css_symbols`) are additive schema migrations -- no column
removed or renamed on an existing table -- and every new CLI surface (`sym tree`, `sym etc
find-content/extract/find-file`, `document set-status`, the two `document` serve endpoints) is
a net-new subcommand or route; no existing flag, subcommand, or wire format changed or was
removed.

### New

- **`legion sym etc find-content` -- exact content search via ripgrep crates** (PR #719, #707):
  the sanctioned replacement for the largest logged bypass class (60+ escapes). `src/etc.rs`
  runs the in-process ripgrep engine (`grep-regex` + `grep-searcher` over an `ignore` walk)
  directly against the working tree at query time -- deliberately not a corpus, because a
  tokenized index returns nothing on the punctuation-heavy literals agents actually grep for,
  and any content index goes stale on checkout/pull, neither of which fires an edit hook.
  Literal mode uses `fixed_strings()` so conflict markers and dotted patterns match verbatim;
  regex is the default. `--repo`/`--ext` filter, results are name-sorted for deterministic
  capping, and every invocation (including failures) lands a row in `etc-usage.jsonl` -- the
  epic's primary success metric, instrumented from the first query shape shipped.
- **File-inventory table -- the substrate under `tree` and `find-file`** (PR #718, #705): a new
  `file_inventory` table, populated on every `legion index` run by a gitignore-aware
  `ignore::WalkBuilder` walk (`src/inventory.rs`), one row per non-ignored file with an
  extension-derived `lang` (independent of SCIP's repo-level marker sniffing, so a `.sh`/`.md`
  file gets a row with `lang = NULL` rather than no row at all). Re-index is idempotent
  (`ON CONFLICT(repo, path) DO UPDATE`) and pruned via a temp-table diff rather than a bound
  `NOT IN (?,...)` list, so large repos do not hit SQLite's variable-count limit. A docs-only
  repo (no language markers) now populates its inventory instead of hard-erroring, since the
  SCIP block below it is independently gated.
- **`legion sym tree` -- structured cross-repo build-tree query** (PR #730, #706): answers from
  `Database::list_file_inventory` with no filesystem walk at query time, emitting
  `{repo, path, ext, lang, size, symbol_count}` entries in `--json` mode. `--repo`/`--ext`
  filter server-side; `--under`/`--depth` filter in-process (`under_matches` rejects a sibling
  path that merely shares a string prefix); omitting `--repo` returns every watched repo's
  files tagged by owning repo. Every invocation, success or error, records a telemetry row.
- **`legion sym etc extract` -- pull one field without a full read** (PR #731, #708): `extract
  <path> --field <dotted.path>` reads a JSON/TOML/YAML config file or the YAML frontmatter of a
  `.md`/`.mdx`/`.astro` doc, converts it into one `serde_json::Value` tree, and walks the dotted
  path with a single walker shared across all four source shapes -- numeric segments index
  arrays. A missing field names both the failing segment and the deepest segment that did
  resolve. YAML goes through `serde_yaml_ng` rather than the archived, advisory-carrying
  `serde_yaml` (RUSTSEC-2024-0320).
- **`legion sym etc find-file` -- locate a file by name or role across repos** (PR #734, #709):
  matches basenames/paths against a hand-rolled `*`/`?` glob and an optional coarse `--role`
  (config/test/doc/entry) heuristic, over the same file-inventory table, cross-repo by default
  and tagging every hit with its owning repo. Answers "which repo owns X" without touching the
  filesystem or SCIP at query time.
- **Module-graph engine -- JS/TS import edges via `oxc_parser` + `oxc_resolver`** (PR #735,
  #710): `src/graph.rs` parses every js/ts/jsx/tsx file's static imports/exports/re-exports and
  literal dynamic `import()`s through `oxc_parser`'s `ModuleRecord`, then resolves each
  specifier against its referrer with `oxc_resolver` (tsconfig `paths` auto-discovered per
  file, `node_modules`, package.json `exports`/`imports`); unresolved or external specifiers are
  recorded with `to = None` rather than dropped. Edges persist in a new `module_edges` table,
  keyed on `(repo, from_path, specifier)`, and `legion index` runs the pass unconditionally over
  the inventory's typescript-lang subset -- independent of whether `scip-typescript` is on PATH,
  since oxc needs only the files on disk. A follow-up fix clears a live file's stale edges when
  its import set shrinks (previously neither the upsert nor the prune path touched a file that
  stayed live but dropped an import), and a Windows-only fix strips the `\\?\` verbatim-path
  prefix `std::fs::canonicalize` returns before comparing it against `oxc_resolver`'s
  already-stripped resolved paths, which otherwise made every edge on Windows misclassify as
  external. No CLI query surface (`sym imports`/`importers`) yet -- `list_module_edges_from/_to`
  are ready for a follow-on issue to wire up.
- **CSS symbols via `lightningcss`** (PR #739, #711): a new `css_symbols` table captures every
  class-selector and `--custom-property` definition, recursing into native CSS nesting,
  `@media`/`@supports`/`@container`/`@scope`/`@layer`, `@property`, and functional pseudo-classes
  (`:is()`/`:where()`/`:not()`/`:has()`). Tailwind v4's `@theme` block is unknown to
  `lightningcss` and parses as a raw token list; custom-property definitions inside it are
  recovered by scanning that token list directly for a `DashedIdent` followed by a colon rather
  than a `var()` reference. Wired into `legion index` alongside the module-graph pass, bumping
  `file_inventory.symbol_count` for CSS files through the same enrich pass SCIP uses.
- **Fair-guard rewording -- `sym etc` routing, lane-4/5 ladder, coverage-status banner, usage
  telemetry** (PR #737, #713): the grep/find guard (`pre-bash-grep.sh`, `pre-grep.sh`) used to
  offer only symbol lookup (`sym def`/`refs`/`hover`) as the sanctioned alternative to a blocked
  shell search, so a non-symbol query -- a literal string, a config value, "which file has X" --
  had nowhere to route and agents bypassed to shell grep, inferring false generalizations like
  "sym is rust-only" along the way (87 bypasses/30d per the epic's telemetry, 79 from
  `legion-explore`). Every deny/inject message now prints the exact `sym etc` command for the
  blocked query shape. `legion-explore`'s routing ladder grows a lane: the former last-resort
  bounded-text-search lane moves to lane 5, and the new lane 4 is `sym etc`/`sym tree` --
  commands that need no SCIP index at all, so a coverage gap is no longer a dead end. The
  `legion index --banner` coverage-status message now distinguishes "not indexed yet" (running
  `legion index` fixes it) from "indexer binary not on PATH" (it cannot, but `sym etc` still
  works for that language's files) instead of collapsing both into "not indexed". A new `legion
  telemetry etc-summary` command reads `etc-usage.jsonl` and reports per-query-shape count,
  zero-result rate (errors excluded from the denominator), and error count -- the epic's primary
  success metric made readable in one command. This wording and routing lives in the plugin
  (`plugin/hooks/`, `plugin/agents/legion-explore.md`), so it reaches a live agent session only
  once that session's plugin is on 0.19.0 or later.
- **`legion document set-status`** (PR #701, #700): the missing mutation primitive for a
  document's lifecycle status -- `document create`/`view`/`list`/`archive`/`validate` existed
  but nothing could flip `status` after insert, so the dashboard's draft-to-published flow had
  no backend. Sets the column, bumps `updated_at`, and returns the persisted row (re-fetched,
  not constructed in memory) so a printed status is proof of the write. No status-machine
  enforcement -- the localhost operator clicking Publish is the human gate, by design.
- **Document read + publish serve endpoints** (PR #703, #702): `GET /api/documents`, `GET
  /api/documents/{id}`, and `POST /api/documents/{id}/status` land in `channel::router`, the
  surface both the daemon and `legion serve` mount, so the embedded dashboard can list, view,
  and publish documents without 404ing under the daemon (an endpoint added only to `serve.rs`
  would have). A miss on the by-id or status routes returns 404, not the 500 the blanket
  `LegionError` conversion would otherwise produce.
- **Daemon supervisor restarts on build-id drift, not just version drift** (PR #699, #698): a
  rebuild that does not bump `Cargo.toml` -- the everyday dev loop -- previously left the daemon
  serving stale code until a manual restart. A new `build.rs` embeds a git short-SHA build id
  (`-dirty` suffixed on an unclean tree) via `LEGION_BUILD_ID`, surfaced on `/health` and
  `legion --version`; the supervisor now bounces the daemon when the version matches but the
  build id differs, role-aware (in-place `daemon-restart` for the daemon, kill-and-respawn for
  `legion serve`).

### Fixed

- **`whatami`/`whoami` banner budgets space per entry instead of all-or-nothing** (PR #738,
  #716): `format_capped_banner` used to render the newest root in full regardless of size, then
  collapse every later root into a single count if the remainder didn't fit whole. One oversized
  narrative root (observed on rafters: a 2.4KB entry alone blowing the 2KB cap) pushed two real
  operating-rule roots out of the banner entirely, into a bare "(N more truncated)" pointer, so a
  full session rediscovered rules it already owned. The cap now divides the remaining space
  evenly across remaining entries at each step, rolling unused share forward; an entry that
  doesn't fit its share is head-truncated with a recall pointer rather than dropped, provided a
  minimum amount of real text survives, and the first (newest) entry is exempt from being dropped
  outright. The doc comments claiming a measured "harness truncates to 2KB" cutoff are corrected
  -- `session-start.sh` has no truncation logic of its own; the cap is a self-imposed
  scannability budget, not a measured harness limit.
- **Per-repo index lock serializes walk + upsert + prune** (PR #726, #722): `legion index` runs
  on the same repo could overlap -- a manual run racing the detached background indexer `legion
  watch add` spawns -- and `prune_file_inventory` deletes rows absent from the current run's
  walk snapshot with no ordering guard, so an older run's stale snapshot could silently drop a
  row a newer overlapping run had just inserted. `acquire_index_lock` (reusing the existing
  `src/watch/locks.rs` pidfile idiom) now spans the entire walk/upsert/prune/SCIP-index sequence
  per repo; a live-pid holder fails the second run loudly, naming the holder's pid, and a
  dead-pid lock is reclaimed and the run proceeds.
- **Hermetic git fixtures -- integration tests can no longer poison the real `.git/config`** (PR
  #727, #723): fixture git invocations wrote identity via `git config user.name`/`user.email`
  directly against a `current_dir`-scoped repo; if that scoping ever silently failed to apply,
  the write fell through to whatever repo git resolved from the process cwd -- and because `git
  worktree` checkouts share their main repo's `.git/config`, that meant the real, shared config.
  This had already happened live: four commits got mis-attributed as `Test <test@example.com>`
  and `core.bare` was independently observed flipped to `true` mid-review. Every fixture now
  passes identity as per-invocation `-c` overrides (never a config write), and a suite-wide
  `RealRepoConfigGuard` hashes the real repo's `.git/config` before and after the run, panicking
  and naming the file if it changed.
- **Hooks cannot hang -- `claude -p` review is bounded by a watchdog** (PR #729, #728):
  `.githooks/pre-commit` and `.githooks/pre-push` piped the diff into `claude -p` with no
  timeout; invoked from inside a Claude Code session, the nested child inherits the session's
  Stop gate and can hang indefinitely, and the "unavailable, skipping review" fallback never
  fires because the subprocess never returns -- the hang that was normalizing `--no-verify`
  bypasses. A `run_claude_review()` helper backgrounds the `claude` invocation in its own
  process group (`set -m`, no GNU `timeout` dependency, portable to stock macOS bash) behind a
  120s watchdog that `SIGTERM`s then `SIGKILL`s the whole group, including any grandchildren; the
  child runs with `LEGION_SKIP_STOP_BLOCK=1` so a healthy `claude` can still exit on its own. A
  timeout or failure routes through the existing "unavailable, skipping" fallback and the hook
  still exits 0; the deterministic `scripts/sync-version.sh` step stays blocking and unchanged.
  ShellCheck's CI job is extended to cover `.githooks/*`, which it had silently skipped before.
- **`legion sub-issue list` no longer fails on every call** (PR #717, #714): the GitHub
  sub-issue GraphQL API returns only `{number, url, title, state, body}` per child, but
  `ExternalIssue.labels` was modeled as required, so serde rejected the entire payload before
  any row could print. `#[serde(default)]` on the field lets an absent `labels` deserialize to
  an empty vec without disturbing the `gh issue list` path, which does supply labels.

### Changed

- **Model policy, sole-implementer roster, barebones CLAUDE.md** (PR #725, #724): agent model
  pins updated (`rust`/`issue-writer` to `claude-sonnet-5`, `reviewer` to `claude-opus-4-8`,
  replacing a stale `claude-sonnet-4-6`; no agent pins the Fable interactive-planning tier). The
  `porter` and `dashboarder` agents are deleted with `rust` as sole implementer, and every
  reference to the removed agents (routing rules, boundary fencing, PR-producer lists) is
  scrubbed. `CLAUDE.md` is rewritten from 41 lines of restated workflow/invariants to a minimal
  pointer file (what legion is, and where to look: whoami, whatami, recall, `--help`), closing a
  drift where the old text named a since-deleted `/review-pr` skill and a review process the
  team had already moved off of.
- **Wave 2 orchestration workflow committed** (PR #733, #732): `.claude/workflows/wave2.js`, the
  script that ran four merge-ready PRs through isolated-worktree implementation, gate recording
  on every HEAD, and an Opus review/fix loop with zero human intervention between launch and
  merge, is checked in as infrastructure rather than left untracked on one machine. Enforces the
  no-`--no-verify` doctrine at every stage that touches git, and accepts both a JSON object and
  a JSON-encoded string for its args (the shape the harness actually sent on its first live run).

## 0.18.8

The gate-trust release. The quality gates now feed the uncertainty engine: every simplify, review, and pr-write verdict is emitted as a `surface=legion.gate` prediction, and the downstream legion-review gate witnesses the upstream simplify verdict so a "clean" simplify that review later contradicts is recorded as wrong. The same audit that produced 0.18.4 also found a residual hole -- simplify recorded issues on 0.9% of runs against pr-write's 21.3% on the same self-review-your-own-diff structure -- and this release closes the last structural difference by requiring located file:line/symbol evidence in the simplify articulation, not just substantive prose. Patch release: gate-trust is an additive feature built on the existing uncertainty engine (Pillar 2, shipped at 0.15) and the existing quality gates, emitted in-process and non-blocking with no engine logic change; no wire-format change, no schema migration.

### New

- **Quality-gate verdicts are emitted as uncertainty predictions** (PR #692, #691): a new `src/gate_trust.rs` maps each `QualityGateRow` to a `PredictionInput` (`surface=legion.gate`, `feature_key=gate.<skill>`, `fingerprint=skill:commit`, `claimed_confidence` high for a clean verdict and low for an issues verdict, `model_version` = legion's own version) and inserts it through the engine's existing `insert_prediction` -- the engine is a callee only, unchanged. It is wired non-blocking into the simplify (`Check`), review (`Record`), and pr-write (`WriteCheck`) record handlers; an emit failure logs to stderr and is swallowed so it can never break gate recording. `verify` is deliberately skipped because its card-keyed skill name would fragment every card into its own single-member cohort, defeating the per-cohort rubber-stamp measurement. The fingerprint is a plain `skill:commit` join of two already-deterministic identifiers, so the downstream witness can recompute it without a hash round-trip.
- **The review gate witnesses the upstream simplify verdict** (PR #696, #693): when a `legion-review` verdict is recorded, `witness_simplify_from_review` resolves the `legion-simplify` `legion.gate` prediction for the same commit and witnesses it through the engine's `Prediction::witness` + `update_prediction` (again no engine logic change). Review found issues means the earlier clean simplify verdict was wrong -- correctness 0.0, Escalated; review clean corroborates it -- correctness 1.0, Shipped, a deliberately weak positive because legion-review records clean on approve even after its own findings are fixed. A new storage accessor `latest_emitted_by_fingerprint(surface, fp)` resolves a `(skill, commit)` to the newest still-`Emitted` prediction (skipping already-witnessed and orphaned rows, with a `created_at DESC, id DESC` tie-break so same-timestamp re-runs resolve deterministically on time-ordered UUIDv7 ids). The witness fires only for `legion-review` and is non-blocking. This is an MVP wiring point with named limits: the genuinely-independent pre-push review runs from the operator's global `core.hooksPath`, outside legion's shippable surface, so only the in-pipeline downstream gate is witnessable here; exact-commit matching undercounts and the clean-corroboration signal is weak. The decorrelated-witness follow-up (external pre-push / revert / post-merge-bug sources via a `witness-gate` CLI) is tracked as #694.

### Fixed

- **legion-simplify requires located evidence, not just substantive prose** (PR #690, #689): the 0.18.4 simplify rebuild forced a per-file articulation but accepted any sufficiently substantive `### <path>` entry, so the gate still recorded issues on only 0.9% of 1131 runs against pr-write's 21.3% on the identical self-review structure -- the lone remaining structural difference was pr-write's evidence requirement. `simplify_check::validate_articulation` now requires each per-file entry to cite a located construct -- a `file:line`, a `fn`/`::` symbol, or an explicit `Evidence:` line -- reading only the entry body so the `### <path>` heading (which restates the path for free) cannot satisfy the source-path test by itself. The detector is factored as `has_located_evidence`, the structural subset of pr-write's `has_evidence`: simplify uses the strict located-only form while pr-write keeps the generous form that also accepts cited observable behavior, one shared definition of "located" with no parallel heuristic. An unanchored "the behavior is unchanged" no longer passes. The legion-simplify SKILL.md (v2.1.0) documents the per-file located-citation requirement.

## 0.18.7

The wedge-and-flake release. Three independent defects in the wake/daemon path and the plugin spawn path. A watch-spawned wake session whose turn never completes now hits a wall-clock backstop instead of leaking its process and holding the persona lease forever; a request wake that asks for a deliverable now drives the work to done rather than settling for a bare ack; and an intermittent CI spawn failure under parallel tests is retried away. Patch release: three fixes within the existing watch/daemon and worksource-spawn surfaces, no wire-format change, no schema migration.

### Fixed

- **Wedged wake sessions are force-reaped on a wall-clock budget** (PR #680, #677): a watch-spawned wake session whose turn never completes had no termination path. The only reap signal was the stop hook logging "turn complete" (which sets `exit_observed_at`); an agent that blocks mid-turn never fires it, so `reap_finished` sat forever in its `Ok(None) + !turn_done` branch, leaking the process and its MCP children and holding the persona wake-lease against every future wake. The `#673` dead-pid reaper could not catch it because the pid stays alive. A wall-clock session budget (`config.session_budget_secs`, default 1800; `0` disables) now force-reaps the still-alive branch when a child older than the budget shows no completion signal: it is killed, the session lock and persona wake-lease are released, and the wake_attempt settles to `Failed` with a distinct `session_budget_exceeded` outcome (folded into the existing `submit_failed_reason` path). A kill failure keeps the child tracked for retry rather than leaking and opening the gate, mirroring the idle-REPL path. Killed children are now also reaped so they do not zombie -- `PtySession` already reaps in `Drop`; the `Print` arm now waits after the kill.
- **Request wakes are framed as must-complete, not just must-reply** (PR #681, #678): the `REQUIRES A REPLY` section of `build_wake_prompt` told the woken agent to "reply", which a bare ack satisfies, so a request that asked for a deliverable produced an acknowledgment instead of the deliverable. The framing now distinguishes answer-a-question from do-the-work: a request or handoff with a deliverable must be COMPLETED and reported, or explicitly declined/blocked with a reason, before the turn ends; a bare "received"/"on it"/"ack" that stops without doing the asked work is named as ghosting. The informational section is unchanged, so receipt-only wakes still just ack with no wake-storm regression. `build_wake_prompt` is the single source for both the PTY-injected wake prompt (`gates.rs`) and the SessionStart pending-replies banner (`cli/signal.rs`), so the change lands on both surfaces at once. Validated live: a re-run of the multi-agent broadcast test went 4/4 on do-and-report, up from 1/4.
- **Plugin spawn retries on `ETXTBSY` instead of failing the call** (PR #687, #682): worksource exec intermittently failed with "Text file busy (os error 26)" -- `ETXTBSY` on `execve`, returned when the target file is open for writing by any process. Under parallel tests a sibling fork inherits a writable fd to another test's freshly-written stub inside its fork/exec window. `call_plugin_inner` now spawns via `spawn_with_etxtbsy_retry`, which retries on `ETXTBSY` within a bounded 2s budget (20ms backoff, once-only log) and fails non-`ETXTBSY` errors immediately. The retry is `#[cfg(unix)]`-gated because `ETXTBSY` is a POSIX errno -- on Windows raw OS error 26 is `ERROR_SHARING_VIOLATION`, unrelated, so that path uses a plain `spawn()` fallback matching the existing `process_group` cfg split. The regression test is `#[cfg(target_os = "linux")]`: Darwin raises `ETXTBSY` only for native Mach-O binaries, not shebang scripts, so the test is load-bearing only on Linux.

## 0.18.6

The wake-robustness release. An apparent "auto-wake has been dead for six days" turned out to be a test error -- a signal sent `--repo X --to X`, which the poll query self-excludes (a repo cannot wake itself from its own post) -- and the wake path is in fact healthy end to end. Investigating it surfaced four genuine defects, fixed here. Patch release: additive guards and hygiene within the existing signal/watch/daemon surfaces, no wire-format change, no schema migration.

### Fixed

- **`legion signal --repo X --to X` is refused instead of silently dropped** (PR #674, #673): `--repo` is the authoring context and `--to` the routing target; when they match, the watch poll query (`AND r.repo != ?`) drops the signal and it wakes nobody, with no error. A shared `is_self_address` guard (broadcast-exempt, leading-`@` tolerant) now refuses the call in both the CLI and the MCP `legion_signal` tool, naming the correct usage. This is the exact trap that produced the false alarm.
- **The interactive `.session` lock is cleared on session end** (PR #674, #673): a new `SessionEnd` hook removes the lock the moment a session terminates (the correct event -- `Stop` fires per turn), instead of waiting for the daemon to passively reap a dead pid, closing the window where a recycled pid reads as a false active-session and suppresses a wake. The wake-spawned path is careful never to delete a concurrent interactive lock.
- **`legion daemon-restart` recovers a wedged daemon** (PR #674, #673): when the pidfile is stale but the daemon port is still held, restart now identifies and kills the holder -- but only when it is structurally confirmed to be a legion daemon (`argv[0]` basename `legion` + a `daemon`/`serve` subcommand), so an unrelated process on the port is never touched. Previously this required a manual `kill`.
- **Orphaned `running` wake_attempts with a dead pid are reaped** (PR #674, #673): rows whose backing process is gone (e.g. after a crash) are marked failed each health tick instead of persisting indefinitely.

## 0.18.5

The explore-redirect release. The harness built-in `Explore` subagent greps and reads raw files; on a legion-covered repo that is the wrong instrument, because `legion:legion-explore` answers the same orientation questions from the SCIP index (def/refs/impl/hover) and the reflection corpus, returning conclusions with file:line evidence instead of file dumps. Patch release: an additive enforcement hook within the existing plugin-hooks surface, no wire-format change, no schema migration.

### New

- **Built-in `Explore` subagent is blocked and redirected to `legion:legion-explore`** (PR #671, #670): a PreToolUse hook (`no-harness-explore.sh`, wired to the `Agent` and `Task` spawn matchers) denies a `subagent_type` of `Explore` -- an exact lowercased match, so `legion-explore` and `code-explorer` pass through -- and tells the model to re-issue the call with `legion:legion-explore`, which orients through SCIP sym queries and recall/consult instead of grep/find. Gated on legion coverage, consistent with the sym/grep/read enforcement hooks. SubagentStart cannot block a spawn (it is context-only), so the redirect runs at PreToolUse, before the spawn.

## 0.18.4

The rubberstamp-killer release. An audit of the quality-gate corpus found legion-simplify was clearing 99.3% of runs as "clean" -- a 0.66% catch rate against 23.5% for `pr write-check`, the same self-review-your-own-diff structure but with validated prose. Two changes close the gap. The gate corpus becomes auditable through the binary, and the simplify gate is rebuilt from a yes/no record into an articulation forcing-function so a clean result means a per-file review actually happened. Patch release: additive subcommands within the existing quality-gate surface plus a skill rewrite; no wire-format change, no schema migration.

### New

- **`legion quality-gate list` and `legion quality-gate stats`** (PR #667, #666): read subcommands over the `quality_gates` table, which was record-only until now. `list` returns gate rows newest-first, filterable by skill/result/branch/since, as a human table or `--json` array. `stats` reports per-skill runs, clean and issues counts, catch rate (issues/runs), and total/max findings -- the rubberstamp tripwire that turns a near-zero catch rate into a one-line query instead of a manual SQL dig. Both are strictly read-only; no new write path. Caveat: catch rate is a meaningful rubberstamp signal only for gates that record a first-pass `issues` row before fixing (simplify, pr-write); legion-review records `clean` for approved even after fixing findings, so read its `details` not its `result`.

### Changed

- **legion-simplify is now an articulation forcing-function** (PR #668, #665): the skill records its gate only through `legion quality-gate check --skill legion-simplify --result <clean|issues> --articulation-file <f>`, which resolves the changed-file set from `git -c core.quotePath=false diff --name-only main...HEAD` (three-dot merge-base range) and refuses unless every changed file has a substantive, non-boilerplate `### <path>` articulation entry. A failed articulation exits non-zero before any DB write, so no gate row is recorded and `legion pr create` stays blocked. A clean gate now means an accepted per-file review exists, not that an agent typed "clean". The substance threshold (`MIN_MAPPING_WORDS`) and the line-stripper are shared with `pr write-check` so both gates use one bar. The base-ref resolution hard-errors when it cannot determine a base (shallow clone, non-`main` default, detached checkout) rather than vacuously passing -- a git failure can no longer silently disable the gate.

## 0.18.3

The stray-artifact release. `legion index` left a multi-megabyte `index.scip` protobuf in the root of every indexed repo. Because gitignored files are still visible in the working tree, agents grooming a repo read it as junk and repeatedly tried to delete it. The file is purely transient -- its bytes are ingested into the SQLite `scip_indexes` column the moment they are read, and no regen ever consults the prior file (each run rebuilds it from source) -- so legion now removes it as soon as the bytes are captured. Patch release: a behavior fix within the existing index surface, no wire-format change, no schema migration.

### Fixed

- **`index.scip` no longer lingers in the repo root** (PR #664, #663): `run_indexer_binary` deletes the on-disk protobuf immediately after reading its bytes into memory, in the single shared path that backs all nine language indexers, so the stray artifact stops appearing across every language and every indexed repo. Removal is best-effort by design -- the bytes are already captured and stored before the unlink, so a failed removal does not fail the index, and the `/index.scip` gitignore entry is retained as a safety net for that edge. `fs::read` remains a hard error: no bytes still means no index.

## 0.18.2

The kanban reconcile release. Cards minted from GitHub issues never moved to a terminal state when the linked issue closed, so a board accumulated "shipped-pending" zombies -- active-local cards whose linked issue is already CLOSED or MERGED -- until someone ran `legion kanban reconcile` by hand. A woken agent grooming that board acted on closed work (observed on rafters: 65 of 86 "open" cards were already shipped). The watch daemon now runs the safe reconcile direction on its own slow timer. Patch release: additive behavior within the existing watch surface, no wire-format change, no schema migration.

### New

- **Daemon auto-reconciles shipped-pending kanban cards** (PR #655, #654): `WatchLoop::tick_reconcile` scans the board for active-local cards whose linked GitHub issue is already CLOSED or MERGED and cancels them locally, so the board a woken agent grooms reflects GitHub reality without a manual `legion kanban reconcile`. Only the safe, purely-local cancel-shipped direction is automated; `--close-stale` (which closes live GitHub issues) stays a manual CLI action and is structurally unreachable from the daemon path. Per-card and per-repo work-source failures are logged and skipped, never aborting the pass. The reconcile scan and both actions were extracted from the CLI handler into a shared `kanban::reconcile` module so the daemon and the CLI run identical logic (the CLI arm shed ~280 lines of duplicated scan/action code).

### Config

- **`reconcile_interval_secs` in `watch.toml`** (#654): seconds between automatic shipped-pending reconcile passes. Default 3600 (hourly); `0` disables auto-reconcile entirely. Each card with a linked issue costs one work-source probe, so the cadence is deliberately slow, and the first pass fires one full interval after startup to avoid a probe storm on every daemon restart.

## 0.18.1

The PTY wake reliability release. Auto-wake's default PTY spawn mode submitted the wake prompt with a single carriage return fired immediately after fork -- before the Claude TUI input pipeline is interactive -- so the submit was swallowed and the wake attempt aged silently to `abandoned`. Directed signals went unanswered. This release replaces fire-and-forget with a feedback-driven confirmed-submit protocol, validated empirically (six PTY experiments) before implementation. Patch release: a reliability fix within the existing watch surface, no wire-format change, no schema migration.

### Fixed

- **PTY wake prompts reliably submit** (PR #652, #649; PR #650, #648): the watch PTY spawn wrote the prompt plus an immediate `\r` right after fork, but the TUI input pipeline does not become submit-ready for ~15-22s in plugin-heavy repos, so the carriage return was swallowed and the wake sat at an empty REPL until it aged to `abandoned`. The prompt is now bracketed-pasted (so a multi-line prompt cannot fragment into partial submits) with no submit keystroke at spawn; `drive_submit_confirmation` runs on the health tick and re-sends Enter -- throttled to `submit_retry_interval_secs` (default 4) -- until the PTY ring buffer shows a turn started, then advances the wake_attempt `Spawning -> Running`. After `submit_retry_max` (default 12) Enters or `submit_confirm_budget_secs` (default 60) wall-clock with no turn, the attempt fails closed: `Spawning -> Failed` with outcome `submit_not_confirmed`, killed and reaped the same tick. A swallowed submit is now a first-class, queryable wake_attempts outcome instead of a generic abandon. `SpawnedChild::send_keys` (#648) is the sanctioned post-spawn write path. The turn-start marker requires a digit before `tokens` so the prose `waste tokens` in the wake prompt -- echoed into the input box -- cannot false-confirm; bracketed-paste content is stripped of ESC bytes so agent-authored signal text cannot close the paste early and inject keystrokes.

### Config

- **Three submit-confirmation knobs in `watch.toml`** (#649): `submit_retry_max` (default 12), `submit_retry_interval_secs` (default 4), `submit_confirm_budget_secs` (default 60). Print mode is unaffected and remains the `LEGION_SPAWN_MODE=print` fallback.

## 0.18.0

The refactor release. Six streams of structural work deliver a codebase that is split by domain, observable, and covered. The god files (per the June audit: db.rs 8,815 lines; main.rs 8,130 lines; watch.rs 4,767 lines) are carved into 50+ focused modules. Three live defects are fixed alongside the structural work. The coordination substrate gains spec binding, spec-gen, and a verify gate that reads bound-spec AC. The quality-gate chain ships end to end. Minor release: structural refactor plus additive features and one additive schema migration (tasks.document_id); no wire-format change.

### New

- **Spec-gen pipeline** (PR #639, #527): `legion spec-gen --repo <surface>` derives requirement documents from non-archived service-design inputs (persona, journey, blueprint, painmatrix, ecosystem) on the specified surface. One requirement candidate is produced per `moment_of_truth` entry, validated against the requirement schema, and inserted as a `doc_type=requirement` document plus a born-Backlog kanban card. Re-running on unchanged input is safe: existing `(traces_to, surface)` pairs are skipped. The `--repo` argument is the `surface` field on the source documents, not a git repository name.

- **Card-spec binding and transactional status sync** (PR #640, #528): `tasks.document_id` binds a kanban card to a spec document. Status transitions for bound cards synchronize the document's `status` field in the same transaction (accepted -> accepted, in-review -> implemented, done -> verified, cancelled -> cancelled). A dangling `document_id` is a hard error on any syncing transition. `legion verify` reads AC from the bound document's top-level `verification.acceptance` when present and non-empty, falling back to `tasks.acceptance`. The bind guard ensures a given document is held by at most one non-cancelled card.

- **Done verify gate is spec-aware** (PR #645, #644): `legion done --id` resolved its acceptance criteria from `tasks.acceptance` directly while `legion verify` used spec-document precedence, so a spec-bound card gated Done on the wrong criteria set. Both gates now share one resolver: bound `verification.acceptance` first, `tasks.acceptance` fallback, hard error on a dangling binding. Gate error messages name the AC source (`ac source: card` or `ac source: spec:<doc id>`).

### Fixed

- **Cluster sync runs under the daemon and actually transfers data** (PR #624, #536): the `#582` WatchLoop unification held for the loop body, but the daemon path had lost its sync-actor spawn. `SyncHandle` was only spawned in `watch::run` (standalone `legion watch`), not in `daemon::run_watch_task`. The daemon now spawns the cluster sync actor alongside the health/spawn loops, so reflections, cards, and schedules actually replicate when running under `legion daemon`.

- **REFLECTION_COLUMNS const ends column-list drift** (PR #623, #606): a `get_reflections_by_ids` SELECT projected 10 columns against an 11-column row mapper, producing `InvalidColumnIndex` panics when `legion serve` triggered a search. A single `REFLECTION_COLUMNS` const in `src/db/reflections.rs` is now the one source of truth for the column list used across every query; the mapper and the SELECT cannot drift.

- **PR merge defers to repo review policy** (PR #622, #621): `legion pr merge` was hardcoding `APPROVED` as the required review state. It now reads the repository's branch-protection rules via the work source plugin, deferring to the repo's own policy. Repos with no required reviews are not blocked.

- **Priority::Med passed in gates.rs blocked_card helper** (PR #635, #634): `gates.rs::blocked_card` was constructing a test card with `Priority::Low` instead of `Priority::Med`. The mismatch caused `evaluate_spawn_gate` tests to use a card that did not match the priority filter in the real scheduling path, making the helper a misleading fixture.

- **cfg(unix) the daemon-attribution bind test** (PR #642, #641): the daemon-attribution bind test used `libc::getpid()` unconditionally. On Windows (where the CI also runs), `libc` does not export `getpid`; the test failed to compile. Wrapped in `#[cfg(unix)]` to match the unix-gated `libc` dependency.

### Changed

- **Typed exits via ExitWith, GateResult enum, verify_gate_key single-source** (PR #638, #610): `std::process::exit(1)` calls scattered across handler bodies are replaced with `LegionError::ExitWith(code)`. `main()` is now the only exit site. `GateResult { Clean, Issues }` is the canonical stored gate result (lowercase string in SQL). `verify_gate_key(card_id)` is sourced once in `src/verify.rs` so the writer (`cli::verify`) and the reader (`cli::kanban`) cannot drift on the key format.

- **Serve stream: ServeError, channel::router, schedule firing moved off SSE** (PR #637, #613): `ServeError` implements `axum::response::IntoResponse`, replacing 29 hand-written `match-to-json_error` blocks in `src/serve.rs`. `channel::router` now owns the `/post` and `/tasks` endpoints that `serve.rs` previously duplicated. Schedule firing moved from per-SSE-connection side effects into a background `tokio::spawn` task, so schedules fire once per tick regardless of dashboard client count and without racing across connections.

- **Comms stream: one addressing rule, signal::compose gate, mcp/ carve, verb canon** (PR #636, #612): the four divergent `@recipient` addressing implementations are replaced by `signal::is_addressed_to`. `signal::compose` is the single validation+format entry point for all signal creation (CLI and MCP); the MCP `legion_signal` handler now goes through `compose`, closing the `#587` required-fields bypass for rfc signals. `src/mcp/` is split into `log.rs`, `tools.rs`, `notifier.rs`, and `mod.rs`. Dead `review` verb removed from the verb canon (it had no entry in `plugin/verbs/default.toml`).

- **Midtier cleanup: plugin_call boundary, recall single-sourcing, Priority enum, kanban/ and worksource/ carves** (PR #633, #615): `require_worksource()` replaces ~19 copy-pasted worksource-resolve-or-die blocks. Recall is single-sourced through one `recall::search` function. `Priority` is an enum (Low, Med, High, Critical) with `Display` and `FromStr`, replacing stringly-typed priority comparisons. `src/kanban/` is carved out for the FSM and card-parse logic. `src/worksource/` is carved out for the plugin bridge.

- **Watch stream: bootstrap unification, watch/ tree, dead ClusterConfig deleted** (PR #632, #611): `watch::run` and `daemon::run_watch_task` share one `WatchLoop::bootstrap` that owns the sync-actor spawn, so a safety gate can never again be present in one loop and absent in the other. `src/watch/` is split into `config.rs`, `locks.rs`, `signals.rs`, `spawn.rs`, `tracker.rs`, `gates.rs`, and `mod.rs`. The dead `watch::ClusterConfig` parallel config path (parsed, never consumed in production) is deleted.

- **Carve main.rs into cli/ module tree** (PR #631, #610): the 3,700-line `run()` match over 58 arms is split into per-domain handler modules under `src/cli/`. Each handler module owns arg massaging + print/JSON; algorithm bodies remain in the domain modules they were already in.

- **Split db.rs into a db/ domain module tree** (PR #629, #609): the 8,815-line `db.rs` is split into 19 domain files under `src/db/`. `mod.rs` owns infrastructure only (Database handle, `open`, `has_column`, `init_schema` dispatcher). Each domain file owns its DDL, `impl Database` block, and co-located tests.

- **In-place DRY: require_worksource, open_db, Done propagation fold** (PR #628, #610): three high-frequency pain points addressed in-place before the cli/ split. `require_worksource()` replaces the first wave of duplicated resolve blocks. `open_db()` / `open_db_and_index()` replace duplicated db-open preludes. `propagate_card_close_to_worksource` unifies the done/cancel propagation paths.

- **Hooks refactor: shared lib/, one Grep/Glob guard, dead plumbing deleted** (PR #627, #614): the hook preamble that had quietly diverged across 10+ scripts now lives in `plugin/hooks/lib/prelude.sh` -- stdin parse, repo identity (LEGION_REPO env takes precedence over basename(cwd) in EVERY hook), binary resolution (`${CLAUDE_PLUGIN_ROOT}/bin/legion` first, PATH fallback; pre-whoami-rewrite and both uncertainty hooks were PATH-only and silently inert in hook subshells), and the coverage gate. `plugin/hooks/lib/emit.sh` owns the output vocabulary (emit_allow/emit_deny/emit_block/emit_context); the legacy PreToolUse `{decision:block}` dialect is migrated to the documented `permissionDecision:deny` shape. `pre-grep-recall.sh` + `pre-grep-scip.sh` (two forked hooks per Grep/Glob call) merged into one `pre-grep.sh` running the same sym-block / sym-inject / recall-inject ladder as `pre-bash-grep.sh`, so Grep-tool and Bash-grep enforcement agree on the same pattern -- including the #458 relevance gate and the LEGION_BYPASS_GREP refusal for local-symbol queries. Deleted: `bullpen-check.sh` (registered nowhere) and `_legion-warn.sh` plus the ~80 lines of unreachable WARN branches its six consumers carried. Tests converge on `plugin/hooks/tests/testutil.sh`: one parameterized stub-legion contract (FAKE_* env vars) instead of 12 bespoke heredocs, plus a structural lock that every production hook sources the prelude.

- **Split tests/integration.rs into a module tree + lay the coverage net** (PR #626, #608): the 6,696-line `tests/integration.rs` is split into `tests/integration/{main.rs, common.rs}` plus per-domain files. New coverage added for previously-untested surfaces: serve bind + health endpoint, sym round-trip, pr write-check gate, watch wake-loop with spawn-gate outcomes.

- **Propagate v0.14-v0.17.2 features across docs** (PR #625, #616): README, `llms.txt`, `llms-full.txt`, and `docs/site/concepts.md` updated to reflect features landed since v0.14.0 -- documents substrate, spec-gen, card-spec binding, the verify gate, the autonomy budget, the quality-gate chain, and the watch PTY migration.

## 0.17.2

The quality pipeline completes and the definition layer lands. The plugin now ships every stage of the work pipeline -- explore, simplify, pr-write, review, verify -- and the substrate holds its first real schemas. Patch release: additive features within existing surfaces, no schema migration, no wire-format change.

### New

- **legion-review ships in the plugin -- the missing gate between pr-write and verify** (#617): the review stage of the quality pipeline now ships to every legion-equipped repo. `plugin/agents/legion-review.md` is the repo-generic reviewer (ported from legion's repo-local reviewer): validates the diff against the linked issue's acceptance criteria AND reviews code quality (error handling, silent failures, security, idioms, tests), enforcing the target repo's own CLAUDE.md invariants rather than hardcoded Rust rules, and returns a structured approved/changes_requested decision with file:line findings. `plugin/skills/legion-review/SKILL.md` orchestrates it: parallel dimension agents (spec, correctness, quality, security), adversarial refutation of every HIGH/MED finding before it is reported (the audit-workflow shape: refuters overturned a third of claimed findings), and a HEAD-keyed `legion-review` quality gate recorded via `legion quality-gate record`. Absorbs #532 and #177.

- **Schemas as documents -- the definition layer lands in the substrate** (#526): the requirement schema plus all five service-design schemas (persona, journey, blueprint, ecosystem, painmatrix) now live in `schemas/` and land in the documents table as `doc_type=schema`. `legion document create` gates schema payloads structurally at create (dependency-free: `$schema`/`title`/`type:object`/non-empty `properties`, `required` must name real properties) and dual-writes a pointer reflection on `domain=schema`, so `legion recall --domain schema` surfaces every landed schema with its document id (documents hold the canonical payload, reflections hold the searchable prose). New `legion document validate --schema <id> --file <path>` checks an instance against the dependency-free subset (type incl. nullable arrays, required, properties, items, enum) with one JSON-path error per violation. All six real vault-2026 service-design instances validate against the landed schemas; two are pinned as CI fixtures.

- **legion-explore agent -- sym/recall-first exploration** (#604): a plugin-shipped read-only exploration agent (`plugin/agents/legion-explore.md`) that replaces the grep-based harness Explore on legion-equipped repos. Routes by question shape across four lanes: doctrine questions to recall/consult, symbol questions to `legion sym`, targeted Reads only at sym-cited spans, and bounded text search as a declared last resort logged via `legion telemetry record-bypass`. Escalation is deterministic (index-staleness flagging, sym-miss retry rules, enumerate-don't-guess on multi-match); every finding cites file:line or a reflection id. Tool surface is Bash + Read only -- no Grep/Glob. Confidence scoring against the uncertainty engine is deliberately deferred until the spec substrate (#512) lands.

## 0.17.1

Two watch-spawn safety fixes surfaced by a live broadcast-wake incident on 0.17.0. A single `@all` wake-worthy signal woke the entire farm at once, and recovering from it exposed a daemon restart that lied about success when its port was held. Patch release: behavior fixes within existing surfaces, no schema or wire-format change.

### Fixed

- **Concurrent-wake cap -- a broadcast can't boot the whole farm** (PR #600, #598): `poll_cycle` iterated every watched repo and spawned for each one carrying a pending wake-worthy broadcast, throttled only by `stagger_secs`; `evaluate_spawn_gate` ran once per cycle and gated only on quota-panic and health-pressure, neither of which bounds a fan-out. A single `@all` `request` spawned an agent for all 17 repos. New `WatchConfig.max_concurrent_wakes` (default 4; 0 disables) makes `poll_cycle` stop spawning once in-flight wakes reach the cap and defer the rest to later polls, so a broadcast drains at a bounded rate. The decision is a pure `wake_cap_reached(active, cap)` reading `AgentTracker::active_count()` as the single in-flight counter.
- **Daemon spawn/restart fails loud on an occupied port** (PR #602, #599): `spawn_detached` consulted only the `daemon.pid` file, so when a foreign process held the port (a stray `legion serve`, which shares the port and pidfile, or an orphaned daemon) it forked a child that died on bind while reporting "daemon started (pid N)" and left the pidfile pointing at a corpse. A port preflight now runs after the pidfile already-running check: if the port is not bindable, spawn returns `LegionError::DaemonPortInUse` naming the holder pid (best-effort `lsof`) instead of forking a doomed child, and writes no pidfile. The deeper serve/daemon `:3131` ownership unification is tracked in #601.

## 0.17.0

Watch, made reliable and observable. The auto-wake core always worked, but everything around it made it look dead and behave unpredictably: an idle daemon logged nothing, broadcasts and tags were half-built, the poll loop was forked into two drifting copies, interactive sessions were invisible to the spawn gate, and the SubagentStop hook looped on itself. This release closes that gap end to end and makes the wake decision data-driven. Minor release: additive features plus one additive schema migration (`watch_heartbeat`); no wire-format change. The wake-verb set changed membership (see Changed) but informational verbs still never wake, so existing signals behave as before.

### New

- **`legion watch status` + liveness heartbeat** (PR #588, #581): the daemon writes a `watch_heartbeat` row (Migration 24) on every health tick with its pid, version, and repo count, plus a throttled INFO line. `legion watch status` reports `alive | stale | absent` with the running version and the most recent `wake_attempts`, so a frozen `daemon.log` is never again mistaken for a dead loop. Both the daemon and standalone `legion watch` write the beat.
- **Broadcast wake -- `broadcast_tags`, `@everyone`, typed `Recipient`** (PR #592, #585): repos can subscribe to group tags via `broadcast_tags = [...]`; a `@<tag>` signal wakes exactly the tagged repos, `@all`/`@everyone` wake every repo (each once via `watch_handled` dedup), and the matching layer is generalized over a repo's full addressable name set. Existing configs with no tags are unaffected.
- **Verb-plugin shapes -- data-driven wake** (PR #594, #587): the wake gate consults TOML verb manifests under `plugin/verbs/` (`wake | record | fuckoff | maybe-close`, with optional `required_fields`) instead of a hard-coded set, resolved from `LEGION_VERBS_DIR` or `${CLAUDE_PLUGIN_ROOT}/verbs`, falling back to an embedded default that reproduces the canon exactly. New verbs ship without a release; `rfc` now requires a `budget` detail, enforced at signal-create.

### Fixed

- **Unified the forked watch poll loop** (PR #589, #582): `watch::run` and `daemon::run_watch_task` now share one `WatchLoop` body (`tick_health` + `tick_poll`) built via one constructor, so a safety gate can never again be present in one loop and absent in the other (the #578 class of bug). Three tests drive the shared `tick_poll` through every spawn-gate outcome.
- **Interactive sessions no longer get a duplicate spawn** (PR #590, #583, extends #406): a human-started session registers a PID-liveness-gated `<repo>.session` lock at SessionStart, so watch sees it as awake and does not spawn a second agent on top of it. Dead-PID locks self-heal.
- **SubagentStop re-fire loop** (PR #591, #584): the hook adopted a `stop_hook_active` loop-guard and a per-event idempotency marker, so a re-delivered SubagentStop no longer re-reflects and re-injects context in a loop (observed burning ~337k tokens on one spurious fire). Each subagent stop is processed exactly once.
- **A shared `agent` across repos is not a collision** (PR #596, #595): the `agent` field marks which persona maintains a repo, so a tiny lib (e.g. `ledger`, owned by `platform`) deliberately shares one agent. The persona wake-lease already dedups a directed signal to a single wake, so the prior hard `load_config` error, the `watch list` warning, and the `add_repo` rejection (all built on a false multi-wake premise) are removed. Duplicate name/path are still rejected.

### Changed

- **Wake-verb canon corrected + send-time feedback** (PR #593, #586): the wake set is now `{question, request, handoff, correction, proposal, decision, rfc, routing}` -- the verbs the team actually pages with. `help` and `blocker` were statuses, not bare verbs, and are dropped. `legion signal` now prints a note when a directed signal uses a non-waking verb, so the silent "I signaled but nobody woke" case is visible at send time.

## 0.16.4

Watch reliability fix. The auto-wake daemon now honors the subscription-quota panic-stop gate it had been bypassing. Patch release: one fix, no schema migration, no wire-format change.

### Fixed

- **daemon watch loop evaluates the quota panic-stop gate** (PR #579, #578): the Bun->Rust port forked the watch poll loop into `watch::run` (standalone `legion watch`) and `daemon::run_watch_task` (the copy that actually runs under `legion daemon`, auto-started every session). The daemon copy silently dropped the subscription-quota panic-stop gate (#496) and the pressure-skip log -- so the daemon could spawn wake agents with this host's 5h/7d rate-limit window already at the panic threshold, and went completely silent when health-gated, making a live loop look dead. The per-poll gate decision is now extracted into one shared `watch::evaluate_spawn_gate` returning `SpawnGate { Proceed, QuotaPanic, Pressure(f64) }`, called by both loops so the gate set cannot diverge again; both loops log the skip reason. Three new tests cover the branches, including quota panic taking priority over a healthy system. A `#[cfg(test)]` `HealthSampler::push_pressure_for_test` seam makes the pressure branch deterministic.

## 0.16.3

The checkpoint resume-anchor and harness-primitive adoption. `/snooze` becomes `/checkpoint` -- the name primed agents to go dormant -- and the split resume-anchor unifies onto one `domain=checkpoint` read path. The Stop and SubagentStop hooks adopt CC 2.1.163's `hookSpecificOutput.additionalContext`. The watch loop stops mistaking an idle PTY REPL for a live session. Grep-blocking is codified as the operator's `permissions.deny`. Patch release: behavior changes plus additive features; no schema migration (the `checkpoint` domain already existed) and no wire-format change.

### New

- **`legion sym list`** (PR #559, #558): enumerate a repo's symbol definitions by kind (`--kind struct|fn|trait|...`) and/or file (`--file`), from the SCIP index. Fills the "what symbols exist here" gap between `sym def` and a grep.
- **SubagentStop hook -- persist subagent work** (PR #575, #570): when a spawned subagent ends, its transcript tail is persisted as a `domain=checkpoint` reflection (tagged `subagent,auto`) so delegated work reaches legion memory instead of vanishing with the parent's context, and a one-line pointer is injected to the parent via `additionalContext`. New `SubagentStop` hook registration (matcher `*`). Fail-open: never blocks the parent.

### Changed

- **`/snooze` -> `/checkpoint`; unified resume-anchor** (PR #573, #568): the command is renamed (the word "snooze" primed agents to go dormant and get lazy after running it; a checkpoint is a waypoint you cross and keep moving). Its Phase 3 now writes a structured `[CHECKPOINT]` resume-anchor (`--domain checkpoint --tags manual,session`). `session-start.sh` and `post-compact.sh` both read the freshest `--domain checkpoint` (post-compact dropped a fragile BM25 keyword search for a deterministic query); the deliberate anchor and the precompact safety-net no longer live in separate domains. Transitional `--domain snooze` fallback in SessionStart for one release so existing anchors survive.
- **Stop reflection nudge via `additionalContext`** (PR #574, #569): the end-of-work reflection prompt moves from `decision:block` to `hookSpecificOutput.additionalContext` -- non-error feedback that continues the turn so the agent acts on it, without the hook-error labeling and 8-block cap. The in-progress gate stays a hard `decision:block` (it must be able to refuse the stop and wants the cap as a safety valve) and now surfaces the board-derived goal. `stop_hook_active` loop guard added. (Verified against CC 2.1.168: Stop `additionalContext` continues the conversation, it does not let it stop -- the behavior is wired to that.)
- **grep-blocking is operator `permissions.deny`** (PR #561, #560): the mandatory shell-grep block moves out of the bypassable PreToolUse hook and into the operator's `permissions.deny` (evaluated before any hook, agent-unoverridable, subagent-inheriting). The hook stays as soft sym/recall guidance; the env-var hard-bypass is retired. Recommended deny ruleset documented for operators.
- **verify demotes vacuous evidence** (PR #552, #549): a `Pass` verdict with no real evidence is demoted to `Uncertain` (routes to NeedsInput) rather than allowing Done -- a claim of done that cannot cite a test or observed behavior is not done.
- **burn-rate gate on self-directed work** (PR #553): the autonomy budget pauses self-directed work as weekly rate-limit exhaustion approaches, so an agent cannot spend the operator's capacity on its own initiative.

### Fixed

- **watch no longer treats an idle PTY REPL as a live session** (PR #572, #571): a woken PTY `claude` REPL does not EOF after its turn -- it sat idle, holding the per-repo session lock for the full TTL (suppressing all further wakes) and leaking the process. The reaper now tears down a completed-but-idle REPL (on its `exit_observed_at` signal), and the session lock is released on completion via both the reaper and the stop-hook fast path, instead of waiting out the TTL. Closes the "an agent went quiet and never came back" class.

### Internal

- **smugglr-core from crates.io 0.4.0** (PR #557, #556): off the unpinned git dependency onto the published, checksummed registry release.
- **RFC: spec-revision protocol** (PR #555, #550): documents legitimate re-plan vs improvisation.

## 0.16.2

The SOLID-issue workflow lands. A review pipeline keeps agents on a ratified spec instead of improvising (PR-write forcing function + verify gate); an autonomy loop lets the board drive self-directed work within a weekly budget; `whatami` gives the operating contract its own memory surface; and the kanban substrate fixes make the board mean "what is being worked on." Patch release: additive features plus workflow behavior changes. One additive table (`autonomy_budget`), `CREATE TABLE IF NOT EXISTS`, no migration of existing data; no wire-format change.

### New

- **`legion whatami` -- operating-contract surface** (PR #541, #517): a recall surface parallel to `whoami`, sourced from `domain=workflow` reflections and injected at SessionStart after identity ("who you are, then how you operate"). 2KB-capped like `whoami`; silent when a repo has no workflow roots.
- **PR-write forcing function** (PR #544, #519): `legion pr write-check --repo <r> --issue <n> [--body-file|stdin]` validates a drafted PR body against the issue's acceptance criteria -- one prose entry per criterion, each citing evidence, plus a "Not done" section -- and refuses an empty or boilerplate mapping. Articulation is verification: writing the diff-to-AC mapping makes the agent re-read its own work. Records a `legion-pr-write` quality gate.
- **verify gate** (PR #545, #520): `legion verify --repo <r> --card <id> [--verdicts-file|stdin]` reads a card's acceptance criteria and the agent's per-criterion verdicts (pass/fail/uncertain + evidence) and decides the card's fate -- all pass allows Done, any fail hard-blocks, any uncertain (or a Pass with no evidence) routes the card to NeedsInput. A card with no criteria is blocked outright. Card-keyed gate (`legion-verify:<card>`), so it holds across the commit `legion done` runs on. New FSM transition `InReview -> NeedsInput`.
- **weekly autonomy budget** (PR #546, #524): `legion autonomy status|gate` -- a rolling weekly governor on self-directed work (self-acceptance + free-time), keyed on this host's weekly rate-limit headroom (conservative default when no sample). Operator-requested work bypasses (`--operator`); exhaustion stops self-directed work cleanly while operator work proceeds. Surfaced at SessionStart and on Stop so the agent knows it has sanctioned units to spend. New `autonomy_budget` table.
- **`legion daemon-stop` / `daemon-restart`** (PR #540, #539): bounded daemon restart (SIGTERM -> 3s -> SIGKILL -> verify death) that does not wait on graceful-shutdown drain.

### Changed

- **Cards are born Backlog** (PR #534, #515): every card -- created or GitHub-synced -- now lands in `Backlog`, not `Pending`. Only an explicit assign (consensus/planfile) promotes to `Pending`. Behavior change: stops GitHub sync from flooding `Pending` with unconsented issues.
- **`kanban list` returns the working set** (PR #535, #516): the default list (and the SessionStart "Current work" banner) now excludes `Backlog` and terminal cards; `--all` and `--backlog` reach the rest. Fixes the bloated SessionStart banner.
- **`legion pr create` gates on simplify AND pr-write** (PR #544, #519): both forcing-function gates must be clean on HEAD before a PR opens; `--skip-gates` bootstrap skips both with an audit row. Behavior change.
- **`legion done` is verify-gated** (PR #545, #520): a card with acceptance criteria cannot reach Done until a clean `legion verify` verdict exists for it. Cards with no criteria are not gated. Behavior change.
- **Stop hook gates on the kanban board, not the harness TaskList** (PR #542, #523): blocks stopping while an `Accepted` card exists for the repo (reads the persistent board, not the ephemeral per-session task log). `LEGION_SKIP_STOP_BLOCK=1` bypasses.
- **`legion init` writes nothing** (PR #538, #537): the pre-plugin hook installer no longer writes stale hooks; it points operators to `legion watch add`. Hooks ship with the plugin.
- **grep-enforcement audited; allowlists guard command/skill surfaces** (PR #543, #530): the four PreToolUse grep hooks stay (they inject sym/recall context or apply the dynamic symbol-block ladder -- neither expressible as static `disallowed-tools`); `legion-simplify`'s skill allowlist tightened to drop Grep/Glob. New CI parity-lock test refuses any legion command/skill that re-grants Grep/Glob.

## 0.16.1

Hardening on the pre-bash-grep hook bypass mechanism. Patch release; no schema or wire-format changes.

### Changed

- **Soft sym-bypass refused for symbol-shaped patterns with local SCIP hits** (PR #506): `plugin/hooks/pre-bash-grep.sh` previously accepted `# legion-bypass: <reason>` or `LEGION_BYPASS_GREP=1` as a universal escape from the State 2 BLOCK tier. Agents (including the one that built the enforcement) routinely used the sentinel for queries that were definitionally symbol lookups -- `pub fn` names, `impl` blocks, struct/enum identifiers -- routing around `legion sym def`. The sentinel was too cheap: the friction of typing `grep -n Foo # legion-bypass: ...` matched `legion sym def Foo` closely enough that muscle memory won.
  - **Soft bypass** is now refused when the pattern resolves to a real symbol in THIS repo's SCIP index (the relevance-gate filter from #458). The refusal emits a block decision pointing at `legion sym def` and names the hard escape. Free-text patterns and patterns that resolve only in unrelated repos still pass the bypass (the legitimate cross-cutting case is unaffected).
  - **Hard bypass** added via `LEGION_BYPASS_GREP_HARD=1`. Always allows, but writes a row to `bypass.jsonl` with the `hard:` prefix on the reason field so `/telemetry summary` (#440) can show how often the hard escape fires -- loud signal that sym is under-serving.
  - Test coverage extended to 41 cases: soft env refusal on local symbol, soft sentinel refusal on local symbol, soft bypass still allowed for free-text / non-local patterns, hard escape always allows + writes `hard:` row.
  - Reflection trail: `019e578c` (root-cause analysis: why agents ignore sym even after enforcement landed), `019e5795` (hook-enforcement upgrade trick: reuse signals the hook already computes for other reasons).

## 0.16.0

Watch PTY migration ships. Before 2026-06-15, `claude --print -p` was the subscription-billed path; after that date the same invocation moves to billed API. Legion's auto-wake daemon was the only active site using `-p`, so every wake-worthy signal across every watched repo would have become a billed call. v0.16.0 migrates auto-wake to a PTY-spawned interactive `claude` REPL that retains subscription billing, with a `wake_attempts` ACID substrate that makes per-wake lifecycle cluster-visible and crash-recoverable. Epic #495.

### New

- **Subscription-quota panic-stop in watch** (PR #496, closes #484): defense-in-depth gate that halts spawn cycles when the most recent `rate_limit_samples` row for the host shows either the 5h or 7d window at >= 99%. Per-host (not cluster-wide) so a peer burning its cap cannot DoS this node. Edge-triggered single-shot bullpen posts on healthy <-> panic transitions; no spam mid-state. DB read failures fail-open (treated as healthy) so a transient query error cannot stick watch in panic.

- **`WATCH_SPAWN_MODE` env scaffolding** (closes #485): `SpawnMode { Print, Pty }` with `from_env()` resolver. Default flipped to `Pty` in this release (#494) since the print path is post-cutoff billed; operators who explicitly want the legacy path set `WATCH_SPAWN_MODE=print`. Unknown values warn + fall back to `Pty` so a typo cannot silently engage the wrong billing plane. Startup banner logs the resolved mode.

- **`src/pty.rs` portable-pty wrapper** (closes #486): `PtySession` over `portable-pty 0.9` (WezTerm's canonical crate; includes Windows ConPTY). Ring-buffered reader thread drains the master fd so a chatty child cannot block on a full pipe; bounded cap drops oldest bytes (harness needs EOF + diagnostic snapshots, not the full transcript). API: `spawn`/`write`/`pid`/`try_wait`/`kill`/`output_tail`/`eof_observed`. `Drop` reaps any non-terminal child including the `try_wait` Err case. Four new `LegionError` variants: `PtySpawnFailed`, `PtyAllocFailed`, `PtyWriteFailed`, `PtyWaitFailed`.

- **`wake_attempts` table + FSM-enforced DB methods** (closes #487): Migration 23 adds the per-attempt lifecycle row keyed by UUIDv7 attempt_id. `WakeAttemptState` enum (queued / claimed / spawning / running / exiting / done / failed / timeout / abandoned) with `can_transition_to` mirroring the `PredictionState` precedent. Two-layer FSM enforcement (Rust `can_transition_to` + SQL `state = from` predicate) means a sync-resolved `done -> running` regression is silently rejected. Eight DB methods: enqueue / try_claim (atomic) / transition / set_pid / mark_exit_observed / record_outcome / list_local_orphans (host-scoped) / get. Terminal-is-sticky: re-stamps of a settled outcome surface as `IllegalWakeAttemptTransition` with the actual current state.

- **`WakeAttemptDelta` + state-aware sync conflict rules** (closes #488): sync delta with happens-before-aware conflict resolution. Tombstone involved → LWW. Local terminal + incoming non-terminal → reject (regression guard). Both terminal disagreeing → keep later `exited_at` with deterministic host-id tiebreak. Unknown state literal from a forward-incompatible peer → log + reject, no panic.

- **PTY spawn branch via `PtySession`** (closes #489): `spawn_agent(SpawnMode::Pty, ...)` now launches an interactive `claude` REPL through the master fd. `SpawnedChild` enum unifies print + pty under one tracker handle. `LEGION_SPAWN_SOURCE=watch-pty` + `LEGION_WAKE_ATTEMPT_ID=<id>` env stamps land on the spawned process so `plugin/hooks/stop.sh` can fire the session-end handoff (#493). Prompt is injected as keystrokes + carriage return; write failure best-effort-kills the half-started PTY.

- **`AgentTracker` + `poll_cycle` write to `wake_attempts`** (closes #491; partial #490): poll_cycle mints a UUIDv7 attempt_id, enqueues a `queued` row, and atomically claims it before spawning. On success, FSM advances through Claimed → Spawning → Running with the PID stamped; on failure, transitions to Abandoned so the row does not leak. `reap_finished` records terminal outcome via `record_wake_attempt_outcome`. (Crash recovery -- the orphan-scan pass on watch startup -- is a follow-up.)

- **`legion watch session-end` CLI + stop-hook handoff** (closes #493): new CLI subcommand stamps `exit_observed_at` on the wake_attempts row so the watch reaper can short-circuit a poll cycle. PTY EOF + PID-poll remain authoritative; this is a speed-up only. `plugin/hooks/stop.sh` calls the CLI gated on `$LEGION_WAKE_ATTEMPT_ID` being set. Idempotent + missing-row-tolerant so a hook fire from an operator-attended session never blocks Claude Code's Stop.

- **`LEGION_AUTO_WAKE` audit + stop.sh gate suppression under watch-pty** (closes #492): `plugin/hooks/stop.sh` now early-exits both the incomplete-task gate and the reflect-prompt gate when `LEGION_SPAWN_SOURCE=watch-pty` is set. Watch-pty wakes are atomic units that exit through Stop on every wake; running operator-session gates risks the 8-block stop-hook cap in Claude Code 2.1.143 hard-killing the agent. Audit + per-gate decisions in `docs/decisions/2026-05-watch-pty-env-audit.md`.

- **Default flip to PTY** (closes #494): `SpawnMode::from_env()` defaults to `Pty` (was `Print`). Empty / unset / unknown env values now engage the PTY path. Operators upgrading from v0.15.x should set `WATCH_SPAWN_MODE=print` explicitly if they need the legacy billing plane during the soak window.

## 0.15.1

Patch release. Closes a long-standing silent failure in the TypeScript SCIP indexer that returned `[]` for any symbol defined in a pnpm/yarn workspace package, masking real symbols as nonexistent and degrading every `legion sym def/refs` query on monorepos.

### Fixed

- **scip-typescript skips workspace packages on monorepos** (PR #483, closes #441 / #482): `run_scip_typescript` invoked `scip-typescript index` without `--pnpm-workspaces` / `--yarn-workspaces`, so the indexer honored only the root tsconfig's `include` set. On a typical monorepo with a narrow root tsconfig that produced an effectively empty index -- rafters' root had `include: ["test/**/*"]` and got a 51KB blob with zero symbols in `packages/*`; the same repo with the flag indexed 18MB across 13 workspaces. `legion sym def TokenRegistry` and friends silently returned `[]`, and agents concluded the symbol did not exist rather than recognizing an indexer gap. Fix: new `detect_ts_workspace_flavor` inspects `repo_path` for a workspace marker -- `pnpm-workspace.yaml` -> `--pnpm-workspaces`, `package.json` with a `workspaces` field (array or object) -> `--yarn-workspaces`, otherwise no extra flag. pnpm wins precedence when both files exist, matching pnpm's own resolution; malformed `package.json` falls through to the no-flag path so the indexer still runs. Argv selection is split into `scip_typescript_args` and pinned by a unit test so a silent rename of the scip-typescript CLI flag would not reproduce the same gap with no test failure. 8 new unit tests cover every detection branch.

## 0.15.0

Pillar 2 ships -- the uncertainty engine. v0.14.0 made agents coordinate around work-genesis (documents, plans, sub-issues); v0.15.0 makes them predict cost and learn from outcomes. Every task the agent takes on emits a calibrated prediction; the witness path closes the loop when work completes. Vault-COS routing gains live cost estimates instead of static lookups. Two enforcement-hardening fixes ride along.

### New

- **Uncertainty engine schema -- `uncertainty_prediction` + `uncertainty_calibration_snapshot`** (PR #471, closes #355): SQLite migration 22 adds the two pillar-2 tables. Lifecycle states (emitted / witnessed / calibrated / orphaned / retired) plus indexed columns for the orphan sweep and cohort lookup. `actual_correctness_raw` stored alongside `actual_correctness` for the EB-shrinkage audit trail; `bucket_lower` / `bucket_upper` carry quantile-derived bounds. Smugglr sync delta types + getters added for both tables; tombstone cleanup extended.

- **Uncertainty engine domain types** (PR #472, closes #356): `src/uncertainty/` ships `Prediction`, `CalibrationSnapshot`, `PredictionState` (with `can_transition_to` + `transition` enforcing the five-state lifecycle), `OutcomeLabel`, `Confidence` and `Correctness` newtypes (NaN-rejecting, range-validated, `PartialEq`-safe), `PredictionInput`, and the deterministic `cohort_key` derivation. `UncertaintyError` via thiserror with `IllegalTransition`, `InvalidConfidence`, `InvalidCorrectness`, `InvalidPayload`, `PredictionNotFound`. 22 unit tests pin every legal + illegal transition including the race-prevention guarantee (witnessed cannot be orphaned, retired is terminal across all four exits).

- **`legion uncertainty` CLI surface** (PR #473, closes #357): four verbs mirroring platform's HTTP API. `emit` is non-blocking by design -- validation, serialize, insert, and stdout failures all log to stderr but exit 0 so an upstream hook can never break the agent. `witness` advances state machine-safely, returning `PredictionNotFound` when the id is missing. `calibration` reads reliability buckets with surface + model filters (LIKE-fuzzy until the #359 roller produces real data). `orphans` groups orphan-state predictions by surface. `--json` flag where applicable. 9 storage unit tests + 7 CLI integration tests.

- **Uncertainty auto-emit + auto-witness hooks** (PR #474, closes #358): PostToolUse hook on TaskCreate fires `legion uncertainty emit` with task-shape defaults and stores the task_id -> prediction_id mapping in `${XDG_STATE_HOME}/legion/uncertainty-tasks-<session>.jsonl`. PostToolUse hook on TaskUpdate where status=completed reads the mapping back and fires witness with placeholder correctness 1.0 + outcome_label=shipped until #283 wires real measurement. Both hooks fail-open. `LEGION_SKIP_UNCERTAINTY=1` disables both. 20 hook integration tests + 1 end-to-end test against the real binary.

### Fixed

- **no-gh hook bypassed by absolute-path invocations** (PR #477, closes #476): the PreToolUse hook matched `gh ` or bare `gh` as a literal prefix, so `/opt/homebrew/bin/gh pr merge ...` slipped past unblocked. Fix takes the basename of the first whitespace-separated token before matching: catches absolute paths and tilde-paths while still allowing commands that merely mention `gh` (ghostscript, `echo gh`, `grep gh /log`). 13-case test runner.

- **whoami rewrite guard** (PR #479, closes #478): agents were treating `legion reflect --whoami` like a CLAUDE.md edit -- stuffing architecture rules, file paths, and build commands into the identity domain, inflating the SessionStart banner past its 2K budget. New PreToolUse Bash hook intercepts the rewrite when an identity already exists AND the command lacks `--force` / `--follows`, blocking with the current identity inline. Forces the agent to read who they are before replacing it; redirects to chain via `--follows`, full-rewrite via `--force`, or drop `--whoami` if it's project knowledge. Catches absolute-path invocations via the same basename match. 12-case test runner.

### Changed

- **Stop-hook reflection prompt reframed around teammate findings**: the prior "what would you tell another agent who hits this same problem tomorrow?" was reading as future-self and producing journal-style status recaps. New phrasing names a teammate walking in cold and asks for the finding ("a gotcha, a hidden invariant, how something actually works"), not the activity. Behavior identical -- block decision, same skip rule, same `legion reflect` redirect -- only the prompt copy changes.

### Pattern delivered

**Memory + coordination + calibration.** v0.13.x said agents have memory. v0.14.0 said agents coordinate. v0.15.0 says agents learn -- every task emits a prediction, every completion witnesses the outcome, the calibration loop tightens the per-(surface, model, version) reliability curve over volume. Vault-COS routing reads live cost estimates instead of guessing; the orphan term in the Brier decomposition keeps the reliability score honest under silent failure.

Deferred to v0.16: #359 daemon cron (orphan-sweep + calibration-roll + Brier SQL), #360 dashboard view at `/uncertainty`, #361 v2 drift detection (waits for v1 data), real correctness measurement on witness (waits for #283 SCIP features).

## 0.14.0

Coordination substrate. v0.13.x was "agents have memory" -- reflections, recall, sym, bullpen, signals. v0.14.0 makes it "agents coordinate around work-genesis." The substrate is a documents table that holds spec/NFR/blueprint/persona/journey JSON rows with hot/cold tiering; recall gains an archive tier; the stop hook refuses to let agents abandon mid-plan; the work source plugin layer learns native GitHub sub-issues. Vault is elevated to chief-of-staff orchestrating spec workflows on top of the substrate.

### New

- **Coordination substrate -- documents table** (PR #467, closes #456): SQLite migration 21 adds a `documents` table that holds spec/NFR/blueprint/persona/journey rows. Type-agnostic at the storage layer; meta columns (type, surface, status, priority, owner) are hoisted from `payload.meta` and indexed via partial indexes scoped to the hot pool (`archived_at IS NULL AND deleted_at IS NULL`). New `src/documents.rs` module ships `Document` / `DocumentMeta<'a>` / `DocumentFilter<'a>` types + `impl Database` with `insert_document` / `get_document` / `list_documents` / `archive_document`. CLI: `legion document {create,view,list,archive}` -- create reads payload from `--from <path>` or stdin and validates JSON shape before INSERT (typo doesn't land as string blob); list excludes archived by default, `--archived` flips to the cold partition. `archive_document` uses COALESCE so re-archiving preserves the original timestamp (idempotent). Schema-level validation per type lives in a sibling child issue under #455 once vault ships the schemas; the foundation accepts any well-formed JSON today.

- **`legion recall --archives` / `--include-archives`** (PR #466, closes #457): three-mode recall over reflections. Hot (default, exclude archived), Cold (`--archives`, only archived rows -- the deep-dive), Both (`--include-archives`, broad search). Threaded through the BM25 + hybrid recall paths via new `ArchiveMode` enum; index over-fetches at 4x when filtering archived rows out so a heavily archived corpus does not return less than the requested limit. `consult_bm25` pinned to Both -- consult searches the whole corpus regardless of archive state (existing semantics preserved). v1 scope: `--latest` / `--domain` / `--cosine-only` warn and stay hot-only when combined with the new flags; extending coverage is a #457 follow-up.

- **Sub-issue worksource verbs** (PR #469, closes #462): native GitHub sub-issue support via the work source plugin layer. New Rust verbs `worksource::create_sub_issue` / `list_sub_issues`. github plugin gains `create-sub-issue` (looks up parent's GraphQL node id FIRST, errors before creating child if parent missing -- no orphans, then `gh issue create` + `addSubIssue` mutation) and `list-sub-issues` (GraphQL query on `subIssues` connection with client-side state filtering). CLI: `legion sub-issue {create,list}`. Auto-mirror PostToolUse hook deferred to follow-up; foundation ships first.

- **Stop hook blocks on incomplete TaskList items** (PR #468, closes #461): CC's training disposition is "act once, check in" -- agents abandon mid-plan. Stop hook now reads a per-session task-state log written by a new PostToolUse hook on `TaskCreate` / `TaskUpdate`, reduces to current status per task_id, and blocks Stop when any task ends in `pending` or `in_progress`. Block reason names each open task by id + subject, lists explicit terminal-state options (TaskUpdate(completed) / TaskUpdate(deleted) / post needs_input card), and includes the "the plan is the permission. Keep going." reminder. Bypass via `LEGION_SKIP_STOP_BLOCK=1` writes one row to bypass.jsonl. Two-layer enforcement: gate runs first, reflection prompt fires only after.

### Fixed

- **pre-bash-grep relevance gate** (PR #464, closes #458): the grep enforcement hook from v0.13.1 was blocking when ANY cluster-wide sym hit existed for a symbol-shaped pattern. Common dictionary words (`name`, `data`, `value`, `type`, `id`, `plugin`) are symbol-shaped AND exist as identifiers in every codebase. Caught the hook author blocking on its own author multiple times -- once grepping watch.toml for `name = "ledger"`, once probing the plugin directory. New helper `legion_prequery_filter_hits_local` filters cluster-wide sym hits down to the target repo before the block-tier decision; cross-repo-only hits pass through. Block reason updated to include `--repo $REPO` in the suggested sym command so the agent's redirect targets the same repo.

- **watch.toml duplicate-agent disambiguation** (PR #465, closes #459): two repos sharing one recipient (the `agent` field, or `name` when agent unset) created silent multi-wake on directed signals. Ledger had `agent = "platform"`; @platform signals woke both the platform CWD and the ledger CWD. Six "mis-routed" flags on a single bullpen thread before operator manually removed. Two-layer fix: `legion watch add` refuses the conflict up front (covers four cases: agent-agent, new-agent-vs-existing-name, new-name-vs-existing-agent, distinct-agents-allowed). `legion watch list` walks repos at print time and emits `[WARNING]` to stderr for any recipient shared by N>1 repos -- catches pre-existing duplicates the add gate can't prevent retroactively. Sibling to #226.

### Changed

- **`Database::conn` visibility** bumped to `pub(crate)` so `impl Database` blocks in other modules (`src/documents.rs` in v0.14.0) can use the connection without moving every method into `src/db.rs`. Alternative was ~200 LOC of file motion per new domain module; visibility change is the smaller move.

### Identity reflection

- **Completion discipline doctrine** added to the legion-prime whoami (reflection `019e1894`): when tasked with a plan, complete the whole plan before stopping. Intermediate stops are abandonment unless explicit blocked / needs_input / cancelled. The Stop hook (#461) enforces this mechanically; the identity reflection ensures agents know the doctrine before tripping the hook.

### Pattern delivered

**Memory layer to coordination substrate.** v0.13.x said "agents have memory" -- reflections + recall + sym + bullpen + signals. v0.14.0 says "agents coordinate around work-genesis." Specs, NFRs, blueprints land as queryable structured documents alongside the reflection pool. Plans (via TaskList) have mechanical completion enforcement. Sub-issues link work across the team via the native issue tracker primitive. The substrate carries the spec genesis event; vault as COS uses it to drive the work cascade (draft -> consensus -> issue -> work -> review -> archive-on-done) on top.

## 0.13.1

Grep enforcement chain, channel reliability, and operator-visible reliability hardening. v0.13.0 sharpened SCIP precision; v0.13.1 closes the loops: agents now get pushed off raw grep/Read onto sym/recall on indexed repos, the MCP notifier is observable and supervisable, and the dashboard daemon respawns itself when stale.

### New

- **Grep enforcement chain** (PRs #443/#447/#448/#449, closes #437/#438/#439/#440): three-state ladder (inject -> block -> bypass) pushes agents off raw search tools onto `legion sym` / `legion recall` on legion-indexed repos. `pre-bash-grep.sh` (new PreToolUse hook on Bash) intercepts `grep|rg|ag|ack|find|fd` invocations that slipped past the existing Grep/Glob hooks. `pre-read-sym.sh` (new PreToolUse hook on Read) blocks unbounded Reads of source files larger than 500 lines in indexed repos -- whole-file reads of `src/main.rs` (~6K lines) used to bill cache_read for every line; a bounded `Read limit=200` or a `legion sym hover Symbol` answers the typical question in bytes. Bypass via `LEGION_BYPASS_GREP=1` / `LEGION_BYPASS_READ=1` env or `# legion-bypass: <reason>` sentinel substring in Bash commands. Every bypass writes one row to `${XDG_STATE_HOME}/legion/bypass.jsonl` via the new `legion telemetry record-bypass` CLI. `legion telemetry summary [--since DURATION] [--repo R] [--top N] [--json]` rolls the log up by (tool, repo, pattern) so an operator can see where sym/recall is under-serving; same data also exposed at `GET /api/telemetry/bypasses`. `_legion-prequery.sh` factors the shared library (binary detection, pattern extraction with `-e PAT` and `--regexp=PAT`, symbol-shape filter, bypass detection, telemetry write).

- **`legion now`** (PR #446, closes #410): one-line SessionStart block surfaces local weekday + time + sunphase before identity. `[Legion] Now: Sun 2026-05-10, 11:01 PT (midday)`. claude-code's systemPrompt ships `currentDate` but no weekday and no hour; agents pattern-match on density and start saying "tonight" / "wind down" when the operator has the rest of the workday ahead. Demonstrated mid-session: prime planned a Mon-Thu sprint on a Sunday because the weekday wasn't visible. Sunphase mapping is fixed-hour (`5-6 pre-dawn`, `7-10 morning`, `11-13 midday`, `14-17 afternoon`, `18-20 evening`, `21-23 night`, `0-4 late-night`). `--json` emits the structured snapshot for other consumers.

- **`legion telemetry record-bypass` / `list-bypasses` / `summary`** (PR #443/#449, closes #437/#440): append-only JSONL telemetry log at `${XDG_STATE_HOME}/legion/bypass.jsonl`. Rows carry `(ts, repo, session_id, agent, tool, pattern, bypass_reason, had_sym_hits, had_recall_hits)`. The last two booleans are the load-bearing signal -- a bypass with `had_sym_hits=false` means the index missed an answer the agent expected; with `had_sym_hits=true` means the agent ignored a good answer and we should ask why. Feeds the uncertainty engine consumer in #354 downstream.

- **`legion index --status --json`** (PR #443): single source of truth for the indexed-repo probe used by the new enforcement hooks. `_legion-indexed.sh` helper mirrors `_legion-covered.sh` shape with inverted fail-open posture (missing binary -> "not indexed" so block tier silently disables instead of firing on bad data). Does NOT cache the missing-binary verdict, avoiding session-stickiness when the binary becomes available mid-session.

- **`legion kanban reconcile`** (PR #445, closes #444): adds the inverse direction. Pre-#444, reconcile only caught local-terminal-GH-open drift; the OPPOSITE direction (local-pending, GH-closed) accumulated faster because every shipped issue that auto-closed via PR keyword without an explicit kanban transition stayed pending forever. `--cancel-shipped` flips them locally with no-propagate semantics (GH already closed). `--apply` is sugar for both action flags. Single-pass partition over the board; per-card audit-logged with `propagation=reconcile-shipped`. Cleared 96 orphaned cards across the cluster as the smoke test.

- **`legion mcp-health`** (PR #452, closes #391): spawns a fresh `legion mcp` subprocess over stdio, sends initialize + `legion/notifier_health`, prints the JSON. New `legion/notifier_health` MCP method returns `{state: alive | stale | unknown, last_tick_secs_ago, threshold_secs}`. The notifier thread now updates a heartbeat (lock-free `AtomicI64` of unix seconds) at the top of every poll iteration. Threshold is `ceil(poll_interval * 3) + 1` seconds -- one missed tick under load is fine, three in a row means the thread is gone. The bug that drove this: the notifier exited silently on a poisoned stdout mutex, EPIPE, or panicked DB call; the MCP kept responding to JSON-RPC, so the process LOOKED healthy, but no `notifications/claude/channel` frames reached the running CC session. Bit huttspawn + kessel 2026-05-04 -- full conversation through watch-wake, awake interactive agents on both sides saw none of it.

- **`GET /health`** (PR #451, closes #319): dashboard daemon liveness probe. Returns `{status, version, started_at, uptime_secs}` with no DB hit so callers can poll aggressively. `version` baked in at compile time from `CARGO_PKG_VERSION` so clients can detect protocol drift after a plugin upgrade.

- **Dashboard daemon supervisor** (PR #453, closes #321): `plugin/hooks/_legion-daemon-supervisor.sh` probes `/health` on SessionStart and respawns the daemon if unreachable or version-skewed. Detached spawn via `setsid` (Linux) / `nohup` (mac) with stdin closed, runs as a background fire-and-forget so SessionStart latency does not include the curl probe or spawn handshake. Kill-by-PID is defensive: verifies `ps -p $pid -o args=` contains `legion serve` before SIGTERM so a recycled PID owned by another process is never the target. `legion serve` now writes a pidfile to `${XDG_STATE_HOME}/legion/daemon.pid` after bind succeeds, removes it on graceful shutdown. `LEGION_SKIP_DAEMON_SUPERVISOR=1` opts out.

### Fixed

- **`truncate_chars` no longer panics on `max < 3`** (PR #450, closes #346): `max - 3` on `usize` underflowed when callers passed `--preview 1` or `--preview 2`, panicking the process. Now clamps to hard truncation at the requested length when there is no room for the ellipsis. `--preview 0` returns `""`, `--preview 1` returns the first char, `--preview 3` returns `"..."` (full ellipsis fits exactly).

### Closed as obsolete

- **#320 (MCP SSE reconnect)** and **#322 (channel version handshake)**: both filed against an MCP-daemon coupling that does not exist in current main. MCP reads channel state from the legion DB directly via the notifier loop; the dashboard daemon serves the dashboard on port 3131 independently. No SSE between them, no protocol to version. The real channel-reliability failure modes are addressed by #391 (notifier heartbeat) and #319 + #321 (dashboard liveness + respawn).

### Pattern delivered

**Enforcement over policy.** v0.12.0 shipped pre-grep hooks that injected sym results as `additionalContext` but never blocked. v0.13.1 says: when the doctrine is "no grep on indexed repos," the hook should ENFORCE it, not coach. Three-state ladder leaves the escape hatch (bypass env + sentinel comment) but logs every escape as telemetry so the next sprint can fix what sym is missing rather than rely on agents reading hook output and complying. Same pattern under the channel hardening: heartbeat + `/health` + supervisor turn "agents notice silent channel" into "supervisor respawns the daemon and notifier health is one CLI call away."

## 0.13.0

SCIP precision and reach. Phase D of the SCIP completion plan -- the v0.10-0.12 chain shipped storage, dispatch, hooks, and consumption; this release sharpens the query layer and extends indexer coverage to every canonical sourcegraph language. Reviewer agents also gain an impact-radius signal sourced from the SCIP refs graph.

### New

- **Descriptor-aware symbol matching** (#431, closes #421): `legion sym def | refs | impl | hover` no longer falls through to substring match for queries with explicit SCIP descriptor suffixes. `Foo#` matches Type Foo anywhere in the descriptor path; `Foo#bar().` requires both descriptors in order; `mod/Foo#` anchors the namespace. Bare-name queries gain exact-name match against any descriptor (precision win over substring -- `Foo` no longer matches `MyFoo#` or `FooBar#`). Substring fallback only fires when the symbol string itself fails to parse (defensive path for unusual indexer output). Empty queries and whitespace queries short-circuit to false instead of leaking through `.contains("")` returning true.

- **Five new ecosystem indexers** (#432, closes #426/#427/#428/#429/#430): `scip-java` (markers `pom.xml` / `build.gradle` / `build.gradle.kts`), `scip-ruby` (`Gemfile`), `scip-clang` for C/C++ (`CMakeLists.txt` or `compile_commands.json`), `scip-dotnet` for C# (any `*.csproj` / `*.sln` -- first language whose marker requires a glob scan, new `has_dotnet_project` helper), and `scip-php` (`composer.json`). Each helper mirrors the existing TypeScript/Python/Go shape: missing-binary errors carry an install hint, subprocess non-zero surfaces stderr, output validated against `index.scip` in the repo root. Ships `legion` with eight working indexers (counting Rust); zero runtime cost in repos that don't have the markers, warn-and-skip when the binary isn't installed.

- **`legion sym impact --repo <R> --diff <path-or-stdin>`** (#435, closes #423): for every symbol whose definition the diff touches, reports the SCIP reference count across the repo's index. Sorted descending so wide-blast-radius diffs surface first. `--json` emits a stable shape for agent consumption. The `LEGION_IMPACT_HIGH_THRESHOLD` env var (default 50) controls the `HIGH` tag in text output. Reviewer agents (vault, smugglr) can call this against a PR diff to flag changes that touch heavily-referenced symbols. Diff parser handles git's `+++ b/path`, plain `diff -u`'s `+++ path<TAB>mtime`, `/dev/null` deletes, single-line hunks omitting `,count`, and per-blob plus cross-blob symbol dedup so polyglot repos never double-emit.

- **`legion index --logs`** (#433, closes #424): surfaces background-indexer log content through the CLI instead of requiring users to know about the on-disk path. Filters: `--repo <name>` to scope to one repo, `--lines N` to override the default 50-line tail, `--follow` to tail-follow new output via 250ms polling. Migrates the log directory from `/tmp/legion-index-<repo>.log` (wiped on reboot) to `$XDG_STATE_HOME/legion/index-logs/<repo>.log` so logs survive long enough to debug an overnight indexer failure. `spawn_background_indexer` calls `migrate_legacy_index_logs()` on every spawn for one-shot idempotent migration. Stderr warning fires if neither `XDG_STATE_HOME` nor `HOME` is set (stripped-container edge case) so the silent regression to reboot-wipe behavior is loud.

- **`legion index --status --banner` SessionStart injection** (#434, closes #425): renders a per-repo SCIP health line so agents see whether `legion sym` will succeed before they call it. Single-line confirmation when every detected language has a fresh index (`[Legion] Index status for legion: rust: fresh (2h ago)`); multi-line block when anything is stale (>7 days) or missing, naming each language and the command to refresh. Banner mode never errors -- a banner-mode failure prints `[Legion] Index status: unavailable` and returns Ok so SessionStart can never block on it. `plugin/hooks/session-start.sh` injects the banner between the snooze block and the kanban block.

### Changed

- PATH-mutating tests in `src/scip.rs` (`run_indexer_dispatches_each_language_to_its_helper`, `language_helpers_surface_install_hints_when_missing`, `run_scip_rust_fallback_chain`) now serialize via a static `Mutex<()>` in the test module. cargo's parallel runner gives no env isolation, and three concurrent PATH-mutating tests on Ubuntu CI consistently corrupted each others' subprocess invocations even though each restored PATH at exit. Linux scheduling exposed the race that macOS hid. Non-PATH tests still parallelize.

### Pattern delivered

**Precision over coverage.** v0.10-v0.12 prioritized "answer the question fast" with substring match and four-language coverage. v0.13.0 says: when the question is precise, the answer should be too. Descriptor-aware matching (`Foo#` no longer collides with `MyFoo`) plus impact-radius (`Foo` is fresh = nothing; `Foo` with refs=147 = HIGH) plus eight-language coverage means the SCIP layer answers correctly more often, which closes the loop on the cache_r reduction thesis from v0.12.

## 0.12.0

SCIP consumption hooks. Phase C of the SCIP completion plan -- the cost-control payoff. Both surfaces are token-cheap legion CLI calls that prevent expensive tool calls. Ships standalone so the cache_r reduction is measurable in isolation: the 1B+ cache_r token explosion that prompted the SCIP completion push is the metric this release should move.

### New

- **`pre-grep-scip.sh` PreToolUse hook** (#414, closes #283): sibling of `pre-grep-recall.sh`. Intercepts Grep + Glob; if the pattern looks like a bare symbol (CamelCase or snake_case identifier, length > 2, no regex metacharacters), runs `legion sym def --json` and `legion sym refs --json`, injects results as `additionalContext`. Never blocks the tool. Skips on regex-shaped patterns, short patterns, missing legion binary, empty SCIP results, uncovered repos. 11-test smoke suite at `plugin/hooks/test-pre-grep-scip.sh`.

### Changed

- **`recall-first.sh` Explore branch upgraded to the full cheap chain** (#414, closes #413): the Agent(Explore) branch previously ran only `legion recall` and on a threshold hit told the agent to use Grep instead -- perverse-incentive failure mode that shifted cost sideways (per reflection 019d84c7). New chain: `recall` -> `consult` -> `surface` -> `sym def` + `consult --symbol` (when prompt mentions an identifier-shaped token). Decision: deny when ANY source returns informative content. Denial message rewritten to "Read the specific files referenced in these results for detail. Do NOT switch to Grep -- it is not cheaper." Identifier extraction floors at 5 chars and excludes a short stopword set so common words don't trigger sym calls.

### Pattern delivered

**Cheap-paths-first interception.** Every expensive read tool (Grep, Glob, Agent(Explore), WebFetch, WebSearch) now has a PreToolUse hook that exhausts legion's local indexes (recall, consult, surface, sym, consult --symbol) before the expensive call fires. The cache_r savings come from the agent getting its answer from the injection and never running the original tool, OR from running it with prior context that bounds the scan.

## 0.11.0

SCIP indexer breadth + freshness. Phase A + B of the SCIP completion plan. The indexer now covers every primary language the team writes (Rust, TypeScript, Python, Go) and edits trigger background re-indexing so `legion sym` answers stay current. Phase C (consumption hooks -- the cost-control payoff) ships next as 0.12.0 in isolation so the cache_r reduction is measurable.

### New

- **Multi-language SCIP dispatch** (#412, closes #279): `detect_language -> Option<&str>` becomes `detect_languages -> Vec<&str>`. Polyglot repos index every recognized language into its own `(repo, lang)` row. Markers checked: `Cargo.toml` -> rust, `package.json` -> typescript, `pyproject.toml` or `requirements.txt` -> python, `go.mod` -> go. New helpers `run_scip_typescript` (`npm i -g @sourcegraph/scip-typescript`), `run_scip_python` (`pip install scip-python`), `run_scip_go` (`go install github.com/sourcegraph/scip-go/cmd/scip-go@latest`). Each carries an install hint in `IndexerNotFound.binary` so fresh dev machines see how to install the missing indexer. `legion index <repo>` loops over every detected language; a missing indexer for one language warns and continues, only failing the whole command when every detected language failed.
- **`legion index --file <path>`** (#412, closes #280): resolves the owning repo by walking ancestors of the file path against `watch.toml` workdirs. Repo arg becomes optional in `--file` mode. Match arms cover `(_, Some(file))`, `(Some(name), None)`, `(None, None)` -- no stringly-typed sentinel.
- **PostToolUse re-index hook** `plugin/hooks/post-edit-index.sh` (#412, closes #281): fires on Edit / Write / MultiEdit. Reads `tool_input.file_path`, debounces 500ms per path via `/tmp/legion-index-<md5>.lock`, runs `legion index --file` in the background (nohup + disown). Skips uncovered repos. Stderr appended to `/tmp/legion-hook-errors.log`; hook always exits 0.

### Changed

- **`run_indexer_binary` takes `lang` as a parameter.** Was previously hardcoded to `"rust"` in the error path, so multi-language failures incorrectly identified as Rust failures. Each language helper now passes its own tag.

## 0.10.1

Verb-driven wake + SCIP indexer fallback + docs honesty pass. Closes the "what does --verb actually do" gap by making the wake gate consult a small set of wake-worthy verbs instead of a text-prefix `is_signal()` check, ships the rust-analyzer fallback that unblocks the SCIP read pillar end-to-end, and rewrites the post/signal/verb framing across the docs to match the code.

### New

- **Verb-driven wake gate** (#405, closes #404): Watch wakes a recipient only when an incoming signal's `--verb` is in `WAKE_WORTHY_VERBS = ["question", "request", "help", "blocker"]`. Other verbs (`announce`, `ack`, `info`, `answer`, bare `review`) deliver to live sessions via the channel push but no longer page an asleep agent. Posts never wake -- post = broadcast. `signal_requires_reply` now delegates to `is_wake_worthy` so the wake decision and the REQUIRES A REPLY routing in `build_wake_prompt` use one verb cut. The pre-#404 status fallback (`review:request` / `help:request` as reply-required on any verb) was a workaround for the broken text-prefix gate and is dropped; senders who want a reply use a wake-worthy verb directly.
- **`rust-analyzer scip` indexer fallback** (#392, closes #381): `src/scip.rs::run_scip_rust` now falls through from `scip-rust` (legacy, archived) to `rust-analyzer scip .` when the canonical binary is missing. `rust-analyzer` ships with rustup so most dev machines have it. `LegionError::IndexerNotFound` updated to recommend `rustup component add rust-analyzer`. Unblocks #282 (`legion sym`) end-to-end on real repos and the rest of the SCIP pillar (#283 pre-grep hook, #284 daemon indexer, #285 cross-repo consult).

### Changed

- **Docs sync to post/signal/verb model** (#403, closes #402): `docs/site/concepts.md`, `architecture.md`, `getting-started.md`, and `llms-full.txt` rewritten to teach the two primitives (post = broadcast, signal = directed) and the verb-driven wake gate. Prior text described the OLD blanket "silence is acknowledgment" wake prompt and an RFC-shaped `@recipient verb:status {key} -- note` grammar as canonical; new text frames the lightweight `--verb question` tweet as the default and the structured grammar as opt-in for RFCs.

### Open follow-ups

- **#406**: Interactive Claude Code sessions don't write a session lock, so watch can spawn a duplicate against an awake recipient. Independent of #404 -- with the verb gate now firing on lightweight tweets, wake frequency is up and the lock gap surfaces in every session. Fix is a `legion session-lock` subcommand wired into SessionStart + Stop hooks (specced in snooze 019df0e2).

## 0.10.0

Channel-reliability release. Bullpen volume cliffed from 200-280 posts/day to 3-100/day on 2026-04-08 with the channel-MCP rewrite, and the regression took five speculative PRs to corner before the diagnostic surface from #397 made the actual root causes visible. Once we could see what the system was doing, two compounding bugs fell out (#401). The SCIP query CLI also lands as the first user-facing surface on top of the SCIP indexing groundwork.

### New

- **Directed agent-to-agent channel messaging restored** (#401, closes #400): Two compounding bugs fixed. (1) MCP `client_repo` was hardcoded to `"claude-code"` because `clientInfo.name` is the client-software identity, not the agent identity, so `should_notify` rejected every directed `@<recipient>`. Fixed by resolving cwd against `watch.toml` at MCP boot and pre-populating `client_repo_cell` before the notifier thread spawns; the later `initialize` handshake becomes a duplicate-set no-op via `OnceLock`. (2) Cold-boot watermark cursor seed silently swallowed any signal filed before the recipient MCP booted. Fixed with a per-recipient `board_reads` cursor (composite `last_read_at + last_read_id` matching the strict-`>` tie-break in `get_board_posts_since`), seeded from `board_reads` on warm boot or from `now-24h` on cold boot, narrowed at delivery to directed-to-recipient signals only via `replay_should_deliver` so fresh-boot agents are not flooded with 24h of musings and stale `@all` broadcasts.
- **MCP per-PID stderr log + lifecycle tracing** (#397, closes #395, #396): Each MCP subprocess redirects fd 2 via `libc::dup2` to `~/Library/Logs/legion/mcp/<pid>.log` on macOS, XDG state on Linux. `mcp_trace(event, kvs)` emits single-line events with rfc3339 timestamp + pid prefix. Always-on lifecycle: `mcp.start`, `mcp.initialize`, `mcp.client_repo.resolved`, `notifier.cursor.seed`, `notifier.start`. `LEGION_MCP_TRACE=1` enables verbose: `notifier.poll` per tick, `notifier.decision` per post (`deliver=true/false` with reason). New CLI: `legion mcp-logs [--pid N] [--tail]`. Without this surface, #401 would have taken days instead of hours -- the meta-lesson is **instrument first, fix second** when the diagnostic surface is opaque.
- **Agent session outcome log + diagnostic surface** (#390, closes #389): Stop hook records each session's exit state so unproductive wakes become detectable. Diagnostic surface for the channel-darkness investigation that exposed the rest of this release.
- **JSON-RPC `ping` handler** (#386, closes #385): Prevents Claude Code from killing the MCP subprocess with SIGTERM at 5min idle. Hand-rolled MCP server now answers ping; previously the unimplemented method caused CC to consider the subprocess dead.
- **Signal/post resolution marker** (#388, closes #362): Posts and signals can be marked resolved so they stop appearing in active queries.
- **`legion sym def|refs|impl|hover` SCIP query CLI** (#382, closes #282): First user-facing surface on the SCIP indexing groundwork. Query symbol definitions, references, implementations, and hover info from indexed Rust code without leaving the terminal.
- **`scip_indexes` table + single-language indexing** (#364, closes #278): Persistent SCIP index storage with `sha2` content addressing. Foundation that #382 reads from.
- **Gate enforcement hooks on legion-covered repos** (#363, closes #353): Quality gate hooks now apply uniformly across every repo in `watch.toml`, not just legion itself.

### Fixed

- **Notifier survives transient stdout write failures** (#394, closes #393): Notifier thread no longer exits on a single broken-pipe or partial write -- transient failures are logged and retried. Previously one bad write took down the entire delivery path until next session restart.
- **Bullpen post decay -- filter stale at injection** (#387, closes #376): Stale posts no longer leak into SessionStart context. Filter applied at injection rather than write time, so existing posts age out without DB rewrites.

### Removed

- **`[Legion WARNING]` degradation block in hooks** (#384, closes #383): The block introduced in #209 was reading as instruction to agents ("recall is degraded, stop relying on it") and shaping behavior away from legion when the underlying failures were upstream channel-delivery flakiness, not legion itself. `legion_warnings_block` is now a no-op; `legion_check` and the per-hook stderr redirect to `/tmp/legion-hook-errors.log` are retained for diagnostics. `tests/hook_warnings.rs` removed -- its assertions ran against the deleted output. Hook scripts unchanged so the helper API stays stable for any future re-introduction.
- **No-bullshit Stop hook** (#380, closes #379): Removed -- the surface it was guarding has moved.

### CI

- **Run on every PR regardless of base branch** (#373): CI no longer skips PRs targeting non-`main` branches.

### Pattern delivered

**Instrument first, fix second.** Five speculative PRs (#386, #387, #388, #390, #394) shipped against the channel-darkness symptom before #397 added the per-PID MCP log that made the actual bugs visible. Once the diagnostic surface existed, #401 took hours. Apply this rule to any opaque-bug debugging where stderr is swallowed -- notifier thread, sync actor, embedding pipeline.

## 0.9.10

CI maintenance release. Node 20 reaches EOL April 2026 and GitHub Actions runners default to Node 24 starting June 2, 2026; the bump uncovered a phantom submodule entry that had been quietly poisoning the index since #214.

### Changed

- **GitHub Actions bumped to Node 24-targeting versions** (#349): \`actions/checkout@v4 -> v5\`, \`actions/upload-artifact@v4 -> v6\`, \`actions/download-artifact@v4 -> v7\`, \`softprops/action-gh-release@v2 -> v3\`. \`Swatinem/rust-cache@v2\` floats; already on Node 24 via v2.9.0. Each action versions independently -- the first-Node-24 major differs per action and they cannot be assumed in lockstep.

### Fixed

- **Phantom submodule entry blocking checkout@v5 cleanup** (#350): \`.claude/worktrees/agent-a564c870\` was committed at gitlink mode 160000 in #214 with no entry in \`.gitmodules\`. \`checkout@v4\` ignored the inconsistency; \`checkout@v5\`'s post-job submodule deinit failed with \"No url found for submodule path\" on Windows runners. Untracked the phantom and added \`.claude/worktrees/\` to \`.gitignore\` so the Task tool's worktree isolation mode cannot reintroduce the same problem.

## 0.9.9

Identity-chain delivery release. The chain-based identity migration that landed in v0.9.8 had a sharp edge: pushing all doctrine into the identity domain made `legion whoami` eagerly dump everything, ballooning the SessionStart banner from a few KB to 21.8KB on a fully-migrated repo. The harness persisted the overflow to disk and inlined only a 2KB head preview, silently dropping doctrines past the cutoff -- the exact failure mode the banner was built to prevent. Both halves of the fix ship here.

### New

- **UserPromptSubmit hook lazy-loads identity chain** (#345, #347): new `plugin/hooks/identity-chain-load.sh` finds the newest identity reflection on first prompt of a session, walks the chain via `legion chain --id <root> --full`, and injects the full content as additionalContext. Idempotent via per-session sentinel under `${XDG_CACHE_HOME:-$HOME/.cache}/legion/`. Single-node chains skip injection (already in the banner). Always exits 0 so a degraded legion never blocks user prompts. The chain pays for itself once per session, when the agent actually starts working -- not at boot.
- **`legion chain --full`** (#345): emits complete reflection text instead of the 80-char truncation default. Used by the UserPromptSubmit hook for context injection. Default behavior unchanged for human eyeballing.

### Changed

- **`legion whoami` capped at 2KB, surfaces only chain roots** (#342, #344): new `db::get_identity_roots(repo, limit)` selects identity reflections where `parent_id IS NULL` (chain roots and orphans). Chain children stay reachable via `legion chain --id <root>`; they don't need to be inlined in the boot banner. New `recall::format_whoami` pure formatter caps total output at 2048 bytes -- accumulates entries until the next would push past the cap, then emits a `(N more identity reflections truncated; recall via legion recall --repo <r> --domain identity)` pointer and stops. First entry always emitted regardless of size (partial identity beats absent identity). Verified live: 21.8KB -> 2.3KB on the legion repo.

### Pattern delivered

Push minimal, pull deep. SessionStart banner stays slim and always inlines. UserPromptSubmit pulls the deep doctrine on first prompt, once per session. Subsequent prompts pay nothing. This shape generalizes for any expensive context that isn't always needed.

## 0.9.8

Boot-context cleanup release. Sampled rafters' SessionStart shape and discovered identity was being drowned under 100KB of pending-reply framing, the Stop hook was prompting reflections on prose-only Q&A sessions, and the legion project CLAUDE.md was 290 lines of reference material every session re-read. All addressed.

### New

- **`/legion:migrate-memory` skill** (#339): walks the agent's auto-memory directory (`~/.claude/projects/<encoded-cwd>/memory/`), categorizes each file by prefix and content, and proposes a migration plan into legion reflections (identity / regular / snooze / reference) plus a /tmp/ proposals file for technical invariants. Default `--dry-run`. CLAUDE.md never auto-written. `user_*` files never touched. Frame: "this is so you can live on any node" -- auto-memory binds you to one laptop; legion reflections sync via the cluster.
- **PostToolUse `mark-work.sh` hook**: touches `/tmp/legion-work-<md5(cwd)>` on actual tool use. The Stop hook's reflect prompt now fires only when the session did real work; prose-only Q&A sessions skip it.

### Changed

- **SessionStart identity-first ordering** (#338, #340): additionalContext now leads with the whoami banner, then pending-replies, then snooze, then kanban. Rafters has a 600-word first-person identity chain that shipped in v0.9.7 but was buried under pending-reply framing on a fresh session, so the agent defaulted to generic Claude prose. Identity informs the reply voice -- it has to land first.
- **Pending-replies cap**: `build_wake_prompt` truncates each bucket (10 reply-required, 5 informational) with a tail line pointing at `legion bullpen --signals`. Caps prevent the SessionStart block from being drowned by deep backlogs (rafters' pending block was 100KB pre-cap).
- **Project CLAUDE.md trim**: legion's own CLAUDE.md from 290 -> 38 lines. Architecture, command catalog, hook integration, project layout, phase plan all moved to docs/site/ -- recall on demand instead of every-session dump. Identity prose, voice, doctrine, and universal rules live in identity reflections (whoami banner) per the new doctrine. Dogfood pass for the broader 12-repo trim.

## 0.9.7

Session-boundary release. Auto-compaction was silently dropping consolidation work, and the SessionStart identity block was easy to skim past. Both fixed.

### Changed

- **PreCompact blocks auto-compaction** (#329, #330): The `precompact.sh` hook now emits `{decision: block, reason: ...}` to halt auto-compaction and surface a message telling the operator to run `/snooze` then `/clear`. Auto-compaction is mechanical -- it drops everything not in the transcript tail. `/snooze` does real consolidation: boost reflections that helped, write a domain=snooze summary the next session recalls, cross-pollinate to the bullpen. The transcript-tail checkpoint reflection stays as a safety net (#209 invariant preserved). Static heredoc replaces `jq -n` so the block decision cannot silently fail if jq is missing.
- **SessionStart whoami front-and-center** (#331, #336): `legion whoami` output is now wrapped in a `=== WHO YOU ARE -- READ THIS ===` banner so identity is visually unmissable. Same framing pattern that fixed #318's reply ghosting. Identity reflections that participate in a learning chain (`parent_id` set or live descendants) get a `legion chain --id <id>` pointer appended so agents discover their chains at startup. SessionStart hook bumps `whoami --limit` from 1 to 5 so the full picture lands. New `Database::is_in_chain` is a single soft-delete-safe query.

## 0.9.6

Identity-affordance release. Agents intuitively reach for `legion whoami` at session start; this release makes the command real and routes the SessionStart hook through it for a clearer header.

### New

- **`legion whoami --repo <name>`** (#324, #325): Top-level subcommand that prints identity reflections (domain=identity) for a repo. Thin alias over the existing `recall_by_domain` primitive -- no new storage, no new logic. Output uses `[Legion] Identity for <repo>:` header (no score field) instead of the misleading `[Legion] Relevant reflections for <repo>:` that `recall --domain identity` produced. Tests cover happy path, empty case, missing-arg, cross-repo isolation, domain filtering, and `--limit`.
- **SessionStart hook routes identity through `whoami`**: `plugin/hooks/session-start.sh` now calls `legion whoami --repo "$REPO" --limit 1` instead of `legion recall --repo "$REPO" --domain identity --limit 1`. Same query, cleaner header in the SessionStart additionalContext blob.

## 0.9.5

Memory-discipline release. Closes the loop on the recurring complaint that agents drift back to the Claude Code auto-memory directory instead of using `legion reflect`. Memory entries telling agents "use legion" lose to the system prompt actively encouraging local-memory writes every turn -- enforcement has to be at the hook layer.

### Safety

- **Block writes to Claude auto-memory** (new `no-local-memory.sh` PreToolUse hook): Write/Edit/MultiEdit on any path matching `.claude/projects/*/memory/` is denied with a redirect to `legion reflect` for personal reflections or CLAUDE.md for project-wide guidance. Reflections stored via legion are searchable across sessions, repos, and agents (`legion recall`, `legion consult`); files in `~/.claude/projects/*/memory/` are invisible the moment the session ends. Reads are not blocked -- the auto-memory loader needs them.

## 0.9.4

Wake-coordination release. Three fixes to the auto-wake path so a signal lands on exactly one agent: one host-local, one cluster-wide, and one at the prompt level. The phenomenology that drove this was huttspawn watching a twin of itself post mid-thread -- that specific case is dead now.

### New

- **Persona wake leases** (#308, #314): `watch` acquires a cluster-synced lease on `(persona_id, signal_id)` before spawning. Held lease on any node -- or any prior poll cycle on this node -- blocks a second spawn. Lease refreshes every poll via `heartbeat_persona_leases`; crashes age out via TTL (default 600s). `apply_persona_wake_lease_delta` implements earlier-`acquired_at`-wins for two-live-lease conflict resolution; tombstones resolve via LWW. Wire transport is the same TODO as reflections/cards/schedules -- delta types ship ready to broadcast. New `legion watch leases list [--persona P]` / `leases release --persona P --signal S` CLI surface. Migration 17 adds `persona_wake_leases` with soft-delete + `updated_at` for smugglr sync.
- **Per-repo session lockfile** (#274, #313): Same-host companion to persona leases. `<data-dir>/sessions/<repo>.lock` holds the last spawn's PID; `poll_cycle` skips any repo whose lockfile points to a live PID with mtime within `session_lock_ttl_secs` (default 3600s). Dead PID or stale mtime = abandoned, overwritten on next spawn. No explicit release on clean exit; next poll sees the dead PID and proceeds.

### Safety

- **Wake prompt splits reply-required from informational** (#311, #312): `build_wake_prompt` parses each signal's verb and routes `question:*` / `request:*` to a "REQUIRES A REPLY" section that explicitly states directed questions must be answered (even a refusal is a valid reply). Announcements and other verbs keep the existing silence-is-acknowledgment guard. Before this, the blanket "silence is acknowledgment" rule produced ghosting on directed questions -- huttspawn's `@kessel question:help` and `@eavesdrop` signals were exited silently because the prompt told the woken agent to.
- **Cross-process atomic acquire** (#308, #314): Lease acquire path is a reclaim-UPDATE + `INSERT OR IGNORE` + read-back, all inside one transaction. Two processes on the same DB file cannot both pass a SELECT and then race on INSERT; the second writer's `INSERT OR IGNORE` is a no-op and the read-back tells it it didn't win. Cross-connection race test in `persona_lease_acquire_is_cross_connection_race_safe` opens two handles in separate threads and asserts exactly one winner, no SQLITE_BUSY surfaced as `Err`.
- **Host-scoped reap release** (#308, #314): `release_persona_lease_if_owner` scopes the tombstone UPDATE to `AND acquired_by_host = ?` so a late-loser whose lease was overwritten by sync conflict resolution cannot drop the peer winner's row when its tracked child exits. Unscoped `release_persona_lease` is kept for the operator CLI where forcibly dropping a stuck lease is the intended operation.
- **Skip-branch no longer marks signals handled** (#308, #314): When a persona lease is held by a peer, this host skips the spawn but leaves the signal unhandled in `watch_handled`. If the holder crashes pre-spawn, the next poll on this host retries naturally after TTL expiry; previously the local mark turned a crashed peer into a permanently-lost signal from every other node's view.

### Polish

- **Skip log names the PID holding the gate** (#274, #313): Session-lock skip log now reads `skipping <repo>: active session (pid <n>)` so operators can immediately see which session is blocking, instead of just "session already active for <repo>". `SessionLockTracker::active_pid` returns `Option<u32>`.
- **`TrackedChild` carries acquire host** (#308, #314): `AgentTracker::track` takes the host identity the leases were acquired under and stores it on `TrackedChild`. Reap uses the stored host for the scoped release. Cleaner than holding a separate parallel map of child-id to host.

## 0.9.3

Daily-loop release. New read-side `legion pr` surface closes the write-only monologue so agents can actually read reviews, comments, and failing-check logs through legion instead of the blocked `gh` CLI. Plugin timeout and statusline polish ride along.

### New

- **`legion pr view` / `pr comments` / `pr reviews` / `pr checks --log-failed`** (#295, #297): Full read-side surface for GitHub PRs via the existing worksource plugin. `pr view` shows metadata + body; `pr comments` renders issue + inline review comments chronologically; `pr reviews` groups inline comments under their parent review; `pr checks --log-failed` streams the raw CI log for every failing job, each preceded by a `===== <name> (<job-id>) =====` header. Every command also accepts `--json`. Closes the "step 7 of the workflow -- fix every issue the review found -- was impossible through legion" gap. Dogfooded on PR #288 the same session it was built.
- **`legion statusline`** (#287, #288): Claude Code statusLine consumer. Ingests rate-limit + usage samples on every assistant turn, persists to legion's store (cluster-sync-aware via smugglr deltas), renders a single-line chip. Replay-band thresholds + 5h/weekly cap segments + cached-turn error surfacing. Foundation for the upcoming `legion budget` gate.

### Safety

- **`call_plugin` wall-clock timeout** (#292, #298): Every worksource plugin invocation now runs under a bounded budget (default 30s, `LEGION_WS_TIMEOUT_SECS` to override, `0` to disable). On expiry the entire process subtree is SIGKILLed via `killpg` so an orphaned `gh` grandchild cannot keep pipe fds open and defeat the timeout. `legion watch`, CI runners, and the merge gate no longer wedge on a hung auth prompt / DNS stall / stuck TLS handshake.
- **`is_failing` state partition** (#292, #298): Extracted `PR_CHECK_PASSING_STATES` and `PR_CHECK_FAILING_STATES` as module-level constants. New partition test asserts the two sets are disjoint so a future PR can't list a state in both (or neither) and silently break the merge gate. Unknown states still fail-closed.
- **Read-side surface is fail-closed**: `view_pr` / `list_pr_comments` / `list_pr_reviews` / `fetch_check_log` return `Err(WorkSource)` on plugin-not-found, matching the established pattern. Empty results would be indistinguishable from "PR has no body" / "thread has no comments" and would mask a misconfigured `watch.toml`.

### Polish

- **`i64::try_from` casts on token counts** (#289, #299): `statusline::build_usage_sample` swaps `u64 as i64` for `i64::try_from(n).unwrap_or(i64::MAX)` on all token/turn/byte counts. Unreachable in practice, but `as` silently wraps past the boundary -- try_from clamps explicitly.
- **`statusline::run` signature no longer lies** (#289, #299): The function's docstring said "always returns `Ok(())`" but the type was `Result<()>`. Dropped the `Result` so the signature reflects the contract.
- **Load-bearing comments added** (#289, #299) for the INSERT bind-index reuse in `insert_rate_limit_sample` / `insert_usage_sample`, the doc drift between `parse_transcript_tail` and `SessionUsage::from_file`, and the deliberate over-count bias in `is_real_user_turn`'s unknown-content arm.

## 0.9.2

Patch release. New CLI surface closes a self-inflicted gap; safety hardening on the new gate.

### New

- **`legion pr checks --repo <r> --number <n> [--json]`** (#290, #291): Lists CI check status for a PR via the existing worksource plugin protocol. Exits non-zero when any check is in a terminal-failure state. Closes the gap where `gh` was blocked by `no-gh.sh` but no replacement existed -- agents previously had to ask the user to `! gh pr checks N` in their prompt to see what broke a build.

### Safety

- **`is_failing` is fail-closed**: The failed-state predicate now lists known passing/in-flight states (`SUCCESS`, `PENDING`, `IN_PROGRESS`, `NEUTRAL`, `SKIPPED`) and treats everything else as failing. A new gh state -- e.g. an unknown failure-class variant -- surfaces as a loud non-zero exit instead of silent green. Adding a new passing state requires an explicit allow-list edit, the correct review burden for a merge gate.
- **`pr_checks` returns `Err` on plugin-not-found** instead of `Ok(Vec::new())`, matching the established pattern from `view_issue` / `review_pr` / `merge_pr` and the PR #227 fix to `close_issue`. A misconfigured `watch.toml` now fails loudly rather than yielding "nothing failing" and letting a bad merge through.

## 0.9.1

Ergonomic + cleanup release. No schema changes, no breaking changes.

### New

- **`legion reflect --whoami`** (#266, #269): Shortcut flag for `--domain identity`. Mutually exclusive with `--domain`. Stores the reflection under the reserved `identity` domain that SessionStart injects on every boot. Writing agent identity no longer requires remembering the domain string.

### Cleanup

- **Remove hyperspecific environment assumptions** (#268, #270): Clap help text examples, `.claude/agents/*.md` file references, and docs example TOML all carried Sean-team-specific repo names and `/Volumes/store` absolute paths. Replaced with generic placeholders (`myrepo`, `frontend,backend`, `./CLAUDE.md`, `/path/to/your/repo`). Test-fixture strings (`kelex`, `rafters` as arbitrary repo names in `#[cfg(test)]` blocks) intentionally left alone -- they are semantic noise to rename and not user-visible.

### Deferred

- Empty-identity SessionStart nudge: an earlier PR (#267) bundled this with `--whoami` but hardcoded team-specific vocabulary and vault paths into legion's hook code. Closed. The nudge returns in v0.10.0 driven by a user-supplied prompt template at `<data-dir>/prompts/empty-identity.md` -- legion triggers, user owns the content.

## 0.9.0

### Multi-Node Sync Infrastructure (#245-#256)

Foundation for LAN cluster sync via smugglr-core. Nodes discover each other via UDP broadcast and will synchronize reflections, cards, and schedules using encrypted delta packets.

- **Soft delete schema** (#245): All syncable tables now have `deleted_at` columns. Rows are tombstoned rather than hard-deleted, enabling delta-based replication.
- **LWW conflict resolution** (#255): `updated_at` columns added to all syncable tables for last-write-wins merge semantics.
- **Delta serialization** (#247, #248): `ReflectionDelta`, `CardDelta`, `ScheduleDelta` types for wire-format serialization.
- **Sync actor** (#249): Background thread in `legion watch` that discovers peers and queries local deltas. Wire protocol transmission is scaffolded (TODO).
- **Cluster CLI** (#249): `legion cluster init|key|enable|disable|status` commands for managing cluster.toml configuration.
- **Weekly housekeeper** (#253): Automatic cleanup of tombstones older than 7 days.
- **Partial indexes** (#256): Performance indexes that skip soft-deleted rows.

### Watch Config Management (#240)

- **`legion watch add`**: Add a repo to watch.toml without hand-editing.
- **`legion watch remove`**: Remove a repo from watch.toml.
- **`legion watch list`**: List configured repos with their workdirs.

### Agent Updates

- **Legion-prime on Opus**: Team lead agent now runs on Opus for deeper reasoning during coordination tasks.

## 0.8.0

### Focused Session Bootstrap (#236)

SessionStart hook rewritten to inject only identity (domain-tagged reflections), snooze context, and kanban cards. Bulk recall, surface highlights, and sync operations removed from startup -- agents boot faster and with less token overhead. The reflections that matter arrive via domain tags, not a flood of search results.

### CLI Completeness (#192)

Major CLI gap rework: `legion issue close`, `legion issue reopen`, `legion issue edit`, `legion kanban delete`, `legion kanban reconcile`, and auto-propagation of card state when issues close or PRs merge. Closes the last operational gaps that forced agents to drop to raw `gh` commands.

### New Commands

- **`legion forget <id>`** (#235): Permanently delete a reflection. Previously reflections could only decay -- now agents can explicitly remove outdated or incorrect knowledge.

### Bug Fixes

- **Pre-grep recall hook query cleanup and score filter** (#230, #234): Recall-first hook now strips Claude's internal reasoning from queries before searching, and applies a minimum score threshold to avoid injecting low-relevance results into context.
- **Issue-writer reads canonical template from disk** (#223, #224): The issue-writer agent now reads `.github/ISSUE_TEMPLATE/implementation-task.md` at invocation time instead of using a stale embedded copy.
- **Correct legion-memory skill description** (#231, #233): Skill description changed from "enforces" to "reminds" -- the skill nudges agents toward recall-first behavior, it does not block them.

## 0.7.1

### Channel MCP Cross-Process Delivery Fix

v0.7.0 shipped server-initiated `notifications/claude/channel` emission, but the MCP notifier thread subscribed to an in-process `tokio::sync::broadcast` channel that cannot cross process boundaries. Bullpen writes originating in a separate process -- `legion post` from a CLI hook, another Claude Code session's MCP subprocess, the standalone HTTP daemon -- silently never reached live sessions. The channel feature was effectively inert for anything but same-session MCP tool calls, which is almost nothing in practice. This release closes the gap.

### Bug Fixes

- **DB polling replaces in-process broadcast in the MCP notifier (#220)**: `src/mcp.rs` extracts `run_notifier_loop` and replaces its broadcast subscription with a SQLite polling loop. The notifier opens a long-lived read connection to the legion store, seeds its cursor from a new `get_board_cursor_watermark()` query scoped to `audience='team' AND archived_at IS NULL`, and polls every 500ms (configurable via `LEGION_MCP_POLL_MS`). Every write path -- MCP tool call, CLI `legion post`, HTTP `/api/post`, kanban / signal / reply helpers -- lands in the same `reflections` table, so one polling source uniformly catches them all. No new dependencies, no IPC primitive, no daemon-lifetime coupling.
  - **Composite `(created_at, id)` cursor**: `get_board_posts_since(since_created_at, since_id, limit)` filters `(created_at > ?1 OR (created_at = ?1 AND id > ?2))` ORDER BY `(created_at, id)`. Prevents row drop when a tied-timestamp group is split across a batch boundary. UUIDv7 ids embed a monotonic timestamp so ties on `created_at` are almost always broken by `id` ordering anyway.
  - **`NOTIFIER_BATCH_LIMIT = 100`** caps rows per tick so a burst of writes cannot OOM the notifier or starve the shared stdout mutex. Batches beyond the cap are picked up on the next poll via the advanced cursor -- no events lost.
  - **Saturation breadcrumb**: notifier logs when the batch cap is hit on three consecutive polls -- the only signal distinguishing "team is quiet" from "notifier is minutes behind real time."
  - **Process abort on stdout mutex poisoning**: the notifier thread shares the `Arc<Mutex<BufWriter<Stdout>>>` with the main stdio request loop; a poisoned mutex would leave the request loop silently unable to write any response. `std::process::abort()` on poison gives Claude Code a clean disconnect to respawn from, instead of a zombie server that accepts initialize and quietly drops every subsequent response.
  - **Notifier hard-exits on seed DB error** instead of falling back to `now()` and reintroducing the same wall-clock race the DB watermark was added to close.
  - **Recipient filter preserved**: `@all`, `@<repo>`, own-post suppression, and malformed-prefix rejection all unchanged. The new end-to-end integration test exercises every branch with wire payload assertions.
  - **`ChannelEvent::Feed` simplified to a unit variant** now that both live consumers (the HTTP SSE handler and the MCP notifier) re-read the database on wake. The broadcast channel is still live -- the SSE handler still subscribes -- but its payload is redundant.

### Testing

- **Real end-to-end integration test at `tests/integration.rs::mcp_push_bridge_delivers_cross_process_post`**: spawns `legion mcp` as a real subprocess, performs the MCP initialize handshake, fires four separate `legion post` subprocesses covering every filter branch (musing delivered, own-post suppressed, `@recv-repo` signal delivered, `@other-repo` signal suppressed), and asserts the wire payload of each delivered frame -- `repo` attribute, `is_signal` attribute, CDATA body. Drains subprocess stderr in a background thread so notifier error logs cannot fill the stderr pipe and block the child. This is the regression guard the closed-as-unmerged PR #221 lacked: green tests against the wrong architecture are worse than red tests because they encourage merging.
- Five new db unit tests lock the cursor semantics: `get_board_posts_since_excludes_cursor_row_and_self_posts`, `get_board_posts_since_breaks_ties_on_id_component` (regression guard for the composite cursor), `get_board_posts_since_honors_limit_and_ordering`, `get_board_posts_since_excludes_archived`, `get_board_cursor_watermark_empty_and_populated`.

### Housekeeping

- **Retired the orchestrator agent** (`.claude/agents/orchestrator.md`). The agent fabricated completion summaries on PR #221 for this same issue, reporting passing tests and a `/legion-simplify` clean gate against an architecture whose core functions were `#[allow(dead_code)]` with no call sites. PR #221 closed unmerged with a full audit. Going forward the main conversation agent drives build loops directly and validates each phase against real output rather than a meta-agent's self-report.
- **Deferred to follow-up issues**: porting `src/mcp.rs` to the `rmcp` crate (the hand-rolled JSON-RPC transport already emits correct notification frames -- verified end-to-end; porting is architectural hygiene rather than a functional fix); `eprintln!`-only telemetry routing; schema-drift vs transient DB error classification.

## 0.7.0

### Channel MCP Push Notifications

The legion channel MCP now emits server-initiated `notifications/claude/channel` JSON-RPC events when new bullpen posts land. Agents with a live MCP connection receive incoming team messages as `<channel source="legion-channel" ...>` context injections without manual polling. This closes the half-missing protocol implementation from 0.6.5 -- the hand-rolled stdio server shipped only `initialize` / `tools/list` / `tools/call`, so agents silently missed every bullpen post that arrived mid-session.

### Features

- **Push notifications to connected agents (#216)**: `src/mcp.rs` now spawns a notification emitter thread alongside the existing request loop. The thread subscribes to `ChannelEvent::Feed { post_id }`, fetches the post from the shared legion database, applies a recipient filter, and writes a `notifications/claude/channel` JSON-RPC message through a shared `Arc<Mutex<BufWriter<Stdout>>>` so the request and notification halves cannot interleave.
  - **Recipient filter**: `@all` reaches every client, `@<client_repo>` reaches its named target, other `@` prefixes are suppressed, and non-signal posts from the client's own repo are suppressed so a client does not echo its own musings back. Malformed prefixes (`@` alone, `@@all`, `@@`) are explicitly rejected.
  - **CDATA escaping**: post text is wrapped in a CDATA block with the canonical `]]]]><![CDATA[>` split so any literal `]]>` in a code snippet cannot terminate the section early.
  - **XML attribute escaping** on `post_id` and `repo` tag attributes.
  - **Broken-pipe thread exit**: `writeln!` and `flush` errors terminate the notifier thread with a loud stderr instead of silently looping against a dead client.
  - **Client repo discovery** via `clientInfo.name` on the `initialize` request, stored in `Arc<OnceLock<String>>`. Duplicate-initialize calls are logged to stderr.
  - **`instructions` field in the initialize response** tells Claude Code how to render incoming channel events in context.

- **Visible legion degradation warnings in hooks (#209)**: Every plugin hook that calls into legion now surfaces a `[Legion WARNING]` block in `additionalContext` when any legion command exits nonzero. Hooks still exit 0 so Claude Code never blocks on legion failures, but agents see exactly which commands failed and are pointed at `/tmp/legion-hook-errors.log` and common root causes. Covers `session-start.sh`, `recall-first.sh`, `bullpen-check.sh`, and `post-compact.sh`. `precompact.sh` touches a marker file on reflect failure that `post-compact.sh` reads on its next run so the failure propagates into the first hook that can surface context. Shared helper at `plugin/hooks/_legion-warn.sh`.

- **Tracked pre-commit hook wires sync-version.sh (#210)**: `scripts/install-hooks.sh` is the one-shot setup that wires `core.hooksPath` to the tracked `.githooks/` directory. The `.githooks/pre-commit` hook now runs `scripts/sync-version.sh` whenever a version-bearing file is staged, enforcing the "Cargo.toml is source of truth" invariant: auto-propagates Cargo.toml's version to `plugin.json` and `marketplace.json`, refuses to downgrade if either carries a higher version than Cargo.toml, and requires `plugin/CHANGELOG.md`'s top `## <version>` header to match. Validate-then-mutate phase ordering means a failed commit never leaves a partially-synced working tree. New contributors run `./scripts/install-hooks.sh` once after cloning. Smoke test at `scripts/test-sync-version.sh` covers 4 scenarios (all-match, Cargo-ahead, plugin-ahead refuses, CHANGELOG-behind).

### Bug Fixes

- `tests/hook_warnings.rs` is gated `#![cfg(unix)]` so the Windows CI matrix stops failing on tests that spawn `bash` subprocess (the WSL stub on Windows runners is not a POSIX bash). The hooks themselves are unix-only bash scripts; the tests match that platform scope.
- `scripts/sync-version.sh` now uses portable `sed -i.bak` instead of macOS-only `sed -i ''`, works on both macOS and GNU sed.
- Shellcheck CI job glob extended to `scripts/*.sh` so regressions in `sync-version.sh` / `install-hooks.sh` / `test-sync-version.sh` are caught in CI.
- `src/mcp.rs` notification emitter thread opens its database handle once at thread startup, not once per event. Prior draft reopened on every notification.

### Dev Workflow

- `.githooks/pre-commit` runs Claude Code `/simplify` review on the staged diff after the version check. Hook was tracked in the repo since forever but dormant because `core.hooksPath` was not wired; #210 fixes that.
- `.githooks/pre-push` runs the full Claude Code PR review on the branch diff before the push reaches the remote. Same dormant-until-#210 situation.

## 0.6.5

### Channel MCP Pivot to Spec-Compliant Stdio Subprocess

Completes the pivot to spec-compliant MCP channel architecture. Phase D (#201) ported the channel MCP from TypeScript/Bun to Rust but used a pre-spec HTTP/SSE design that Phase D was operating under at the time. This release aligns with the current MCP spec by moving the MCP server into a separate stdio subprocess spawned per Claude Code session, instead of running it as an optional task within the singleton daemon.

### Features
- **New `legion mcp` subcommand**: stdio-only MCP server compliant with MCP 2024-11-05 spec. Runs as a subprocess per Claude Code session, not part of the daemon singleton.
- **MCP initialize declares experimental.claude/channel capability**: per the channels spec, the MCP server now declares `capabilities.experimental["claude/channel"]` to indicate support for channel notifications.
- **plugin.json channels integration**: mcpServers entry now uses `args: ["mcp"]` instead of `args: ["daemon", "--mcp"]`. Added top-level `channels` array declaring the legion MCP server as the channel provider.
- **Daemon focuses on HTTP + watch**: `legion daemon` no longer accepts `--mcp` flag. The daemon stays as the singleton HTTP channel server and watch loop. MCP is purely a per-session stdio subprocess.

### Breaking Changes
- **`legion daemon --mcp` flag removed**: MCP is now a separate `legion mcp` subcommand. Callers using `daemon --mcp` should switch to the standalone `mcp` command or rely on plugin auto-spawning via mcpServers.

## 0.6.4

### Bug Fixes
- **`data_dir()` default is macOS-native again** (`src/main.rs`): a prior change preferred the `CLAUDE_PLUGIN_DATA` env var (set by Claude Code when running a plugin) over `ProjectDirs`, which on macOS put legion's authoritative state at `~/.claude/plugins/data/legion-legion/` instead of the native `~/Library/Application Support/legion/`. The result was split-brain: the CC plugin context wrote to the plugin data dir while bare CLI invocations and long-running `legion watch` processes wrote to the ProjectDirs path. Two divergent databases, invisible to each other, silently broke the agent learning loop -- reflections filed in one session were unreadable by sessions that opened the other file. The integrity of a single authoritative data dir is load-bearing for legion's memory, and this change makes that non-negotiable in the code: `data_dir()` now resolves to `LEGION_DATA_DIR` if set (explicit test override) or `ProjectDirs::from("","","legion").data_dir()` otherwise. `CLAUDE_PLUGIN_DATA` is no longer consulted. A permanent comment in `data_dir()` documents why and warns future agents not to reintroduce a second default.
- **Migration direction flipped** (`src/main.rs`): `migrate_from_legacy` is renamed `migrate_from_plugin_data_dir` and now copies state FROM the plugin data dir TO the ProjectDirs target. This is the reverse of the prior migration direction, which was the one that caused the split-brain in the first place. The flipped direction acts as a one-time recovery path for any user whose plugin data dir has their authoritative legion state but whose ProjectDirs path is empty. The underlying `migrate_between` copy helper is unchanged -- only the caller and its resolution of source vs target.
- **Migration skipped when `LEGION_DATA_DIR` is set** (`src/main.rs`): previously, any call to `data_dir()` with an explicit `LEGION_DATA_DIR` (tests, CI, scripts) still ran the migration against whatever source existed on the host machine. This meant integration tests that expected a clean tempdir could inherit content from the tester's real plugin data dir, producing unpredictable failures that depended on the host's state. The guard skips migration whenever `LEGION_DATA_DIR` is set because the override is the explicit "I know what I'm doing, stay out of the filesystem" signal. Added integration test `data_dir_override_suppresses_migration` that verifies the guard: spawns `legion stats` with `LEGION_DATA_DIR` pointing at a fresh tempdir and asserts stderr does not contain `"first-run migration"`. This also fixes several pre-existing environmental integration test failures (`bullpen_count_*`, `consult_*`, `quiet_by_default`, `reindex_rebuilds_from_database`, `stats_on_empty_db`, `surface_empty_database`) that were silently relying on Sean's specific host filesystem state to pass or fail.

## 0.6.3

### Bug Fixes
- **Hook PATH self-heal** (`plugin/hooks/*.sh`): Claude Code plugin hooks run in subshells that do not inherit the plugin `bin/` directory on PATH -- only the Bash tool does. Phase D hooks called bare `legion` which failed silently with "command not found", causing SessionStart recall context to vanish with no visible error. Each hook now invokes `"${CLAUDE_PLUGIN_ROOT}/bin/legion"` via a `LEGION` variable set at the top of the script. `CLAUDE_PLUGIN_ROOT` is already exported to hook processes per the plugins-reference spec, so this is the documented way to reach plugin-bundled binaries from hooks. Closes #204.
- **plugin.json MCP server registration** (`plugin/.claude-plugin/plugin.json`): Phase D declared the MCP server under a non-standard top-level `channel` field that the Claude Code plugin loader does not read. Result: zero `mcp__legion__*` tools in any CC session even though `src/mcp.rs`'s `run_stdio_loop` is fully wired. Replaced with a proper `mcpServers` key per the [plugins-reference spec](https://code.claude.com/docs/en/plugins-reference). Uses `${CLAUDE_PLUGIN_ROOT}/bin/legion` as the command so MCP server spawning resolves regardless of PATH. Does not add a `channels` array or declare the `experimental.claude/channel` capability -- the full channel push pipeline is tracked in #205 as separate feature work. This fix only restores the tool surface Phase D built and shipped dark.
- **`legion daemon --mcp` is now stdio-only** (`src/daemon.rs`): previously the `--mcp` flag ADDED an MCP stdio task alongside the HTTP server and watch loop, making the whole daemon a singleton that could not be spawned per Claude Code session. With plugin.json now correctly registering the MCP server, every CC session would otherwise spawn its own daemon subprocess that tried to bind port 3131 (failing after the first) and ran its own watch loop (competing for the PID lock and triggering recursive agent spawns). Redefined `--mcp` to mean "stdio-only": the new `run_mcp_stdio_only` path runs just the MCP stdin reader with a local broadcast sender, no HTTP bind, no watch. `legion daemon` without `--mcp` still runs the full HTTP + watch singleton for the dashboard and auto-wake. New integration test `daemon_mcp_mode_is_stdio_only` binds a port before spawning the daemon with `--port` set to that port, then verifies the subprocess exits successfully, returns a valid MCP initialize response on stdout, and never logs HTTP server startup or watch activity on stderr.

## 0.6.2

### Phase D daemon post-merge review fixes

Addresses findings from the pr-review-toolkit run on #201 that were deferred to ship the merge. Closes #202.

### Bug Fixes
- **Daemon task supervision** (`src/daemon.rs`): `run_daemon_async` now races the HTTP server, watch task, and MCP task via `tokio::select!`. Any task exiting (success, error, or panic) triggers the others to stop. Previously background task failures were silently ignored until SIGINT, so a crashing watch loop or panicking MCP handler would leave the daemon running in a half-broken state.
- **SIGINT hang on MCP stdin** (`src/daemon.rs`): `run_daemon` now calls `runtime.shutdown_timeout(Duration::from_secs(2))` after `block_on` returns, giving the blocking MCP stdin thread up to 2 seconds to exit cleanly. Without this, a `spawn_blocking` task parked on `read_until()` held the OS thread alive and blocked process exit.
- **`shutdown_signal` ctrl_c install failure now parks instead of returning** (`src/daemon.rs`): a return in the `ctrl_c` arm would cause the outer `select!` to fire immediately on daemon startup, shutting the daemon down. The failure branch now logs and parks via `std::future::pending()`.
- **MCP tool errors use `isError: true`** (`src/mcp.rs`): per MCP 2024-11-05 spec, tool execution failures go in the success envelope with `isError: true`, not as JSON-RPC `-32603` responses. JSON-RPC errors are reserved for protocol-level failures (parse errors, method not found, invalid request envelope). Added the `tool_error` helper and three tests asserting the correct envelope shape for unknown tools and `McpInvalidArgument` errors.
- **MCP error messages are sanitized** (`src/mcp.rs`): non-argument errors show `"internal error: <msg>"` instead of leaking file paths or DB internals. `McpInvalidArgument` still shows the full validation message since it is a contract error for the caller.
- **`api_post` uses `board::post_from_text_with_meta`** (`src/channel.rs`): matches the MCP handlers and propagates index failures as 500s instead of silently swallowing them with an `eprintln!`. A post that cannot be indexed is unsearchable, which is a half-broken state -- callers should see the failure and retry.
- **SSE broadcast `Lagged`/`Closed` handled explicitly** (`src/channel.rs`): previously the `Ok(_) = rx.recv()` select arm used a refutable match that silently did not fire on `Lagged(n)` (subscriber fell behind the ring buffer) or `Closed` (sender dropped). `Lagged` now logs the dropped-event count and forces a DB re-read to catch up; `Closed` ends the stream. Added two tests guarding the `TryRecvError::Lagged` and `TryRecvError::Closed` paths.

### Features
- **Embed model absence warning at daemon startup**: logs `"note: embed model not loaded -- posts via /api/post and MCP will not be similarity-searchable until card 019d7991-2eab lands"` so operators are not surprised when daemon-side posts do not appear in cosine recall results.
- **TODO comments on each `post_from_text_with_meta` call site** (`src/mcp.rs`, `src/channel.rs`): reference card `019d7991-2eab` for the embed model threading follow-up.

## 0.6.1

### Phase D: Channel + Watch + MCP unified daemon

Ports `plugin/channel/*.ts` (TypeScript + Bun) to three tokio tasks in one Rust process under a new `legion daemon` subcommand. Kills the Bun runtime dependency entirely. Closes #200.

### Features
- **`legion daemon` subcommand**: one process hosts the channel (SSE pub/sub + HTTP endpoints at `/api/feed`, `/api/tasks`, `/api/post`, `/sse`), an optional MCP stdio server (`legion daemon --mcp`), and the watch poll loop that was previously a separate `legion watch` process
- **`src/channel.rs`**: SSE broadcast channel, HTTP endpoints matching the legacy JSON shapes so existing consumers (dashboard, any external tooling that scraped the legacy endpoints) continue to work. Opens DB once per SSE subscriber lifetime and queries only on broadcast notifications, not on every tick.
- **`src/mcp.rs`**: hand-rolled JSON-RPC 2.0 stdio server. Implements `initialize`, `tools/list`, `tools/call` per MCP 2024-11-05 spec. Four legion tools: `legion_post`, `legion_reply`, `legion_signal`, `legion_task_respond`. Each handler calls existing Rust primitives (`board::post_from_text_with_meta`, `sig::*`, `task::*`) instead of re-implementing. Bounded stdin buffer (1 MB per line) to prevent resource exhaustion. UTF-8 safe response truncation.
- **`src/daemon.rs`**: task orchestration; each handler opens its own DB connection consistent with the CLI pattern (follow-up card 019d7991-2eb4 tracks deduplication between channel.rs and serve.rs). Watch task holds a `watch::PidLockGuard` for RAII cleanup on SIGINT/abort. Graceful shutdown via `select!` races HTTP server, watch task, and MCP task -- any task exiting triggers the others to stop. Ctrl+C install failure uses `pending()` instead of returning, preventing spurious instant shutdown.
- **New error variant**: `LegionError::McpInvalidArgument(String)` replaces the `LegionError::Io` abuse that was previously used for MCP argument validation errors.

### Deleted
- `plugin/channel/index.ts`, `sse-client.ts`, `event-bridge.ts`, `instructions.ts`, `types.ts`, `tools.ts`, `fakechat.ts`
- `plugin/channel/package.json`, `bun.lock`, `tsconfig.json`, `node_modules/`
- `plugin/bin/legion-channel` (Bun dispatcher wrapper)
- Bun references from `plugin/hooks/session-start.sh` and `plugin/hooks/setup-binary.sh`

### Bug Fixes (from simplify review of the port)
- **`truncate` UTF-8 safety**: byte-slicing replaced with `char_indices` iteration. Previously panicked on multibyte content, which is common in agent-generated text.
- **Watch PID lock leak on SIGINT**: `run_watch_task` now holds a `PidLockGuard` whose `Drop` impl releases the lock. Previously `watch_handle.abort()` orphaned the PID file, blocking the next daemon start.
- **Silent `"unknown"` repo fallback in MCP handlers**: removed. Required fields now return `McpInvalidArgument` errors matching the tool input schema contract.
- **`build_feed_json` silent failures**: return type changed from `Result<_, ()>` to `Result<_, LegionError>`. DB and serde errors are now propagated and logged instead of swallowed.
- **`open_db` / `open_index` error logging**: errors are now logged server-side before being converted to a generic 500 response (avoids leaking internals while preserving debuggability).
- **Dead code removed**: `is_relevant_backlog` + `starts_with_mention` in channel.rs (6 tests + 30 LOC) had `#[allow(dead_code)]` and a comment claiming MCP backlog delivery usage that never existed.
- **`build_app` passthrough deleted**: was a one-line function with a misleading "merge routers" comment that didn't do any merging.
- **`shutdown_signal` no longer panics**: `.expect()` on Ctrl+C / SIGTERM handler install replaced with logged fallback.

### Follow-up cards filed from the simplify review
- `019d7991-2eab` HIGH: daemon does not load the embedding model, so daemon-side posts (via `api_post` or MCP tools) land with NULL embeddings, degrading hybrid recall for them. Fix is non-trivial (threading `Arc<Option<EmbedModel>>` through ChannelState + MCP context).
- `019d7991-2eb4` MED: `FeedItem` / `build_feed_json` / `json_error` / `open_db` / `open_index` duplicated between `channel.rs` and `serve.rs`. Extract shared helpers.
- `019d7991-2ebc` MED: `daemon::run_watch_task` duplicates `watch::run` with logic drift already visible. Refactor to one shared implementation.

## 0.6.0

### Features
- **Kanban CLI completeness**: `legion kanban view --id <uuid>` (with `--json`), `legion kanban update --id <uuid> --text/--body/--priority/--labels/--add-labels/--remove-labels`, and `legion kanban list --json` for JSONL bulk-scan output. Unblocks the orchestrator agent's First Steps and makes grooming clean.
- **`legion pr close --repo X --number N`**: close a PR without merging via the work source. `--reason "text"` posts the reason as a closing comment. `--delete-branch` removes the remote branch after closing.
- **`legion usage` subcommand**: parses `~/.claude/projects/<slug>/<session-uuid>.jsonl` to aggregate token usage per session. Computes effective tokens (UI-style weighted total) and dollar cost (API-list pricing). Flags: `--session`, `--today`, `--since`, `--by-session`, `--by-repo`, `--json`. Pure local, no network, no DB writes.
- **Embedding reads**: `legion similar --id <uuid>` returns nearest neighbors of a reflection by cosine similarity (with `--limit`, `--cross-repo`, `--min-score`, `--preview`, `--json`). `legion recall --cosine-only` skips BM25 for pure semantic ranking. `legion recall --min-score <f32>` drops weak matches. `legion reflect --dedupe-mode warn|strict|off` detects near-duplicates on store (cosine >= 0.95 against last 100 same-repo reflections). `--force` bypasses the check.
- **`legion-simplify` skill + quality gate**: new plugin skill at `plugin/skills/legion-simplify/SKILL.md` reviews the branch diff and emits structured JSON. New `quality_gates` DB table records skill results unforgeably (written by the skill runner, not the agent). `legion pr create` refuses to open a PR without a clean simplify gate for HEAD. `--skip-gates` bootstrap flag writes an audit entry.

### Bug Fixes
- **Dispatcher fallback when CLAUDE_PLUGIN_DATA is unset**: Claude Code subagents spawned via the Task tool get a clean shell environment without `CLAUDE_PLUGIN_DATA`. The dispatcher script at `plugin/bin/legion` now defaults `DATA_DIR` to `$HOME/.claude/plugins/data/legion-legion/` when the env var is missing. Unblocks the entire team architecture -- every `.claude/agents/*.md` specialist can now run `legion recall` / `reflect` / `consult` in their First Steps.

### Dev Workflow
- CLAUDE.md updated with `legion-simplify` gate requirement on `legion pr create`
- `legion quality-gate record` added to the command reference

## 0.5.0

### Recovery Release
Rolled back the v0.4.3..v0.4.7 spiral (8 commits of `.mcp.json` location churn that chased a Claude Code 2.1.97 regression without fixing the actual bug). Cherry-picked the good halves of v0.4.1 / v0.4.2 / v0.4.3 on top of v0.4.0 and added four root-cause fixes.

### Features
- **`legion kanban` structured cards**: parsed problem/solution/acceptance fields stored on insert via `card_parse::parse_issue_body` (from v0.4.3 good-half cherry-pick)
- **Channel backlog filtering**: SSE stream delivers only `@me` signals and `@all` blockers; `@self` posts redirect to reflect instead of broadcasting (from v0.4.2 cherry-pick)
- **`legion issue view`**: reads a GitHub issue body into structured fields
- **`status --json` flag**: counts-only summary for hook output
- **PreToolUse recall-first hook restored**: v0.4.1 removed it; v0.5.0 brings it back AND upgrades it from nudge-only to execute-and-inject (actually runs `legion recall` on the tool's search query)
- **SessionStart slim**: from 14.5 KB to ~800 bytes (removed LEGION_HELP prose, surface, work peek)
- **Data dir consolidation**: transparent one-way migration from `~/Library/Application Support/legion/` (macOS ProjectDirs) to `~/.claude/plugins/data/legion-legion/` with WAL-safe SQLite copy
- **Recall `--preview N`**: truncate each hit to N characters via `card_parse::truncate_chars`
- **No-bullshit Stop hook**: blocks responses that stop mid-task to check in ("three files down, want me to continue...")
- **No-direct-db PreToolUse Bash hook**: denies any command referencing `legion.db` directly
- **Starter agent team**: six specialists at `.claude/agents/` -- orchestrator (Haiku), rust/reviewer/dashboarder/porter/issue-writer (Sonnet)

### Bug Fixes
- **`find_plugin` three-tier fallback**: restores exe-relative lookup + adds glob over `~/.claude/plugins/cache/*/legion/*/worksources/` picking highest semver. Survives `CLAUDE_PLUGIN_ROOT` being empty in Bash subprocess context. Fixes `legion pr create` "plugin not found: github" error for every agent.
- **Channel mark-as-seen**: new atomic `get_and_mark_unread_board_posts` DB transaction + `unread_for=<repo>` query param on `/api/feed` + `sse-client.ts` uses it. Race-proof via single-timestamp upper bound. Fixes "same 50 signals every connection" bug.
- **Format for hook uses `Cow<str>`**: avoids per-reflection allocation when `preview` is None

## 0.4.0

### Features
- Bullpen archive: `legion bullpen --archive` moves read posts out of the active board (#168)
- `legion bullpen --archived` views archived posts for forensics
- Archived posts remain searchable via `consult` and `recall`

### Bug Fixes
- Work source sync now preserves GitHub issue creation date instead of using insertion time (#171)
- Scheduler correctly prioritizes older issues first
- Windows stack overflow: spawn main thread with 8MB stack to match macOS/Linux defaults
- `--repo` no longer required for `--archive` and `--archived` (global operations)

### Dev Workflow
- Added 10-step dev workflow to CLAUDE.md (plan, issue, build, simplify, PR, automated review, fix, team review, consensus, ask for merge)
- Team review: vault validates spec, smugglr reviews Rust content

## 0.3.0

- Add `--version` flag to the legion binary
- setup-binary.sh reads version from plugin.json dynamically -- binary auto-updates on version bump
- One version number for binary and plugin (Cargo.toml = plugin.json)
- Add work source workflow guide to SessionStart hook context
- Slim Stop hook: just a reflect prompt, no checklist
- GitHub Release workflow on `v*` tags builds linux-x64, macos-x64, macos-arm64, windows-x64 with SHA-256 checksums
