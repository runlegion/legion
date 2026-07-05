//! CLI end-to-end tests for the CSS symbol wiring in `legion index` (#711).
//!
//! `src/css.rs` and `src/db/css_symbols.rs` carry their own unit tests for
//! the parse/extract logic and the persistence API in isolation; these tests
//! exercise the actual binary surface -- a real `legion index <repo>` run
//! over a small CSS fixture (including a Tailwind-v4-shaped `@theme` block)
//! must populate `css_symbols` and bump `file_inventory.symbol_count` for
//! the css file. No CLI query surface exists yet (see
//! `Database::find_css_symbol`'s doc comment), so assertions read the
//! `css_symbols` and `file_inventory` tables directly out of the data dir's
//! `legion.db`. Mirrors `module_graph.rs`'s shape (#710).

use rusqlite::Connection;

use crate::common::{legion_cmd, run_ok};

/// Seed a watch.toml in the data dir pointing at `repos` (name, workdir).
/// Mirrors `module_graph::seed_watch_toml` -- forward-slash normalized so a
/// Windows path interpolated into this basic TOML string does not have its
/// backslashes read back as escape sequences.
fn seed_watch_toml(data_dir: &std::path::Path, repos: &[(&str, &std::path::Path)]) {
    let mut toml = String::new();
    for (name, workdir) in repos {
        toml.push_str(&format!(
            "[[repos]]\nname = \"{}\"\nworkdir = \"{}\"\n\n",
            name,
            workdir.display().to_string().replace('\\', "/")
        ));
    }
    std::fs::write(data_dir.join("watch.toml"), toml).expect("seed watch.toml");
}

/// One `(path, kind, name, line)` row read back from `css_symbols`.
fn css_symbols_for_repo(
    data_dir: &std::path::Path,
    repo: &str,
) -> Vec<(String, String, String, i64)> {
    let conn = Connection::open(data_dir.join("legion.db")).expect("open legion.db");
    let mut stmt = conn
        .prepare(
            "SELECT path, kind, name, line FROM css_symbols WHERE repo = ?1 ORDER BY path, name",
        )
        .expect("prepare query");
    stmt.query_map([repo], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })
    .expect("query_map")
    .collect::<Result<Vec<_>, _>>()
    .expect("collect rows")
}

fn symbol_count_for_file(data_dir: &std::path::Path, repo: &str, path: &str) -> i64 {
    let conn = Connection::open(data_dir.join("legion.db")).expect("open legion.db");
    conn.query_row(
        "SELECT symbol_count FROM file_inventory WHERE repo = ?1 AND path = ?2",
        [repo, path],
        |row| row.get(0),
    )
    .expect("query symbol_count")
}

#[test]
fn index_populates_css_symbols_and_tolerates_a_theme_block() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");

    // Escaped dot: Tailwind v4's actual compiled output escapes a literal
    // `.` inside a custom-property name (a bare unescaped dot is not a
    // valid ident code point and would tokenize as two separate tokens).
    std::fs::write(
        repo_dir.path().join("rafters.css"),
        "@import \"tailwindcss\";\n\n\
         @theme {\n  --spacing-0\\.5: 0.125rem;\n  --color-red-500: oklch(0.637 0.237 25.331);\n}\n\n\
         .text-red-500 {\n  color: var(--color-red-500);\n}\n",
    )
    .expect("write rafters.css");

    seed_watch_toml(data_dir.path(), &[("cssrepo", repo_dir.path())]);

    // A real `legion index` run: file inventory walk, then the css-symbol
    // pass wired in src/cli/index_cmd.rs::handle_index. No package.json is
    // present, so no SCIP language even attempts to run -- the css pass
    // must be independent of that marker-file detection.
    run_ok(legion_cmd(data_dir.path()).args(["index", "cssrepo"]));

    let symbols = css_symbols_for_repo(data_dir.path(), "cssrepo");

    assert!(
        symbols
            .iter()
            .any(|(path, kind, name, _)| path == "rafters.css"
                && kind == "custom-property"
                && name == "--spacing-0.5"),
        "expected --spacing-0.5 recovered from the @theme block, got {symbols:?}"
    );
    assert!(
        symbols
            .iter()
            .any(|(path, kind, name, _)| path == "rafters.css"
                && kind == "custom-property"
                && name == "--color-red-500"),
        "expected --color-red-500 recovered from the @theme block, got {symbols:?}"
    );
    assert!(
        symbols
            .iter()
            .any(|(path, kind, name, _)| path == "rafters.css"
                && kind == "class"
                && name == ".text-red-500"),
        "expected .text-red-500 class selector, got {symbols:?}"
    );

    let count = symbol_count_for_file(data_dir.path(), "cssrepo", "rafters.css");
    assert_eq!(
        count,
        symbols.len() as i64,
        "file_inventory.symbol_count must be bumped to the css symbol count"
    );
}

#[test]
fn reindex_drops_css_symbols_removed_from_a_still_live_file() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");

    std::fs::write(
        repo_dir.path().join("a.css"),
        ".foo { color: red; }\n.bar { color: blue; }\n",
    )
    .expect("write a.css");

    seed_watch_toml(data_dir.path(), &[("reindexcss", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "reindexcss"]));

    let first_pass = css_symbols_for_repo(data_dir.path(), "reindexcss");
    assert_eq!(
        first_pass.len(),
        2,
        "expected both .foo and .bar on the first index, got {first_pass:?}"
    );

    // a.css stays live but drops `.bar` entirely.
    std::fs::write(repo_dir.path().join("a.css"), ".foo { color: red; }\n").expect("rewrite a.css");

    run_ok(legion_cmd(data_dir.path()).args(["index", "reindexcss"]));

    let second_pass = css_symbols_for_repo(data_dir.path(), "reindexcss");
    assert_eq!(
        second_pass.len(),
        1,
        "the dropped .bar class must not survive re-indexing, got {second_pass:?}"
    );
    assert_eq!(second_pass[0].2, ".foo");
}

#[test]
fn index_with_no_css_files_leaves_css_symbols_empty() {
    // A repo with no .css files at all must not error, and must simply
    // produce no css_symbols rows -- the css pass is skipped entirely
    // rather than running over an empty file list.
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    std::fs::write(repo_dir.path().join("README.md"), "# hi\n").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("docsonlycss", repo_dir.path())]);

    run_ok(legion_cmd(data_dir.path()).args(["index", "docsonlycss"]));

    let symbols = css_symbols_for_repo(data_dir.path(), "docsonlycss");
    assert!(
        symbols.is_empty(),
        "expected no css symbols, got {symbols:?}"
    );
}
