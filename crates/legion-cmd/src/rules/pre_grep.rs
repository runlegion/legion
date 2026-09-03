//! The `pre-grep` rule module (#1051, FR-CMD-005): the Grep/Glob search guard.
//!
//! Ported from `plugin/hooks/pre-grep.sh`, narrowed to the Allow/Deny
//! contract FR-CMD-005 sets for non-Bash modules -- this module never
//! constructs `Rewrite`, `Proxy`, or `Ask`. The script's other two states
//! (INJECT: sym cross-repo hits and the recall probe on regex-stripped/
//! glob-segment tokens) are out of scope here: both inject FETCHED content,
//! and this crate's router does no I/O (FR-CMD-001). They belong to the
//! separate pre-load design (FR-CMD-014, slice 8), which has the two-pass
//! contract that can fetch. This module implements only the script's BLOCK
//! and BYPASS-REFUSAL states.
//!
//! SYMBOL-HIT GAP, tracked openly rather than resolved here: the script's
//! deny text embeds the actual `legion sym def` JSON response inline
//! (`` `legion sym def $SYMBOL --repo $REPO` returned: ```json ... ``` ``).
//! This crate's router does no I/O, so that payload has to arrive through
//! `Ctx` -- and no accepted `Ctx` field carries it yet (the issue names this
//! explicitly as an unresolved dependency on a future slice). `Ctx.sym_local_hit`
//! (added alongside this module, same placeholder pattern as `file_lines`
//! for #1056) carries only the one bit this module's Allow/Deny branch
//! needs: does the symbol candidate resolve LOCALLY. The deny reasons below
//! are ported from the script verbatim MINUS the embedded JSON hit block,
//! which this module has no data to fill in. `test-pre-grep.sh`'s
//! assertions never check for that JSON body either -- they check for the
//! command names and phrases this port keeps.

use crate::call::{Tool, ToolCall};
use crate::ctx::Ctx;
use crate::decision::Decision;

/// Decide a `Grep`/`Glob` call against the sym-block/bypass-refusal ladder.
///
/// Registered for `Tool::Grep` and `Tool::Glob` only (FR-CMD-005); every
/// other tool is `Allow` (mirrors the script's `case "$TOOL" in Grep|Glob)
/// ;; *) exit 0 ;; esac`).
pub fn decide(call: &ToolCall, ctx: &Ctx) -> Decision {
    let (pattern, tool) = match call {
        ToolCall::Grep { pattern, .. } => (pattern, Tool::Grep),
        ToolCall::Glob { pattern, .. } => (pattern, Tool::Glob),
        _ => return Decision::Allow,
    };

    if is_skip_set(ctx) {
        return Decision::Allow;
    }

    // Missing pattern or missing repo: mirrors the script's `exit 0` on
    // missing `PATTERN`/`REPO` (NFR-CMD-002 -- a known, parseable absence,
    // not the unparseable-input case that rules out a silent Allow).
    if pattern.is_empty() {
        return Decision::Allow;
    }
    let Some(repo) = ctx.repo.as_deref() else {
        return Decision::Allow;
    };

    // Strip a leading/trailing `\b` word-boundary wrapper (the script's
    // `sed -E 's/^\\b//; s/\\b$//'`), for both Grep and Glob alike, then
    // apply the SAME symbol-shape predicate pre-bash-grep's module uses so
    // the two guards cannot disagree on a pattern.
    let symbol = strip_word_boundary(pattern);
    let local_hit = ctx.index_present && ctx.sym_local_hit && is_symbol_shape(symbol);

    // Bypass: refused when the pattern resolves to a real local symbol hit
    // (the escape exists for free-text searches, not symbol queries dressed
    // up as text). Otherwise Allow -- recording the bypass is
    // `CommandRecord`'s job (FR-CMD-009, slice 5), not this module's.
    if ctx.env.get("LEGION_BYPASS_GREP").map(String::as_str) == Some("1") {
        if local_hit {
            return Decision::Deny(bypass_refused_reason(symbol, pattern, repo, &tool));
        }
        return Decision::Allow;
    }

    // Deny when the repo is indexed AND the symbol candidate resolves to a
    // hit local to this repo (the #458 relevance gate -- a cluster-wide hit
    // in an unrelated repo never blocks; that case never sets
    // `sym_local_hit`, so it falls through here).
    if local_hit {
        return Decision::Deny(block_reason(symbol, pattern, repo, &tool));
    }

    Decision::Allow
}

