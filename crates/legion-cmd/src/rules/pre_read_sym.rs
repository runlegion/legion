//! Deny an unbounded `Read` on a large indexed source file (#1056, FR-CMD-005).
//!
//! Ported from `plugin/hooks/pre-read-sym.sh`: whole-file `Read` on a large
//! source file bills `cache_read` for every line when the agent usually only
//! needs one symbol. This module is a NON-BASH rule module (FR-CMD-005): a
//! pure predicate over a `Read` call and `Ctx` returning `Allow` or
//! `Deny(reason)`, never `Rewrite`.
//!
//! DENY-REASON DIVERGENCE FROM THE ISSUE (tracked against #1022): the issue
//! asks every `Deny` to name `legion sym read <symbol>`. That verb has not
//! shipped -- `plugin/hooks/pre-read-sym.sh` still points at `legion sym
//! hover` / `legion sym def` / a bounded `Read ... limit=200` today. This
//! module ports what the live script actually says, matching
//! `test-pre-read-sym.sh`'s assertions (`sym hover`, `limit=200`) rather than
//! naming a command that does not exist. Once #1022 ships, the reason text
//! here should be revisited to name `legion sym read <symbol>` as the
//! issue originally specified.

use crate::call::{Tool, ToolCall};
use crate::ctx::Ctx;
use crate::decision::Decision;

/// A file at or above this many lines is a candidate for the block.
const THRESHOLD: u64 = 500;

/// An explicit `limit` at or below this bound is always allowed -- the
/// issue names this carveout by number, and `test-pre-read-sym.sh` asserts
/// the exact `limit=200` boundary, so the boundary stays a named constant
/// rather than folding into `THRESHOLD` even though today `SMALL_READ_LIMIT
/// <= THRESHOLD` means every limit this catches would also clear the
/// `THRESHOLD` check below it.
const SMALL_READ_LIMIT: u64 = 200;

/// Source extensions the SCIP indexer covers. Mirrors the shell script's
/// case statement, which mirrors the indexer dispatch table.
const INDEXED_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "rb", "php", "c", "cc", "cpp", "cxx", "h",
    "hpp", "cs",
];

/// Decide whether a `Read` may proceed unbounded.
///
/// Registered for `Tool::Read` only (FR-CMD-005); every other tool is
/// `Allow`. The router does no I/O (FR-CMD-001), so both the file's
/// existence and its line count arrive through `Ctx` and `ToolCall` rather
/// than being resolved here.
pub fn decide(call: &ToolCall, ctx: &Ctx) -> Decision {
    if call.tool() != Tool::Read {
        return Decision::Allow;
    }
    let ToolCall::Read { file_path, limit } = call else {
        return Decision::Allow;
    };

    if ctx.env.get("LEGION_SKIP_PRE_READ_SYM").map(String::as_str) == Some("1") {
        return Decision::Allow;
    }

    if !has_indexed_extension(file_path) {
        return Decision::Allow;
    }

    // Repo not covered, or not indexed: pass through.
    if ctx.repo.is_none() || !ctx.index_present {
        return Decision::Allow;
    }

    // `None` means no bound was stated (FR-CMD-001) and must not be read as
    // satisfying either carveout below.
    if let Some(limit) = *limit {
        // Small-read carveout, named by the issue: a targeted Read is
        // always allowed.
        if limit <= SMALL_READ_LIMIT {
            return Decision::Allow;
        }
        // The live script also allows any stated bound up to THRESHOLD --
        // an agent that stated *any* bound is not doing the unbounded dump
        // this guard exists to stop.
        if limit <= THRESHOLD {
            return Decision::Allow;
        }
    }

    // Bypass wins over the block, but not over the carveouts above -- a
    // small, explicitly bounded Read never needs a bypass in the first
    // place. The adapter (not this module) writes the bypass telemetry row.
    if ctx.env.get("LEGION_BYPASS_READ").map(String::as_str) == Some("1") {
        return Decision::Allow;
    }

    // `None` means unresolved, not small (FR-CMD-001 rev 8): a file whose
    // line count the caller could not supply must not pass the size test by
    // defaulting to zero.
    let Some(lines) = ctx.file_lines else {
        return Decision::Allow;
    };

    if lines < THRESHOLD {
        return Decision::Allow;
    }

    Decision::Deny(deny_reason(file_path, lines))
}

