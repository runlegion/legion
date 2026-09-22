//! The one evaluator that walks the policy (FR-CMD-011).
//!
//! No routing branch exists anywhere else: route, the adapters and the ledger
//! contain no per-binary or per-verb conditionals. Every decision that turns
//! on what a name means is a lookup into the [`Policy`] data, performed here.
//!
//! The evaluator decides one [`PartOutcome`] per part of a command -- each
//! resolved invocation, each unreduced region -- and [`combine`] folds those
//! into the single Decision route returns. The fold is a branch, not a plain
//! minimum: a part that routes to a sym job wins outright (FR-CMD-007's "route
//! sends it to sym ... never allowed or proxied"), and only when no part is a
//! sym job do the remaining parts fold by the strictest-Decision order.

use crate::decision::{Deciding, Decision, ProxyReason};
use crate::policy::{Family, Policy, Predicate, Rule, RuleOutcome, ToolKind, ToolRules};
use crate::splitter::{Unreduced, UnreducedReason};
use crate::{Context, Lookup};

/// One part's routing result, before the parts are folded into one Decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartOutcome {
    pub decision: Decision,
    pub deciding: Deciding,
    /// True when this part routes to a sym job (FR-CMD-007). A sym part wins
    /// the fold outright.
    pub is_sym: bool,
    /// The verb this part named, if it matched a managed family: the family
    /// key's subcommand words, or the binary when the family has none. Used to
    /// populate [`crate::Facts::verb`] (FR-CMD-003).
    pub verb: Option<String>,
}

/// Decides one Bash invocation against the policy (FR-CMD-011, FR-CMD-016).
///
/// No family matches the binary and its leading operands -> allow (the command
/// carries no managed binary). A family matches but no rule in it resolves the
/// arguments -> deny (a managed rule that cannot resolve). A rule matches but
/// its required recall or consult result is [`Lookup::NotFetched`] -> deny. A
/// rule matches and its lookups are satisfied -> the rule's outcome.
pub fn decide_bash_invocation(
    policy: &Policy,
    binary: &str,
    args: &[String],
    ctx: &Context,
) -> PartOutcome {
    let Some(ToolRules::Bash { families }) = policy.tools.get(&ToolKind::Bash) else {
        return allow_default();
    };

    let Some((verb, family)) = most_specific_family(families, binary, args) else {
        return allow_default();
    };

    for rule in &family.rules {
        if predicates_hold(&rule.predicates, args) {
            return resolve_rule(policy, rule, ctx, Some(verb));
        }
    }

    // The family is managed, but no rule resolves these arguments.
    PartOutcome {
        decision: deny(
            "this managed command matches no policy rule",
            "run it manually, or add a rule that covers it",
        ),
        deciding: Deciding::Default,
        is_sym: false,
        verb: Some(verb),
    }
}

/// Decides one Fields-tool invocation (Edit/Write/Read/Grep/Agent), matching
/// each rule's predicates against the tool call's string field values.
pub fn decide_fields(
    policy: &Policy,
    kind: ToolKind,
    values: &[String],
    ctx: &Context,
) -> PartOutcome {
    let Some(ToolRules::Fields { rules }) = policy.tools.get(&kind) else {
        return allow_default();
    };
    for rule in rules {
        if predicates_hold(&rule.predicates, values) {
            return resolve_rule(policy, rule, ctx, None);
        }
    }
    allow_default()
}

/// Decides one unreduced region (FR-CMD-004, FR-CMD-007).
///
/// An [`UnreducedReason::InterpreterBody`] region is offered to the sym jobs:
/// if one matches its text, the region routes to sym. Every other region --
/// and every interpreter body no sym job claims -- is proxied with reason
/// opaque, recorded coverage-unknown, never allowed.
pub fn decide_region(policy: &Policy, region: &Unreduced) -> PartOutcome {
    if region.reason == UnreducedReason::InterpreterBody
        && let Some(job) = matching_sym_job(policy, &region.text)
    {
        return sym_outcome(&job.id, &job.sym_command);
    }
    PartOutcome {
        decision: Decision::Proxy {
            reason: ProxyReason::Opaque,
        },
        deciding: Deciding::Default,
        is_sym: false,
        verb: None,
    }
}

