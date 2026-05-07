//! SCIP symbol query layer.
//!
//! Reads `scip_indexes` blobs produced by `legion index` (#278) and
//! answers def/refs/impl/hover queries by parsing the protobuf in-process
//! with the `scip` crate. The blob is opaque to legion at write time;
//! all decoding happens here on the read path.

use crate::error::{LegionError, Result};
use protobuf::Message;
use scip::symbol::parse_symbol as scip_parse_symbol;
use scip::types::{Descriptor, Index, SymbolRole};
use serde::Serialize;

/// A resolved symbol location returned by def and refs queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub repo: String,
    pub lang: String,
}

/// One impact-radius row: a symbol whose definition was touched by the
/// diff, plus the SCIP reference count across the indexed repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactRadius {
    pub symbol: String,
    pub refs_count: u32,
    pub def_location: Option<SymbolLocation>,
}

/// Hover information: signature + docstring extracted from a SCIP document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoverInfo {
    pub symbol: String,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub repo: String,
    pub lang: String,
}

fn parse_blob(blob: &[u8]) -> Result<Index> {
    Index::parse_from_bytes(blob).map_err(|e| LegionError::Search(format!("scip parse error: {e}")))
}

/// SCIP encodes the occurrence range as `[startLine, startCol, endLine, endCol]`
/// or `[startLine, startCol, endCol]` (end line implicit). Returns `(line, col)`
/// in 1-indexed form for human display, or `None` if the range is malformed.
fn range_start_1indexed(range: &[i32]) -> Option<(u32, u32)> {
    let line = *range.first()?;
    let col = *range.get(1)?;
    if line < 0 || col < 0 {
        return None;
    }
    Some((line as u32 + 1, col as u32 + 1))
}

/// True when the SymbolRole bitset has the Definition bit set.
fn is_definition(symbol_roles: i32) -> bool {
    (symbol_roles & SymbolRole::Definition as i32) != 0
}

/// Match a user query against a SCIP symbol string. Behavior depends on
/// whether each side parses cleanly with `scip::symbol::parse_symbol`:
///
/// - Symbol parses + query has explicit suffix(es) -> descriptor path match.
///   The query's descriptor sequence must appear as a contiguous run inside
///   the symbol's descriptor path. `Foo#` matches `mod/Foo#new().`, but does
///   not match `MyFoo#`. `Foo#bar().` matches `mod/Foo#bar().`. `mod/Foo#`
///   matches only when both Namespace `mod` and Type `Foo` appear in order.
///
/// - Symbol parses + bare-name query -> exact-name match against any
///   descriptor in the symbol. `Foo` matches `mod/Foo#` (descriptor name
///   `Foo`) but not `MyFoo#` (descriptor name `MyFoo`). This is the
///   precision win over substring -- bare queries no longer bleed into
///   composite identifiers.
///
/// - Symbol fails to parse -> substring fallback against the raw symbol
///   string. Defensive path for unusual indexer output. Preserves the v1
///   behavior in worst-case scenarios but should be rare in practice.
fn symbol_matches(scip_symbol: &str, query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    let query_descs = parse_query_descriptors(query);
    match scip_parse_symbol(scip_symbol) {
        Ok(parsed) => match query_descs {
            Some(qd) => descriptor_path_match(&parsed.descriptors, &qd),
            None => parsed.descriptors.iter().any(|d| d.name == query),
        },
        Err(_) => scip_symbol.contains(query),
    }
}

/// Parse the user's query string as if it were the descriptor portion of a
/// SCIP symbol. Wraps the query with a synthetic `q . . . ` package prefix
/// so `scip::symbol::parse_symbol` can do the heavy lifting -- it understands
/// suffix escaping, package fields, and edge cases the issue spec calls out.
///
/// Returns `None` for bare names (no trailing suffix character) so the caller
/// can route those through the exact-name / substring tiers instead.
fn parse_query_descriptors(query: &str) -> Option<Vec<Descriptor>> {
    if query.is_empty() || query.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let synthetic = format!("q . . . {query}");
    scip_parse_symbol(&synthetic).ok().and_then(|s| {
        if s.descriptors.is_empty() {
            None
        } else {
            Some(s.descriptors)
        }
    })
}

