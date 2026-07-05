export const meta = {
  name: 'wave2-pipeline',
  description: 'Wave 2 of epic #704: Sonnet 5 builds each issue in an isolated worktree, gates record, Opus reviews with a fix loop',
  whenToUse: 'Run with args {batch:"A"} for #722+#723, then after Sean merges those, {batch:"B"} for #706+#708 off fresh main.',
  phases: [
    { title: 'Build', detail: 'rust implementer per issue, own branch + worktree', model: 'sonnet' },
    { title: 'Gate', detail: 'simplify articulation, push, pr-write, pr create', model: 'sonnet' },
    { title: 'Review', detail: 'combined review + fix loop, gate recorded', model: 'opus' },
  ],
}

// ---------------------------------------------------------------------------
// Batches: A must be merged by Sean before B runs (both B issues touch
// index_cmd.rs; branching them off pre-A main recreates the Wave 1 conflict).
// ---------------------------------------------------------------------------
const BATCHES = {
  A: [
    { number: 722, branch: 'feat/722-index-lock', summary: 'per-repo index lock: pidfile idiom from src/watch/locks.rs as <data_dir>/locks/index-<repo>.lock held across walk+upsert+prune in handle_index; second run errors loudly with holder pid; stale (dead-pid) locks reclaimed' },
    { number: 723, branch: 'fix/723-hermetic-fixtures', summary: 'hermetic test git fixtures: every git config/init/commit in tests/integration runs with explicit tempdir current_dir plus per-invocation -c user.name/-c user.email/-c commit.gpgsign=false; add suite guard in common.rs that hashes the real .git/config at start and panics at end on change' },
  ],
  B: [
    { number: 706, branch: 'feat/706-sym-tree', summary: 'sym tree: SymAction::Tree {repo, ext, under, depth, json} querying Database::list_file_inventory (no filesystem walk at query time); cross-repo when --repo omitted, entries tagged with repo; telemetry per invocation with result count' },
    { number: 708, branch: 'feat/708-etc-extract', summary: 'sym etc extract <path> --field <dotted.path> [--json]: json/yaml/toml plus YAML frontmatter in .md/.mdx/.astro; numeric segments index arrays; missing field errors name the deepest segment that resolved; maintained YAML crate (NOT archived serde_yaml); telemetry per invocation with hit/miss' },
  ],
}

const parsedArgs = typeof args === 'string' ? JSON.parse(args) : args
const batch = parsedArgs && parsedArgs.batch
const issues = BATCHES[batch]
if (!issues) throw new Error('pass args {batch:"A"} (722+723) or {batch:"B"} (706+708, only after batch A is merged)')

const REPO = '/Volumes/store/projects/runlegion/legion'
const WT = '/tmp/legion-wave2'

// Shared context every stranger-agent needs (workflow agents boot cold).
const DOCTRINE = `
Repo: ${REPO}. You work ONLY in your assigned worktree (given below), never the main checkout.
Operating contract: run 'legion whatami --repo legion' FIRST and follow it -- invariants include
no unwrap() in production code, no unsafe, thiserror errors, clippy -D warnings and fmt --check
must pass, tests alongside code, no emoji anywhere.
Prior lessons (recall 019f2420-7306, binding): never interpolate a path into a TOML string via
display() -- backslashes are TOML escapes on Windows; normalize path separators to '/' in any
repo-relative output; cfg(unix)-gate unix-only test APIs (symlink, PermissionsExt); tests must
pass on Windows CI, not just your machine.
Git: plain 'git commit' and 'git push' -- NEVER --no-verify, it is banned (operator rule
2026-07-04). The hooks must pass; if a hook hangs past its timeout or fails, STOP and report
the failure in your notes instead of bypassing. Push only from your own worktree.
`

const BUILD_SCHEMA = {
  type: 'object',
  required: ['ok', 'branch', 'worktree', 'head', 'test_summary', 'notes'],
  properties: {
    ok: { type: 'boolean' },
    branch: { type: 'string' },
    worktree: { type: 'string' },
    head: { type: 'string', description: 'commit hash of the branch tip' },
    test_summary: { type: 'string', description: 'e.g. "1382 passed, clippy clean, fmt clean"' },
    notes: { type: 'string', description: 'deviations from spec, new deps, anything the reviewer must know' },
  },
}

