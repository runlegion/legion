//! Runs the 88-row adversarial battery (FR-CMD-007) against
//! [`legion_cmd::scan`]: 72 rows adapted from RESEARCH-CMD-bounded-
//! tokenizer's hand-labelled fixtures, plus 16 rows this issue's builder
//! derived from that research's adversarial review (each naming its
//! `source`). Every row carries an expected verdict -- `visible`, `opaque`,
//! `benign`, or `error` -- per FR-CMD-007's acceptance criteria, which
//! names all four (the issue's own Fixture battery section states only
//! three; the traced requirement is authoritative).
//!
//! A `visible`/`benign` row's `managed` list is a presence check: each
//! named `(binary, position)` pair must appear among `scan`'s resolved
//! invocations. A `benign` row also names `forbidden` binaries that must
//! NOT appear as a resolved invocation -- the known false positives a
//! naive text-match would wrongly flag. An `opaque` row's `opaque` list
//! names the expected `Opaque` variant per entry (`Interpreter:name` for
//! the interpreter case, matching both the variant and its interpreter
//! field). An `error` row asserts `scan` returns `Err`, never a partial
//! `Scan` (Error Handling: malformed input never yields a partial `Scan`).

use legion_cmd::{Opaque, scan};
use serde::Deserialize;

/// A row's expected verdict. `#[serde(rename_all = "lowercase")]` makes an
/// unrecognized string in `battery.json` a deserialize error at parse time,
/// rather than a runtime panic reached only once the row is evaluated.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Verdict {
    Visible,
    Opaque,
    Benign,
    Error,
}

#[derive(Debug, Deserialize)]
struct Row {
    id: String,
    cmd: String,
    expected: Verdict,
    #[serde(default)]
    managed: Vec<(String, String)>,
    #[serde(default)]
    opaque: Vec<String>,
    #[serde(default)]
    forbidden: Vec<String>,
    #[serde(default)]
    source: Option<String>,
}

const BATTERY_JSON: &str = include_str!("fixtures/battery.json");

fn opaque_matches(entry: &Opaque, expected: &str) -> bool {
    match entry {
        Opaque::Interpreter { interpreter, .. } => expected == format!("Interpreter:{interpreter}"),
        Opaque::ScriptFile => expected == "ScriptFile",
        Opaque::Eval => expected == "Eval",
        Opaque::DynamicCommand => expected == "DynamicCommand",
        Opaque::StdinScript => expected == "StdinScript",
        Opaque::Sourced => expected == "Sourced",
        Opaque::Alias => expected == "Alias",
        Opaque::UnknownWrapper => expected == "UnknownWrapper",
        Opaque::TooDeep => expected == "TooDeep",
    }
}

#[test]
fn battery_has_88_rows_72_research_plus_16_derived() {
    let rows: Vec<Row> = serde_json::from_str(BATTERY_JSON).expect("battery.json parses");
    assert_eq!(
        rows.len(),
        88,
        "expected 72 research rows + 16 derived rows"
    );
    let derived: Vec<&Row> = rows.iter().filter(|r| r.source.is_some()).collect();
    assert_eq!(derived.len(), 16, "each derived row names its source");
    for row in &derived {
        assert!(
            !row.source.as_deref().unwrap_or("").is_empty(),
            "row {} has an empty source",
            row.id
        );
    }
}

#[test]
fn battery_row_ids_are_unique() {
    let rows: Vec<Row> = serde_json::from_str(BATTERY_JSON).expect("battery.json parses");
    let mut ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    ids.sort_unstable();
    let mut deduped = ids.clone();
    deduped.dedup();
    assert_eq!(ids.len(), deduped.len(), "duplicate battery row id");
}

#[test]
fn battery_matches_expected_verdict_per_row() {
    let rows: Vec<Row> = serde_json::from_str(BATTERY_JSON).expect("battery.json parses");
    let total = rows.len();
    let mut unexpected_errors: Vec<String> = Vec::new();

    for row in &rows {
        let result = scan(&row.cmd);
        if row.expected == Verdict::Error {
            assert!(
                result.is_err(),
                "row {}: expected ScanError, got {:?}",
                row.id,
                result
            );
            continue;
        }

        // A row that expected a `Scan` but got a `ScanError` counts
        // against the kill condition's parse-error rate; collect it and
        // move on rather than panicking here, so the rate printed below
        // reflects every such row, not just the first one hit.
        let scanned = match result {
            Ok(scanned) => scanned,
            Err(e) => {
                unexpected_errors.push(format!("row {}: unexpected ScanError: {e}", row.id));
                continue;
            }
        };

        for (binary, position) in &row.managed {
            let found = scanned
                .invocations
                .iter()
                .any(|inv| &inv.binary == binary && format!("{:?}", inv.position) == *position);
            assert!(
                found,
                "row {}: expected invocation {binary:?} at {position} not found in {:?}",
                row.id, scanned.invocations
            );
        }

        for expected_class in &row.opaque {
            let found = scanned
                .opaque
                .iter()
                .any(|entry| opaque_matches(entry, expected_class));
            assert!(
                found,
                "row {}: expected opaque class {expected_class:?} not found in {:?}",
                row.id, scanned.opaque
            );
        }

        for forbidden in &row.forbidden {
            let found = scanned
                .invocations
                .iter()
                .any(|inv| &inv.binary == forbidden);
            assert!(
                !found,
                "row {}: {forbidden:?} must not be a resolved invocation, got {:?}",
                row.id, scanned.invocations
            );
        }

        if row.expected == Verdict::Opaque {
            assert!(
                !scanned.opaque.is_empty(),
                "row {}: expected at least one opaque region",
                row.id
            );
        }
    }

    // RESEARCH-CMD-bounded-tokenizer's kill condition: a parse-error rate
    // above 0.1 percent on cooperative input. This battery's expected
    // errors (malformed input) do not count against it; only a row that
    // expected a `Scan` and got a `ScanError` does, collected above.
    let rate = (unexpected_errors.len() as f64 / total as f64) * 100.0;
    println!(
        "battery parse-error rate: {rate:.3}% ({} of {total} rows)",
        unexpected_errors.len()
    );
    assert!(unexpected_errors.is_empty(), "{unexpected_errors:#?}");
}
