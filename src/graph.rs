//! Module graph engine (#710): parse js/ts/jsx/tsx imports and resolve each
//! specifier against its referrer.
//!
//! `oxc_parser` builds a `ModuleRecord` as part of parsing every file --
//! `requested_modules` already holds every static import / export-from /
//! re-export specifier, deduplicated and in source order, so no hand-rolled
//! AST visitor is needed for the static half. Dynamic `import()` calls are
//! not part of `requested_modules` (their argument is an arbitrary
//! expression, not necessarily a literal); `dynamic_imports` gives the span
//! of that argument expression, and this module treats it as a specifier
//! only when the source text at that span is a plain string literal with no
//! interpolation -- a computed dynamic import (`import(base + name)`) is not
//! a graph edge legion can resolve statically, and is silently skipped
//! rather than guessed at.
//!
//! `oxc_resolver` then resolves each specifier the same way Node / bundlers
//! do: tsconfig `paths` (auto-discovered per file, so a monorepo with nested
//! tsconfig.json files resolves each importer against the config that
//! actually owns it), `node_modules`, extension resolution, and
//! package.json `exports`/`imports`. A specifier that does not resolve
//! (external package, missing file, Node builtin) is recorded with
//! `to: None` rather than dropped -- the module graph must show *that* a
//! file references something external, even when it cannot say where.
//!
//! This module is headless: it produces the graph, it does not bundle or
//! emit code. Rolldown is the reference architecture (it composes the same
//! oxc crates to parse + resolve + graph before bundling); legion embeds
//! the crates directly to stop one step earlier, at the graph.

use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_resolver::{ResolveOptions, Resolver, TsconfigDiscovery};
use oxc_span::SourceType;

use crate::error::Result;

/// One import/export/re-export edge from a file to the module it references.
///
/// `to` is `None` when the specifier did not resolve to a file on disk
/// (an external package, a Node builtin, or a genuinely broken import) --
/// the edge is still recorded, never dropped, so the graph can show that a
/// file references something even when it cannot say where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEdge {
    pub repo: String,
    /// Repo-relative path of the importing file (forward-slash separated).
    pub from: String,
    /// The specifier text exactly as written in the source (e.g. `./foo`,
    /// `lodash`, `@/components/Button`).
    pub specifier: String,
    /// Repo-relative path of the resolved file, or an absolute path when
    /// resolution lands outside `repo_root` (a hoisted/symlinked
    /// `node_modules` in a monorepo). `None` when unresolved/external.
    pub to: Option<String>,
}