/// True when two descriptors agree on both name and SCIP suffix kind.
fn descriptor_eq(q: &Descriptor, s: &Descriptor) -> bool {
    q.name == s.name && q.suffix.enum_value_or_default() == s.suffix.enum_value_or_default()
}

/// Sliding-window match: true when `query_descs` appears as a contiguous
/// subsequence inside `symbol_descs`. This anchors precision at the path
/// level (Type `Foo` inside `mod/Foo#` matches a `Foo#` query) while still
/// finding references whose surrounding context the user did not type out.
fn descriptor_path_match(symbol_descs: &[Descriptor], query_descs: &[Descriptor]) -> bool {
    if query_descs.is_empty() || query_descs.len() > symbol_descs.len() {
        return false;
    }
    let qlen = query_descs.len();
    let last_start = symbol_descs.len() - qlen;
    (0..=last_start).any(|start| {
        symbol_descs[start..start + qlen]
            .iter()
            .zip(query_descs.iter())
            .all(|(s, q)| descriptor_eq(q, s))
    })
}

fn sort_locations(locs: &mut [SymbolLocation]) {
    locs.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
}

pub fn query_definitions(
    blob: &[u8],
    symbol_name: &str,
    repo: &str,
    lang: &str,
) -> Result<Vec<SymbolLocation>> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    let index = parse_blob(blob)?;
    let mut out = Vec::new();
    for doc in &index.documents {
        for occ in &doc.occurrences {
            if !is_definition(occ.symbol_roles) {
                continue;
            }
            if !symbol_matches(&occ.symbol, symbol_name) {
                continue;
            }
            if let Some((line, column)) = range_start_1indexed(&occ.range) {
                out.push(SymbolLocation {
                    file: doc.relative_path.clone(),
                    line,
                    column,
                    repo: repo.to_string(),
                    lang: lang.to_string(),
                });
            }
        }
    }
    sort_locations(&mut out);
    Ok(out)
}

pub fn query_references(
    blob: &[u8],
    symbol_name: &str,
    repo: &str,
    lang: &str,
) -> Result<Vec<SymbolLocation>> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    let index = parse_blob(blob)?;
    let mut out = Vec::new();
    for doc in &index.documents {
        for occ in &doc.occurrences {
            if is_definition(occ.symbol_roles) {
                continue;
            }
            if !symbol_matches(&occ.symbol, symbol_name) {
                continue;
            }
            if let Some((line, column)) = range_start_1indexed(&occ.range) {
                out.push(SymbolLocation {
                    file: doc.relative_path.clone(),
                    line,
                    column,
                    repo: repo.to_string(),
                    lang: lang.to_string(),
                });
            }
        }
    }
    sort_locations(&mut out);
    Ok(out)
}

pub fn query_implementors(
    blob: &[u8],
    trait_name: &str,
    repo: &str,
    lang: &str,
) -> Result<Vec<SymbolLocation>> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    let index = parse_blob(blob)?;

    // Collect the SCIP symbols of every type that declares an
    // `is_implementation` relationship to a symbol matching `trait_name`.
    let mut impl_symbols: Vec<String> = Vec::new();
    for doc in &index.documents {
        for sym in &doc.symbols {
            for rel in &sym.relationships {
                if rel.is_implementation && symbol_matches(&rel.symbol, trait_name) {
                    impl_symbols.push(sym.symbol.clone());
                }
            }
        }
    }
    if impl_symbols.is_empty() {
        return Ok(Vec::new());
    }

    // Resolve each implementing symbol to its definition occurrence.
    let mut out = Vec::new();
    for doc in &index.documents {
        for occ in &doc.occurrences {
            if !is_definition(occ.symbol_roles) {
                continue;
            }
            if !impl_symbols.iter().any(|s| s == &occ.symbol) {
                continue;
            }
            if let Some((line, column)) = range_start_1indexed(&occ.range) {
                out.push(SymbolLocation {
                    file: doc.relative_path.clone(),
                    line,
                    column,
                    repo: repo.to_string(),
                    lang: lang.to_string(),
                });
            }
        }
    }
    sort_locations(&mut out);
    Ok(out)
}

