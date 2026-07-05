//! CSS symbol extraction via lightningcss (#711): parse stylesheets into
//! class-selector and custom-property definitions so `sym` can answer
//! "where is `.foo` / `--token` defined" for the design system.
//!
//! lightningcss is chosen over `oxc-css-parser` deliberately: `oxc-css-parser`
//! 0.0.3 is spec-strict and rejects real Tailwind v4 output, which is
//! pervasive with dotted custom-property names (`--spacing-0.5`). A bare
//! unescaped `.` is not a valid ident code point -- the CSS tokenizer would
//! split it into an `Ident("--spacing-0")` and a `Number(0.5)` -- so
//! Tailwind's compiled output escapes the dot (`--spacing-0\.5`), which the
//! tokenizer resolves into a single ident whose decoded text is the literal
//! `--spacing-0.5`. lightningcss parses that real input with zero errors,
//! and the two are speed-neutral on inputs both can parse (see reflections
//! 019f1eee / 019f1eef). An indexer must tolerate the CSS that actually
//! exists on disk, not conformance-check it.
//!
//! Tailwind v4's `@theme { ... }` block is unknown to lightningcss (it has no
//! built-in knowledge of Tailwind at-rules), so it parses as
//! `CssRule::Unknown` with the block preserved as a raw token list rather
//! than a declaration block. `--custom-property` definitions inside `@theme`
//! are recovered by scanning that token list directly: lightningcss's value
//! tokenizer already rewrites any `--foo`-shaped identifier into a
//! `TokenOrValue::DashedIdent`, and a `var(--foo)` *reference* is wrapped in
//! `TokenOrValue::Var` instead -- so a bare `DashedIdent` immediately
//! followed by a colon (skipping whitespace/comments) is unambiguously a
//! *definition*, never a reference.

use std::path::{Path, PathBuf};

use lightningcss::declaration::DeclarationBlock;
use lightningcss::properties::Property;
use lightningcss::properties::custom::{CustomPropertyName, Token, TokenOrValue};
use lightningcss::rules::CssRule;
use lightningcss::selector::{Component, Selector, SelectorList};
use lightningcss::stylesheet::{ParserOptions, StyleSheet};

use crate::error::{LegionError, Result};

/// What kind of CSS definition a [`CssSymbol`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CssSymbolKind {
    /// A class selector definition (`.foo { ... }`).
    Class,
    /// A `--custom-property` definition, whether written as a normal
    /// declaration (`:root { --foo: ...; }`), inside an `@property` rule, or
    /// inside a Tailwind `@theme` block.
    CustomProperty,
}

/// One CSS symbol definition site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssSymbol {
    pub kind: CssSymbolKind,
    /// The selector/property name exactly as written, including its sigil
    /// (`.foo` for a class, `--foo` for a custom property).
    pub name: String,
    /// Forward-slash-separated path, as given to [`extract_css_symbols`].
    pub path: String,
    /// 1-based line number of the enclosing rule. For symbols recovered from
    /// a raw token list (`@theme` and other at-rules lightningcss does not
    /// parse structurally), this is the line of the *enclosing rule*, not
    /// the exact declaration -- individual raw tokens carry no location of
    /// their own.
    pub line: u32,
}

/// Parse `content` (the text of the file at `path`) and return every class
/// selector and `--custom-property` definition it contains.
///
/// A genuine parse failure (not a tolerated construct -- see the module
/// docs) is returned as `Err`, never panics or silently drops the file;
/// callers walking a repo are expected to log and skip per file, mirroring
/// `graph::build_module_graph`'s per-file error handling.
pub fn extract_css_symbols(path: &Path, content: &str) -> Result<Vec<CssSymbol>> {
    let path_str = to_forward_slash(path);
    let options = ParserOptions {
        filename: path_str.clone(),
        ..ParserOptions::default()
    };
    let stylesheet = StyleSheet::parse(content, options)
        .map_err(|e| LegionError::Css(format!("parse error in {path_str}: {e}")))?;

    let mut symbols = Vec::new();
    walk_rules(&stylesheet.rules.0, &path_str, &mut symbols);
    symbols.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.name.cmp(&b.name)));
    Ok(symbols)
}

/// Extract CSS symbols for a batch of files rooted at `repo_root`, mirroring
/// `graph::build_module_graph`'s shape (read + parse + skip-and-log per
/// file) so `legion index`'s wiring stays a short call site instead of
/// owning the read/error-handling loop itself. Not part of the issue's
/// literal single-file interface, added the same way #710 added a `repo`
/// parameter beyond its given signature: the given signature has no room for
/// batching, and the established convention in this codebase (`graph.rs`) is
/// for the domain module to own its own file reads.
pub fn extract_css_symbols_for_files(repo_root: &Path, files: &[PathBuf]) -> Vec<CssSymbol> {
    let mut symbols = Vec::new();
    for rel_path in files {
        let abs_path = repo_root.join(rel_path);
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "[legion] css: skipped {} (read error: {e})",
                    abs_path.display()
                );
                continue;
            }
        };
        match extract_css_symbols(rel_path, &content) {
            Ok(mut file_symbols) => symbols.append(&mut file_symbols),
            Err(e) => {
                eprintln!("[legion] css: skipped {} ({e})", abs_path.display());
            }
        }
    }
    symbols
}

