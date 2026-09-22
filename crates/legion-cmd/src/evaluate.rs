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

use serde_json::Value;

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
    let Some((verb, rule)) = select_bash_rule(policy, binary, args) else {
        return allow_default();
    };

    match rule {
        Some(rule) => resolve_rule(policy, rule, ctx, Some(verb)),
        // The family is managed, but no rule resolves these arguments.
        None => PartOutcome {
            decision: deny(
                "this managed command matches no policy rule",
                "run it manually, or add a rule that covers it",
            ),
            deciding: Deciding::Default,
            is_sym: false,
            verb: Some(verb),
        },
    }
}

/// The rule that governs one Bash invocation, with the verb its family names.
/// `None` when no family matches the binary (an unmanaged command);
/// `Some((verb, None))` when a family matches but no rule in it resolves the
/// arguments. The one rule-selection step for Bash, shared by
/// [`decide_bash_invocation`] and the lookup pre-pass ([`crate::lookups`]) so
/// the two cannot disagree about which rule applies.
pub(crate) fn select_bash_rule<'a>(
    policy: &'a Policy,
    binary: &str,
    args: &[String],
) -> Option<(String, Option<&'a Rule>)> {
    let ToolRules::Bash { families } = policy.tools.get(&ToolKind::Bash)? else {
        return None;
    };
    let (verb, family) = most_specific_family(families, binary, args)?;
    let rule = family
        .rules
        .iter()
        .find(|rule| predicates_hold(&rule.predicates, args));
    Some((verb, rule))
}

/// Which rule governs one Fields-tool call, walking the kind's rules in order
/// against the call's own `tool_input`.
pub(crate) enum FieldsSelection<'a> {
    /// A rule's field predicates hold; its outcome decides the call.
    Rule(&'a Rule),
    /// A rule reads a field the call does not carry as a usable value. The
    /// walk stops here: the managed rule cannot resolve (FR-CMD-016).
    Unresolvable { rule: &'a Rule, field: String },
    /// No rule under this kind applies to the call.
    NoMatch,
}

/// The one rule-selection step for Fields tools, shared by [`decide_fields`]
/// and the lookup pre-pass ([`crate::lookups`]) so the two cannot disagree
/// about which rule applies.
pub(crate) fn select_fields_rule<'a>(
    policy: &'a Policy,
    kind: ToolKind,
    input: &Value,
) -> FieldsSelection<'a> {
    let Some(ToolRules::Fields { rules }) = policy.tools.get(&kind) else {
        return FieldsSelection::NoMatch;
    };
    for rule in rules {
        match field_predicates_hold(&rule.predicates, input) {
            FieldMatch::Holds => return FieldsSelection::Rule(rule),
            FieldMatch::Fails => {}
            FieldMatch::Unresolvable { field } => {
                return FieldsSelection::Unresolvable { rule, field };
            }
        }
    }
    FieldsSelection::NoMatch
}

/// Decides one Fields-tool call (every tool kind but Bash), matching each
/// rule's field predicates against the call's own `tool_input` in order
/// (FR-CMD-011, FR-CMD-016).
///
/// No rule matches -> allow (nothing managed applies to this call). A rule
/// reads a field the call does not carry as a usable value -> deny, naming the
/// rule and the field: the managed rule cannot resolve, and FR-CMD-016 fails
/// closed rather than guessing. A rule matches -> its outcome, through the
/// same lookup gates as a Bash rule.
pub fn decide_fields(policy: &Policy, kind: ToolKind, input: &Value, ctx: &Context) -> PartOutcome {
    match select_fields_rule(policy, kind, input) {
        FieldsSelection::Rule(rule) => resolve_rule(policy, rule, ctx, None),
        FieldsSelection::Unresolvable { rule, field } => {
            let tool = kind.as_str();
            rule_outcome(
                rule,
                deny(
                    format!(
                        "this {tool} call carries no usable '{field}' field, which rule '{}' reads",
                        rule.id
                    ),
                    format!("retry the {tool} call with '{field}' set"),
                ),
                false,
                None,
            )
        }
        FieldsSelection::NoMatch => allow_default(),
    }
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
        // A field predicate has no Bash argument to resolve against, and the
        // parser rejects one under Bash; should one ever arrive, its rule
        // never fires rather than firing on nothing.
        Predicate::FieldPresent { .. }
        | Predicate::FieldAbsent { .. }
        | Predicate::FieldEquals { .. }
        | Predicate::FieldContains { .. }
        | Predicate::FieldEndsWith { .. }
        | Predicate::FieldGreaterThan { .. } => false,
    })
}

