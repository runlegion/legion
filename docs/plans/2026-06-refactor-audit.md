# Refactor audit -- 2026-06

Produced by an 8-explorer workflow run under the legion-explore discipline (sym-first,
recall-first, telemetry-logged fallback), with every must-change-together duplication
claim adversarially verified by an independent refuter agent. Per-subsystem detail
(every finding with file:line evidence) lives in the substrate:
`legion document list --doc-type audit-finding`.

## Executive summary

Legion is 53,807 lines of Rust in 47 files, plus ~5,200 lines of plugin shell. The
problem is concentrated: four god files (db.rs 8,815; main.rs 8,130; integration.rs
6,696; watch.rs 4,767) hold 53% of the Rust. The remaining 43 files are individually
healthy. 94 findings: 24 high, 37 med, 33 low. 30 duplication clusters were
adversarially verified: 19 true duplicates (must change together; DRY them), 10
coincidences (look alike, must NOT be unified), 1 uncertain. The split is unusually
low-risk because the kessel recipe's core trick -- `impl Database` spanning files --
is already proven in this repo by documents.rs and uncertainty/storage.rs, and the
god files already carry section banners marking their seams.

The audit also surfaced three live defects and confirmed one open issue:

- **#606 (new): runtime panic path.** `get_reflections_by_ids` SELECTs 10 columns
  against an 11-column row mapper; serve search is the trigger (db.rs:1866, serve.rs:1233).
- **#536 (confirmed independently): daemon never spawns the cluster sync actor.**
  The #582 WatchLoop unification held for the loop body, but the pre-loop side
  effects re-forked: watch::run spawns SyncHandle (watch.rs:2292-2308), daemon does
  not, despite a comment claiming it has "its own wiring" (sym refs: SyncHandle's
  only spawn site is watch.rs:2299).
- **MCP `legion_signal` bypasses the #587 required-fields gate.** Verb-manifest
  enforcement lives inline in the CLI arm only (main.rs:4316-4333); the MCP handler
  (mcp.rs:392-441) formats signals with no manifest check.
- **Schedule firing is a side effect of SSE connections.** Schedules fire only while
  a dashboard client is connected, N times per tick with N clients, racing
  get_due/mark_run across connections (serve.rs:272-295).

## Ranked findings (high)

