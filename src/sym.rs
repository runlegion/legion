//! SCIP symbol query CLI (#282).
//!
//! Provides four query primitives over stored SCIP protobuf blobs:
//! `def`, `refs`, `impl`, and `hover`. Blobs are parsed in-process via
//! the `scip` crate; the data layer is read-only here -- writes happen
//! through `legion index` (#278/#279/#280).
//!
//! Symbol matching is name-based: SCIP encodes a fully qualified
//! identifier (`rust-analyzer cargo foo 1.0 src/lib.rs/Foo#`) and we
//! treat the user's `<name>` argument as a substring match on that
//! identifier. Exact matching by descriptor is a future refinement.

use crate::error::{LegionError, Result};
use protobuf::Message;
use serde::Serialize;

/// A definition or reference location resolved from a SCIP index.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SymbolLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub repo: String,
    pub lang: String,
}

/// Hover info: signature text + concatenated docstring.
#[derive(Debug, Clone, Serialize)]
pub struct HoverInfo {
    pub symbol: String,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub repo: String,
    pub lang: String,
}

/// Bitmask value for the `Definition` symbol role. Listed alongside the
/// proto-generated enum in scip-0.7.1 as `SymbolRole::Definition = 1`.
const ROLE_DEFINITION: i32 = 1;

fn parse_index(blob: &[u8]) -> Result<scip::types::Index> {
    scip::types::Index::parse_from_bytes(blob)
        .map_err(|e| LegionError::Search(format!("scip parse error: {e}")))
}

/// Half-open SCIP ranges encode `[startLine, startCharacter, endLine, endCharacter]`
/// or `[startLine, startCharacter, endCharacter]`. Either way we want the
/// (1-indexed) start line and column.
fn occurrence_position(range: &[i32]) -> Option<(u32, u32)> {
    let start_line = (*range.first()?).max(0) as u32 + 1;
    let start_col = (*range.get(1)?).max(0) as u32 + 1;
    Some((start_line, start_col))
}

/// Iterate every occurrence in `index`, collecting locations whose
/// `symbol` string contains `name` and whose role matches `want_def`
/// (true => Definition, false => any non-definition reference).
fn collect_locations(
    index: &scip::types::Index,
    name: &str,
    repo: &str,
    lang: &str,
    want_def: bool,
) -> Vec<SymbolLocation> {
    let mut out: Vec<SymbolLocation> = Vec::new();
    for doc in &index.documents {
        for occ in &doc.occurrences {
            if !occ.symbol.contains(name) {
                continue;
            }
            let is_def = (occ.symbol_roles & ROLE_DEFINITION) != 0;
            if want_def != is_def {
                continue;
            }
            if let Some((line, column)) = occurrence_position(&occ.range) {
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
    out.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));
    out
}

pub fn query_definitions(
    blob: &[u8],
    name: &str,
    repo: &str,
    lang: &str,
) -> Result<Vec<SymbolLocation>> {
    let idx = parse_index(blob)?;
    Ok(collect_locations(&idx, name, repo, lang, true))
}

pub fn query_references(
    blob: &[u8],
    name: &str,
    repo: &str,
    lang: &str,
) -> Result<Vec<SymbolLocation>> {
    let idx = parse_index(blob)?;
    Ok(collect_locations(&idx, name, repo, lang, false))
}

/// Implementors are encoded via SymbolInformation.relationships where
/// `is_implementation == true`. We collect every Document occurrence
/// whose `symbol` matches a SymbolInformation that declares an
/// implementation relationship to a target symbol containing `trait_name`.
pub fn query_implementors(
    blob: &[u8],
    trait_name: &str,
    repo: &str,
    lang: &str,
) -> Result<Vec<SymbolLocation>> {
    use std::collections::HashSet;
    let idx = parse_index(blob)?;
    let mut implementor_symbols: HashSet<String> = HashSet::new();
    for doc in &idx.documents {
        for sym in &doc.symbols {
            for rel in &sym.relationships {
                if rel.is_implementation && rel.symbol.contains(trait_name) {
                    implementor_symbols.insert(sym.symbol.clone());
                }
            }
        }
    }

    let mut out: Vec<SymbolLocation> = Vec::new();
    for doc in &idx.documents {
        for occ in &doc.occurrences {
            if !implementor_symbols.contains(&occ.symbol) {
                continue;
            }
            if (occ.symbol_roles & ROLE_DEFINITION) == 0 {
                continue;
            }
            if let Some((line, column)) = occurrence_position(&occ.range) {
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
    out.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));
    Ok(out)
}

/// One row of cross-repo symbol consult output.
#[derive(Debug, Clone, Serialize)]
pub struct CrossRepoSymbol {
    pub repo: String,
    pub lang: String,
    pub def_location: Option<SymbolLocation>,
    pub refs_count: usize,
}

/// Walk every stored SCIP blob (filtered to `repos` when non-empty)
/// and return per-(repo, lang) hits for `symbol_name`. Sorted by
/// `refs_count` descending, then by repo alphabetically. Repos with
/// no stored index, and blobs that fail to parse, are skipped with a
/// stderr warning -- one bad blob never aborts the consult.
pub fn cross_repo_symbol_query(
    db: &crate::db::Database,
    symbol_name: &str,
    repos: &[String],
) -> Result<Vec<CrossRepoSymbol>> {
    let mut out: Vec<CrossRepoSymbol> = Vec::new();

    let candidate_repos: Vec<String> = if repos.is_empty() {
        Vec::new()
    } else {
        repos.to_vec()
    };

    let blobs: Vec<crate::scip::ScipIndex> = if candidate_repos.is_empty() {
        // Caller did not narrow -- list every repo that has an index row.
        // We cheat the lack of "list everything" by doing a no-arg get on
        // every repo we've ever heard of; without that, we surface nothing,
        // which is a fail-quiet UX. db::list_index_jobs gives us a
        // superset of relevant repo names.
        let mut found: Vec<crate::scip::ScipIndex> = Vec::new();
        for job in db.list_index_jobs()? {
            found.extend(db.list_scip_indexes(&job.repo)?);
        }
        found
    } else {
        let mut found: Vec<crate::scip::ScipIndex> = Vec::new();
        for r in &candidate_repos {
            found.extend(db.list_scip_indexes(r)?);
        }
        found
    };

    for idx in &blobs {
        let defs = match query_definitions(&idx.blob, symbol_name, &idx.repo, &idx.lang) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[legion consult] skipping {}/{}: {e}", idx.repo, idx.lang);
                continue;
            }
        };
        let refs = match query_references(&idx.blob, symbol_name, &idx.repo, &idx.lang) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "[legion consult] skipping {}/{} refs: {e}",
                    idx.repo, idx.lang
                );
                continue;
            }
        };
        if defs.is_empty() && refs.is_empty() {
            continue;
        }
        out.push(CrossRepoSymbol {
            repo: idx.repo.clone(),
            lang: idx.lang.clone(),
            def_location: defs.into_iter().next(),
            refs_count: refs.len(),
        });
    }

    out.sort_by(|a, b| {
        b.refs_count
            .cmp(&a.refs_count)
            .then_with(|| a.repo.cmp(&b.repo))
    });
    Ok(out)
}