const GATE_SCHEMA = {
  type: 'object',
  required: ['ok', 'pr_number', 'head', 'simplify_gate', 'prwrite_ok'],
  properties: {
    ok: { type: 'boolean' },
    pr_number: { type: 'number' },
    head: { type: 'string' },
    simplify_gate: { type: 'string', description: 'gate id from quality-gate check' },
    prwrite_ok: { type: 'boolean' },
    notes: { type: 'string' },
  },
}

const REVIEW_SCHEMA = {
  type: 'object',
  required: ['decision', 'findings'],
  properties: {
    decision: { type: 'string', enum: ['approved', 'changes_requested'] },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        required: ['severity', 'file', 'claim'],
        properties: {
          severity: { type: 'string', enum: ['HIGH', 'MED', 'LOW'] },
          file: { type: 'string' },
          line: { type: 'number' },
          claim: { type: 'string' },
          fix: { type: 'string' },
        },
      },
    },
    criteria_verified: { type: 'string' },
  },
}

const results = await pipeline(
  issues,

  // -------------------------------------------------------------- Build ----
  issue =>
    agent(
      `You are the legion rust implementer. Build issue #${issue.number} on branch ${issue.branch}.
${DOCTRINE}
Setup: git -C ${REPO} fetch origin main, then create your worktree:
git -C ${REPO} worktree add ${WT}/${issue.branch} -b ${issue.branch} origin/main
(if the worktree or branch exists from a dead run, remove/force-recreate them). cd there for everything.
Spec: run 'legion issue view --repo legion --number ${issue.number}' and implement EXACTLY its
acceptance criteria and requirements. Scope summary: ${issue.summary}.
Also run 'legion recall --repo legion --context "${issue.summary.slice(0, 60)}"' before coding.
Write tests alongside code. Run: cargo test, cargo clippy --tests -- -D warnings, cargo fmt -- --check.
All must pass. Commit with message 'feat(#${issue.number}): <what>' (or fix(#...)) -- hooks must
pass; if the pre-commit review blocks, fix what it names and re-commit.
Do NOT push, do NOT open a PR -- the gate stage does that.
Return ok:false with notes if you cannot satisfy a criterion rather than faking it.`,
      { label: `build:#${issue.number}`, phase: 'Build', model: 'sonnet', schema: BUILD_SCHEMA }
    ),

  // --------------------------------------------------------------- Gate ----
  (build, issue) => {
    if (!build || !build.ok) throw new Error(`build failed for #${issue.number}: ${build && build.notes}`)
    return agent(
      `You run the legion quality gates for branch ${build.branch} (issue #${issue.number}).
${DOCTRINE}
Your worktree: ${build.worktree} -- cd there; HEAD should be ${build.head}.
1. SIMPLIFY: git -C ${REPO} fetch origin main:main (keep the base fresh), then read
   'git diff main...HEAD' and write a per-file articulation (one '### <path>' entry per changed
   file, composed prose, each citing a file:line or Evidence: line -- entries under 12 words are
   rejected). Save to ${WT}/simplify-${issue.number}.md and run:
   legion quality-gate check --skill legion-simplify --result clean --findings-count 0 --articulation-file ${WT}/simplify-${issue.number}.md
   If you find real structural issues while articulating, FIX them, re-commit, and redo this step.
2. PUSH: git push origin ${build.branch} (hooks must pass; report a block instead of bypassing)
3. PR-WRITE: compose the PR body -- Summary, one '### N. <criterion>' mapping per acceptance
   criterion of issue #${issue.number} each citing evidence (test name, file:line, or observable
   behavior on an 'Evidence:' line), and a 'Deliberately not done' section. Save it, then:
   legion pr write-check --repo legion --issue ${issue.number} --body-file <file>
   Iterate until clean.
4. PR: legion pr create --repo legion --title '<type>(#${issue.number}): <title>' --body "$(cat <file>)"
Return the PR number, HEAD, and the simplify gate id.`,
      { label: `gate:#${issue.number}`, phase: 'Gate', model: 'sonnet', schema: GATE_SCHEMA }
    )
  },

  // ------------------------------------------------- Review + fix loop ----
  async (gated, issue) => {
    if (!gated || !gated.ok) throw new Error(`gate failed for #${issue.number}: ${gated && gated.notes}`)
    let head = gated.head
    for (let round = 0; round < 3; round++) {
      const review = await agent(
        `You are the legion PR reviewer (combined pass: spec, correctness, quality, security).
Review PR #${gated.pr_number} (issue #${issue.number}, branch ${issue.branch}, head ${head}) in ${REPO}.
Diff: git -C ${REPO} diff main...origin/${issue.branch} (fetch first). Read the issue criteria via
'legion issue view --repo legion --number ${issue.number}' and verify EACH criterion against the
diff -- claims the diff does not implement are HIGH. Check the repo invariants (no unwrap in prod,
thiserror, explicit types), error handling, silent failures, test quality (do the tests pin the
criteria or just exercise happy paths), Windows portability (TOML path interpolation, path
separators, ungated unix APIs), and injection/unchecked input. Read the actual code around every
hunk, not just the diff. Every finding: severity + file:line + claim + evidence + fix.
Be adversarial with yourself before reporting: for each HIGH/MED, try to refute it from the code;
report only findings that survive, and say what you refuted in criteria_verified.`,
        { label: `review:#${issue.number}r${round}`, phase: 'Review', model: 'opus', schema: REVIEW_SCHEMA }
      )
      if (!review) throw new Error(`review agent died for #${issue.number} round ${round + 1}`)
      const blocking = review.findings.filter(f => f.severity === 'HIGH').length > 0 || review.findings.filter(f => f.severity === 'MED').length >= 3
      if (review.decision === 'approved' && !blocking) {
        const gateAgent = await agent(
          `cd ${WT}/${issue.branch} (worktree of ${REPO}, branch ${issue.branch}). Verify HEAD is ${head}.
Record the review gate: legion quality-gate record --skill legion-review --result clean --findings-count ${review.findings.length} --details-json '<compact JSON of the decision and findings>'
Then echo the gate id. Return that id as your final text.`,
          { label: `record:#${issue.number}`, phase: 'Review', model: 'sonnet' }
        )
        return { issue: issue.number, pr: gated.pr_number, head, decision: 'approved', findings: review.findings, review_gate: (gateAgent || '').trim(), rounds: round + 1 }
      }
      log(`#${issue.number} round ${round + 1}: changes_requested (${review.findings.length} findings) -- fixing`)
      const fixed = await agent(
        `You are the legion rust implementer fixing review findings on branch ${issue.branch} (issue #${issue.number}).
${DOCTRINE}
Worktree: ${WT}/${issue.branch}. Findings to fix (address EVERY one; if you believe one is wrong,
say so in notes with file:line evidence instead of silently skipping):
${JSON.stringify(review.findings.filter(f => f.severity !== 'LOW'), null, 2)}
LOW findings: fix if under ~10 lines each, otherwise note them.
Re-run cargo test / clippy --tests -D warnings / fmt --check. Commit and push (hooks must pass).
Then RE-RECORD the HEAD-keyed gates on the new HEAD:
1. update the simplify articulation at ${WT}/simplify-${issue.number}.md for any files your fix touched, re-run legion quality-gate check (same flags as before);
2. re-run legion pr write-check --repo legion --issue ${issue.number} --body-file <the PR body file> (update the body if the fix changed a criterion's evidence).
Return the new HEAD.`,
        { label: `fix:#${issue.number}r${round}`, phase: 'Review', model: 'sonnet', schema: BUILD_SCHEMA }
      )
      if (!fixed || !fixed.ok) throw new Error(`fix round ${round + 1} failed for #${issue.number}`)
      head = fixed.head
    }
    return { issue: issue.number, pr: gated.pr_number, head, decision: 'escalate', note: '3 review rounds without approval -- needs Fable/operator attention' }
  }
)

const done = results.filter(Boolean)
log(`batch ${batch} complete: ${done.map(r => `#${r.issue} PR ${r.pr} ${r.decision}`).join(', ')}`)
return {
  batch,
  results: done,
  next: batch === 'A'
    ? 'Sean merges the batch A PRs, then rerun with {batch:"B"} so 706/708 branch off fresh main.'
    : 'Sean merges; then /legion-verify the Wave 2 card criteria and close it.',
}
