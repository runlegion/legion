//! CLI end-to-end tests for the CSS symbol wiring in `legion index` (#711)
//! and the `sym list`/`sym def`/`sym hover --lang css` query surface (#744).
//!
//! `src/css.rs` and `src/db/css_symbols.rs` carry their own unit tests for
//! the parse/extract logic and the persistence API in isolation; the first
//! group of tests below exercises the actual binary surface -- a real
//! `legion index <repo>` run over a small CSS fixture (including a
//! Tailwind-v4-shaped `@theme` block) must populate `css_symbols` and bump
//! `file_inventory.symbol_count` for the css file -- and reads the
//! `css_symbols`/`file_inventory` tables directly out of the data dir's
//! `legion.db` (mirrors `module_graph.rs`'s shape, #710). The second group
//! drives `sym list`/`sym def`/`sym hover --lang css` through the compiled
//! binary against that same indexed data.

use rusqlite::Connection;

use crate::common::{legion_cmd, run_fail, run_ok, run_ok_stderr};

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

// ---------------------------------------------------------------------------
// `sym list`/`sym def`/`sym hover --lang css` (#744)
// ---------------------------------------------------------------------------

/// Seed a repo with one known class and one known custom property, index
/// it, and return the data dir. Shared setup for the CLI-level tests below.
fn seed_and_index_one_repo(repo: &str) -> tempfile::TempDir {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    std::fs::write(
        repo_dir.path().join("a.css"),
        ".foo { color: red; }\n:root { --brand: blue; }\n",
    )
    .expect("write a.css");
    seed_watch_toml(data_dir.path(), &[(repo, repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", repo]));
    // Keep repo_dir alive for the duration of the index run, then drop it --
    // the data dir (with legion.db) is all the CLI queries below touch.
    drop(repo_dir);
    data_dir
}

#[test]
fn sym_list_lang_css_prints_seeded_rows_in_text_format() {
    let data_dir = seed_and_index_one_repo("cssq");

    let out = run_ok(legion_cmd(data_dir.path()).args(["sym", "list", "--lang", "css"]));
    assert!(
        out.lines().any(|l| l == ".foo\ta.css:1\t[class] cssq/css"),
        "expected the .foo line, got:\n{out}"
    );
    assert!(
        out.lines()
            .any(|l| l == "--brand\ta.css:2\t[custom-property] cssq/css"),
        "expected the --brand line, got:\n{out}"
    );
}

#[test]
fn sym_list_lang_css_json_emits_css_symbol_hits() {
    let data_dir = seed_and_index_one_repo("cssqjson");

    let out = run_ok(legion_cmd(data_dir.path()).args(["sym", "list", "--lang", "css", "--json"]));
    let hits: Vec<serde_json::Value> =
        serde_json::from_str(out.trim()).expect("stdout is a JSON array");
    assert_eq!(hits.len(), 2, "expected both rows, got {hits:?}");
    assert!(
        hits.iter()
            .any(|h| h["kind"] == "class" && h["name"] == ".foo"),
        "expected a class hit, got {hits:?}"
    );
    assert!(
        hits.iter()
            .any(|h| h["kind"] == "custom-property" && h["name"] == "--brand"),
        "expected a custom-property hit, got {hits:?}"
    );
}

#[test]
fn sym_list_lang_css_kind_filters_to_class_only() {
    let data_dir = seed_and_index_one_repo("cssqkindclass");

    let out = run_ok(
        legion_cmd(data_dir.path()).args(["sym", "list", "--lang", "css", "--kind", "class"]),
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1, "expected exactly one row, got {lines:?}");
    assert!(lines[0].contains("[class]"));
}

#[test]
fn sym_list_lang_css_kind_filters_to_custom_property_only() {
    let data_dir = seed_and_index_one_repo("cssqkindprop");

    let out = run_ok(legion_cmd(data_dir.path()).args([
        "sym",
        "list",
        "--lang",
        "css",
        "--kind",
        "custom-property",
    ]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1, "expected exactly one row, got {lines:?}");
    assert!(lines[0].contains("[custom-property]"));
}

#[test]
fn sym_list_lang_css_file_scopes_to_one_file() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    std::fs::write(repo_dir.path().join("a.css"), ".foo { color: red; }\n").expect("write a.css");
    std::fs::write(repo_dir.path().join("b.css"), ".bar { color: blue; }\n").expect("write b.css");
    seed_watch_toml(data_dir.path(), &[("cssqfile", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "cssqfile"]));

    let out = run_ok(
        legion_cmd(data_dir.path()).args(["sym", "list", "--lang", "css", "--file", "a.css"]),
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1, "expected only a.css's row, got {lines:?}");
    assert!(lines[0].starts_with(".foo\t"));
}

#[test]
fn sym_list_lang_css_indexed_but_empty_result_prints_no_matching_definitions() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    std::fs::write(repo_dir.path().join("a.css"), ".foo { color: red; }\n").expect("write a.css");
    seed_watch_toml(data_dir.path(), &[("cssqempty", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "cssqempty"]));

    // Repo has classes only -- filtering to custom-property is a clean
    // empty result (exit 0 via `run_ok`/`run_ok_stderr`, which both assert
    // success), not a "never indexed" error.
    let stdout = run_ok(legion_cmd(data_dir.path()).args([
        "sym",
        "list",
        "--lang",
        "css",
        "--kind",
        "custom-property",
    ]));
    assert_eq!(stdout, "", "empty result must not print any rows");

    let stderr = run_ok_stderr(legion_cmd(data_dir.path()).args([
        "sym",
        "list",
        "--lang",
        "css",
        "--kind",
        "custom-property",
    ]));
    assert!(
        stderr.contains("[legion] no matching definitions"),
        "expected the existing no-matching-definitions line, got: {stderr}"
    );
}

#[test]
fn sym_list_lang_css_never_indexed_prints_css_aware_message_and_fails() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    // No css files at all -- `legion index` runs but never touches css.
    std::fs::write(repo_dir.path().join("README.md"), "# hi\n").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("cssqnever", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "cssqnever"]));

    let (_, stderr) = run_fail(legion_cmd(data_dir.path()).args([
        "sym",
        "list",
        "--lang",
        "css",
        "--repo",
        "cssqnever",
    ]));
    assert!(
        stderr.contains("legion index cssqnever"),
        "expected the message to name the fixing command, got: {stderr}"
    );
    assert!(
        !stderr.contains("found for cssqnever/css"),
        "must not reuse no_index_found's generic wording, got: {stderr}"
    );
}

