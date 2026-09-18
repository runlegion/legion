//! Runs the adversarial battery against `legion_cmd::scan` (#1226, FR-CMD-007's
//! acceptance text). 72 rows are RESEARCH-CMD-bounded-tokenizer's
//! hand-labelled fixtures (from the precog-toy worktree's
//! `toy/tokenizer/fixtures.json`), re-derived for the grammar-parser
//! mechanism rather than the hand-written scanner they were labelled
//! against; the remaining rows are derived from that research's adversarial
//! review (its `caveats` and `next_step`), each row's `source` naming the
//! item it comes from, plus the known-false-positive commands the issue asks
//! for separately.
//!
//! The battery carries 93 rows, not FR-CMD-007's stated 88 (72 + 16): a 2026-09-18
//! verify pass found three of the "16 derived" rows padded with a `source`
//! that named neither `caveats` nor `next_step` (they named the issue's
//! separate known-false-positives bullet instead), and two other rows each
//! folded multiple `next_step` items behind one tested example (`ssh` stood
//! for `ssh`/`watch`/`parallel`/`setsid`; `sed -n` stood for the zgrep
//! family, `sed -n`, and `awk` as well). Honoring "each row names the item
//! it comes from" for those two foldings alone yields 18 caveat/next_step-derived
//! rows, and the 3 known-false-positive commands the issue also asks for
//! (`git log --grep-reflog=x`, `git log -- -Sfoo`, `pnpm grep`) trace to
//! neither `caveats` nor `next_step` and have no slot in a fixed 72+16 count
//! that excludes them. 72 + 18 + 3 = 93. This is a spec undercount in
//! FR-CMD-007's acceptance text and the issue's row-count arithmetic, not a
//! grouping choice available to the implementer -- see the escalation on
//! issue #1226 rather than re-folding rows to force 88.
//!
//! Row shape (`tests/fixtures/battery.json`): `id`, `source`, `cmd`, an
//! optional `note`, and `expected` in `visible` / `opaque` / `benign` /
//! `error`. `managed` lists `[binary, position]` pairs that must be present
//! among `scan`'s invocations (a subset check: a row names only the
//! binaries it cares about, while `scan` also resolves `cd`, `echo`, and
//! the like). `unreduced` lists the `UnreducedReason` values that must
//! appear, matched as a set. `benign` rows additionally carry `absent`: the
//! binaries a naive heuristic could mistake for a real invocation, which
//! must not appear in `scan`'s invocations. `error` rows carry neither: the
//! issue is explicit that a malformed command returns `ScanError`, never a
//! partial `Scan`.

use std::collections::HashSet;
use std::fs;

use legion_cmd::{Position, Scan, UnreducedReason};
use serde_json::Value;

fn parse_position(raw: &str) -> Position {
    match raw {
        "first" => Position::First,
        "after-operator" => Position::AfterOperator,
        "after-assignment" => Position::AfterAssignment,
        "substitution" => Position::Substitution,
        "function-body" => Position::FunctionBody,
        other => panic!("unknown position in battery.json: {other}"),
    }
}

fn parse_reason(raw: &str) -> UnreducedReason {
    match raw {
        "wrapper-payload" => UnreducedReason::WrapperPayload,
        "script-file" => UnreducedReason::ScriptFile,
        "interpreter-body" => UnreducedReason::InterpreterBody,
        "dynamic-name" => UnreducedReason::DynamicName,
        "too-deep" => UnreducedReason::TooDeep,
        "unparsed" => UnreducedReason::Unparsed,
        other => panic!("unknown UnreducedReason in battery.json: {other}"),
    }
}

/// A row's expected verdict (`tests/fixtures/battery.json`'s `expected`
/// field). A closed set, parsed the same disciplined way as `parse_position`
/// and `parse_reason`: an unknown string panics rather than silently gating
/// nothing, so a typo like `"visable"` cannot pass through as a non-error
/// row by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    Visible,
    Opaque,
    Benign,
    Error,
}

fn parse_expected(raw: &str) -> Expected {
    match raw {
        "visible" => Expected::Visible,
        "opaque" => Expected::Opaque,
        "benign" => Expected::Benign,
        "error" => Expected::Error,
        other => panic!("unknown expected verdict in battery.json: {other}"),
    }
}

