//! SCIP symbol query layer.
//!
//! Reads `scip_indexes` blobs produced by `legion index` (#278) and
//! answers def/refs/impl/hover queries by parsing the protobuf in-process
//! with the `scip` crate. The blob is opaque to legion at write time;
//! all decoding happens here on the read path.

use crate::error::{LegionError, Result};
use protobuf::Message;
use scip::types::{Index, SymbolRole};
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

/// Substring match against the SCIP symbol string. SCIP symbols look like
/// `rust-analyzer cargo legion 0.9.10 src/sym.rs/SymbolLocation#`; the user
/// typically supplies a short identifier, so substring is the pragmatic
/// surface for v1. Future: descriptor-aware matching that respects
/// `#`/`.`/`(` suffix semantics.
fn symbol_matches(scip_symbol: &str, query: &str) -> bool {
    scip_symbol.contains(query)
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