/// The exact bug reported live against 0.19.0: `legion index` then
/// immediately `sym list`/`sym def --lang css` must never print "no index
/// found" -- that message is false once css_symbols rows exist.
#[test]
fn regression_index_then_query_never_says_no_index_found() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    std::fs::write(
        repo_dir.path().join("rafters.css"),
        ".foo { color: red; }\n",
    )
    .expect("write rafters.css");
    seed_watch_toml(data_dir.path(), &[("cssregress", repo_dir.path())]);

    run_ok(legion_cmd(data_dir.path()).args(["index", "cssregress"]));

    let list_out = legion_cmd(data_dir.path())
        .args(["sym", "list", "--lang", "css", "--repo", "cssregress"])
        .output()
        .expect("run sym list");
    let list_combined = format!(
        "{}{}",
        String::from_utf8_lossy(&list_out.stdout),
        String::from_utf8_lossy(&list_out.stderr)
    );
    assert!(
        !list_combined.contains("no index found"),
        "sym list must never say 'no index found' right after indexing, got: {list_combined}"
    );

    let def_out = legion_cmd(data_dir.path())
        .args([
            "sym",
            "def",
            ".foo",
            "--lang",
            "css",
            "--repo",
            "cssregress",
        ])
        .output()
        .expect("run sym def");
    let def_combined = format!(
        "{}{}",
        String::from_utf8_lossy(&def_out.stdout),
        String::from_utf8_lossy(&def_out.stderr)
    );
    assert!(
        !def_combined.contains("no index found"),
        "sym def must never say 'no index found' right after indexing, got: {def_combined}"
    );
}

#[test]
fn sym_def_lang_css_prints_the_seeded_row() {
    let data_dir = seed_and_index_one_repo("cssdefq");

    let out = run_ok(
        legion_cmd(data_dir.path())
            .args(["sym", "def", ".foo", "--lang", "css", "--repo", "cssdefq"]),
    );
    assert_eq!(out.trim(), "a.css:1\t[cssdefq/css]");
}

#[test]
fn sym_def_lang_css_json_emits_a_css_symbol_hit() {
    let data_dir = seed_and_index_one_repo("cssdefjson");

    let out = run_ok(legion_cmd(data_dir.path()).args([
        "sym",
        "def",
        ".foo",
        "--lang",
        "css",
        "--repo",
        "cssdefjson",
        "--json",
    ]));
    let hits: Vec<serde_json::Value> =
        serde_json::from_str(out.trim()).expect("stdout is a JSON array");
    assert_eq!(hits.len(), 1, "expected the one seeded match, got {hits:?}");
    assert_eq!(hits[0]["name"], ".foo");
    assert_eq!(hits[0]["kind"], "class");
    assert_eq!(hits[0]["file"], "a.css");
    assert_eq!(hits[0]["line"], 1);
    assert_eq!(hits[0]["repo"], "cssdefjson");
}

