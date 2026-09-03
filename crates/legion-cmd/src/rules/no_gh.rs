//! `no-gh` -- the `gh` section of the embedded route table (#1044).
//!
//! A BASH module (FR-CMD-005) is a route-table section plus the tests
//! carried over from its shell script (FR-CMD-016), holding no match arms
//! of its own. Every deny-or-rewrite mapping this module ports from
//! `plugin/hooks/no-gh.sh` lives in `route-table.toml`'s `gh` section; this
//! file carries only the ported assertions, exercised through the one
//! evaluator in `router.rs` (`Router::route` / `decide_stage`). Ported from
//! `plugin/hooks/test-no-gh.sh`, which stays the live enforcing guard's own
//! test suite until FR-CMD-016's cutover.

#[cfg(test)]
mod tests {
    use crate::call::ToolCall;
    use crate::ctx::Ctx;
    use crate::decision::Decision;
    use crate::router::Router;
    use crate::table::RouteTable;

    fn router() -> Router {
        Router::new(RouteTable::embedded().expect("embedded table parses")).expect("table compiles")
    }

    fn ctx() -> Ctx {
        Ctx {
            repo: Some("legion-test".into()),
            ..Ctx::default()
        }
    }

    fn bash(command: &str) -> ToolCall {
        ToolCall::Bash {
            command: command.into(),
        }
    }

    fn rewrite_command(decision: Decision, case: &str) -> String {
        match decision {
            Decision::Rewrite { command, .. } => command,
            other => panic!("expected a rewrite for {case:?}, got {other:?}"),
        }
    }

    fn deny_reason(decision: Decision, case: &str) -> String {
        match decision {
            Decision::Deny(reason) => reason,
            other => panic!("expected a deny for {case:?}, got {other:?}"),
        }
    }

    // --- #862: rewrite the lossless read subset ----------------------------

    #[test]
    fn rewrites_the_lossless_read_subset() {
        let r = router();
        let cases = [
            (
                "gh pr view 42",
                "legion pr view --repo legion-test --number 42",
            ),
            (
                "gh pr checks 42",
                "legion pr checks --repo legion-test --number 42",
            ),
            ("gh pr list", "legion pr list --repo legion-test"),
            (
                "gh issue view 7",
                "legion issue view --repo legion-test --number 7",
            ),
            ("gh issue list", "legion issue list --repo legion-test"),
            (
                "gh pr view --number 99",
                "legion pr view --repo legion-test --number 99",
            ),
        ];
        for (cmd, expected) in cases {
            let routed = r.route(&bash(cmd), &ctx());
            assert_eq!(
                rewrite_command(routed.decision, cmd),
                expected,
                "case {cmd:?}"
            );
        }
    }

    #[test]
    fn absolute_path_gh_still_rewrites_the_lossless_read_subset() {
        // Absolute-path basename normalization is the tokenizer's job
        // (slice 2), but this proves the wiring carries it through to a
        // real rewrite, not just a `matched` binary.
        let r = router();
        let routed = r.route(&bash("/usr/bin/gh pr view 1"), &ctx());
        assert_eq!(
            rewrite_command(routed.decision, "/usr/bin/gh pr view 1"),
            "legion pr view --repo legion-test --number 1"
        );
    }

    // --- lossy flags: deny, never silently drop the flag --------------------

    #[test]
    fn lossy_flags_deny_and_name_the_flag_never_silently_dropping_it() {
        let r = router();
        let cases: [(&str, &str); 10] = [
            ("gh pr view 42 --json title", "--json"),
            ("gh pr view 42 --json=title", "--json"),
            ("gh pr view 42 -c", "-c"),
            ("gh pr view 42 -R owner/repo", "-R"),
            ("gh pr view 42 -w", "-w"),
            ("gh pr checks 42 --watch", "--watch"),
            ("gh pr checks 42 --required", "--required"),
            ("gh pr list --state closed", "--state"),
            ("gh issue view 7 --jq .title", "--jq"),
            ("gh issue list --label bug", "--label"),
        ];
        for (cmd, needle) in cases {
            let routed = r.route(&bash(cmd), &ctx());
            let reason = deny_reason(routed.decision, cmd);
            assert!(reason.contains(needle), "case {cmd:?}: {reason}");
        }
    }

