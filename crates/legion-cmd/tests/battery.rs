//! The FR-CMD-007 adversarial battery: 88 rows (72 hand-labelled by
//! RESEARCH-CMD-bounded-tokenizer's toy, 16 derived from that research's
//! adversarial review) at `tests/fixtures/battery.json`, each with an
//! expected verdict: `visible`, `opaque`, or `benign`.
//!
//! Row shape and matching convention (there is no single canonical shape
//! for "a git subcommand is managed" once `scan` stopped classifying
//! `managed`, so this file fixes one):
//!
//! - `visible` rows list `binaries: [{binary, position, args_prefix?}]`.
//!   A match requires an invocation with that `binary` and `position`
//!   whose `args` contain `args_prefix` as a contiguous run somewhere in
//!   the list, when given (`git commit` in the toy's fixtures becomes
//!   `{binary: "git", args_prefix: ["commit"]}` here, since `scan` now
//!   reports the executable and its argv rather than folding a subcommand
//!   into the binary name; a run anywhere rather than strictly at index 0
//!   because global options can precede the subcommand, e.g. `git -C dir
//!   -c k=v push`). This is containment, not
//!   exact-set equality: a `visible` row may also carry `opaque_kinds` for
//!   a fixture whose command mixes a resolved call with an opaque region
//!   (e.g. the outer `grep` in a piped `python3 <<'PY' | grep ...` is
//!   visible while the heredoc body is opaque); the `opaque_kinds` are
//!   still checked when present.
//! - `opaque` rows list `opaque_kinds: [<kebab-case Opaque variant name>]`;
//!   every named kind must be present in `scan.opaque`.
//! - `benign` rows have neither: `scan` must succeed, and by convention
//!   they are commands a naive heuristic could false-positive on but this
//!   scanner does not need any special handling to get right.
//!
//! Every row is expected to parse without a [`legion_cmd::ScanError`]: none
//! of the three verdicts is "parse error" (a parse error is tested
//! separately, in `tokenizer.rs`'s unit tests). This file counts and
//! reports the parse-error rate across the battery, since
//! RESEARCH-CMD-bounded-tokenizer's kill condition is keyed on it staying
//! under 0.1 percent.

use legion_cmd::{Opaque, Position, scan};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ExpectedBinary {
    binary: String,
    position: String,
    #[serde(default)]
    args_prefix: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "expected", rename_all = "lowercase")]
enum Expected {
    Visible {
        binaries: Vec<ExpectedBinary>,
        #[serde(default)]
        opaque_kinds: Vec<String>,
    },
    Opaque {
        opaque_kinds: Vec<String>,
    },
    Benign,
    /// Not one of FR-CMD-007's three battery verdicts: a deliberate,
    /// disclosed extension for exactly one row (`unterminated-quote`). That
    /// row's original toy label assumed the toy's lenient recovery from a
    /// malformed command; FR-CMD-007 requires the opposite ("a malformed
    /// command returns ScanError, never a partial Scan"). Re-checking the
    /// label against the spec rather than copying it (the issue's own
    /// instruction) means this row can only demonstrate a `ScanError`, so
    /// it is excluded from the "no row is a parse-error case" assertion and
    /// checked here instead.
    Error,
}

#[derive(Debug, Deserialize)]
struct Row {
    id: String,
    cmd: String,
    #[serde(flatten)]
    expected: Expected,
}

fn position_from_str(s: &str) -> Position {
    match s {
        "first" => Position::First,
        "after-operator" => Position::AfterOperator,
        "after-assignment" => Position::AfterAssignment,
        "wrapper" => Position::Wrapper,
        "inline-shell" => Position::InlineShell,
        "heredoc-shell" => Position::HeredocShell,
        "find-exec" => Position::FindExec,
        "function-body" => Position::FunctionBody,
        "substitution" => Position::Substitution,
        other => panic!("battery.json names an unknown position `{other}`"),
    }
}

fn load_rows() -> Vec<Row> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/battery.json");
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("battery.json is not valid: {e}"))
}

#[test]
fn battery_has_the_expected_row_count() {
    let rows = load_rows();
    assert_eq!(
        rows.len(),
        88,
        "the adversarial battery must carry exactly 88 rows (72 + 16)"
    );
    let mut ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids.len(),
        rows.len(),
        "every battery row must have a unique id"
    );
}

#[test]
fn battery_every_row_matches_its_expected_verdict() {
    let rows = load_rows();
    let mut parse_errors = Vec::new();
    let mut mismatches = Vec::new();

    let total = rows.len();
    let mut expected_error_rows = 0usize;
    for row in &rows {
        if matches!(row.expected, Expected::Error) {
            expected_error_rows += 1;
            if scan(&row.cmd).is_ok() {
                mismatches.push(format!(
                    "{}: expected a ScanError, but scan succeeded",
                    row.id
                ));
            }
            continue;
        }
        match scan(&row.cmd) {
            Err(e) => parse_errors.push(format!("{}: {e}", row.id)),
            Ok(result) => {
                if let Err(reason) = check_row(row, &result) {
                    mismatches.push(format!("{}: {reason}", row.id));
                }
            }
        }
    }

    let measured = total - expected_error_rows;
    let rate = parse_errors.len() as f64 / measured as f64 * 100.0;
    eprintln!(
        "[battery] parse errors: {}/{measured} ({rate:.3}%, {expected_error_rows} row(s) deliberately excluded as expected-error)",
        parse_errors.len(),
    );
    assert!(
        parse_errors.is_empty(),
        "no battery row is a parse-error case (rate {rate:.3}%, kill condition is 0.1%): {parse_errors:#?}"
    );
    assert!(
        mismatches.is_empty(),
        "battery mismatches:\n{}",
        mismatches.join("\n")
    );
}

fn check_row(row: &Row, result: &legion_cmd::Scan) -> Result<(), String> {
    match &row.expected {
        Expected::Visible {
            binaries,
            opaque_kinds,
        } => {
            for expected in binaries {
                let position = position_from_str(&expected.position);
                let found = result.invocations.iter().any(|inv| {
                    inv.binary == expected.binary
                        && inv.position == position
                        && contains_run(&inv.args, &expected.args_prefix)
                });
                if !found {
                    return Err(format!(
                        "expected a `{}` invocation at {:?} (args_prefix {:?}), got {:#?}",
                        expected.binary, position, expected.args_prefix, result.invocations
                    ));
                }
            }
            check_opaque_kinds(opaque_kinds, result)
        }
        Expected::Opaque { opaque_kinds } => check_opaque_kinds(opaque_kinds, result),
        Expected::Benign => Ok(()),
        Expected::Error => {
            unreachable!("Expected::Error rows are handled before check_row is called")
        }
    }
}

/// `needle` appears as a contiguous run somewhere in `haystack` (an empty
/// `needle` always matches -- a row with no `args_prefix` makes no claim
/// about args).
fn contains_run(haystack: &[String], needle: &[String]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn check_opaque_kinds(kinds: &[String], result: &legion_cmd::Scan) -> Result<(), String> {
    for kind in kinds {
        let found = result.opaque.iter().any(|o| o.kind_name() == kind);
        if !found {
            return Err(format!(
                "expected an opaque region of kind `{kind}`, got {:#?}",
                result
                    .opaque
                    .iter()
                    .map(Opaque::kind_name)
                    .collect::<Vec<_>>()
            ));
        }
    }
    Ok(())
}