/// How a Fields rule's predicates resolved against one call.
#[derive(Debug, PartialEq, Eq)]
enum FieldMatch {
    Holds,
    Fails,
    /// A predicate read `field`, and the call carries no such field, carries
    /// it as null, or carries it with a JSON type the predicate cannot read.
    Unresolvable {
        field: String,
    },
}

/// All predicates in order; the first that fails or cannot resolve decides.
fn field_predicates_hold(predicates: &[Predicate], input: &Value) -> FieldMatch {
    for predicate in predicates {
        let result = field_predicate_holds(predicate, input);
        if result != FieldMatch::Holds {
            return result;
        }
    }
    FieldMatch::Holds
}

fn field_predicate_holds(predicate: &Predicate, input: &Value) -> FieldMatch {
    // A top-level key of `tool_input`; an explicit null is treated as absent,
    // which is how the harness sends an omitted optional field.
    let value_of = |field: &str| input.get(field).filter(|v| !v.is_null());
    let string_of = |field: &str, test: &dyn Fn(&str) -> bool| match value_of(field) {
        Some(Value::String(s)) => held(test(s)),
        _ => FieldMatch::Unresolvable {
            field: field.to_string(),
        },
    };

    match predicate {
        // Parse-time scoping keeps arg predicates out of Fields rules; a rule
        // carrying one would otherwise fire on nothing.
        Predicate::ArgPresent(_) | Predicate::ArgAbsent(_) => FieldMatch::Fails,
        Predicate::FieldPresent { field } => held(value_of(field).is_some()),
        Predicate::FieldAbsent { field } => held(value_of(field).is_none()),
        Predicate::FieldEquals {
            field,
            any_of,
            ignore_case,
        } => string_of(field, &|s| {
            any_of.iter().any(|want| {
                if *ignore_case {
                    s.eq_ignore_ascii_case(want)
                } else {
                    s == want
                }
            })
        }),
        Predicate::FieldContains { field, any_of } => string_of(field, &|s| {
            any_of.iter().any(|want| s.contains(want.as_str()))
        }),
        Predicate::FieldEndsWith { field, any_of } => string_of(field, &|s| {
            any_of.iter().any(|want| s.ends_with(want.as_str()))
        }),
        Predicate::FieldGreaterThan { field, value } => {
            match value_of(field).and_then(Value::as_i64) {
                Some(n) => held(n > *value),
                None => FieldMatch::Unresolvable {
                    field: field.clone(),
                },
            }
        }
    }
}