    #[test]
    fn a_lossy_flag_denies_even_when_the_number_capture_is_present() {
        // NFR-CMD-005's fixed evaluation order: Deny/Proxy patterns run
        // before Rewrite patterns, so a blocking flag wins over an
        // otherwise-satisfied capture rather than producing a truncated
        // rewrite that silently drops the flag.
        let r = router();
        let routed = r.route(&bash("gh pr view 42 --json title"), &ctx());
        match routed.decision {
            Decision::Deny(reason) => assert!(reason.contains("--json")),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    // --- pr diff: always denies, never a rewrite -----------------------------

    #[test]
    fn pr_diff_always_denies_naming_the_gap_and_the_closest_command() {
        let r = router();
        let routed = r.route(&bash("gh pr diff 42"), &ctx());
        let reason = deny_reason(routed.decision, "gh pr diff 42");
        assert!(reason.contains("never the diff content"));
        assert!(reason.contains("legion pr view --repo legion-test --number 42"));
    }

    // --- deny-only verbs: each names its own equivalent ----------------------

    #[test]
    fn deny_only_verbs_each_name_their_own_legion_equivalent() {
        let r = router();
        let cases: [(&str, &str); 14] = [
            (
                "gh pr merge 123",
                "legion pr merge --repo legion-test --number 123",
            ),
            (
                "gh pr close 42",
                "legion pr close --repo legion-test --number 42",
            ),
            (
                "gh pr edit 42 --title x",
                "legion pr edit --repo legion-test --number 42",
            ),
            (
                "gh pr review 42 --approve",
                "legion pr review --repo legion-test --number 42",
            ),
            (
                "gh pr comment 42 --body x",
                "legion comment --repo legion-test --number 42",
            ),
            (
                "gh pr comments 42",
                "legion pr comments --repo legion-test --number 42",
            ),
            (
                "gh issue create --title x",
                "legion issue create --repo legion-test",
            ),
            (
                "gh issue close 5",
                "legion issue close --repo legion-test --number 5",
            ),
            (
                "gh issue reopen 5",
                "legion issue reopen --repo legion-test --number 5",
            ),
            (
                "gh issue edit 5 --title x",
                "legion issue edit --repo legion-test --number 5",
            ),
            (
                "gh issue comment 5 --body x",
                "legion comment --repo legion-test --number 5",
            ),
            ("gh run list", "legion pr checks --repo legion-test"),
            (
                "gh run view 1",
                "legion pr checks --repo legion-test --number 1",
            ),
            (
                "gh run watch 1",
                "legion pr checks --repo legion-test --number 1",
            ),
        ];
        for (cmd, needle) in cases {
            let routed = r.route(&bash(cmd), &ctx());
            let reason = deny_reason(routed.decision, cmd);
            assert!(reason.contains(needle), "case {cmd:?}: {reason}");
        }
    }

    // --- missing number: placeholder deny, never a broken rewrite -----------

    #[test]
    fn missing_number_falls_through_to_placeholder_deny_never_a_broken_rewrite() {
        let r = router();
        let routed = r.route(&bash("gh pr view"), &ctx());
        let reason = deny_reason(routed.decision, "gh pr view");
        assert!(reason.contains("--number <n>"), "{reason}");
    }

    // --- unmapped shapes: group help, never a fabricated translation --------

    #[test]
    fn unmapped_shapes_point_at_group_help_and_invent_nothing() {
        let r = router();

        let routed = r.route(&bash("gh api /repos/x/y"), &ctx());
        let reason = deny_reason(routed.decision, "gh api /repos/x/y");
        assert!(reason.contains("legion --help"), "{reason}");
        assert!(!reason.contains("legion api"), "{reason}");

        let routed = r.route(&bash("gh pr sync"), &ctx());
        let reason = deny_reason(routed.decision, "gh pr sync");
        assert!(reason.contains("legion pr --help"), "{reason}");
    }

    #[test]
    fn bare_gh_with_no_args_denies() {
        let r = router();
        let routed = r.route(&bash("gh"), &ctx());
        assert!(matches!(routed.decision, Decision::Deny(_)));
    }

    // --- compound commands never rewrite: updatedInput replaces the WHOLE
    // command string, so a rewrite of one stage would silently drop or
    // misplace everything else in the chain (#886/#862). -------------------

    #[test]
    fn compound_commands_never_rewrite_even_a_rewrite_eligible_verb() {
        let r = router();
        for cmd in [
            "gh pr view 42 | jq .title",
            "gh pr view 42 > out.txt",
            "gh pr view 42 && echo done",
            "echo hi && gh pr view 42",
            "echo hi && gh pr merge 123",
        ] {
            let routed = r.route(&bash(cmd), &ctx());
            assert!(
                matches!(routed.decision, Decision::Deny(_)),
                "case {cmd:?}: expected deny, got {:?}",
                routed.decision
            );
        }
    }

    #[test]
    fn a_command_substitution_among_ghs_own_words_denies_rather_than_dropping_it() {
        // `$(id)` is not a stage boundary -- `gh pr view 42 $(id)` is one
        // stage, and its `Digits` capture ("42") would otherwise be
        // satisfied while the substitution is silently ignored. Denying is
        // the honest answer; rewriting would drop part of what was typed.
        let r = router();
        for cmd in ["gh pr view 42 $(id)", "gh pr list `id`"] {
            let routed = r.route(&bash(cmd), &ctx());
            assert!(
                matches!(routed.decision, Decision::Deny(_)),
                "case {cmd:?}: expected deny, got {:?}",
                routed.decision
            );
        }
    }

    // --- commands that merely mention gh, or another binary entirely --------

    #[test]
    fn allows_commands_that_merely_mention_gh_or_are_unmanaged() {
        let r = router();
        for cmd in [
            "ghostscript --version",
            "git status",
            "echo gh pr merge",
            "grep gh /var/log/foo",
            "ls /opt/ghosts/",
        ] {
            let routed = r.route(&bash(cmd), &ctx());
            assert_eq!(routed.decision, Decision::Allow, "case {cmd:?}");
        }
    }
}