/// Build the module graph for `files` (repo-relative paths, forward-slash
/// separated -- the same convention `file_inventory` uses) rooted at
/// `repo_root`.
///
/// Only files `oxc_span::SourceType::from_path` recognizes as js/ts/jsx/tsx
/// are parsed; every other file in `files` is skipped silently (the caller
/// is expected to pass the inventory's `typescript`-lang subset, but passing
/// the full corpus is harmless). A file that cannot be read, or that the
/// parser panics on, is logged to stderr and skipped -- one bad file must
/// not abort the graph for the rest of the repo.
///
/// `repo` stamps every edge's `repo` field; `build_module_graph` itself
/// never inspects `repo_root`'s directory name for it (worktree checkouts
/// routinely live under a path that does not match the repo's logical
/// name).
pub fn build_module_graph(
    repo: &str,
    repo_root: &Path,
    files: &[PathBuf],
) -> Result<Vec<ImportEdge>> {
    // Canonicalize once: on macOS `/tmp` and `/var` are symlinks (to
    // `/private/tmp` / `/private/var`), and the resolver's internal path
    // cache canonicalizes every path it touches. Comparing an
    // uncanonicalized `repo_root` against the resolver's canonicalized
    // output would make `strip_prefix` fail for every resolution even
    // though the file genuinely lives under `repo_root`. Falls back to the
    // given root if it does not exist (a caller passing a path that is
    // about to be created, or a broken watch entry) -- resolution will
    // simply fail per-file in that case, which is already handled.
    let repo_root = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let repo_root = repo_root.as_path();

    let resolver = Resolver::new(ResolveOptions {
        tsconfig: Some(TsconfigDiscovery::Auto),
        extensions: vec![
            ".ts".into(),
            ".tsx".into(),
            ".d.ts".into(),
            ".js".into(),
            ".jsx".into(),
            ".mjs".into(),
            ".cjs".into(),
            ".mts".into(),
            ".cts".into(),
            ".json".into(),
        ],
        condition_names: vec![
            "node".into(),
            "import".into(),
            "require".into(),
            "default".into(),
        ],
        builtin_modules: true,
        ..ResolveOptions::default()
    });

    let mut edges: Vec<ImportEdge> = Vec::new();

    for rel_path in files {
        let abs_path = repo_root.join(rel_path);

        let source_type = match SourceType::from_path(&abs_path) {
            Ok(t) => t,
            // Not a js/ts/jsx/tsx file (or an oxc_parser-recognized
            // extension) -- not an error, just out of scope for the graph.
            Err(_) => continue,
        };

        let source_text = match std::fs::read_to_string(&abs_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "[legion] graph: skipped {} (read error: {e})",
                    abs_path.display()
                );
                continue;
            }
        };

        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, &source_text, source_type).parse();

        if ret.panicked {
            eprintln!(
                "[legion] graph: skipped {} (parser panicked: {} diagnostic(s))",
                abs_path.display(),
                ret.diagnostics.len()
            );
            continue;
        }

        let from = to_repo_relative(rel_path);

        let mut specifiers: Vec<String> = ret
            .module_record
            .requested_modules
            .keys()
            .map(|s| s.as_str().to_string())
            .collect();

        for dynamic_import in &ret.module_record.dynamic_imports {
            let raw = dynamic_import.module_request.source_text(&source_text);
            if let Some(spec) = literal_specifier(raw)
                && !specifiers.contains(&spec)
            {
                specifiers.push(spec);
            }
        }
        specifiers.sort_unstable();

        for specifier in specifiers {
            let to = match resolver.resolve_file(&abs_path, &specifier) {
                Ok(resolution) => Some(resolve_to_repo_relative(repo_root, resolution.path())),
                Err(_) => None,
            };
            edges.push(ImportEdge {
                repo: repo.to_string(),
                from: from.clone(),
                specifier,
                to,
            });
        }
    }

    edges.sort_unstable_by(|a, b| (&a.from, &a.specifier).cmp(&(&b.from, &b.specifier)));
    Ok(edges)
}