pub fn query_hover(blob: &[u8], name: &str, repo: &str, lang: &str) -> Result<Option<HoverInfo>> {
    let idx = parse_index(blob)?;
    for doc in &idx.documents {
        for sym in &doc.symbols {
            if !sym.symbol.contains(name) {
                continue;
            }
            let signature = sym
                .signature_documentation
                .as_ref()
                .map(|d| d.text.clone())
                .filter(|t| !t.is_empty());
            let docstring = if sym.documentation.is_empty() {
                None
            } else {
                Some(sym.documentation.join("\n"))
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
    use scip::types::{Document, Index, Occurrence, Relationship, SymbolInformation};

    fn build_index() -> Index {
        let mut def_occ = Occurrence::new();
        def_occ.symbol = "rust . crate . `fixture` . MyStruct#".to_string();
        def_occ.range = vec![9, 4, 9, 12]; // line 10, col 5
        def_occ.symbol_roles = ROLE_DEFINITION;

        let mut ref_occ = Occurrence::new();
        ref_occ.symbol = "rust . crate . `fixture` . MyStruct#".to_string();
        ref_occ.range = vec![19, 8, 19, 16]; // line 20, col 9
        ref_occ.symbol_roles = 0;

        let mut other_occ = Occurrence::new();
        other_occ.symbol = "rust . crate . `fixture` . OtherThing#".to_string();
        other_occ.range = vec![1, 0, 1, 5];
        other_occ.symbol_roles = ROLE_DEFINITION;

        let mut sym_info = SymbolInformation::new();
        sym_info.symbol = "rust . crate . `fixture` . MyStruct#".to_string();
        sym_info.documentation = vec!["A test struct.".to_string()];

        let mut impl_rel = Relationship::new();
        impl_rel.symbol = "rust . crate . std . MyTrait#".to_string();
        impl_rel.is_implementation = true;
        sym_info.relationships = vec![impl_rel];

        let mut doc = Document::new();
        doc.relative_path = "src/lib.rs".to_string();
        doc.occurrences = vec![def_occ, ref_occ, other_occ];
        doc.symbols = vec![sym_info];

        let mut idx = Index::new();
        idx.documents = vec![doc];
        idx
    }

    fn build_blob() -> Vec<u8> {
        build_index().write_to_bytes().unwrap()
    }

    #[test]
    fn query_definitions_finds_definition_only() {
        let blob = build_blob();
        let hits = query_definitions(&blob, "MyStruct", "fixture", "rust").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, "src/lib.rs");
        assert_eq!(hits[0].line, 10);
        assert_eq!(hits[0].column, 5);
    }

    #[test]
    fn query_definitions_empty_blob_returns_empty() {
        let hits = query_definitions(&[], "Foo", "r", "rust").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn query_definitions_corrupt_blob_returns_search_error() {
        let err = query_definitions(&[1u8, 2, 3, 4, 5, 6, 7, 8], "x", "r", "rust").unwrap_err();
        match err {
            LegionError::Search(msg) => assert!(msg.contains("scip parse error")),
            other => panic!("expected Search, got {other:?}"),
        }
    }

    #[test]
    fn query_references_excludes_definitions() {
        let blob = build_blob();
        let hits = query_references(&blob, "MyStruct", "fixture", "rust").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 20);
    }

    #[test]
    fn query_references_unknown_name_returns_empty() {
        let blob = build_blob();
        let hits = query_references(&blob, "DoesNotExist", "fixture", "rust").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn query_implementors_returns_struct_that_implements_named_trait() {
        let blob = build_blob();
        let hits = query_implementors(&blob, "MyTrait", "fixture", "rust").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 10);
    }

    #[test]
    fn query_implementors_no_match_returns_empty() {
        let blob = build_blob();
        let hits = query_implementors(&blob, "UnrelatedTrait", "fixture", "rust").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn query_hover_returns_docstring_for_known_symbol() {
        let blob = build_blob();
        let hover = query_hover(&blob, "MyStruct", "fixture", "rust")
            .unwrap()
            .unwrap();
        assert!(hover.symbol.contains("MyStruct"));
        assert_eq!(hover.docstring.as_deref(), Some("A test struct."));
    }

    #[test]
    fn query_hover_unknown_symbol_returns_none() {
        let blob = build_blob();
        let hover = query_hover(&blob, "NoSuch", "fixture", "rust").unwrap();
        assert!(hover.is_none());
    }

    fn make_blob() -> Vec<u8> {
        build_blob()
    }

    fn open_test_db() -> crate::db::Database {
        let dir = tempfile::TempDir::new().unwrap();
        let db = crate::db::Database::open(&dir.path().join("test.sqlite")).unwrap();
        std::mem::forget(dir);
        db
    }

    #[test]
    fn cross_repo_symbol_query_collects_per_repo_hits() {
        let db = open_test_db();
        let blob = make_blob();
        let now = "2026-04-28T00:00:00Z";

        for (repo, lang) in [("alpha", "rust"), ("beta", "rust")] {
            db.upsert_scip_index(&crate::scip::ScipIndex {
                id: uuid::Uuid::now_v7().to_string(),
                repo: repo.to_string(),
                lang: lang.to_string(),
                content_hash: crate::scip::content_hash(&blob),
                blob: blob.clone(),
                updated_at: now.to_string(),
                deleted_at: None,
            })
            .unwrap();
        }

        let rows =
            cross_repo_symbol_query(&db, "MyStruct", &["alpha".to_string(), "beta".to_string()])
                .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].refs_count, 1);
        assert_eq!(rows[1].refs_count, 1);
    }

    #[test]
    fn cross_repo_symbol_query_repo_with_no_index_is_skipped() {
        let db = open_test_db();
        let rows = cross_repo_symbol_query(&db, "MyStruct", &["nope".to_string()]).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn cross_repo_symbol_query_sorted_by_refs_count_desc() {
        // Build two indexes -- the second has the symbol referenced more
        // often, so it should sort first.
        let db = open_test_db();
        let now = "2026-04-28T00:00:00Z";

        // Repo "few": one ref.
        let blob_few = make_blob();
        db.upsert_scip_index(&crate::scip::ScipIndex {
            id: uuid::Uuid::now_v7().to_string(),
            repo: "few".to_string(),
            lang: "rust".to_string(),
            content_hash: crate::scip::content_hash(&blob_few),
            blob: blob_few,
            updated_at: now.to_string(),
            deleted_at: None,
        })
        .unwrap();

        // Repo "many": multiple ref occurrences for the same symbol.
        let mut idx = build_index();
        for i in 0..3 {
            let mut occ = scip::types::Occurrence::new();
            occ.symbol = "rust . crate . `fixture` . MyStruct#".to_string();
            occ.range = vec![100 + i, 0, 100 + i, 5];
            occ.symbol_roles = 0;
            idx.documents[0].occurrences.push(occ);
        }
        let blob_many = idx.write_to_bytes().unwrap();
        db.upsert_scip_index(&crate::scip::ScipIndex {
            id: uuid::Uuid::now_v7().to_string(),
            repo: "many".to_string(),
            lang: "rust".to_string(),
            content_hash: crate::scip::content_hash(&blob_many),
            blob: blob_many,
            updated_at: now.to_string(),
            deleted_at: None,
        })
        .unwrap();

        let rows =
            cross_repo_symbol_query(&db, "MyStruct", &["few".to_string(), "many".to_string()])
                .unwrap();
        assert_eq!(rows[0].repo, "many");
        assert!(rows[0].refs_count > rows[1].refs_count);
    }

    #[test]
    fn json_serialization_round_trips_array() {
        let loc = SymbolLocation {
            file: "a.rs".into(),
            line: 1,
            column: 1,
            repo: "r".into(),
            lang: "rust".into(),
        };
        let v = serde_json::to_string(&[&loc]).unwrap();
        assert!(v.starts_with('[') && v.ends_with(']'));
        assert!(v.contains("\"file\":\"a.rs\""));
    }
}