fn held(holds: bool) -> FieldMatch {
    if holds {
        FieldMatch::Holds
    } else {
        FieldMatch::Fails
    }
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

    // -- Fields tools -----------------------------------------------------

    fn json(text: &str) -> Value {
        serde_json::from_str(text).expect("valid json")
    }

    fn deny_reason(outcome: &PartOutcome) -> &str {
        match &outcome.decision {
            Decision::Deny(details) => details.reason(),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn a_fields_rule_matches_on_its_named_field_case_insensitively() {
        let p = policy(
            r#"{"tools": {"Agent": {"rules": [
            {"id": "explore", "predicates": [{"kind": "field-equals", "field": "subagent_type", "any_of": ["explore"], "ignore_case": true}],
             "outcome": {"kind": "rewrite", "target": "legion:legion-explore", "reason": "r"}}
        ]}}}"#,
        );
        let ctx = Context::default();
        for spelling in ["Explore", "explore", "EXPLORE"] {
            let input = json(&format!(
                r#"{{"subagent_type": "{spelling}", "prompt": "p"}}"#
            ));
            let outcome = decide_fields(&p, ToolKind::Agent, &input, &ctx);
            assert!(
                matches!(outcome.decision, Decision::Rewrite { .. }),
                "{spelling} must rewrite"
            );
            assert_eq!(
                outcome.deciding,
                Deciding::Rule {
                    id: "explore".to_string(),
                    needs_operator: false
                }
            );
        }
        // Exact match, not substring: the redirect target and a lookalike
        // pass through to the allow default.
        for other in [
            "legion-explore",
            "legion:legion-explore",
            "code-explorer",
            "Plan",
        ] {
            let input = json(&format!(r#"{{"subagent_type": "{other}"}}"#));
            let outcome = decide_fields(&p, ToolKind::Agent, &input, &ctx);
            assert_eq!(outcome.decision, Decision::Allow { note: None }, "{other}");
            assert_eq!(outcome.deciding, Deciding::Default);
        }
    }

    #[test]
    fn a_fields_rule_reading_a_missing_field_denies_naming_the_field() {
        // FR-CMD-016: the managed rule cannot resolve, so the call is denied
        // rather than falling through to a later rule or the allow default.
        let p = policy(
            r#"{"tools": {"Write": {"rules": [
            {"id": "memory", "predicates": [{"kind": "field-contains", "field": "file_path", "any_of": ["/memory/"]}],
             "outcome": {"kind": "deny", "reason": "r", "instead": "i"}},
            {"id": "anything", "outcome": {"kind": "allow"}}
        ]}}}"#,
        );
        let outcome = decide_fields(
            &p,
            ToolKind::Write,
            &json(r#"{"content": "hello"}"#),
            &Context::default(),
        );
        assert!(deny_reason(&outcome).contains("'file_path'"));
        assert!(deny_reason(&outcome).contains("'memory'"));
        assert_eq!(
            outcome.deciding,
            Deciding::Rule {
                id: "memory".to_string(),
                needs_operator: false
            }
        );
    }

    #[test]
    fn a_null_field_is_absent_and_a_wrong_typed_field_is_unresolvable() {
        let p = policy(
            r#"{"tools": {"Read": {"rules": [
            {"id": "big", "predicates": [{"kind": "field-greater-than", "field": "limit", "value": 500}],
             "outcome": {"kind": "deny", "reason": "r", "instead": "i"}}
        ]}}}"#,
        );
        let ctx = Context::default();
        // null reads as absent -> unresolvable for a reading predicate.
        let outcome = decide_fields(&p, ToolKind::Read, &json(r#"{"limit": null}"#), &ctx);
        assert!(deny_reason(&outcome).contains("'limit'"));
        // a string where an integer is read -> unresolvable too.
        let outcome = decide_fields(&p, ToolKind::Read, &json(r#"{"limit": "600"}"#), &ctx);
        assert!(deny_reason(&outcome).contains("'limit'"));
        // an integer resolves: 600 > 500 denies by the rule's own outcome.
        let outcome = decide_fields(&p, ToolKind::Read, &json(r#"{"limit": 600}"#), &ctx);
        assert_eq!(deny_reason(&outcome), "r");
        // 200 is not > 500 -> no match -> allow default.
        let outcome = decide_fields(&p, ToolKind::Read, &json(r#"{"limit": 200}"#), &ctx);
        assert_eq!(outcome.decision, Decision::Allow { note: None });
    }

    #[test]
    fn a_presence_rule_ordered_first_lets_a_legitimately_missing_field_through() {
        // The Read shape: no limit is the ordinary case and must reach its own
        // rule (here: deny as unbounded) before any rule that reads `limit`
        // would find it missing and deny for the wrong reason.
        let p = policy(
            r#"{"tools": {"Read": {"rules": [
            {"id": "unbounded", "predicates": [
                {"kind": "field-ends-with", "field": "file_path", "any_of": [".rs", ".py"]},
                {"kind": "field-absent", "field": "limit"}],
             "outcome": {"kind": "deny", "reason": "unbounded", "instead": "i"}},
            {"id": "oversized", "predicates": [
                {"kind": "field-ends-with", "field": "file_path", "any_of": [".rs", ".py"]},
                {"kind": "field-greater-than", "field": "limit", "value": 500}],
             "outcome": {"kind": "deny", "reason": "oversized", "instead": "i"}}
        ]}}}"#,
        );
        let ctx = Context::default();
        let unbounded = decide_fields(
            &p,
            ToolKind::Read,
            &json(r#"{"file_path": "src/main.rs"}"#),
            &ctx,
        );
        assert_eq!(deny_reason(&unbounded), "unbounded");
        let oversized = decide_fields(
            &p,
            ToolKind::Read,
            &json(r#"{"file_path": "src/main.rs", "limit": 2000}"#),
            &ctx,
        );
        assert_eq!(deny_reason(&oversized), "oversized");
        let bounded = decide_fields(
            &p,
            ToolKind::Read,
            &json(r#"{"file_path": "src/main.rs", "limit": 200}"#),
            &ctx,
        );
        assert_eq!(bounded.decision, Decision::Allow { note: None });
        // Not a source file: neither rule's suffix predicate holds, and the
        // absent `limit` is never read -> allow default.
        let prose = decide_fields(
            &p,
            ToolKind::Read,
            &json(r#"{"file_path": "README.md"}"#),
            &ctx,
        );
        assert_eq!(prose.decision, Decision::Allow { note: None });
    }

    #[test]
    fn field_contains_is_a_conjunction_across_predicates_and_a_disjunction_within() {
        let p = policy(
            r#"{"tools": {"Write": {"rules": [
            {"id": "memory", "predicates": [
                {"kind": "field-contains", "field": "file_path", "any_of": [".claude/projects/"]},
                {"kind": "field-contains", "field": "file_path", "any_of": ["/memory/", "/MEMORY/"]}],
             "outcome": {"kind": "deny", "reason": "r", "instead": "i"}}
        ]}}}"#,
        );
        let ctx = Context::default();
        let denied = decide_fields(
            &p,
            ToolKind::Write,
            &json(r#"{"file_path": "/h/.claude/projects/x/memory/MEMORY.md", "content": "c"}"#),
            &ctx,
        );
        assert!(matches!(denied.decision, Decision::Deny(_)));
        // One of the two conjuncts missing -> the rule does not match.
        let allowed = decide_fields(
            &p,
            ToolKind::Write,
            &json(r#"{"file_path": "/repo/src/memory/notes.md", "content": "c"}"#),
            &ctx,
        );
        assert_eq!(allowed.decision, Decision::Allow { note: None });
    }

    #[test]
    fn a_fields_rule_routes_to_a_sym_job() {
        let p = policy(
            r#"{
            "sym_jobs": [{"id": "find-file", "sym_command": "legion sym etc find-file", "interpreter_patterns": []}],
            "tools": {"Glob": {"rules": [{"id": "glob", "outcome": {"kind": "sym", "job": "find-file"}}]}}
        }"#,
        );
        let outcome = decide_fields(
            &p,
            ToolKind::Glob,
            &json(r#"{"pattern": "**/*.rs"}"#),
            &Context::default(),
        );
        assert!(outcome.is_sym);
        match outcome.decision {
            Decision::Deny(details) => assert_eq!(details.instead(), "legion sym etc find-file"),
            other => panic!("expected deny naming sym, got {other:?}"),
        }
    }

    #[test]
    fn a_fields_rule_requiring_recall_is_gated_like_a_bash_rule() {
        let p = policy(
            r#"{"tools": {"WebFetch": {"rules": [
            {"id": "recall-first", "requires_recall": true, "outcome": {"kind": "allow", "note": "read the hits"}}
        ]}}}"#,
        );
        let input = json(r#"{"url": "https://example.com", "prompt": "how does sync work"}"#);
        assert!(matches!(
            decide_fields(&p, ToolKind::WebFetch, &input, &Context::default()).decision,
            Decision::Deny(_)
        ));
        let ctx = Context {
            recall: Lookup::Empty,
            ..Context::default()
        };
        assert_eq!(
            decide_fields(&p, ToolKind::WebFetch, &input, &ctx).decision,
            Decision::Allow {
                note: Some("read the hits".to_string())
            }
        );
    }

    #[test]
    fn a_fields_tool_with_no_rules_entry_allows() {
        let p = policy(
            r#"{"tools": {"Grep": {"rules": [{"id": "g", "outcome": {"kind": "allow"}}]}}}"#,
        );
        let outcome = decide_fields(
            &p,
            ToolKind::WebSearch,
            &json(r#"{"query": "q"}"#),
            &Context::default(),
        );
        assert_eq!(outcome.decision, Decision::Allow { note: None });
        assert_eq!(outcome.deciding, Deciding::Default);
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