/// Normalize a path to forward-slash separators without requiring it be
/// repo-relative already -- callers may pass either.
fn to_repo_relative(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Render a resolved absolute path as repo-relative when it lives under
/// `repo_root`, or as a forward-slash-normalized absolute path otherwise (a
/// hoisted/symlinked `node_modules` can resolve outside the repo root in a
/// monorepo layout).
fn resolve_to_repo_relative(repo_root: &Path, resolved: &Path) -> String {
    match resolved.strip_prefix(repo_root) {
        Ok(rel) => to_repo_relative(rel),
        Err(_) => to_repo_relative(resolved),
    }
}

/// Extract the literal string value from the raw source text of a dynamic
/// `import()` argument, or `None` when it is not a plain string literal
/// (a computed specifier, e.g. `import(base + "/foo")`, cannot be resolved
/// statically).
///
/// A template literal (backtick) with no `${` interpolation is treated as
/// a plain literal, since its value is fixed regardless of context.
fn literal_specifier(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let bytes = raw.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let quote = bytes[0];
    if quote != b'\'' && quote != b'"' && quote != b'`' {
        return None;
    }
    if bytes[bytes.len() - 1] != quote {
        return None;
    }
    let inner = &raw[1..raw.len() - 1];
    if quote == b'`' && inner.contains("${") {
        return None;
    }
    Some(unescape_quotes(inner))
}

/// Unescape the small set of backslash escapes that matter for a module
/// specifier: the quote character itself and a literal backslash. This is
/// deliberately not a full JS string-escape decoder -- specifiers containing
/// `\n`/`\t`/unicode escapes are not valid file paths or package names, so
/// there is nothing meaningful to decode for the graph's purposes.
fn unescape_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    // --- literal_specifier: the dynamic import() text -> specifier helper ---

    #[test]
    fn literal_specifier_double_quoted() {
        assert_eq!(literal_specifier("\"./foo\""), Some("./foo".to_string()));
    }

    #[test]
    fn literal_specifier_single_quoted() {
        assert_eq!(literal_specifier("'./foo'"), Some("./foo".to_string()));
    }

    #[test]
    fn literal_specifier_plain_template() {
        assert_eq!(literal_specifier("`./foo`"), Some("./foo".to_string()));
    }

    #[test]
    fn literal_specifier_rejects_interpolated_template() {
        assert_eq!(literal_specifier("`./${name}`"), None);
    }

    #[test]
    fn literal_specifier_rejects_computed_expression() {
        assert_eq!(literal_specifier("base + \"/foo\""), None);
        assert_eq!(literal_specifier("name"), None);
    }

    #[test]
    fn literal_specifier_unescapes_quote() {
        assert_eq!(
            literal_specifier("\"./foo\\\"bar\""),
            Some("./foo\"bar".to_string())
        );
    }

    // --- build_module_graph: static imports resolve relative specifiers ---

    #[test]
    fn resolves_a_relative_static_import() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/a.ts",
            "import { b } from './b';\nexport { b };\n",
        );
        write(dir.path(), "src/b.ts", "export const b = 1;\n");

        let files = vec![PathBuf::from("src/a.ts"), PathBuf::from("src/b.ts")];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();

        let a_edges: Vec<&ImportEdge> = edges.iter().filter(|e| e.from == "src/a.ts").collect();
        assert_eq!(a_edges.len(), 1);
        assert_eq!(a_edges[0].specifier, "./b");
        assert_eq!(a_edges[0].to.as_deref(), Some("src/b.ts"));
        assert_eq!(a_edges[0].repo, "r");
    }

    // --- unresolved/external specifiers: to = None, not dropped ---

    #[test]
    fn unresolved_specifier_recorded_as_none_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/a.ts",
            "import { z } from 'some-external-package-that-does-not-exist';\n",
        );

        let files = vec![PathBuf::from("src/a.ts")];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();

        assert_eq!(edges.len(), 1, "the edge must be recorded, not dropped");
        assert_eq!(
            edges[0].specifier,
            "some-external-package-that-does-not-exist"
        );
        assert_eq!(edges[0].to, None);
    }

    #[test]
    fn missing_relative_import_is_none_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/a.ts",
            "import { b } from './does-not-exist';\n",
        );

        let files = vec![PathBuf::from("src/a.ts")];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, None);
    }

    // --- tsconfig `paths` resolution ---

    #[test]
    fn resolves_through_tsconfig_paths() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "tsconfig.json",
            r#"{
                "compilerOptions": {
                    "baseUrl": ".",
                    "paths": { "@/*": ["src/*"] }
                }
            }"#,
        );
        write(
            dir.path(),
            "src/index.ts",
            "import { widget } from '@/widget';\nexport { widget };\n",
        );
        write(dir.path(), "src/widget.ts", "export const widget = 1;\n");

        let files = vec![
            PathBuf::from("src/index.ts"),
            PathBuf::from("src/widget.ts"),
        ];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();

        let edge = edges
            .iter()
            .find(|e| e.from == "src/index.ts" && e.specifier == "@/widget")
            .expect("the @/widget edge must be present");
        assert_eq!(edge.to.as_deref(), Some("src/widget.ts"));
    }

    // --- dynamic import() ---

    #[test]
    fn resolves_a_literal_dynamic_import() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/a.ts",
            "export async function load() { return import('./b'); }\n",
        );
        write(dir.path(), "src/b.ts", "export const b = 1;\n");

        let files = vec![PathBuf::from("src/a.ts"), PathBuf::from("src/b.ts")];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();

        let a_edges: Vec<&ImportEdge> = edges.iter().filter(|e| e.from == "src/a.ts").collect();
        assert_eq!(a_edges.len(), 1);
        assert_eq!(a_edges[0].specifier, "./b");
        assert_eq!(a_edges[0].to.as_deref(), Some("src/b.ts"));
    }

    #[test]
    fn computed_dynamic_import_is_skipped_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/a.ts",
            "export async function load(name: string) { return import('./' + name); }\n",
        );

        let files = vec![PathBuf::from("src/a.ts")];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();
        assert!(
            edges.is_empty(),
            "a computed dynamic import must not produce a guessed edge"
        );
    }

    // --- re-exports (export ... from) ---

    #[test]
    fn export_from_produces_an_edge() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.ts", "export { b } from './b';\n");
        write(dir.path(), "src/b.ts", "export const b = 1;\n");

        let files = vec![PathBuf::from("src/a.ts"), PathBuf::from("src/b.ts")];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();

        let a_edges: Vec<&ImportEdge> = edges.iter().filter(|e| e.from == "src/a.ts").collect();
        assert_eq!(a_edges.len(), 1);
        assert_eq!(a_edges[0].specifier, "./b");
        assert_eq!(a_edges[0].to.as_deref(), Some("src/b.ts"));
    }

    #[test]
    fn export_star_produces_an_edge() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.ts", "export * from './b';\n");
        write(dir.path(), "src/b.ts", "export const b = 1;\n");

        let files = vec![PathBuf::from("src/a.ts"), PathBuf::from("src/b.ts")];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();

        let a_edges: Vec<&ImportEdge> = edges.iter().filter(|e| e.from == "src/a.ts").collect();
        assert_eq!(a_edges.len(), 1);
        assert_eq!(a_edges[0].to.as_deref(), Some("src/b.ts"));
    }

    // --- non-js/ts files are silently skipped, not an error ---

    #[test]
    fn non_js_files_are_skipped_without_error() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "README.md", "# hello\n");

        let files = vec![PathBuf::from("README.md")];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();
        assert!(edges.is_empty());
    }

    // --- a file that fails to read is skipped, not fatal to the batch ---

    #[test]
    fn unreadable_file_does_not_abort_the_batch() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/ok.ts",
            "import { x } from './missing-but-fine';\n",
        );

        let files = vec![
            PathBuf::from("src/does-not-exist.ts"),
            PathBuf::from("src/ok.ts"),
        ];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();

        assert_eq!(
            edges.len(),
            1,
            "the missing file must be skipped, not fatal"
        );
        assert_eq!(edges[0].from, "src/ok.ts");
    }

    // --- jsx/tsx source types parse correctly ---

    #[test]
    fn tsx_file_parses_and_resolves() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/App.tsx",
            "import { Button } from './Button';\nexport const App = () => <Button />;\n",
        );
        write(
            dir.path(),
            "src/Button.tsx",
            "export const Button = () => <div />;\n",
        );

        let files = vec![
            PathBuf::from("src/App.tsx"),
            PathBuf::from("src/Button.tsx"),
        ];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();

        let app_edges: Vec<&ImportEdge> =
            edges.iter().filter(|e| e.from == "src/App.tsx").collect();
        assert_eq!(app_edges.len(), 1);
        assert_eq!(app_edges[0].to.as_deref(), Some("src/Button.tsx"));
    }

    // --- multiple distinct specifiers from the same file each get a row ---

    #[test]
    fn multiple_specifiers_each_produce_their_own_edge() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/a.ts",
            "import { b } from './b';\nimport { c } from './c';\n",
        );
        write(dir.path(), "src/b.ts", "export const b = 1;\n");
        write(dir.path(), "src/c.ts", "export const c = 1;\n");

        let files = vec![
            PathBuf::from("src/a.ts"),
            PathBuf::from("src/b.ts"),
            PathBuf::from("src/c.ts"),
        ];
        let edges = build_module_graph("r", dir.path(), &files).unwrap();

        let a_edges: Vec<&ImportEdge> = edges.iter().filter(|e| e.from == "src/a.ts").collect();
        assert_eq!(a_edges.len(), 2);
        let specifiers: Vec<&str> = a_edges.iter().map(|e| e.specifier.as_str()).collect();
        assert!(specifiers.contains(&"./b"));
        assert!(specifiers.contains(&"./c"));
    }
}