/// Folds every part into the one Decision route returns (FR-CMD-001,
/// FR-CMD-007).
///
/// A sym part wins outright, so a sym-served command is never allowed or
/// proxied by a sibling part. Otherwise the strictest Decision wins, in the
/// order deny, ask, proxy, rewrite, allow. The strictest RANK is
/// order-independent; a tie at the same rank resolves to the first part in
/// evaluation order (route evaluates all invocations, then all unreduced
/// regions -- not strict command order), which fixes the Decision payload and
/// the `deciding`/`verb` attribution. Nothing here depends on which same-rank
/// part wins, so this is not made command-faithful; #1237 must not assume it is
/// when it keys confirmations off `deciding.id`. An empty part list is the
/// allow default (nothing to route).
pub fn combine(parts: Vec<PartOutcome>) -> PartOutcome {
    if let Some(sym) = parts.iter().find(|p| p.is_sym) {
        return sym.clone();
    }
    // `min_by_key` keeps the last element on a tie; the fold order breaks ties
    // by command order, so keep the first strictest part instead.
    let mut winner: Option<PartOutcome> = None;
    for part in parts {
        let take = match &winner {
            None => true,
            Some(current) => strictness_rank(&part.decision) < strictness_rank(&current.decision),
        };
        if take {
            winner = Some(part);
        }
    }
    winner.unwrap_or_else(allow_default)
}

/// Resolves a matched rule into a [`PartOutcome`], applying the lookup gates
/// (FR-CMD-016) and the sym action (FR-CMD-007).
fn resolve_rule(policy: &Policy, rule: &Rule, ctx: &Context, verb: Option<String>) -> PartOutcome {
    for (required, lookup, name) in [
        (rule.requires_recall, &ctx.recall, "recall"),
        (rule.requires_consult, &ctx.consult, "consult"),
    ] {
        if required && *lookup == Lookup::NotFetched {
            return rule_outcome(
                rule,
                deny(
                    format!("this command needs a {name} result that was not fetched"),
                    format!("fetch the {name} result, then retry"),
                ),
                false,
                verb,
            );
        }
    }

    let (decision, is_sym) = match &rule.outcome {
        RuleOutcome::Allow { note } => (Decision::Allow { note: note.clone() }, false),
        RuleOutcome::Rewrite { target, reason } => (
            Decision::Rewrite {
                target: crate::ManagedTarget::new(target.clone()),
                reason: reason.clone(),
            },
            false,
        ),
        RuleOutcome::Proxy { reason } => (Decision::Proxy { reason: *reason }, false),
        RuleOutcome::Deny { reason, instead } => (deny(reason, instead), false),
        // The operator mark is never copied from the rule into route's output
        // (FR-CMD-006): every ask this issue produces carries it unset.
        RuleOutcome::Ask {
            question, reason, ..
        } => (ask(question, reason), false),
        RuleOutcome::Sym { job } => {
            // The reference was validated at parse time, so the job exists.
            if let Some(sym_job) = policy.sym_job(job) {
                return sym_outcome_with_verb(&sym_job.id, &sym_job.sym_command, verb);
            }
            (
                deny(
                    "this command routes to a sym job that is missing",
                    "run it manually",
                ),
                false,
            )
        }
    };

    rule_outcome(rule, decision, is_sym, verb)
}

/// A [`PartOutcome`] naming `rule` as the deciding entry, with the operator
/// mark unset (FR-CMD-006 -- route never copies the rule's mark here).
fn rule_outcome(
    rule: &Rule,
    decision: Decision,
    is_sym: bool,
    verb: Option<String>,
) -> PartOutcome {
    PartOutcome {
        decision,
        deciding: Deciding::Rule {
            id: rule.id.clone(),
            needs_operator: false,
        },
        is_sym,
        verb,
    }
}