/// One file's worth of changed line ranges from a unified diff. Stores
/// `(start_line, end_line_inclusive)` on the new (post-diff) side. We
/// extract from `@@ -old +new @@` hunk headers; per-line + / - markers
/// inside the hunk are not needed for impact-radius -- the hunk new
/// range is the "touched area" and any def whose start line falls in
/// that range counts as touched.
type LineRange = (u32, u32);

/// Map relative_path -> Vec<LineRange> on the new side.
type DiffRanges = std::collections::HashMap<String, Vec<LineRange>>;

/// Parse a unified diff (the output of `git diff` or similar) into the
/// minimal per-file new-side line ranges that `diff_impact_radius`
/// needs. Lines that the diff format does not understand (binary diffs,
/// rename-only entries with no `@@` hunk) are silently skipped.
pub fn parse_unified_diff(diff_text: &str) -> DiffRanges {
    let mut out: DiffRanges = std::collections::HashMap::new();
    let mut current_path: Option<String> = None;
    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            // `+++ b/path/to/file.rs` -- strip the "b/" git convention.
            // `/dev/null` (file deleted) leaves current_path None so the
            // following hunks land nowhere.
            let path = rest.trim();
            if path == "/dev/null" {
                current_path = None;
                continue;
            }
            let stripped = path.strip_prefix("b/").unwrap_or(path);
            current_path = Some(stripped.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            // "@@ -10,5 +12,7 @@ optional context"
            // We want the +start,count token.
            if let Some(plus_token) = rest.split_whitespace().find(|t| t.starts_with('+')) {
                let plus = plus_token.trim_start_matches('+');
                let (start_str, count_str) = match plus.split_once(',') {
                    Some((s, c)) => (s, c),
                    None => (plus, "1"),
                };
                let start: u32 = match start_str.parse() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                let count: u32 = count_str.parse().unwrap_or(1);
                if let Some(path) = &current_path {
                    let end = start.saturating_add(count.saturating_sub(1)).max(start);
                    out.entry(path.clone()).or_default().push((start, end));
                }
            }
        }
    }
    out
}

/// Given a SCIP index blob and a unified diff, return one ImpactRadius
/// row for every symbol whose definition occurrence falls inside any
/// changed line range. `refs_count` is the total non-definition
/// occurrences for that symbol across the entire index. Result is
/// unsorted; the CLI handler sorts descending by refs_count.
///
/// An empty index or a diff that touches no SCIP-indexed file returns
/// an empty Vec, not an error -- callers route that to a friendly
/// "nothing to report" message.
pub fn diff_impact_radius(blob: &[u8], diff_text: &str) -> Result<Vec<ImpactRadius>> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    let ranges = parse_unified_diff(diff_text);
    if ranges.is_empty() {
        return Ok(Vec::new());
    }
    let index = parse_blob(blob)?;

    // First pass: collect (symbol -> refs_count) across the entire index.
    let mut refs_count: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for doc in &index.documents {
        for occ in &doc.occurrences {
            if !is_definition(occ.symbol_roles) {
                *refs_count.entry(occ.symbol.clone()).or_insert(0) += 1;
            }
        }
    }

    // Second pass: pick definitions whose range falls inside a hunk on
    // the file's diff entry. Dedup on symbol so a symbol with multiple
    // definitions (rare but legal) only contributes one row.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut hits = Vec::new();
    for doc in &index.documents {
        let Some(file_ranges) = ranges.get(&doc.relative_path) else {
            continue;
        };
        for occ in &doc.occurrences {
            if !is_definition(occ.symbol_roles) {
                continue;
            }
            let Some((line, column)) = range_start_1indexed(&occ.range) else {
                continue;
            };
            if !file_ranges.iter().any(|(s, e)| line >= *s && line <= *e) {
                continue;
            }
            if !seen.insert(occ.symbol.clone()) {
                continue;
            }
            let count = refs_count.get(&occ.symbol).copied().unwrap_or(0);
            hits.push(ImpactRadius {
                symbol: occ.symbol.clone(),
                refs_count: count,
                def_location: Some(SymbolLocation {
                    file: doc.relative_path.clone(),
                    line,
                    column,
                    repo: String::new(),
                    lang: String::new(),
                }),
            });
        }
    }
    Ok(hits)
}

