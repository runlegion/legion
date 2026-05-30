---
name: legion-simplify
description: |
  Review the current branch diff for code quality issues: duplicate logic, unnecessary
  abstraction, stringly-typed state, and other structural problems. Produces structured
  JSON output and records the result via `legion quality-gate record` so `legion pr create`
  can verify clean state before opening a PR. Run this on every feature branch before
  creating a PR.
version: 1.0.0
user-invocable: true
allowed-tools: Bash, Read
---

# Legion Simplify

Review the git diff on this branch for structural quality issues. Record the result
via `legion quality-gate record` so `legion pr create` can gate on it.

## What to Review

Inspect the output of `git diff main..HEAD` (or `git diff origin/main..HEAD`).

Look for:

- **Duplicate logic** -- same transformation or check written twice in different places;
  could be extracted to a shared helper
- **Unnecessary abstraction** -- a wrapper or trait that adds indirection without buying
  extensibility; flatten it
- **Stringly-typed state** -- `status: String` where `"pending" | "done"` would be an enum;
  suggest the enum
- **Hand-rolled helpers** -- functions that duplicate something already in `std` or an
  existing module helper
- **Unnecessary new parameters** -- a parameter that is always the same value at every
  call site
- **Copy-paste variations** -- two near-identical blocks that differ by one variable;
  could be a loop or a shared function with a parameter
- **Error swallowing** -- `.unwrap_or(())` or `let _ =` on a result that should propagate
- **Unused comments that narrate the change** -- comments that say what the diff did rather
  than why the code exists

Do NOT flag:
- Style preferences (naming, ordering) unless they violate project conventions
- Things that are outside the diff (pre-existing issues)
- Hypothetical future improvements unrelated to the changed code

## Output Format

Produce a JSON object:

```json
{
  "result": "clean",
  "findings_count": 0,
  "findings": []
}
```

Or with findings:

```json
{
  "result": "issues",
  "findings_count": 2,
  "findings": [
    {
      "severity": "HIGH",
      "file": "src/db.rs",
      "line": 142,
      "issue": "same date-formatting logic duplicated from format_date helper on line 13",
      "suggestion": "call format_date() instead"
    },
    {
      "severity": "MED",
      "file": "src/main.rs",
      "line": 310,
      "issue": "status field is String but only ever 'pending' or 'done'",
      "suggestion": "introduce a Status enum with Pending and Done variants"
    }
  ]
}
```

Severity is `HIGH` (structural problem that will compound) or `MED` (worth fixing but
not blocking).

## Recording the Result

After producing the JSON, record it via:

```bash
legion quality-gate record \
  --skill legion-simplify \
  --result <clean|issues> \
  --findings-count <N> \
  --details-json '<full JSON>'
```

The command reads `git rev-parse HEAD` and `git rev-parse --abbrev-ref HEAD` automatically.
It prints the gate row ID on success.

## Procedure

1. Run `git diff main..HEAD` (fall back to `git diff origin/main..HEAD` if main is not local).
2. Read the changed files named in the diff to understand context.
3. Review each changed hunk for the issues listed above.
4. Produce the JSON result.
5. Call `legion quality-gate record` to persist it.
6. Print the JSON so the caller can see the findings.

## Pass Criteria for `legion pr create`

`legion pr create` calls `legion quality-gate record` implicitly by checking the DB before
opening the PR. The gate passes only when:

- A gate row exists for the current HEAD commit hash
- The row's `result` is `"clean"`

If you find issues, fix them (or note them as acceptable with a rationale), then re-run
this skill. The most recent gate row for the commit wins.