| # | Subsystem | Finding | Evidence |
|---|-----------|---------|----------|
| 1 | db | god file: ~120 methods, 7 impl blocks, 700-line init_schema, 3,700-line test module | db.rs:312,3725,4075,383 |
| 2 | db | get_reflections_by_ids column shift -> InvalidColumnIndex (filed #606) | db.rs:85,1866; serve.rs:1233 |
| 3 | db | tasks + cards: two divergent CRUD families over one table; priority ORDER BY already differs | db.rs:2557-3148,2670 |
| 4 | db | init_schema interleaves ~18 tables in patch order; documents/uncertainty DDL stranded away from their modules | db.rs:383-1085,899,948 |
| 5 | cli | run() is a 3,700-line match over 58 arms; six arms >150 lines carry real domain logic inline | main.rs:3942-7648 |
| 6 | cli | worksource resolve-or-die block pasted ~19x (+5 variants) | main.rs:6040,6294,6790 |
| 7 | cli | fat domain logic inline: Reconcile algorithm, Verify gate recording, Pr Create gates | main.rs:5570,7089,6236 |
| 8 | comms | mcp.rs is five domains in one file (log, tools, dispatch, notifier, stdio loop) | mcp.rs:33-1270 |
| 9 | comms | '@recipient' addressing implemented 4 divergent ways; colon-suffix delivers live but never wakes | mcp.rs:637,684; signal.rs:43; db.rs LIKE |
| 10 | comms | MCP legion_signal bypasses required-fields gate (#587 leak) | main.rs:4316; mcp.rs:392 |
| 11 | comms/serve | serve.rs and channel.rs are parallel live HTTP feed stacks, already diverged (api_post shape, index-failure policy) | channel.rs:73-193; serve.rs:402-484 |
| 12 | serve | no error type w/ IntoResponse: 29 hand-written match-to-json_error blocks | serve.rs:39,44,365,427 |
| 13 | serve | schedule firing rides per-connection SSE streams (fires only with dashboard open; races) | serve.rs:272-295 |
| 14 | watch | daemon path lost the cluster-sync actor spawn (#536 confirmed) | watch.rs:2292; daemon.rs:449-518 |
| 15 | watch | god file: seven banner-marked domains | watch.rs:16-2329 |
| 16 | watch | watch::ClusterConfig is a dead parallel config path (parsed, never consumed in production) | watch.rs:113-289,2292 |
| 17 | watch | wake-address-set construction duplicated 3x incl. main.rs pending-replies -- the #585/#586 conflict surface | watch.rs:1595,1625; main.rs:4398 |
| 18 | hooks | four generations of 'intercept search, substitute legion'; only newest uses the shared library | hooks.json:66-125; pre-grep-*.sh |
| 19 | hooks | 10-site hook preamble diverging on LEGION_REPO honor (9 yes, 7 no) | pre-bash-grep.sh:54; stop.sh:42 |
| 20 | hooks | binary resolution split: 3 hooks PATH-resolve and are silently inert without a system install | pre-whoami-rewrite.sh:70; uncertainty-*.sh |
| 21 | tests | god file: 161 tests, ~14 domains, seams already banner-marked | integration.rs:2092,3690 |
| 22 | tests | zero integration coverage: serve, health, pty, cluster, sync_actor, wake_attempts, verbs, pr_write; watch thin (config CRUD only) | integration.rs:4633 |
| 23 | midtier | worksource plugin-call boilerplate ~19x; list_issues/list_prs silently return empty on missing plugin (policy split vs #190 lesson) | worksource.rs:300,632,347 |
| 24 | cli | 18 std::process::exit(1) inside handler bodies block extraction and testing | main.rs:6277,7156,5208 |

Med/low findings (37/33) and all cluster verdicts: `legion document list --doc-type audit-finding`.

## Verified duplication clusters

True duplicates (19) -- DRY these, each is one invariant written N times:
db-open prelude (~50 arms), done-vs-propagate-close (already drifted: audit parity lost),
verify-gate-key construction (write site vs read site), active-team-post WHERE fragment (6
sites), reflection column list (11+ sites, caused #606), tasks-vs-cards dual CRUD,
recipient matching (4 implementations), details wire parsing (CLI + MCP), serve/channel
HTTP clone (16 sites), open-db match boilerplate (29 sites), agents-json (2), feed-item
mapping (4), hook preamble (10 hooks), symbol-redirect logic (2 generations),
process-alive (watch + daemon), wake-address-set (3 sites incl. cross-file),
recall join+score (BM25 + hybrid paths), scip install-hint error mapping (9 sites),
test-harness stub-legion contract.

Coincidences (10) -- look-alike, deliberately NOT unified: worksource resolve-or-die
(error text should be pinned by a helper, but the 23 sites are policy-independent),
sync delta-getter SQL shape, verb-classification sets (fix is manifest-driven, not
extraction), emit-JSON dialects (migrate to one shape instead), cluster-config twins
(one is dead code -- delete, don't merge), recall hot-shims (delete), kanban promotion
ladder in tests, worksource error-string assertions, pr-stub generators, C1 stderr greps.

## Sequencing plan

Kessel lessons encoded: PRs touching the same hot file are NOT independent (21-PR
cascade); mechanical moves never mix with semantic changes; the test net lands before
the trapeze. Streams below are serial within themselves; separate streams may interleave.

**Stream 0 -- pre-split in-place DRY (small PRs, land first; each shrinks the later move):**
0a. #606 fix + REFLECTION_COLUMNS const.
0b. require_worksource() + open_db()/open_db_and_index() helpers; fold Commands::Done
    through propagate_card_close_to_worksource (behavior fix: restores audit parity).
0c. ServeError + IntoResponse; handlers become Result-returning (kills 29 match blocks).
0d. signal::recipient_token/is_addressed_to + signal::parse_details_arg + compose/validate
    entry point (closes the MCP required-fields bypass -- behavior fix).
0e. WatchRepoConfig::wake_addresses(); shared process_alive.

**Stream 1 -- test net:** split tests/integration.rs into tests/integration/{main,common,domain}.rs
(one PR, mechanical); then add missing coverage: watch wake-loop (cap, preflight),
sym round-trip, pr write-check gate, serve bind+health. Coverage PRs gate streams 2-4.

**Stream 2 -- db/ split:** ONE extraction PR per the kessel recipe. mod.rs = infra only
(Database, open, has_column, thin init_schema dispatcher); domain files own DDL + methods +
tests (reflections, board, kanban, schedules, sync, leases/wake_attempts, health,
statusline_samples, stats, quality_gates, autonomy, audit, scip, heartbeat); db/testutil.rs.
Tasks/cards land side-by-side in db/kanban.rs; collapse is a follow-up.

**Stream 3 -- cli split:** 3a. extract cli/datadir.rs (lowest blast radius, 95 crate refs
via re-export hub). 3b+. one domain handler file per PR, largest arms first (pr, kanban,
issue, autonomy, watch, index); arms keep arg massaging + print/json only; algorithm
bodies push into domain modules; exits become typed errors as each arm moves.

**Stream 4 -- watch/ split + sync-actor decision:** decide #536 fix shape FIRST (shared
WatchLoop::bootstrap owning the sync-actor spawn), then split watch/ {config, locks,
signals, spawn, tracker, gates}; delete dead watch::ClusterConfig.

**Stream 5 -- comms:** mcp/ split (log, tools, dispatch, notifier); serve mounts
channel::router and deletes its four duplicate handlers; schedule firing moves to a
background tokio task.

**Stream 6 -- hooks (independent of Rust streams, may run anytime):** lib/{prelude,emit,
probe,markers}.sh; merge the grep-guard generations onto _legion-prequery; delete
bullpen-check.sh (orphaned) and _legion-warn.sh dead plumbing; tests/testutil.sh with
parameterized stub-legion.

**Stream 7 -- the crate question:** answered LAST with compile-time data from the
post-split tree. Prior: scip/sym and pty are the only candidates with a real boundary;
default answer remains modules-not-crates.

## Coverage gaps (pre-refactor net)

Zero integration tests: serve.rs (1,492 lines -- the HTTP surface), health.rs, pty.rs,
cluster.rs, sync_actor.rs, wake_attempts.rs, verbs.rs, now.rs, embed.rs, pr_write.rs.
Thin: watch.rs (config CRUD only -- no wake loop, no gates). The Stream 1 coverage PRs
are the precondition for Streams 2-4.

## Explore-tooling appendix (dogfood measurement)

Index fresh for all 8 explorers. 13 lane-4 bypasses total: tests 4 (test files are not
richly SCIP-indexed for fn-level lookup), hooks 2 + cli 2 + serve 2 (shell, literal
strings, static assets), db 1, watch 1, midtier 1, comms 0. Reading: sym served the
symbol-shaped load; the honest gaps are shell scripts and literal-string lookups,
matching the lane-4 design rationale. Verifier verdicts overturned 10 of 30 claimed
must-change-together clusters -- the adversarial stage earned its cost.