/// A row's `managed` array, or a panic naming the row and why it must have
/// one. Both the `Visible` and `Benign` arms need this same extraction with
/// their own message; a shared helper means the message is written once
/// instead of drifting between two near-identical panics.
fn managed_list<'a>(id: &str, row: &'a Value, why: &str) -> &'a Vec<Value> {
    row.get("managed")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{id}: {why}"))
}

/// Asserts every `(binary, position)` pair the row names is present in
/// `scan`'s invocations *with at least the multiplicity the row names it*.
/// A plain per-pair `.any()` check would let a row like `process-substitution`
/// (`[["ls","substitution"],["ls","substitution"]]`) pass on a single `ls`,
/// asserting the same thing twice instead of two distinct invocations.
fn assert_managed_subset(id: &str, scan: &Scan, expected: &[Value]) {
    let mut expected_counts: std::collections::HashMap<(&str, Position), usize> =
        std::collections::HashMap::new();
    for pair in expected {
        let pair = pair
            .as_array()
            .expect("managed entry must be [binary, position]");
        let binary = pair[0].as_str().expect("binary must be a string");
        let position = parse_position(pair[1].as_str().expect("position must be a string"));
        *expected_counts.entry((binary, position)).or_insert(0) += 1;
    }

    for ((binary, position), expected_count) in expected_counts {
        let actual_count = scan
            .invocations
            .iter()
            .filter(|inv| inv.binary == binary && inv.position == position)
            .count();
        assert!(
            actual_count >= expected_count,
            "{id}: expected at least {expected_count} invocation(s) of {binary:?} at {position:?}, found {actual_count}, got {:#?}",
            scan.invocations
        );
    }
}

/// Asserts a benign row's `managed` list is exhaustive: every pair the row
/// names is present (`assert_managed_subset`'s multiplicity check), and no
/// other invocation resolves alongside them. A benign row exists to prove a
/// naive heuristic's false positive stays absent; a subset check alone would
/// pass whether or not that false positive resolved, since it only ever
/// asserts about what the row lists, never about what else showed up.
fn assert_managed_exact(id: &str, scan: &Scan, expected: &[Value]) {
    assert_managed_subset(id, scan, expected);
    assert_eq!(
        scan.invocations.len(),
        expected.len(),
        "{id}: a benign row's managed list must account for every invocation, got {:#?}",
        scan.invocations
    );
}

fn assert_unreduced_set(id: &str, scan: &Scan, expected: &[Value]) {
    let expected_reasons: HashSet<UnreducedReason> = expected
        .iter()
        .map(|v| parse_reason(v.as_str().expect("unreduced entry must be a string")))
        .collect();
    let actual_reasons: HashSet<UnreducedReason> =
        scan.unreduced.iter().map(|u| u.reason).collect();
    assert_eq!(
        actual_reasons, expected_reasons,
        "{id}: unreduced reasons differ, got {:#?}",
        scan.unreduced
    );
}

fn assert_absent(id: &str, scan: &Scan, absent: &[Value]) {
    for binary in absent {
        let binary = binary.as_str().expect("absent entry must be a string");
        assert!(
            scan.invocations.iter().all(|inv| inv.binary != binary),
            "{id}: {binary:?} must not appear as an invocation (this row asserts it is a false positive), got {:#?}",
            scan.invocations
        );
    }
}

#[test]
fn battery_has_ninety_three_rows() {
    // See the module doc comment: FR-CMD-007 states 72 + 16 = 88, but an
    // honest split of the folded next_step rows plus the required
    // known-false-positive rows the issue asks for separately lands at
    // 72 + 18 + 3 = 93. Escalated on issue #1226 rather than re-folded to
    // force the stated count.
    let rows = load_rows();
    assert_eq!(
        rows.len(),
        93,
        "battery count drifted from 72 research + 18 derived + 3 known-false-positive rows -- \
         see the module doc comment and issue #1226's escalation before changing this number"
    );
}

#[test]
fn battery_ids_are_unique() {
    let rows = load_rows();
    let mut seen = HashSet::new();
    for row in &rows {
        let id = row["id"].as_str().expect("id must be a string");
        assert!(seen.insert(id), "duplicate battery row id: {id}");
    }
}