pub fn query_hover(
    blob: &[u8],
    symbol_name: &str,
    repo: &str,
    lang: &str,
) -> Result<Option<HoverInfo>> {
    if blob.is_empty() {
        return Ok(None);
    }
    let index = parse_blob(blob)?;
    for doc in &index.documents {
        for sym in &doc.symbols {
            if !symbol_matches(&sym.symbol, symbol_name) {
                continue;
            }
            let signature = sym
                .signature_documentation
                .as_ref()
                .map(|d| d.text.clone())
                .filter(|s| !s.is_empty());
            let docstring = if sym.documentation.is_empty() {
                None
            } else {
                Some(sym.documentation.join("\n\n"))
            };
            return Ok(Some(HoverInfo {
                symbol: sym.symbol.clone(),
                signature,
                docstring,
                repo: repo.to_string(),
                lang: lang.to_string(),
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scip::types::{Document, Occurrence, Relationship, SymbolInformation};

    fn occ(symbol: &str, range: Vec<i32>, is_def: bool) -> Occurrence {
        let mut o = Occurrence::new();
        o.symbol = symbol.to_string();
        o.range = range;
        if is_def {
            o.symbol_roles = SymbolRole::Definition as i32;
        }
        o
    }

    fn doc(relative_path: &str, occurrences: Vec<Occurrence>) -> Document {
        let mut d = Document::new();
        d.relative_path = relative_path.to_string();
        d.occurrences = occurrences;
        d
    }

    fn build_index(documents: Vec<Document>) -> Vec<u8> {
        let mut idx = Index::new();
        idx.documents = documents;
        idx.write_to_bytes().unwrap()
    }

    #[test]
    fn parse_unified_diff_extracts_per_file_new_ranges() {
        let diff = "\
diff --git a/src/foo.rs b/src/foo.rs
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -10,3 +12,5 @@ fn ctx
 a
-b
+c
+d
+e
@@ -50,1 +60,2 @@
+x
+y
diff --git a/src/bar.rs b/src/bar.rs
--- a/src/bar.rs
+++ b/src/bar.rs
@@ -1,2 +1,3 @@
 keep
+new
+also new
";
        let ranges = parse_unified_diff(diff);
        assert_eq!(ranges.get("src/foo.rs").unwrap(), &vec![(12, 16), (60, 61)]);
        assert_eq!(ranges.get("src/bar.rs").unwrap(), &vec![(1, 3)]);
    }

    #[test]
    fn parse_unified_diff_skips_dev_null_for_deleted_files() {
        let diff = "\
diff --git a/old.rs b/old.rs
--- a/old.rs
+++ /dev/null
@@ -1,3 +0,0 @@
-removed
";
        let ranges = parse_unified_diff(diff);
        assert!(ranges.is_empty());
    }

    #[test]
    fn parse_unified_diff_handles_single_line_hunks_without_count() {
        let diff = "\
+++ b/src/lib.rs
@@ -42 +42 @@
-old
+new
";
        let ranges = parse_unified_diff(diff);
        assert_eq!(ranges.get("src/lib.rs").unwrap(), &vec![(42, 42)]);
    }

    #[test]
    fn diff_impact_radius_returns_definitions_in_changed_ranges_with_ref_counts() {
        let blob = build_index(vec![doc(
            "src/foo.rs",
            vec![
                occ("hot()", vec![19, 0, 19, 3], true),
                occ("cold()", vec![100, 0, 100, 3], true),
                occ("hot()", vec![5, 0, 5, 3], false),
                occ("hot()", vec![6, 0, 6, 3], false),
                occ("cold()", vec![200, 0, 200, 3], false),
            ],
        )]);
        let diff = "\
+++ b/src/foo.rs
@@ -18,3 +18,5 @@
 ctx
+changed
+changed
";
        let hits = diff_impact_radius(&blob, diff).unwrap();
        assert_eq!(hits.len(), 1, "only `hot` is in the changed range");
        assert_eq!(hits[0].symbol, "hot()");
        assert_eq!(hits[0].refs_count, 2);
        let loc = hits[0].def_location.as_ref().unwrap();
        assert_eq!(loc.file, "src/foo.rs");
        assert_eq!(loc.line, 20);
    }

    #[test]
    fn diff_impact_radius_empty_when_diff_touches_no_indexed_file() {
        let blob = build_index(vec![doc(
            "src/foo.rs",
            vec![occ("Foo#", vec![5, 0, 5, 3], true)],
        )]);
        let diff = "\
+++ b/src/unrelated.rs
@@ -1,1 +1,1 @@
 line
";
        let hits = diff_impact_radius(&blob, diff).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn diff_impact_radius_dedupes_repeated_definitions() {
        let blob = build_index(vec![
            doc("src/a.rs", vec![occ("Foo#", vec![9, 0, 9, 3], true)]),
            doc("src/b.rs", vec![occ("Foo#", vec![19, 0, 19, 3], true)]),
        ]);
        let diff = "\
+++ b/src/a.rs
@@ -8,3 +8,3 @@
 line
+changed
+++ b/src/b.rs
@@ -18,3 +18,3 @@
 line
+changed
";
        let hits = diff_impact_radius(&blob, diff).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn empty_blob_returns_empty_for_definitions() {
        let out = query_definitions(&[], "Foo", "r", "rust").unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn empty_blob_returns_empty_for_references() {
        let out = query_references(&[], "Foo", "r", "rust").unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn empty_blob_returns_none_for_hover() {
        let out = query_hover(&[], "Foo", "r", "rust").unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn corrupt_blob_returns_search_error() {
        let err = query_definitions(b"not a protobuf", "x", "r", "rust").unwrap_err();
        match err {
            LegionError::Search(msg) => assert!(msg.contains("scip parse error")),
            other => panic!("expected Search error, got {other:?}"),
        }
    }

    #[test]
    fn definitions_returns_only_definition_occurrences() {
        let blob = build_index(vec![doc(
            "src/lib.rs",
            vec![
                occ("Foo#", vec![10, 4, 10, 7], true),   // definition
                occ("Foo#", vec![25, 8, 25, 11], false), // reference
            ],
        )]);
        let defs = query_definitions(&blob, "Foo", "myrepo", "rust").unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].file, "src/lib.rs");
        assert_eq!(defs[0].line, 11); // 10 + 1 for 1-indexing
        assert_eq!(defs[0].column, 5); // 4 + 1
        assert_eq!(defs[0].repo, "myrepo");
        assert_eq!(defs[0].lang, "rust");
    }

    #[test]
    fn references_excludes_definitions() {
        let blob = build_index(vec![doc(
            "src/lib.rs",
            vec![
                occ("Foo#", vec![10, 4, 10, 7], true),
                occ("Foo#", vec![25, 8, 25, 11], false),
                occ("Foo#", vec![40, 12, 40, 15], false),
            ],
        )]);
        let refs = query_references(&blob, "Foo", "myrepo", "rust").unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].line, 26);
        assert_eq!(refs[1].line, 41);
    }

    #[test]
    fn references_for_unknown_name_returns_empty() {
        let blob = build_index(vec![doc(
            "src/lib.rs",
            vec![occ("Foo#", vec![10, 4, 10, 7], false)],
        )]);
        let refs = query_references(&blob, "Bar", "myrepo", "rust").unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn results_sorted_by_file_then_line() {
        let blob = build_index(vec![
            doc("src/zzz.rs", vec![occ("Foo#", vec![5, 0, 5, 3], false)]),
            doc(
                "src/aaa.rs",
                vec![
                    occ("Foo#", vec![20, 0, 20, 3], false),
                    occ("Foo#", vec![10, 0, 10, 3], false),
                ],
            ),
        ]);
        let refs = query_references(&blob, "Foo", "myrepo", "rust").unwrap();
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].file, "src/aaa.rs");
        assert_eq!(refs[0].line, 11);
        assert_eq!(refs[1].file, "src/aaa.rs");
        assert_eq!(refs[1].line, 21);
        assert_eq!(refs[2].file, "src/zzz.rs");
    }

    #[test]
    fn implementors_finds_types_implementing_trait() {
        let mut sym_dog = SymbolInformation::new();
        sym_dog.symbol = "Dog#".to_string();
        let mut rel = Relationship::new();
        rel.symbol = "Animal#".to_string();
        rel.is_implementation = true;
        sym_dog.relationships = vec![rel];

        let mut d = Document::new();
        d.relative_path = "src/lib.rs".to_string();
        d.symbols = vec![sym_dog];
        d.occurrences = vec![occ("Dog#", vec![3, 0, 3, 3], true)];

        let blob = build_index(vec![d]);
        let impls = query_implementors(&blob, "Animal", "myrepo", "rust").unwrap();
        assert_eq!(impls.len(), 1);
        assert_eq!(impls[0].file, "src/lib.rs");
        assert_eq!(impls[0].line, 4);
    }

    #[test]
    fn implementors_returns_empty_when_no_relationships() {
        let blob = build_index(vec![doc(
            "src/lib.rs",
            vec![occ("Foo#", vec![10, 0, 10, 3], true)],
        )]);
        let impls = query_implementors(&blob, "Animal", "myrepo", "rust").unwrap();
        assert!(impls.is_empty());
    }

    #[test]
    fn hover_returns_signature_and_docstring() {
        let mut sig_doc = Document::new();
        sig_doc.text = "fn foo(x: u32) -> u32".to_string();

        let mut sym = SymbolInformation::new();
        sym.symbol = "foo().".to_string();
        sym.documentation = vec!["Adds one to the input.".to_string()];
        sym.signature_documentation = ::protobuf::MessageField::some(sig_doc);

        let mut d = Document::new();
        d.relative_path = "src/lib.rs".to_string();
        d.symbols = vec![sym];

        let blob = build_index(vec![d]);
        let hover = query_hover(&blob, "foo", "myrepo", "rust")
            .unwrap()
            .unwrap();
        assert_eq!(hover.symbol, "foo().");
        assert_eq!(hover.signature.as_deref(), Some("fn foo(x: u32) -> u32"));
        assert_eq!(hover.docstring.as_deref(), Some("Adds one to the input."));
    }

    #[test]
    fn hover_returns_none_when_symbol_absent() {
        let blob = build_index(vec![doc("src/lib.rs", vec![])]);
        let hover = query_hover(&blob, "foo", "myrepo", "rust").unwrap();
        assert!(hover.is_none());
    }

    #[test]
    fn json_serialization_round_trips() {
        let loc = SymbolLocation {
            file: "src/lib.rs".to_string(),
            line: 10,
            column: 5,
            repo: "r".to_string(),
            lang: "rust".to_string(),
        };
        let json = serde_json::to_string(&loc).unwrap();
        assert!(json.contains("\"file\":\"src/lib.rs\""));
        assert!(json.contains("\"line\":10"));
    }
}