/// Normalize a path to forward-slash separators, matching the convention
/// `file_inventory` and `graph::ImportEdge` use.
fn to_forward_slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Walk a rule list, recursing into every container-style rule
/// (`@media`, `@supports`, native CSS nesting, etc.) that lightningcss
/// models structurally, plus the raw token list of at-rules it does not
/// recognize (Tailwind's `@theme` and friends).
fn walk_rules<'i, R>(rules: &[CssRule<'i, R>], path: &str, out: &mut Vec<CssSymbol>) {
    for rule in rules {
        match rule {
            CssRule::Style(style_rule) => {
                collect_selector_list(&style_rule.selectors, path, style_rule.loc.line, out);
                collect_declarations(&style_rule.declarations, path, style_rule.loc.line, out);
                walk_rules(&style_rule.rules.0, path, out);
            }
            CssRule::Nesting(nesting_rule) => {
                let style_rule = &nesting_rule.style;
                collect_selector_list(&style_rule.selectors, path, style_rule.loc.line, out);
                collect_declarations(&style_rule.declarations, path, style_rule.loc.line, out);
                walk_rules(&style_rule.rules.0, path, out);
            }
            CssRule::NestedDeclarations(nested) => {
                collect_declarations(&nested.declarations, path, nested.loc.line, out);
            }
            CssRule::Media(r) => walk_rules(&r.rules.0, path, out),
            CssRule::Supports(r) => walk_rules(&r.rules.0, path, out),
            CssRule::Container(r) => walk_rules(&r.rules.0, path, out),
            CssRule::Scope(r) => walk_rules(&r.rules.0, path, out),
            CssRule::StartingStyle(r) => walk_rules(&r.rules.0, path, out),
            CssRule::LayerBlock(r) => walk_rules(&r.rules.0, path, out),
            CssRule::MozDocument(r) => walk_rules(&r.rules.0, path, out),
            CssRule::Property(property_rule) => {
                out.push(CssSymbol {
                    kind: CssSymbolKind::CustomProperty,
                    name: property_rule.name.0.as_ref().to_string(),
                    path: path.to_string(),
                    line: property_rule.loc.line + 1,
                });
            }
            CssRule::Unknown(unknown_rule) => {
                if let Some(block) = &unknown_rule.block {
                    collect_custom_props_from_tokens(&block.0, path, unknown_rule.loc.line, out);
                }
            }
            // Import, Keyframes, FontFace, Page, Viewport, CustomMedia,
            // LayerStatement, Namespace, ViewTransition, CounterStyle,
            // FontPaletteValues, FontFeatureValues, Ignored, Custom: none of
            // these define a class selector or a custom property.
            _ => {}
        }
    }
}

fn collect_selector_list(
    selectors: &SelectorList<'_>,
    path: &str,
    loc_line: u32,
    out: &mut Vec<CssSymbol>,
) {
    for selector in selectors.0.iter() {
        collect_classes_from_selector(selector, path, loc_line, out);
    }
}

fn collect_classes_from_selector(
    selector: &Selector<'_>,
    path: &str,
    loc_line: u32,
    out: &mut Vec<CssSymbol>,
) {
    for component in selector.iter_raw_match_order() {
        collect_classes_from_component(component, path, loc_line, out);
    }
}

/// Extract `Component::Class` from a selector component, recursing into the
/// nested selector lists of functional pseudo-classes (`:is()`, `:where()`,
/// `:not()`, `:has()`, `::slotted()`, `:host()`) so a class named only
/// inside one of those still surfaces.
fn collect_classes_from_component(
    component: &Component<'_>,
    path: &str,
    loc_line: u32,
    out: &mut Vec<CssSymbol>,
) {
    match component {
        Component::Class(ident) => {
            out.push(CssSymbol {
                kind: CssSymbolKind::Class,
                name: format!(".{}", ident.0.as_ref()),
                path: path.to_string(),
                line: loc_line + 1,
            });
        }
        Component::Negation(list)
        | Component::Where(list)
        | Component::Is(list)
        | Component::Has(list) => {
            for nested in list.iter() {
                collect_classes_from_selector(nested, path, loc_line, out);
            }
        }
        Component::Any(_, list) => {
            for nested in list.iter() {
                collect_classes_from_selector(nested, path, loc_line, out);
            }
        }
        Component::Slotted(nested) => {
            collect_classes_from_selector(nested, path, loc_line, out);
        }
        Component::Host(Some(nested)) => {
            collect_classes_from_selector(nested, path, loc_line, out);
        }
        _ => {}
    }
}