fn has_indexed_extension(file_path: &str) -> bool {
    match file_path.rsplit_once('.') {
        Some((_, ext)) => INDEXED_EXTENSIONS.contains(&ext),
        None => false,
    }
}

/// The deny reason, ported verbatim (modulo real path/line-count
/// interpolation) from `plugin/hooks/pre-read-sym.sh`'s `REASON`.
///
/// Names `legion sym hover` / `legion sym def` / a bounded `limit=200`
/// Read, NOT `legion sym read <symbol>` -- see the module-level divergence
/// note. `test-pre-read-sym.sh` asserts on this exact wording.
fn deny_reason(file_path: &str, lines: u64) -> String {
    format!(
        "`Read {file_path}` would bill {lines} lines x cache_read. This repo has a SCIP index; consider:\n\n\
         - `legion sym hover <Symbol>` -- one symbol's signature + docstring, in bytes.\n\
         - `legion sym def <Symbol>` -- jump straight to the definition site.\n\
         - `Read {file_path} limit=200` -- targeted chunk if you really need source bytes.\n\n\
         If none of those answer your question (e.g. you need to see how multiple symbols interact, or the file is configuration not code), bypass with:\n\n\
         - `LEGION_BYPASS_READ=1 Read {file_path}`\n\n\
         The bypass writes one row to bypass.jsonl so #440's summary will see it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(file_path: &str, limit: Option<u64>) -> ToolCall {
        ToolCall::Read {
            file_path: file_path.to_owned(),
            limit,
        }
    }

    /// A `Ctx` shaped like the covered, indexed `repo` fixture in
    /// `test-pre-read-sym.sh`.
    fn covered_ctx() -> Ctx {
        Ctx {
            repo: Some("repo".into()),
            cwd: Some("/work/repo".into()),
            index_present: true,
            ..Ctx::default()
        }
    }

    fn ctx_with_lines(lines: u64) -> Ctx {
        Ctx {
            file_lines: Some(lines),
            ..covered_ctx()
        }
    }

    #[test]
    fn non_read_tool_passes_through() {
        let call = ToolCall::Bash {
            command: "cat src/big.rs".into(),
        };
        assert_eq!(decide(&call, &ctx_with_lines(800)), Decision::Allow);
    }

    #[test]
    fn non_source_extension_passes_through() {
        let call = read("README.md", None);
        assert_eq!(decide(&call, &ctx_with_lines(800)), Decision::Allow);
    }

    #[test]
    fn small_file_under_threshold_passes_through() {
        let call = read("src/small.rs", None);
        assert_eq!(decide(&call, &ctx_with_lines(100)), Decision::Allow);
    }

    #[test]
    fn large_file_with_explicit_small_limit_passes_through() {
        let call = read("src/big.rs", Some(150));
        assert_eq!(decide(&call, &ctx_with_lines(800)), Decision::Allow);
    }

    #[test]
    fn large_file_with_limit_200_boundary_passes_through() {
        let call = read("src/big.rs", Some(200));
        assert_eq!(decide(&call, &ctx_with_lines(800)), Decision::Allow);
    }

    #[test]
    fn large_file_with_no_limit_is_denied() {
        let call = read("src/big.rs", None);
        let decision = decide(&call, &ctx_with_lines(800));
        match decision {
            Decision::Deny(reason) => {
                assert!(
                    reason.contains("800"),
                    "reason should name the line count: {reason}"
                );
                assert!(
                    reason.contains("legion sym hover"),
                    "reason should mention sym hover: {reason}"
                );
                assert!(
                    reason.contains("limit=200"),
                    "reason should mention the limit alternative: {reason}"
                );
                assert!(
                    reason.contains("LEGION_BYPASS_READ=1"),
                    "reason should offer the env bypass: {reason}"
                );
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn large_file_with_limit_above_small_read_but_within_threshold_passes_through() {
        // Between SMALL_READ_LIMIT (200) and THRESHOLD (500): the issue is
        // silent here, but the live script allows any stated bound up to
        // THRESHOLD, so this discriminates the second `if` from the first.
        let call = read("src/big.rs", Some(300));
        assert_eq!(decide(&call, &ctx_with_lines(800)), Decision::Allow);
    }

    #[test]
    fn large_file_with_limit_501_is_denied_pinning_the_threshold_edge() {
        let call = read("src/big.rs", Some(501));
        assert!(matches!(
            decide(&call, &ctx_with_lines(800)),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn large_file_with_limit_above_threshold_is_still_denied() {
        let call = read("src/big.rs", Some(600));
        assert!(matches!(
            decide(&call, &ctx_with_lines(800)),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn bypass_env_allows_regardless_of_size() {
        let mut ctx = ctx_with_lines(800);
        ctx.env.insert("LEGION_BYPASS_READ".into(), "1".into());
        let call = read("src/big.rs", None);
        assert_eq!(decide(&call, &ctx), Decision::Allow);
    }

    #[test]
    fn skip_env_allows_regardless_of_size() {
        let mut ctx = ctx_with_lines(800);
        ctx.env
            .insert("LEGION_SKIP_PRE_READ_SYM".into(), "1".into());
        let call = read("src/big.rs", None);
        assert_eq!(decide(&call, &ctx), Decision::Allow);
    }

    #[test]
    fn unresolved_file_passes_through() {
        // Mirrors the nonexistent-file case: the caller could not resolve a
        // line count, so `Ctx.file_lines` is `None`. This must not be read
        // as small -- it is Allow only because the router genuinely has no
        // information, not because zero is being treated as "under threshold".
        let call = read("/does/not/exist.rs", None);
        let ctx = covered_ctx();
        assert_eq!(ctx.file_lines, None);
        assert_eq!(decide(&call, &ctx), Decision::Allow);
    }

    #[test]
    fn none_limit_is_not_treated_as_a_zero_bound() {
        // A no-limit Read on a large file must deny -- if `limit: None`
        // were ever conflated with `limit: Some(0)`, this would still deny
        // (0 <= 200), so this test pins the *reason* the deny fires: no
        // stated bound, not a bound of zero passing some other check.
        let call = read("src/big.rs", None);
        assert!(matches!(
            decide(&call, &ctx_with_lines(800)),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn repo_uncovered_or_not_indexed_passes_through() {
        let call = read("src/big.rs", None);

        let mut uncovered = ctx_with_lines(800);
        uncovered.repo = None;
        assert_eq!(decide(&call, &uncovered), Decision::Allow);

        let mut unindexed = ctx_with_lines(800);
        unindexed.index_present = false;
        assert_eq!(decide(&call, &unindexed), Decision::Allow);
    }

    #[test]
    fn an_adapter_resolved_uncovered_repo_passes_through() {
        // LEGION_REPO precedence (env overrides basename(cwd)) resolves in
        // the adapter, before Ctx exists (slice 9) -- not reproducible
        // in-crate, see the work summary's "criteria not met" list. This
        // test only confirms the shape this module DOES see once that
        // resolution has already redirected `Ctx.repo` to an uncovered
        // name: `index_present` false for that repo, same code path as
        // `repo_uncovered_or_not_indexed_passes_through`.
        let call = read("src/big.rs", None);
        let mut ctx = ctx_with_lines(800);
        ctx.repo = Some("uncovered-elsewhere".into());
        ctx.index_present = false;
        assert_eq!(decide(&call, &ctx), Decision::Allow);
    }
}