/// `LEGION_SKIP_PRE_GREP=1`, and the back-compat alias
/// `LEGION_SKIP_PRE_GREP_SCIP=1` from the hook this module's script merged
/// away.
fn is_skip_set(ctx: &Ctx) -> bool {
    ctx.env.get("LEGION_SKIP_PRE_GREP").map(String::as_str) == Some("1")
        || ctx.env.get("LEGION_SKIP_PRE_GREP_SCIP").map(String::as_str) == Some("1")
}

/// Port of the script's `sed -E 's/^\\b//; s/\\b$//'`: strip a literal
/// leading/trailing two-character `\b` sequence (backslash, b), not a regex
/// word-boundary match against the string's actual edges.
fn strip_word_boundary(pattern: &str) -> &str {
    let stripped = pattern.strip_prefix("\\b").unwrap_or(pattern);
    stripped.strip_suffix("\\b").unwrap_or(stripped)
}

/// Port of `legion_prequery_is_symbol` (`_legion-prequery.sh`): a bare
/// symbol identifier, CamelCase or snake_case, length > 2, no regex
/// metacharacters. The SAME predicate `pre-bash-grep`'s module uses.
///
/// Bash pattern: `^[A-Z][A-Za-z0-9_]{2,}$|^[a-z_][a-z_0-9]{2,}$`. Hand-rolled
/// rather than pulling in a regex dependency this crate does not otherwise
/// need (mirrors `router.rs`'s own hand-scanned placeholder parser).
fn is_symbol_shape(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 3 {
        return false;
    }
    let (first, rest) = (chars[0], &chars[1..]);
    if first.is_ascii_uppercase() {
        rest.iter().all(|c| c.is_ascii_alphanumeric() || *c == '_')
    } else if first.is_ascii_lowercase() || first == '_' {
        rest.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
    } else {
        false
    }
}

/// The non-symbol-shape redirect set, ported verbatim from the script and
/// shared by both deny reasons below (FR-CMD-016).
fn non_symbol_redirects(pattern: &str, repo: &str) -> String {
    format!(
        "`legion sym etc find-content '{pattern}' --repo {repo}` (content), \
         `legion sym tree --repo {repo}` (structure), \
         `legion sym etc extract <path> --field <field>` (a config/frontmatter value), \
         `legion sym etc find-file '{pattern}' --repo {repo}` (locate by name/role). \
         There is no env-var hard escape -- the mandatory search block is the operator's \
         permissions.deny."
    )
}

/// State 2 BLOCK reason, ported from the script's `emit_deny` text (minus
/// the embedded `legion sym def` JSON body -- see the module-level gap
/// note).
fn block_reason(symbol: &str, pattern: &str, repo: &str, tool: &Tool) -> String {
    let tool = tool.as_str();
    format!(
        "Use `legion sym def {symbol} --repo {repo}` -- it answered this in bytes from the SCIP \
         index. {tool} on `{pattern}` would scan files and bill cache_read.\n\n\
         The soft bypass (LEGION_BYPASS_GREP=1) is REFUSED for symbol-shaped patterns that \
         resolve in this repo's SCIP index -- it exists for free-text searches, not symbol \
         queries dressed up as text. For symbols use `legion sym def {symbol}` / `sym refs` / \
         `sym list` -- sym covers every indexed language, not just Rust. For non-symbol shapes: \
         {redirects}",
        redirects = non_symbol_redirects(pattern, repo)
    )
}

