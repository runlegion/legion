//! CLI end-to-end tests for the module-graph wiring in `legion index` (#710).
//!
//! `src/graph.rs` and `src/db/module_edges.rs` carry their own unit tests for
//! the parse/resolve logic and the persistence API in isolation; these tests
//! exercise the actual binary surface -- a real `legion index <repo>` run
//! over a small TS fixture must populate `module_edges`, honoring tsconfig
//! `paths` and recording unresolved specifiers as `to_path = NULL` rather
//! than dropping them. No CLI query surface exists yet (see
//! `Database::list_module_edges_from`'s doc comment), so assertions read the
//! `module_edges` table directly out of the data dir's `legion.db`.

use rusqlite::Connection;

use crate::common::{legion_cmd, run_ok};

/// Seed a watch.toml in the data dir pointing at `repos` (name, workdir).
/// Mirrors `sym_tree::seed_watch_toml` -- forward-slash normalized so a
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

/// One `(from_path, specifier, to_path)` row read back from `module_edges`.
fn edges_for_repo(data_dir: &std::path::Path, repo: &str) -> Vec<(String, String, Option<String>)> {
    let conn = Connection::open(data_dir.join("legion.db")).expect("open legion.db");
    let mut stmt = conn
        .prepare("SELECT from_path, specifier, to_path FROM module_edges WHERE repo = ?1 ORDER BY from_path, specifier")
        .expect("prepare query");
    stmt.query_map([repo], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query_map")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect rows")
}

#[test]
fn index_populates_module_edges_through_tsconfig_paths_and_records_unresolved() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");

    std::fs::write(
        repo_dir.path().join("tsconfig.json"),
        r#"{
            "compilerOptions": {
                "baseUrl": ".",
                "paths": { "@/*": ["src/*"] }
            }
        }"#,
    )
    .expect("write tsconfig.json");
    std::fs::create_dir_all(repo_dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        repo_dir.path().join("src/index.ts"),
        "import { widget } from '@/widget';\n\
         import unresolved from 'some-external-package-that-does-not-exist';\n\
         export { widget, unresolved };\n",
    )
    .expect("write index.ts");
    std::fs::write(
        repo_dir.path().join("src/widget.ts"),
        "export const widget = 1;\n",
    )
    .expect("write widget.ts");

    seed_watch_toml(data_dir.path(), &[("graphrepo", repo_dir.path())]);

    // A real `legion index` run: file inventory walk, then the module-graph
    // pass wired in src/cli/index_cmd.rs::handle_index. No package.json is
    // present, so no SCIP `typescript` indexer even attempts to run -- the
    // graph pass must be independent of that marker-file detection.
    run_ok(legion_cmd(data_dir.path()).args(["index", "graphrepo"]));

    let edges = edges_for_repo(data_dir.path(), "graphrepo");

    let resolved = edges
        .iter()
        .find(|(from, spec, _)| from == "src/index.ts" && spec == "@/widget")
        .unwrap_or_else(|| panic!("expected an @/widget edge from src/index.ts, got {edges:?}"));
    assert_eq!(
        resolved.2.as_deref(),
        Some("src/widget.ts"),
        "tsconfig `paths` must resolve @/widget to src/widget.ts, got {edges:?}"
    );

    let unresolved = edges
        .iter()
        .find(|(from, spec, _)| {
            from == "src/index.ts" && spec == "some-external-package-that-does-not-exist"
        })
        .unwrap_or_else(|| {
            panic!("expected the external-package edge to be recorded, not dropped, got {edges:?}")
        });
    assert_eq!(
        unresolved.2, None,
        "an unresolved/external specifier must be recorded with to_path = NULL, not dropped"
    );
}

#[test]
fn index_with_no_ts_files_leaves_module_edges_empty() {
    // A repo with no js/ts/jsx/tsx files at all must not error, and must
    // simply produce no module_edges rows -- the graph pass is skipped
    // entirely rather than running over an empty file list.
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    std::fs::write(repo_dir.path().join("README.md"), "# hi\n").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("docsonly", repo_dir.path())]);

    run_ok(legion_cmd(data_dir.path()).args(["index", "docsonly"]));

    let edges = edges_for_repo(data_dir.path(), "docsonly");
    assert!(edges.is_empty(), "expected no module edges, got {edges:?}");
}