/// Extract `--custom-property` definitions from a normal declaration block
/// (a `StyleRule` or `NestedDeclarationsRule`'s declarations).
fn collect_declarations(
    block: &DeclarationBlock<'_>,
    path: &str,
    loc_line: u32,
    out: &mut Vec<CssSymbol>,
) {
    for property in block
        .declarations
        .iter()
        .chain(block.important_declarations.iter())
    {
        if let Property::Custom(custom) = property
            && let CustomPropertyName::Custom(name) = &custom.name
        {
            out.push(CssSymbol {
                kind: CssSymbolKind::CustomProperty,
                name: name.0.as_ref().to_string(),
                path: path.to_string(),
                line: loc_line + 1,
            });
        }
    }
}

/// Extract `--custom-property` definitions from the raw token list of an
/// at-rule block lightningcss does not know how to parse structurally (e.g.
/// Tailwind's `@theme`). See the module docs for why a bare `DashedIdent`
/// immediately followed by a colon is unambiguously a definition, not a
/// `var()` reference.
fn collect_custom_props_from_tokens(
    tokens: &[TokenOrValue<'_>],
    path: &str,
    loc_line: u32,
    out: &mut Vec<CssSymbol>,
) {
    for (i, token) in tokens.iter().enumerate() {
        let TokenOrValue::DashedIdent(ident) = token else {
            continue;
        };
        let is_definition = tokens[i + 1..]
            .iter()
            .find(|t| !is_skippable(t))
            .is_some_and(|t| matches!(t, TokenOrValue::Token(Token::Colon)));
        if is_definition {
            out.push(CssSymbol {
                kind: CssSymbolKind::CustomProperty,
                name: ident.0.as_ref().to_string(),
                path: path.to_string(),
                line: loc_line + 1,
            });
        }
    }
}

fn is_skippable(token: &TokenOrValue<'_>) -> bool {
    token.is_whitespace() || matches!(token, TokenOrValue::Token(Token::Comment(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- class selectors ---

    #[test]
    fn extracts_a_simple_class_selector() {
        let symbols = extract_css_symbols(Path::new("a.css"), ".foo { color: red; }").unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].kind, CssSymbolKind::Class);
        assert_eq!(symbols[0].name, ".foo");
        assert_eq!(symbols[0].path, "a.css");
        assert_eq!(symbols[0].line, 1);
    }

    #[test]
    fn multiple_comma_separated_selectors_each_produce_a_symbol() {
        let symbols =
            extract_css_symbols(Path::new("a.css"), ".foo, .bar {\n  color: red;\n}").unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&".foo"));
        assert!(names.contains(&".bar"));
        assert_eq!(symbols.len(), 2);
    }

    #[test]
    fn nested_class_under_native_css_nesting_is_found() {
        // Tailwind v4 output uses native nesting for state variants.
        let symbols = extract_css_symbols(
            Path::new("a.css"),
            ".foo {\n  color: red;\n\n  &:hover {\n    color: blue;\n  }\n\n  .bar {\n    color: green;\n  }\n}",
        )
        .unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&".foo"), "got: {names:?}");
        assert!(
            names.contains(&".bar"),
            "a class nested inside another rule must be found, got: {names:?}"
        );
    }

    #[test]
    fn class_inside_is_pseudo_class_is_found() {
        let symbols =
            extract_css_symbols(Path::new("a.css"), ":is(.foo, .bar) { color: red; }").unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&".foo"), "got: {names:?}");
        assert!(names.contains(&".bar"), "got: {names:?}");
    }

    // --- custom properties: plain declarations ---

    #[test]
    fn extracts_a_custom_property_declaration() {
        let symbols =
            extract_css_symbols(Path::new("a.css"), ":root { --brand-color: #ff0000; }").unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].kind, CssSymbolKind::CustomProperty);
        assert_eq!(symbols[0].name, "--brand-color");
    }

    #[test]
    fn extracts_a_dotted_custom_property_name() {
        // A bare unescaped `.` inside a CSS ident is grammar-illegal -- the
        // tokenizer splits `--spacing-0.5` into an Ident("--spacing-0") and
        // a Number(0.5), which is why `oxc-css-parser` rejects it outright.
        // Tailwind v4's actual compiled output escapes the dot
        // (`--spacing-0\.5`), which the CSS tokenizer resolves into a
        // SINGLE ident whose decoded text is the literal `--spacing-0.5` --
        // this is the real on-disk shape lightningcss must (and does)
        // tolerate.
        let symbols =
            extract_css_symbols(Path::new("a.css"), ":root { --spacing-0\\.5: 0.125rem; }")
                .unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "--spacing-0.5");
    }

    // --- custom properties: @property ---

    #[test]
    fn extracts_an_at_property_rule_name() {
        let symbols = extract_css_symbols(
            Path::new("a.css"),
            "@property --my-color {\n  syntax: '<color>';\n  inherits: false;\n  initial-value: #c0ffee;\n}",
        )
        .unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].kind, CssSymbolKind::CustomProperty);
        assert_eq!(symbols[0].name, "--my-color");
    }

    // --- custom properties: @theme (Tailwind v4, raw-token recovery) ---

    #[test]
    fn extracts_custom_properties_from_a_theme_block() {
        let symbols = extract_css_symbols(
            Path::new("a.css"),
            "@theme {\n  --spacing-0\\.5: 0.125rem;\n  --color-red-500: oklch(0.63 0.24 25);\n}",
        )
        .unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"--spacing-0.5"), "got: {names:?}");
        assert!(names.contains(&"--color-red-500"), "got: {names:?}");
        assert_eq!(symbols.len(), 2);
    }

    #[test]
    fn theme_block_var_reference_is_not_mistaken_for_a_definition() {
        // `--color-red-500` inside `var(...)` is a REFERENCE, not a
        // definition -- only `--other` (the declaration name) must surface.
        let symbols = extract_css_symbols(
            Path::new("a.css"),
            "@theme {\n  --other: var(--color-red-500);\n}",
        )
        .unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["--other"],
            "the var() target must not be reported as a definition"
        );
    }

    // --- tolerance: the full documented Tailwind v4 hazard set at once ---

    #[test]
    fn tolerates_a_representative_tailwind_v4_sheet_with_zero_errors() {
        let content = "@import \"tailwindcss\";\n\n@theme {\n  --spacing-0\\.5: 0.125rem;\n  --color-red-500: oklch(0.637 0.237 25.331);\n  --font-sans: ui-sans-serif, system-ui;\n}\n\n:root {\n  --brand: var(--color-red-500);\n}\n\n.text-red-500 {\n  color: var(--color-red-500);\n}\n";
        let symbols = extract_css_symbols(Path::new("rafters.css"), content).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"--spacing-0.5"));
        assert!(names.contains(&"--color-red-500"));
        assert!(names.contains(&"--font-sans"));
        assert!(names.contains(&"--brand"));
        assert!(names.contains(&".text-red-500"));
    }

    // --- error path: a genuine parse failure is Err, not a panic ---

    #[test]
    fn excessive_nesting_depth_is_a_parse_error_not_a_panic() {
        // TokenList::parse enforces a max nesting depth of 500 inside a raw
        // at-rule block; past that it is a real ParseError, not a tolerated
        // construct -- the one reliable way to force a hard failure out of
        // an otherwise very lenient parser. Reaching the depth check itself
        // takes ~500 recursive-descent stack frames, so this runs on a
        // thread with a generous stack rather than risking an overflow on
        // whatever stack size the test harness's own thread happens to have.
        let handle = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                let opens = "(".repeat(600);
                let closes = ")".repeat(600);
                let content = format!("@theme {{ --x: {opens}{closes}; }}");
                extract_css_symbols(Path::new("a.css"), &content).is_err()
            })
            .expect("spawn thread");
        assert!(
            handle.join().expect("thread panicked"),
            "excessive nesting must surface as Err"
        );
    }

    // --- path normalization ---

    #[test]
    fn path_is_forward_slash_normalized() {
        let symbols = extract_css_symbols(Path::new("src\\styles\\a.css"), ".foo {}").unwrap();
        assert_eq!(symbols[0].path, "src/styles/a.css");
    }

    // --- batch extraction over files ---

    #[test]
    fn extract_for_files_reads_and_parses_each_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.css"), ".foo {}").unwrap();
        std::fs::write(dir.path().join("b.css"), ":root { --x: 1; }").unwrap();

        let files = vec![PathBuf::from("a.css"), PathBuf::from("b.css")];
        let symbols = extract_css_symbols_for_files(dir.path(), &files);
        assert_eq!(symbols.len(), 2);
    }

    #[test]
    fn extract_for_files_skips_unreadable_file_without_aborting_the_batch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.css"), ".foo {}").unwrap();

        let files = vec![PathBuf::from("does-not-exist.css"), PathBuf::from("ok.css")];
        let symbols = extract_css_symbols_for_files(dir.path(), &files);
        assert_eq!(
            symbols.len(),
            1,
            "the missing file must be skipped, not fatal"
        );
        assert_eq!(symbols[0].path, "ok.css");
    }
}
