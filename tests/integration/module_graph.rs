//! CLI end-to-end tests for the module-graph wiring in `legion index` (#710)
//! and the `sym imports`/`sym importers` query surface (#772).
//!
//! `src/graph.rs` and `src/db/module_edges.rs` carry their own unit tests for
//! the parse/resolve logic and the persistence API in isolation; the first
//! group of tests below exercises the actual binary surface -- a real
//! `legion index <repo>` run over a small TS fixture must populate
//! `module_edges`, honoring tsconfig `paths` and recording unresolved
//! specifiers as `to_path = NULL` rather than dropping them -- and reads the
//! `module_edges` table directly out of the data dir's `legion.db`. The
//! second group drives `sym imports`/`sym importers` through the compiled
//! binary against that same indexed data (mirrors `css_symbols.rs`'s shape
//! for #744).

use rusqlite::Connection;

use crate::common::{legion_cmd, run_fail, run_ok, run_ok_stderr};

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
fn reindex_drops_edges_for_imports_removed_from_a_still_live_file() {
    // Regression test for the module_edges upsert bug where a file that
    // stays live but sheds an import (or all of them) left the stale edge
    // rows behind forever: the upsert only ever inserts/updates, and
    // `prune_module_edges` only removes edges for files that are no longer
    // walked at all, so neither path used to clear a specifier that
    // disappeared from a file's *current* content.
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");

    std::fs::create_dir_all(repo_dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        repo_dir.path().join("src/index.ts"),
        "import { b } from './b';\nimport { c } from './c';\nexport { b, c };\n",
    )
    .expect("write index.ts");
    std::fs::write(repo_dir.path().join("src/b.ts"), "export const b = 1;\n").expect("write b.ts");
    std::fs::write(repo_dir.path().join("src/c.ts"), "export const c = 1;\n").expect("write c.ts");

    seed_watch_toml(data_dir.path(), &[("reindexrepo", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "reindexrepo"]));

    let first_pass = edges_for_repo(data_dir.path(), "reindexrepo");
    assert_eq!(
        first_pass.len(),
        2,
        "expected both ./b and ./c edges on the first index, got {first_pass:?}"
    );

    // src/index.ts stays live but drops its `./c` import entirely.
    std::fs::write(
        repo_dir.path().join("src/index.ts"),
        "import { b } from './b';\nexport { b };\n",
    )
    .expect("rewrite index.ts");

    run_ok(legion_cmd(data_dir.path()).args(["index", "reindexrepo"]));

    let second_pass = edges_for_repo(data_dir.path(), "reindexrepo");
    assert_eq!(
        second_pass,
        vec![(
            "src/index.ts".to_string(),
            "./b".to_string(),
            Some("src/b.ts".to_string())
        )],
        "the dropped ./c import must not survive re-indexing, got {second_pass:?}"
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

/// Seed a small TS fixture (a resolved edge, an unresolved/external edge,
/// and an ambiguous basename shared by two files under different
/// directories), index it, and return the data dir. Shared setup for the
/// `sym imports`/`sym importers` CLI tests below (#772).
fn seed_and_index_import_graph(repo: &str) -> tempfile::TempDir {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    std::fs::create_dir_all(repo_dir.path().join("src/components")).expect("mkdir components");
    std::fs::create_dir_all(repo_dir.path().join("src/widgets")).expect("mkdir widgets");
    std::fs::create_dir_all(repo_dir.path().join("src/pages")).expect("mkdir pages");
    std::fs::write(
        repo_dir.path().join("src/index.ts"),
        "import { widget } from './widgets/widget';\n\
         import unresolved from 'some-external-package-that-does-not-exist';\n\
         export { widget, unresolved };\n",
    )
    .expect("write index.ts");
    std::fs::write(
        repo_dir.path().join("src/widgets/widget.ts"),
        "export const widget = 1;\n",
    )
    .expect("write widget.ts");
    // Two distinct Button files under different directories, both importing
    // the same widget and both themselves imported by a page -- so a bare
    // "Button.tsx" suffix query is ambiguous whether asked as an importer
    // (`sym imports`) or as an importee (`sym importers`).
    std::fs::write(
        repo_dir.path().join("src/components/Button.tsx"),
        "import { widget } from '../widgets/widget';\nexport const Button = () => widget;\n",
    )
    .expect("write components/Button.tsx");
    std::fs::write(
        repo_dir.path().join("src/widgets/Button.tsx"),
        "import { widget } from './widget';\nexport const Button2 = () => widget;\n",
    )
    .expect("write widgets/Button.tsx");
    std::fs::write(
        repo_dir.path().join("src/pages/Home.tsx"),
        "import { Button } from '../components/Button';\nexport const Home = () => Button;\n",
    )
    .expect("write pages/Home.tsx");
    std::fs::write(
        repo_dir.path().join("src/pages/Other.tsx"),
        "import { Button2 } from '../widgets/Button';\nexport const Other = () => Button2;\n",
    )
    .expect("write pages/Other.tsx");
    seed_watch_toml(data_dir.path(), &[(repo, repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", repo]));
    drop(repo_dir);
    data_dir
}

/// Cross-repo (`--repo` omitted): two separately-indexed repos, each with
/// its own `widget.ts` importer, share one data dir. `sym imports`/`sym
/// importers` with no `--repo` must aggregate across both -- the least
/// exercised path, since every other test above pins `--repo` explicitly
/// (#772 review finding).
#[test]
fn sym_imports_cross_repo_aggregates_across_every_indexed_repo() {
    let data_dir = tempfile::tempdir().expect("data dir");

    let repo_a_dir = tempfile::tempdir().expect("repo a dir");
    std::fs::write(
        repo_a_dir.path().join("index.ts"),
        "import { w } from './widget';\nexport { w };\n",
    )
    .expect("write repo a index.ts");
    std::fs::write(repo_a_dir.path().join("widget.ts"), "export const w = 1;\n")
        .expect("write repo a widget.ts");

    let repo_b_dir = tempfile::tempdir().expect("repo b dir");
    std::fs::write(
        repo_b_dir.path().join("index.ts"),
        "import { w } from './widget';\nexport { w };\n",
    )
    .expect("write repo b index.ts");
    std::fs::write(repo_b_dir.path().join("widget.ts"), "export const w = 1;\n")
        .expect("write repo b widget.ts");

    seed_watch_toml(
        data_dir.path(),
        &[
            ("crossrepo_a", repo_a_dir.path()),
            ("crossrepo_b", repo_b_dir.path()),
        ],
    );
    run_ok(legion_cmd(data_dir.path()).args(["index", "crossrepo_a"]));
    run_ok(legion_cmd(data_dir.path()).args(["index", "crossrepo_b"]));

    // `sym importers widget.ts` with no --repo must find the importer in
    // both repos, each row tagged with its own repo.
    let out = run_ok(legion_cmd(data_dir.path()).args(["sym", "importers", "widget.ts"]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expected one importer edge per repo, got:\n{out}"
    );
    assert!(lines.iter().any(|l| l.ends_with("[crossrepo_a]")));
    assert!(lines.iter().any(|l| l.ends_with("[crossrepo_b]")));

    // The --json envelope's freshness snapshots must cover both repos too,
    // not just the first one encountered.
    let json_out =
        run_ok(legion_cmd(data_dir.path()).args(["sym", "importers", "widget.ts", "--json"]));
    let envelope: serde_json::Value =
        serde_json::from_str(json_out.trim()).expect("stdout is a JSON envelope object");
    let snapshots = envelope["snapshots"].as_array().expect("snapshots array");
    let snapshot_repos: std::collections::HashSet<&str> = snapshots
        .iter()
        .filter_map(|s| s["repo"].as_str())
        .collect();
    assert!(
        snapshot_repos.contains("crossrepo_a") && snapshot_repos.contains("crossrepo_b"),
        "cross-repo snapshots must cover both indexed repos, got {snapshots:?}"
    );
}

#[test]
fn sym_imports_prints_resolved_and_unresolved_edges_in_text_format() {
    let data_dir = seed_and_index_import_graph("importsq");

    let out = run_ok(legion_cmd(data_dir.path()).args(["sym", "imports", "src/index.ts"]));
    assert!(
        out.lines()
            .any(|l| l.starts_with("./widgets/widget\tsrc/index.ts -> src/widgets/widget.ts\t")),
        "expected the resolved widget edge, got:\n{out}"
    );
    assert!(
        out.lines().any(|l| {
            l.starts_with("some-external-package-that-does-not-exist\tsrc/index.ts -> unresolved\t")
        }),
        "the unresolved/external edge must be shown as 'unresolved', not dropped, got:\n{out}"
    );
}

#[test]
fn sym_imports_json_wraps_entries_with_snapshot_freshness() {
    let data_dir = seed_and_index_import_graph("importsqjson");

    let out = run_ok(legion_cmd(data_dir.path()).args([
        "sym",
        "imports",
        "src/index.ts",
        "--repo",
        "importsqjson",
        "--json",
    ]));
    let envelope: serde_json::Value =
        serde_json::from_str(out.trim()).expect("stdout is a JSON envelope object");
    let entries = envelope["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 2, "expected both edges, got {entries:?}");
    assert!(
        entries.iter().any(
            |e| e["specifier"] == "some-external-package-that-does-not-exist" && e["to"].is_null()
        ),
        "unresolved edge must serialize to as JSON null, got {entries:?}"
    );

    let snapshots = envelope["snapshots"].as_array().expect("snapshots array");
    assert_eq!(
        snapshots.len(),
        1,
        "explicit --repo yields exactly one snapshot: {snapshots:?}"
    );
    assert_eq!(snapshots[0]["repo"], "importsqjson");
}

#[test]
fn sym_importers_finds_every_file_importing_a_shared_module() {
    let data_dir = seed_and_index_import_graph("importersq");

    let out = run_ok(legion_cmd(data_dir.path()).args(["sym", "importers", "widget.ts"]));
    assert!(
        out.lines()
            .any(|l| l.contains("src/index.ts -> src/widgets/widget.ts")),
        "expected src/index.ts as an importer, got:\n{out}"
    );
    assert!(
        out.lines()
            .any(|l| l.contains("src/components/Button.tsx -> src/widgets/widget.ts")),
        "expected src/components/Button.tsx as an importer, got:\n{out}"
    );
    assert!(
        out.lines()
            .any(|l| l.contains("src/widgets/Button.tsx -> src/widgets/widget.ts")),
        "expected src/widgets/Button.tsx as an importer, got:\n{out}"
    );
}

#[test]
fn sym_importers_ambiguous_suffix_returns_every_match() {
    let data_dir = seed_and_index_import_graph("importersqambig");

    // "Button.tsx" is a bare basename shared by two distinct importee files
    // under different directories (src/components/Button.tsx and
    // src/widgets/Button.tsx), each imported by its own page -- both edges
    // must be returned, not just one (#772).
    let out = run_ok(legion_cmd(data_dir.path()).args(["sym", "importers", "Button.tsx"]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expected one importer edge per ambiguous Button.tsx file, got:\n{out}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("-> src/components/Button.tsx"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("-> src/widgets/Button.tsx"))
    );
}

#[test]
fn sym_imports_indexed_but_no_matching_edges_is_a_clean_empty_exit() {
    let data_dir = seed_and_index_import_graph("importsqempty");

    // widget.ts has no imports of its own -- indexed, genuinely empty.
    let stdout =
        run_ok(legion_cmd(data_dir.path()).args(["sym", "imports", "src/widgets/widget.ts"]));
    assert_eq!(stdout, "", "empty result must not print any rows");

    let stderr = run_ok_stderr(legion_cmd(data_dir.path()).args([
        "sym",
        "imports",
        "src/widgets/widget.ts",
    ]));
    assert!(
        stderr.contains("[legion] no imports found"),
        "expected the no-imports-found line, got: {stderr}"
    );
}

/// An empty `file` argument is a suffix of every path -- without an explicit
/// guard, `""` would silently match and dump every edge in scope instead of
/// erroring on a clearly-wrong query (#772 review finding).
#[test]
fn sym_imports_empty_file_argument_errors_instead_of_dumping_everything() {
    let data_dir = seed_and_index_import_graph("importsqemptyarg");

    let (_, stderr) = run_fail(legion_cmd(data_dir.path()).args(["sym", "imports", ""]));
    assert!(
        stderr.contains("file argument must not be empty"),
        "expected the empty-argument guard to fire, got: {stderr}"
    );

    let (_, stderr) = run_fail(legion_cmd(data_dir.path()).args(["sym", "importers", ""]));
    assert!(
        stderr.contains("file argument must not be empty"),
        "expected the empty-argument guard to fire for importers too, got: {stderr}"
    );
}

#[test]
fn sym_imports_never_indexed_repo_fails_loudly_not_silently_empty() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    std::fs::write(repo_dir.path().join("README.md"), "# hi\n").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("importsqnever", repo_dir.path())]);
    // Deliberately never run `legion index` -- no file_inventory rows exist.

    let (_, stderr) = run_fail(legion_cmd(data_dir.path()).args([
        "sym",
        "imports",
        "anything.ts",
        "--repo",
        "importsqnever",
    ]));
    assert!(
        stderr.contains("legion index importsqnever"),
        "expected the message to name the fixing command, got: {stderr}"
    );
}

/// A repo indexed cleanly but with zero js/ts/jsx/tsx files (pure docs, or
/// pure Rust) must NOT be reported as never-indexed just because
/// `module_edges` is empty for it -- that would be the exact misroute
/// `css_symbol_no_index_message` exists to prevent for `--lang css`, mirrored
/// here without the extension narrowing (Build Notes, #772).
#[test]
fn sym_imports_indexed_repo_with_no_ts_files_is_empty_not_never_indexed() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    std::fs::write(repo_dir.path().join("README.md"), "# hi\n").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("importsqdocsonly", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "importsqdocsonly"]));

    let stderr = run_ok_stderr(legion_cmd(data_dir.path()).args([
        "sym",
        "imports",
        "anything.ts",
        "--repo",
        "importsqdocsonly",
    ]));
    assert!(
        stderr.contains("[legion] no imports found"),
        "an indexed repo with no ts files must be a clean empty result, not a \
         never-indexed error, got: {stderr}"
    );
}