/// The sym job whose patterns all match `text` (FR-CMD-007). Patterns within a
/// job are a conjunction; the disjunction across shapes is the several jobs,
/// tried in policy order (most specific first). A job with no interpreter
/// patterns never claims an interpreter body -- it exists only for visible
/// commands that reference it by a sym action.
fn matching_sym_job<'a>(policy: &'a Policy, text: &str) -> Option<&'a crate::SymJob> {
    policy.sym_jobs.iter().find(|job| {
        !job.interpreter_patterns.is_empty()
            && job.interpreter_patterns.iter().all(|p| text.contains(p))
    })
}

fn sym_outcome(job_id: &str, sym_command: &str) -> PartOutcome {
    sym_outcome_with_verb(job_id, sym_command, None)
}

fn sym_outcome_with_verb(job_id: &str, sym_command: &str, verb: Option<String>) -> PartOutcome {
    PartOutcome {
        // #1228 turns the lossless cases into rewrites; here every sym job
        // yields the deny naming the sym command (FR-CMD-007).
        decision: deny(
            format!("this is a job legion sym serves: use `{sym_command}`"),
            sym_command,
        ),
        deciding: Deciding::Rule {
            id: job_id.to_string(),
            needs_operator: false,
        },
        is_sym: true,
        verb,
    }
}

fn allow_default() -> PartOutcome {
    PartOutcome {
        decision: Decision::Allow { note: None },
        deciding: Deciding::Default,
        is_sym: false,
        verb: None,
    }
}

/// The most specific family whose binary and operand words match, with the
/// verb it names. A family key is whitespace-separated: the first token is the
/// binary, any further tokens are subcommand words the invocation must carry as
/// its leading operands in order. The verb is those subcommand words, or the
/// binary when the key names none.
///
/// Operands are the non-option arguments, with option tokens (those starting
/// with `-`) removed rather than stopped at, so a global option before the
/// subcommand (`git --no-pager push`, `git --git-dir=x show`) does not hide it.
/// Each argument is dequoted first, so a quoted subcommand (`git "push"`) still
/// matches. KNOWN LIMITATION (FR-CMD-007 residual, flagged for the hook-parity
/// issue): a global option that takes a SEPARATE value (`git -C /tmp push`)
/// leaves its value (`/tmp`) in the operand run, because deciding it is a value
/// rather than a subcommand needs per-binary option metadata the policy does
/// not yet carry. Such a command misses its family and reaches the allow
/// default.
fn most_specific_family<'a>(
    families: &'a std::collections::BTreeMap<String, Family>,
    binary: &str,
    args: &[String],
) -> Option<(String, &'a Family)> {
    let operands: Vec<&str> = args
        .iter()
        .map(|a| dequote_outer(a))
        .filter(|a| !a.starts_with('-'))
        .collect();

    let mut best: Option<(usize, String, &Family)> = None;
    for (key, family) in families {
        let mut tokens = key.split_whitespace();
        let Some(key_binary) = tokens.next() else {
            continue;
        };
        if key_binary != binary {
            continue;
        }
        let subcommands: Vec<&str> = tokens.collect();
        if subcommands.len() > operands.len() {
            continue;
        }
        if subcommands
            .iter()
            .zip(operands.iter())
            .all(|(want, have)| want == have)
        {
            let verb = if subcommands.is_empty() {
                binary.to_string()
            } else {
                subcommands.join(" ")
            };
            let specificity = subcommands.len();
            if best.as_ref().is_none_or(|(rank, _, _)| specificity > *rank) {
                best = Some((specificity, verb, family));
            }
        }
    }
    best.map(|(_, verb, family)| (verb, family))
}

/// Removes one matched outer pair of single or double quotes if the whole
/// string is wrapped in it, so matching compares against the value the shell
/// sees, not the raw source text the splitter preserves (`"push"` -> `push`).
/// Not general shell dequoting -- a string with mixed or concatenated quoting
/// is left as-is -- but it covers the ordinary fully-quoted word.
pub(crate) fn dequote_outer(text: &str) -> &str {
    let bytes = text.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'\'' || first == b'"') && first == last {
            return &text[1..text.len() - 1];
        }
    }
    text
}

fn predicates_hold(predicates: &[Predicate], args: &[String]) -> bool {
    predicates.iter().all(|predicate| match predicate {
        Predicate::ArgPresent(word) => args.iter().any(|a| dequote_outer(a) == word),
        Predicate::ArgAbsent(word) => !args.iter().any(|a| dequote_outer(a) == word),
    })
}