/// The research's kill condition stays measurable here: a parse-error rate
/// above 0.1 percent would mean the mechanism has regressed against real
/// input, and this count is where that would show up first.
#[test]
fn battery_rows_run_and_report_parse_errors() {
    let rows = load_rows();
    let mut parse_errors = 0usize;
    let mut expected_error_rows = 0usize;

    for row in &rows {
        let id = row["id"].as_str().expect("id must be a string");
        let cmd = row["cmd"].as_str().expect("cmd must be a string");
        let expected = parse_expected(row["expected"].as_str().expect("expected must be a string"));

        let result = legion_cmd::scan(cmd);
        if result.is_err() {
            parse_errors += 1;
        }

        if expected == Expected::Error {
            expected_error_rows += 1;
            assert!(
                result.is_err(),
                "{id}: expected a ScanError, got {result:?}"
            );
            assert!(
                row.get("managed").is_none(),
                "{id}: an error row must carry no managed list (never a partial Scan)"
            );
            continue;
        }

        let scan =
            result.unwrap_or_else(|err| panic!("{id}: expected a Scan, got ScanError: {err}"));

        // A row that names no `unreduced` key means none: this is the
        // mechanical check that no `Unreduced` region -- in particular no
        // `WrapperPayload` or `ScriptFile`, which would mean a name crept
        // into the walk -- appears unless the row declares it.
        let unreduced = row
            .get("unreduced")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_unreduced_set(id, &scan, &unreduced);

        // Each verdict gates something beyond the optional keys a row
        // happens to carry: a declared `expected` is a claim about the row,
        // not just a label that skips the malformed-input branch above.
        match expected {
            Expected::Error => unreachable!("Error rows continue above"),
            Expected::Visible => {
                let managed =
                    managed_list(id, row, "a visible row must carry a non-empty managed list");
                assert!(
                    !managed.is_empty(),
                    "{id}: a visible row must carry a non-empty managed list"
                );
                assert_managed_subset(id, &scan, managed);
            }
            Expected::Opaque => {
                assert!(
                    !unreduced.is_empty(),
                    "{id}: an opaque row must carry a non-empty unreduced list"
                );
                if let Some(managed) = row.get("managed").and_then(Value::as_array) {
                    assert_managed_subset(id, &scan, managed);
                }
            }
            Expected::Benign => {
                // A benign row's whole claim is that nothing beyond its
                // declared invocations resolves, so `managed` must name every
                // invocation the command produces -- not merely a subset --
                // or the row would pass whether or not the false positive it
                // exists to catch actually appeared.
                let managed = managed_list(
                    id,
                    row,
                    "a benign row must carry the managed list of what it resolves",
                );
                assert_managed_exact(id, &scan, managed);
            }
        }

        // `absent` is not exclusive to benign rows -- `comment-hides-nothing`
        // is `visible` and still names a false positive (`grep`, hidden
        // behind a `#` comment) that must never resolve. Checking it here,
        // once, for every row that declares it, covers that shape instead of
        // only the row shapes the match arms happen to test.
        if let Some(absent) = row.get("absent").and_then(Value::as_array) {
            assert_absent(id, &scan, absent);
        }
    }

    // The battery deliberately carries exactly one malformed-input row
    // (`unterminated-quote`, `expected: "error"`), so the 0.1% kill
    // condition is not asserted against this fixed, curated set -- it is a
    // property of the 47,151-command research corpus. A row not marked
    // `"expected": "error"` that unexpectedly returns `ScanError` already
    // fails above, at the `unwrap_or_else` that turns that `Err` into a
    // panic, and an `error` row that does not return `ScanError` already
    // fails at the `assert!` above that. This block only reports the
    // parse-error rate so the kill condition stays visible in the output.
    println!(
        "battery parse-error rate: {parse_errors}/{} ({:.4}%); {expected_error_rows} row(s) expect ScanError by design",
        rows.len(),
        parse_errors as f64 / rows.len() as f64 * 100.0
    );
}

fn load_rows() -> Vec<Value> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/battery.json");
    let text =
        fs::read_to_string(path).unwrap_or_else(|err| panic!("failed to read {path}: {err}"));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("failed to parse {path}: {err}"))
}