/// State 3 BYPASS-REFUSAL reason, ported from the script's `emit_deny` text
/// inside the `LEGION_BYPASS_GREP=1` branch (minus the embedded JSON body).
fn bypass_refused_reason(symbol: &str, pattern: &str, repo: &str, tool: &Tool) -> String {
    let tool = tool.as_str();
    format!(
        "Soft bypass refused: `{symbol}` resolves to a symbol in this repo's SCIP index. Use \
         `legion sym def {symbol} --repo {repo}` (or `sym refs` / `sym hover`) instead -- sym \
         covers every indexed language, not just Rust. LEGION_BYPASS_GREP exists for free-text \
         searches; it cannot route around sym for symbol queries.\n\n\
         For non-symbol shapes, {tool} is not the sanctioned surface either: {redirects}",
        redirects = non_symbol_redirects(pattern, repo)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grep(pattern: &str) -> ToolCall {
        ToolCall::Grep {
            pattern: pattern.into(),
            path: "src".into(),
        }
    }

    fn glob(pattern: &str) -> ToolCall {
        ToolCall::Glob {
            pattern: pattern.into(),
            path: "src".into(),
        }
    }

    /// A `Ctx` shaped like `test-pre-grep.sh`'s covered, indexed "legion"
    /// fixture with a local symbol hit for the pattern under test.
    fn local_hit_ctx() -> Ctx {
        Ctx {
            repo: Some("legion".into()),
            cwd: Some("/tmp/legion".into()),
            index_present: true,
            sym_local_hit: true,
            ..Ctx::default()
        }
    }

    /// Same coverage, but the symbol resolves ONLY in another repo -- the
    /// #458 relevance gate case. `sym_local_hit` stays `false`: the caller
    /// never sets it for a cluster-wide-only hit.
    fn cross_repo_only_ctx() -> Ctx {
        Ctx {
            repo: Some("legion".into()),
            cwd: Some("/tmp/legion".into()),
            index_present: true,
            sym_local_hit: false,
            ..Ctx::default()
        }
    }

    #[test]
    fn sym_tier_indexed_repo_plus_local_hit_denies() {
        let decision = decide(&grep("Symbol"), &local_hit_ctx());
        match decision {
            Decision::Deny(reason) => {
                assert!(reason.contains("legion sym def"), "{reason}");
                assert!(reason.contains("Symbol"), "names the pattern: {reason}");
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn deny_reason_routes_non_symbol_shapes_to_sym_etc() {
        let decision = decide(&grep("Symbol"), &local_hit_ctx());
        let Decision::Deny(reason) = decision else {
            panic!("expected Deny");
        };
        assert!(reason.contains("sym etc find-content"), "{reason}");
        assert!(reason.contains("sym tree"), "{reason}");
        assert!(reason.contains("sym etc extract"), "{reason}");
        assert!(reason.contains("sym etc find-file"), "{reason}");
        assert!(reason.contains("not just Rust"), "{reason}");
    }

    #[test]
    fn word_boundary_wrapped_pattern_still_resolves_and_denies() {
        let decision = decide(&grep("\\bSymbol\\b"), &local_hit_ctx());
        assert!(matches!(decision, Decision::Deny(_)));
    }

    #[test]
    fn glob_pattern_gets_the_same_symbol_tier_as_grep() {
        let decision = decide(&glob("Symbol"), &local_hit_ctx());
        assert!(matches!(decision, Decision::Deny(_)));
    }

    #[test]
    fn cross_repo_only_hit_does_not_block() {
        // #458 relevance gate: a cluster-wide hit in an unrelated repo never
        // justifies a block.
        assert_eq!(
            decide(&grep("commonword"), &cross_repo_only_ctx()),
            Decision::Allow
        );
    }

    #[test]
    fn no_index_present_does_not_block_even_with_a_symbol_shaped_pattern() {
        let mut ctx = local_hit_ctx();
        ctx.index_present = false;
        assert_eq!(decide(&grep("Symbol"), &ctx), Decision::Allow);
    }

    #[test]
    fn non_symbol_pattern_is_allowed_even_with_a_local_hit_flag_set() {
        // A caller should never set `sym_local_hit` for a non-symbol
        // pattern, but the predicate itself must not depend on that
        // discipline -- `is_symbol_shape` gates the block independently.
        let ctx = local_hit_ctx();
        assert_eq!(decide(&grep("[A-Z][a-z]+"), &ctx), Decision::Allow);
    }

    #[test]
    fn regex_heavy_pattern_is_allowed() {
        let ctx = local_hit_ctx();
        assert_eq!(decide(&grep("\\s+\\w+"), &ctx), Decision::Allow);
    }

    #[test]
    fn pure_wildcard_glob_is_allowed() {
        let ctx = local_hit_ctx();
        assert_eq!(decide(&glob("**/*.rs"), &ctx), Decision::Allow);
        assert_eq!(decide(&glob("*.toml"), &ctx), Decision::Allow);
    }

    #[test]
    fn non_grep_glob_calls_are_always_allowed() {
        let ctx = local_hit_ctx();
        assert_eq!(
            decide(
                &ToolCall::Bash {
                    command: "grep Symbol src".into()
                },
                &ctx
            ),
            Decision::Allow
        );
        assert_eq!(
            decide(
                &ToolCall::Read {
                    file_path: "src/lib.rs".into(),
                    limit: None
                },
                &ctx
            ),
            Decision::Allow
        );
    }

    #[test]
    fn skip_env_allows_regardless_of_a_local_hit() {
        let mut ctx = local_hit_ctx();
        ctx.env.insert("LEGION_SKIP_PRE_GREP".into(), "1".into());
        assert_eq!(decide(&grep("Symbol"), &ctx), Decision::Allow);
    }

    #[test]
    fn legacy_skip_alias_also_allows() {
        let mut ctx = local_hit_ctx();
        ctx.env
            .insert("LEGION_SKIP_PRE_GREP_SCIP".into(), "1".into());
        assert_eq!(decide(&grep("Symbol"), &ctx), Decision::Allow);
    }

    #[test]
    fn bypass_is_refused_for_a_pattern_with_a_local_symbol_hit() {
        let mut ctx = local_hit_ctx();
        ctx.env.insert("LEGION_BYPASS_GREP".into(), "1".into());
        let decision = decide(&grep("Symbol"), &ctx);
        match decision {
            Decision::Deny(reason) => {
                assert!(reason.contains("free-text searches"), "{reason}");
                assert!(reason.contains("sym etc find-content"), "{reason}");
                assert!(reason.contains("sym tree"), "{reason}");
                assert!(reason.contains("sym etc extract"), "{reason}");
                assert!(reason.contains("sym etc find-file"), "{reason}");
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn bypass_is_allowed_for_a_non_local_pattern() {
        let mut ctx = cross_repo_only_ctx();
        ctx.env.insert("LEGION_BYPASS_GREP".into(), "1".into());
        assert_eq!(decide(&grep("commonword"), &ctx), Decision::Allow);
    }

    #[test]
    fn missing_pattern_is_allowed() {
        assert_eq!(decide(&grep(""), &local_hit_ctx()), Decision::Allow);
    }

    #[test]
    fn missing_repo_is_allowed() {
        let mut ctx = local_hit_ctx();
        ctx.repo = None;
        assert_eq!(decide(&grep("Symbol"), &ctx), Decision::Allow);
    }

    #[test]
    fn a_two_character_candidate_is_not_symbol_shaped() {
        // Boundary of `is_symbol_shape`'s length > 2 requirement -- a
        // caller-set `sym_local_hit` cannot make a too-short candidate
        // block, since the predicate itself governs the gate.
        let ctx = local_hit_ctx();
        assert_eq!(decide(&grep("ab"), &ctx), Decision::Allow);
    }
}