/// The strictest-Decision order (FR-CMD-007): deny, ask, proxy, rewrite,
/// allow. Lower rank is stricter.
fn strictness_rank(decision: &Decision) -> u8 {
    match decision {
        Decision::Deny(_) => 0,
        Decision::Ask(_) => 1,
        Decision::Proxy { .. } => 2,
        Decision::Rewrite { .. } => 3,
        Decision::Allow { .. } => 4,
    }
}

/// Builds a deny whose fields are known non-empty; the fixed messages here are
/// never empty, so the construction cannot fail.
fn deny(reason: impl Into<String>, instead: impl Into<String>) -> Decision {
    Decision::deny(reason, instead).unwrap_or(Decision::Proxy {
        reason: ProxyReason::Opaque,
    })
}

/// Builds an ask whose fields are known non-empty.
fn ask(question: impl Into<String>, reason: impl Into<String>) -> Decision {
    Decision::ask(question, reason).unwrap_or(Decision::Proxy {
        reason: ProxyReason::Opaque,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_policy;

    fn policy(text: &str) -> Policy {
        parse_policy(text).expect("valid policy")
    }

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn no_family_match_allows() {
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"git push": {"rules": [
            {"id": "r", "outcome": {"kind": "deny", "reason": "no", "instead": "x"}}
        ]}}}}}"#,
        );
        // `git stash push` does not match the `git push` family: the subcommand
        // word is `stash`, not `push`.
        let outcome =
            decide_bash_invocation(&p, "git", &args(&["stash", "push"]), &Context::default());
        assert_eq!(outcome.decision, Decision::Allow { note: None });
        assert_eq!(outcome.deciding, Deciding::Default);
    }

    #[test]
    fn family_match_with_no_matching_rule_denies() {
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"git push": {"rules": [
            {"id": "r", "predicates": [{"kind": "arg-present", "arg": "--force"}],
             "outcome": {"kind": "deny", "reason": "force", "instead": "x"}}
        ]}}}}}"#,
        );
        let outcome = decide_bash_invocation(&p, "git", &args(&["push"]), &Context::default());
        // A managed rule that cannot resolve yields deny (FR-CMD-016).
        assert!(matches!(outcome.decision, Decision::Deny(_)));
        assert_eq!(outcome.verb.as_deref(), Some("push"));
    }

    #[test]
    fn missing_required_recall_denies() {
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
            {"id": "r", "requires_recall": true, "outcome": {"kind": "allow"}}
        ]}}}}}"#,
        );
        let outcome = decide_bash_invocation(&p, "gh", &args(&[]), &Context::default());
        assert!(matches!(outcome.decision, Decision::Deny(_)));
        assert_eq!(
            outcome.deciding,
            Deciding::Rule {
                id: "r".to_string(),
                needs_operator: false
            }
        );
    }

    #[test]
    fn missing_required_consult_denies_and_a_fetched_consult_lets_it_through() {
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
            {"id": "r", "requires_consult": true, "outcome": {"kind": "allow"}}
        ]}}}}}"#,
        );
        // NotFetched -> deny (FR-CMD-016).
        assert!(matches!(
            decide_bash_invocation(&p, "gh", &args(&[]), &Context::default()).decision,
            Decision::Deny(_)
        ));
        // Fetched (even Empty) -> the rule's own outcome.
        let ctx = Context {
            consult: Lookup::Empty,
            ..Context::default()
        };
        assert_eq!(
            decide_bash_invocation(&p, "gh", &args(&[]), &ctx).decision,
            Decision::Allow { note: None }
        );
    }

    #[test]
    fn a_quoted_subcommand_still_matches_its_family() {
        // The splitter keeps the raw quotes on args; matching must dequote, or
        // a quoted subcommand word evades a deny/ask rule (a policy bypass).
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"git push": {"rules": [
            {"id": "r", "outcome": {"kind": "deny", "reason": "no push", "instead": "x"}}
        ]}}}}}"#,
        );
        assert!(matches!(
            decide_bash_invocation(&p, "git", &args(&["\"push\""]), &Context::default()).decision,
            Decision::Deny(_)
        ));
    }

    #[test]
    fn a_quoted_flag_still_satisfies_a_predicate() {
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"git push": {"rules": [
            {"id": "r", "predicates": [{"kind": "arg-present", "arg": "--force"}],
             "outcome": {"kind": "deny", "reason": "no force", "instead": "x"}}
        ]}}}}}"#,
        );
        assert!(matches!(
            decide_bash_invocation(
                &p,
                "git",
                &args(&["push", "\"--force\""]),
                &Context::default()
            )
            .decision,
            Decision::Deny(_)
        ));
    }

    #[test]
    fn a_valueless_global_option_before_the_subcommand_does_not_hide_it() {
        // `git --no-pager push` and `git --git-dir=x push` must still match the
        // `git push` family (the H2 unambiguous half). A separated option value
        // (`git -C /tmp push`) is a documented residual, not covered here.
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"git push": {"rules": [
            {"id": "r", "outcome": {"kind": "deny", "reason": "no push", "instead": "x"}}
        ]}}}}}"#,
        );
        for args_list in [
            vec!["--no-pager", "push"],
            vec!["--git-dir=x", "push"],
            vec!["--no-pager", "push", "origin", "main"],
        ] {
            assert!(
                matches!(
                    decide_bash_invocation(&p, "git", &args(&args_list), &Context::default())
                        .decision,
                    Decision::Deny(_)
                ),
                "args {args_list:?} should match git push",
            );
        }
    }

    #[test]
    fn ask_rule_marked_needing_operator_still_produces_an_unset_mark() {
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
            {"id": "r", "outcome": {"kind": "ask", "question": "q?", "reason": "why",
             "needs_operator": true}}
        ]}}}}}"#,
        );
        let outcome = decide_bash_invocation(&p, "gh", &args(&[]), &Context::default());
        assert!(matches!(outcome.decision, Decision::Ask(_)));
        // The rule's mark is set, but route's output mark is unset (FR-CMD-006).
        assert_eq!(
            outcome.deciding,
            Deciding::Rule {
                id: "r".to_string(),
                needs_operator: false
            }
        );
    }

    #[test]
    fn each_decision_arm_is_reachable() {
        let p = policy(
            r#"{
            "sym_jobs": [{"id": "find", "sym_command": "legion sym find-content", "interpreter_patterns": ["rglob"]}],
            "tools": {"Bash": {"families": {
                "a": {"rules": [{"id": "ra", "outcome": {"kind": "allow"}}]},
                "b": {"rules": [{"id": "rb", "outcome": {"kind": "rewrite", "target": "legion x", "reason": "r"}}]},
                "c": {"rules": [{"id": "rc", "outcome": {"kind": "proxy", "reason": "binary"}}]},
                "d": {"rules": [{"id": "rd", "outcome": {"kind": "deny", "reason": "r", "instead": "x"}}]},
                "e": {"rules": [{"id": "re", "outcome": {"kind": "ask", "question": "q", "reason": "r"}}]}
            }}}
        }"#,
        );
        let ctx = Context::default();
        assert!(matches!(
            decide_bash_invocation(&p, "a", &[], &ctx).decision,
            Decision::Allow { .. }
        ));
        assert!(matches!(
            decide_bash_invocation(&p, "b", &[], &ctx).decision,
            Decision::Rewrite { .. }
        ));
        assert!(matches!(
            decide_bash_invocation(&p, "c", &[], &ctx).decision,
            Decision::Proxy { .. }
        ));
        assert!(matches!(
            decide_bash_invocation(&p, "d", &[], &ctx).decision,
            Decision::Deny(_)
        ));
        assert!(matches!(
            decide_bash_invocation(&p, "e", &[], &ctx).decision,
            Decision::Ask(_)
        ));
    }

    #[test]
    fn strictest_order_folds_ask_over_proxy() {
        let proxy = PartOutcome {
            decision: Decision::Proxy {
                reason: ProxyReason::Binary,
            },
            deciding: Deciding::Default,
            is_sym: false,
            verb: None,
        };
        let ask = PartOutcome {
            decision: ask("q", "r"),
            deciding: Deciding::Rule {
                id: "r".to_string(),
                needs_operator: false,
            },
            is_sym: false,
            verb: None,
        };
        let folded = combine(vec![proxy, ask]);
        assert!(matches!(folded.decision, Decision::Ask(_)));
    }

    #[test]
    fn a_sym_part_wins_over_a_proxy_sibling() {
        let proxy = PartOutcome {
            decision: Decision::Proxy {
                reason: ProxyReason::Opaque,
            },
            deciding: Deciding::Default,
            is_sym: false,
            verb: None,
        };
        let sym = sym_outcome("find", "legion sym find-content");
        let folded = combine(vec![proxy, sym]);
        assert!(folded.is_sym);
        match folded.decision {
            Decision::Deny(details) => assert_eq!(details.instead(), "legion sym find-content"),
            other => panic!("expected deny naming sym, got {other:?}"),
        }
    }

    #[test]
    fn interpreter_body_matching_all_patterns_routes_to_sym() {
        let p = policy(
            r#"{"sym_jobs": [
            {"id": "find", "sym_command": "legion sym find-content",
             "interpreter_patterns": ["rglob", "read_text"]}
        ]}"#,
        );
        let region = Unreduced {
            text: "import pathlib; [f for f in pathlib.Path('.').rglob('*.rs') if 'x' in f.read_text()]"
                .to_string(),
            reason: UnreducedReason::InterpreterBody,
            depth: 1,
        };
        let outcome = decide_region(&p, &region);
        assert!(outcome.is_sym);
    }

    #[test]
    fn interpreter_body_missing_a_pattern_is_proxied_opaque() {
        // Only `read_text` is present, not `rglob`: the conjunction fails, so
        // this reads one file rather than searching, and is not a sym job.
        let p = policy(
            r#"{"sym_jobs": [
            {"id": "find", "sym_command": "legion sym find-content",
             "interpreter_patterns": ["rglob", "read_text"]}
        ]}"#,
        );
        let region = Unreduced {
            text: "open('one.txt').read_text()".to_string(),
            reason: UnreducedReason::InterpreterBody,
            depth: 1,
        };
        let outcome = decide_region(&p, &region);
        assert!(!outcome.is_sym);
        assert_eq!(
            outcome.decision,
            Decision::Proxy {
                reason: ProxyReason::Opaque
            }
        );
    }

    #[test]
    fn every_unreduced_reason_that_is_not_a_sym_job_is_proxied_opaque() {
        let p = policy("{}");
        for reason in [
            UnreducedReason::WrapperPayload,
            UnreducedReason::ScriptFile,
            UnreducedReason::InterpreterBody,
            UnreducedReason::DynamicName,
            UnreducedReason::TooDeep,
            UnreducedReason::Unparsed,
        ] {
            let region = Unreduced {
                text: "whatever".to_string(),
                reason,
                depth: 0,
            };
            assert_eq!(
                decide_region(&p, &region).decision,
                Decision::Proxy {
                    reason: ProxyReason::Opaque
                },
                "reason {reason:?} must proxy opaque"
            );
        }
    }

    #[test]
    fn combine_of_no_parts_allows() {
        assert_eq!(combine(vec![]).decision, Decision::Allow { note: None });
    }

    #[test]
    fn sym_action_on_a_visible_command_routes_to_sym() {
        let p = policy(
            r#"{
            "sym_jobs": [{"id": "find", "sym_command": "legion sym find-content", "interpreter_patterns": ["rglob"]}],
            "tools": {"Bash": {"families": {"grep": {"rules": [
                {"id": "grep-search", "outcome": {"kind": "sym", "job": "find"}}
            ]}}}}
        }"#,
        );
        let outcome = decide_bash_invocation(
            &p,
            "grep",
            &args(&["-rn", "foo", "src"]),
            &Context::default(),
        );
        assert!(outcome.is_sym);
        match outcome.decision {
            Decision::Deny(details) => assert_eq!(details.instead(), "legion sym find-content"),
            other => panic!("expected deny naming sym, got {other:?}"),
        }
    }
}