#[test]
fn sym_def_lang_css_name_matching_is_exact_and_sigil_inclusive() {
    let data_dir = seed_and_index_one_repo("cssdefsigil");

    let with_sigil = run_ok(legion_cmd(data_dir.path()).args([
        "sym",
        "def",
        ".foo",
        "--lang",
        "css",
        "--repo",
        "cssdefsigil",
    ]));
    assert_eq!(with_sigil.trim(), "a.css:1\t[cssdefsigil/css]");

    let without_sigil = run_ok(legion_cmd(data_dir.path()).args([
        "sym",
        "def",
        "foo",
        "--lang",
        "css",
        "--repo",
        "cssdefsigil",
    ]));
    assert_eq!(
        without_sigil.trim(),
        "",
        "a sigil-less query must not fuzzy-match the stored sigil-bearing name"
    );
}

#[test]
fn sym_def_lang_css_repo_scoping_covers_both_call_shapes() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_a = tempfile::tempdir().expect("repo a");
    let repo_b = tempfile::tempdir().expect("repo b");
    std::fs::write(repo_a.path().join("a.css"), ".shared { color: red; }\n")
        .expect("write repo a css");
    std::fs::write(repo_b.path().join("b.css"), ".shared { color: blue; }\n")
        .expect("write repo b css");
    seed_watch_toml(
        data_dir.path(),
        &[("cssrepoa", repo_a.path()), ("cssrepob", repo_b.path())],
    );
    run_ok(legion_cmd(data_dir.path()).args(["index", "cssrepoa"]));
    run_ok(legion_cmd(data_dir.path()).args(["index", "cssrepob"]));

    // --repo scopes to one repo (find_css_symbol(Some(repo), name)).
    let scoped = run_ok(legion_cmd(data_dir.path()).args([
        "sym", "def", ".shared", "--lang", "css", "--repo", "cssrepoa",
    ]));
    assert_eq!(scoped.trim(), "a.css:1\t[cssrepoa/css]");

    // Omitting --repo queries cross-repo -- both repos' rows come back.
    let cross_repo =
        run_ok(legion_cmd(data_dir.path()).args(["sym", "def", ".shared", "--lang", "css"]));
    let lines: Vec<&str> = cross_repo.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expected a row from each repo, got {lines:?}"
    );
    assert!(lines.contains(&"a.css:1\t[cssrepoa/css]"));
    assert!(lines.contains(&"b.css:1\t[cssrepob/css]"));
}

#[test]
fn sym_def_lang_css_miss_on_indexed_repo_prints_nothing_and_exits_0() {
    let data_dir = seed_and_index_one_repo("cssdefmiss");

    let out = run_ok(legion_cmd(data_dir.path()).args([
        "sym",
        "def",
        ".does-not-exist",
        "--lang",
        "css",
        "--repo",
        "cssdefmiss",
    ]));
    assert_eq!(out, "", "a def miss must print nothing, not an error");
}

#[test]
fn sym_def_lang_css_never_indexed_fails_with_css_aware_message() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    std::fs::write(repo_dir.path().join("README.md"), "# hi\n").expect("write fixture");
    seed_watch_toml(data_dir.path(), &[("cssdefnever", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "cssdefnever"]));

    let (_, stderr) = run_fail(legion_cmd(data_dir.path()).args([
        "sym",
        "def",
        ".foo",
        "--lang",
        "css",
        "--repo",
        "cssdefnever",
    ]));
    assert!(stderr.contains("legion index cssdefnever"));
    assert!(!stderr.contains("found for cssdefnever/css"));
}

#[test]
fn sym_hover_lang_css_always_reports_no_hover_surface() {
    let data_dir = seed_and_index_one_repo("csshover");

    // Query the exact name that DOES have real css_symbols rows -- proving
    // this is a "no such surface" message, not a disguised "no data" one.
    let (_, stderr) = run_fail(legion_cmd(data_dir.path()).args([
        "sym", "hover", ".foo", "--lang", "css", "--repo", "csshover",
    ]));
    assert!(
        stderr.contains("css symbols have no hover surface"),
        "got: {stderr}"
    );
    assert!(stderr.contains("sym list --lang css"));
    assert!(stderr.contains("sym etc find-content"));
}
