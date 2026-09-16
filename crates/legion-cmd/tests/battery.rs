//! Runs the FR-CMD-007 adversarial battery at `tests/fixtures/battery.json`:
//! 88 rows, each an `id`, a `cmd`, and one of FR-CMD-007's four expected
//! verdicts: `visible`, `opaque`, `benign`, or `error`.
//!
//! - `visible` rows list `binaries: [{binary, position, args_prefix?}]`. A
//!   match requires an invocation with that `binary` and `position` whose
//!   `args` contain `args_prefix` as a contiguous run anywhere in the list
//!   (anywhere rather than only at index 0 because global options can
//!   precede a subcommand, e.g. `git -C dir -c k=v push`). This is
//!   containment, not exact-set equality: a `visible` row may also carry
//!   `opaque_kinds`, checked the same way an `opaque` row's are, for a
//!   command that mixes a resolved call with an opaque region.
//! - `opaque` rows list `opaque_kinds: [<kebab-case Opaque variant name>]`;
//!   every named kind must be present in `scan.opaque`.
//! - `benign` rows have neither, but may carry `absent: [binary...]`: none
//!   of those binaries may appear as an invocation anywhere except at
//!   `First` (the row's own head command may legitimately be one of them,
//!   e.g. `pnpm` in `pnpm wrangler deploy`; what must never happen is that
//!   binary showing up as a second, separately-resolved invocation).
//! - `error` rows (currently one, `unterminated-quote`) expect a
//!   [`legion_cmd::ScanError`]: a malformed command must return `ScanError`,
//!   never a partial `Scan`. Excluded from the parse-error-rate assertion
//!   below, since that rate is about commands expected to parse cleanly,
//!   and checked separately.
//!
//! Every other row is expected to parse without a `ScanError`. This file
//! counts and reports that rate, since RESEARCH-CMD-bounded-tokenizer's
//! kill condition is keyed on it staying under 0.1 percent.

use legion_cmd::{Opaque, Position, scan};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ExpectedBinary {
    binary: String,
    position: Position,
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
    Benign {
        #[serde(default)]
        absent: Vec<String>,
    },
    Error,
}

#[derive(Debug, Deserialize)]
struct Row {
    id: String,
    cmd: String,
    #[serde(flatten)]
    expected: Expected,
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
                let found = result.invocations.iter().any(|inv| {
                    inv.binary == expected.binary
                        && inv.position == expected.position
                        && contains_run(&inv.args, &expected.args_prefix)
                });
                if !found {
                    return Err(format!(
                        "expected a `{}` invocation at {:?} (args_prefix {:?}), got {:#?}",
                        expected.binary,
                        expected.position,
                        expected.args_prefix,
                        result.invocations
                    ));
                }
            }
            check_opaque_kinds(opaque_kinds, result)
        }
        Expected::Opaque { opaque_kinds } => check_opaque_kinds(opaque_kinds, result),
        Expected::Benign { absent } => {
            for binary in absent {
                let bad = result
                    .invocations
                    .iter()
                    .any(|inv| &inv.binary == binary && inv.position != Position::First);
                if bad {
                    return Err(format!(
                        "expected `{binary}` to never appear except at First, got {:#?}",
                        result.invocations
                    ));
                }
            }
            Ok(())
        }
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
        let found = result.opaque.iter().any(|o| opaque_kind_matches(o, kind));
        if !found {
            return Err(format!(
                "expected an opaque region of kind `{kind}`, got {:#?}",
                result.opaque
            ));
        }
    }
    Ok(())
}

/// Matches a battery-fixture kebab-case kind name against an [`Opaque`]
/// variant, without needing a stringly-typed accessor on the public type:
/// this file is the only place battery.json's kind names and the enum's
/// variants need to agree.
fn opaque_kind_matches(o: &Opaque, kind: &str) -> bool {
    match kind {
        "interpreter" => matches!(o, Opaque::Interpreter { .. }),
        "script-file" => matches!(o, Opaque::ScriptFile),
        "eval" => matches!(o, Opaque::Eval),
        "dynamic-command" => matches!(o, Opaque::DynamicCommand),
        "stdin-script" => matches!(o, Opaque::StdinScript),
        "sourced" => matches!(o, Opaque::Sourced),
        "alias" => matches!(o, Opaque::Alias),
        "unknown-wrapper" => matches!(o, Opaque::UnknownWrapper),
        "too-deep" => matches!(o, Opaque::TooDeep),
        other => panic!("battery.json names an unknown opaque kind `{other}`"),
    }
}
