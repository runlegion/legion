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

use crate::decision::{Deciding, Decision, ManagedTarget, ProxyReason};
use crate::policy::{
    ArgSpec, FallbackDecision, Family, OperandShape, Policy, Predicate, Rule, RuleOutcome,
    ToolKind, ToolRules,
};
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
/// rule matches and its lookups are satisfied -> the rule's outcome. A
/// rewrite rule's outcome is judged from the arguments beyond the family's
/// subcommand words (FR-CMD-008): the rewrite when all of them translate,
/// the rule's fallback otherwise.
pub fn decide_bash_invocation(
    policy: &Policy,
    binary: &str,
    args: &[String],
    ctx: &Context,
) -> PartOutcome {
    let Some(selection) = select_bash_rule(policy, binary, args) else {
        return allow_default();
    };

    match selection.rule {
        // The family's own subcommand words are what the target replaces;
        // every other argument must translate for a rewrite to fire. The
        // arguments go through whole, with the governed positions beside them,
        // so a valued flag is judged against its true adjacent argument.
        Some(rule) => resolve_rule(
            policy,
            rule,
            ctx,
            Some(selection.verb),
            args,
            &selection.governed,
        ),
        // The family is managed, but no rule resolves these arguments.
        None => PartOutcome {
            decision: deny(
                "this managed command matches no policy rule",
                "run it manually, or add a rule that covers it",
            ),
            deciding: Deciding::Default,
            is_sym: false,
            verb: Some(selection.verb),
        },
    }
}

/// The result of selecting the rule for one Bash invocation.
pub(crate) struct BashSelection<'a> {
    /// The verb the matched family names.
    pub(crate) verb: String,
    /// The first rule whose predicates hold; `None` when none does.
    pub(crate) rule: Option<&'a Rule>,
    /// Indices into the invocation's arguments of the family's subcommand
    /// words, as the family match consumed them. A rewrite's coverage check
    /// excludes exactly these positions, so a later argument that happens to
    /// repeat a subcommand word is still judged.
    pub(crate) governed: Vec<usize>,
}

/// The rule that governs one Bash invocation, with the verb its family names.
/// `None` when no family matches the binary (an unmanaged command); a
/// selection whose `rule` is `None` when a family matches but no rule in it
/// resolves the arguments. The one rule-selection step for Bash, shared by
/// [`decide_bash_invocation`] and the lookup pre-pass ([`crate::lookups`]) so
/// the two cannot disagree about which rule applies.
pub(crate) fn select_bash_rule<'a>(
    policy: &'a Policy,
    binary: &str,
    args: &[String],
) -> Option<BashSelection<'a>> {
    let ToolRules::Bash { families } = policy.tools.get(&ToolKind::Bash)? else {
        return None;
    };
    let (verb, family, governed) = most_specific_family(families, binary, args)?;
    let rule = family
        .rules
        .iter()
        .find(|rule| predicates_hold(&rule.predicates, args));
    Some(BashSelection {
        verb,
        rule,
        governed,
    })
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
        // A Fields call has no argument list: a rewrite here patches one
        // field and keeps the rest, lossless by construction.
        FieldsSelection::Rule(rule) => resolve_rule(policy, rule, ctx, None, &[], &[]),
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
///
/// A rewrite replaces the whole command, so it is kept only when the command
/// is that one part. When a rewrite wins a fold of more than one part, the
/// command is refused instead (FR-CMD-008: nothing is silently dropped): the
/// deny names the rewrite rule and says the rest of the command would have
/// been dropped. The count is of route's own parts, so no caller scans the
/// command to tell a compound from a single invocation (FR-CMD-017).
pub fn combine(parts: Vec<PartOutcome>) -> PartOutcome {
    if let Some(sym) = parts.iter().find(|p| p.is_sym) {
        return sym.clone();
    }
    let compound = parts.len() > 1;
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
    match winner {
        Some(part) if compound => refuse_compound_rewrite(part),
        Some(part) => part,
        None => allow_default(),
    }
}

/// Turns a rewrite that won a compound command's fold into a deny naming the
/// rule, keeping the part's `deciding` and `verb`; any other outcome is
/// returned unchanged.
fn refuse_compound_rewrite(part: PartOutcome) -> PartOutcome {
    let Decision::Rewrite { target, .. } = &part.decision else {
        return part;
    };
    let rule = match &part.deciding {
        Deciding::Rule { id, .. } => format!("rule '{id}'"),
        _ => "a rewrite rule".to_string(),
    };
    let decision = deny(
        format!(
            "{rule} rewrites one command in this compound command to `{}`; replacing the \
             command would drop the rest of it, so it is not rewritten",
            target.as_str()
        ),
        format!(
            "run `{}` as its own command, and the other commands separately",
            target.as_str()
        ),
    );
    PartOutcome { decision, ..part }
}

/// Resolves a matched rule into a [`PartOutcome`], applying the lookup gates
/// (FR-CMD-016), the sym action (FR-CMD-007), and a rewrite's argument
/// coverage (FR-CMD-008). `args` are the invocation's arguments and
/// `governed` the indices of the family's subcommand words among them -- both
/// empty for a Fields call.
fn resolve_rule(
    policy: &Policy,
    rule: &Rule,
    ctx: &Context,
    verb: Option<String>,
    args: &[String],
    governed: &[usize],
) -> PartOutcome {
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
        RuleOutcome::Rewrite { spec, reason } => {
            let decision = match first_untranslatable(&spec.translatable, args, governed) {
                None => Decision::Rewrite {
                    target: spec.target.clone(),
                    reason: reason.clone(),
                },
                Some(arg) => fallback(&spec.otherwise, arg, &spec.target),
            };
            (decision, false)
        }
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

/// The first argument `spec` does not cover, as typed, or `None` when every
/// argument translates (FR-CMD-008). Naming the argument, rather than
/// answering yes or no, is what lets the default fallback's deny tell the
/// agent exactly what the target cannot carry (FR-CMD-005).
///
/// The family's subcommand words, at the `governed` indices, are skipped in
/// place: the target replaces them, so they need not translate, but they keep
/// their positions so no argument is paired with a word it was never adjacent
/// to.
///
/// Every word starting with `-` is an option word and must be declared
/// verbatim: a combined short-option cluster (`-rn`) or `--` is
/// untranslatable unless the spec lists it, because splitting or
/// reinterpreting it needs per-binary knowledge the spec does not carry. A
/// bare `-` is always untranslatable: the spec parser rejects option words
/// shorter than two characters, so it can never be declared. A valued flag
/// consumes its true next argument as its value, or carries it joined with
/// `=`; a valued flag with no value left, or whose next argument is one of the
/// family's subcommand words, is untranslatable. Every other word is an
/// operand, matched against the declared shapes by position.
fn first_untranslatable<'a>(
    spec: &ArgSpec,
    args: &'a [String],
    governed: &[usize],
) -> Option<&'a str> {
    let declared = |list: &[String], word: &str| list.iter().any(|f| f == word);
    let mut operand_position = 0;
    let mut index = 0;
    while index < args.len() {
        let current = index;
        index += 1;
        if governed.contains(&current) {
            continue;
        }
        let raw = args[current].as_str();
        let arg = dequote_outer(raw);
        if arg.starts_with('-') {
            if declared(&spec.flags, arg) {
                continue;
            }
            if declared(&spec.valued_flags, arg) {
                // The value is the adjacent argument or nothing: a subcommand
                // word there is not a value, and a later word is not adjacent.
                if index >= args.len() || governed.contains(&index) {
                    return Some(raw);
                }
                index += 1;
                continue;
            }
            if let Some((name, _)) = arg.split_once('=')
                && declared(&spec.valued_flags, name)
            {
                continue;
            }
            return Some(raw);
        }
        let fits = match spec.operands.get(operand_position) {
            Some(OperandShape::Any) => true,
            Some(OperandShape::Integer) => {
                !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit())
            }
            None => false,
        };
        if !fits {
            return Some(raw);
        }
        operand_position += 1;
    }
    None
}

/// The Decision a rewrite rule yields when `arg` does not translate to
/// `target` (FR-CMD-008). The default names both, so the agent learns which
/// argument blocked the rewrite and which managed command to run instead
/// (FR-CMD-005). A declared ask never carries the rule's operator mark into
/// route's output (FR-CMD-006).
fn fallback(otherwise: &FallbackDecision, arg: &str, target: &ManagedTarget) -> Decision {
    match otherwise {
        FallbackDecision::DenyNamingTarget => deny(
            format!(
                "`{arg}` has no lossless translation to `{}`, so this command is not rewritten",
                target.as_str()
            ),
            target.as_str(),
        ),
        FallbackDecision::Allow { note } => Decision::Allow { note: note.clone() },
        FallbackDecision::Proxy { reason } => Decision::Proxy { reason: *reason },
        FallbackDecision::Ask {
            question, reason, ..
        } => ask(question, reason),
    }
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
        // Every sym job yields the deny naming the sym command (FR-CMD-007).
        // The argument-coverage rewrite (FR-CMD-008, #1228) is built for
        // rewrite rules only; a lossless rewrite TO the sym command (operator
        // decision 01a0ab48) is still open -- no issue builds it yet.
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
/// verb it names and the argument indices its subcommand words matched. A
/// family key is whitespace-separated: the first token is the
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
) -> Option<(String, &'a Family, Vec<usize>)> {
    // Each operand with its index in `args`, so the matched subcommand words
    // can be reported by position.
    let operands: Vec<(usize, &str)> = args
        .iter()
        .map(|a| dequote_outer(a))
        .enumerate()
        .filter(|(_, a)| !a.starts_with('-'))
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
            .all(|(want, (_, have))| want == have)
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
    best.map(|(specificity, verb, family)| {
        let governed: Vec<usize> = operands
            .iter()
            .take(specificity)
            .map(|(index, _)| *index)
            .collect();
        (verb, family, governed)
    })
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
                "b": {"rules": [{"id": "rb", "outcome": {"kind": "rewrite", "target": "legion x", "reason": "r", "translatable": {}}}]},
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

    // -- rewrite eligibility is a function of the arguments (FR-CMD-008) -----

    /// `gh pr view [N] [--comments] [--json F]` translates to `legion pr view`;
    /// anything else it carries does not.
    const PR_VIEW: &str = r#"{"tools": {"Bash": {"families": {"gh pr view": {"rules": [
        {"id": "pr-view", "outcome": {"kind": "rewrite", "target": "legion pr view",
         "reason": "legion tracks PRs",
         "translatable": {"flags": ["--comments"], "valued_flags": ["--json"], "operands": ["integer"]}}}
    ]}}}}}"#;

    fn decide(p: &Policy, binary: &str, words: &[&str]) -> PartOutcome {
        decide_bash_invocation(p, binary, &args(words), &Context::default())
    }

    fn rewrite_target(outcome: &PartOutcome) -> &str {
        match &outcome.decision {
            Decision::Rewrite { target, .. } => target.as_str(),
            other => panic!("expected rewrite, got {other:?}"),
        }
    }

    #[test]
    fn one_verb_is_rewritten_for_translatable_arguments_and_denied_for_an_untranslatable_flag() {
        // The FR-CMD-008 pair: same binary, same verb, different arguments,
        // different Decisions.
        let p = policy(PR_VIEW);
        let translatable = decide(&p, "gh", &["pr", "view", "12"]);
        let untranslatable = decide(&p, "gh", &["pr", "view", "12", "--web"]);

        assert_eq!(translatable.verb.as_deref(), Some("pr view"));
        assert_eq!(
            translatable.verb, untranslatable.verb,
            "the pair shares a verb"
        );

        assert_eq!(rewrite_target(&translatable), "legion pr view");
        match &untranslatable.decision {
            Decision::Deny(details) => {
                assert!(details.reason().contains("`--web`"), "{}", details.reason());
                assert!(details.reason().contains("`legion pr view`"));
                assert_eq!(details.instead(), "legion pr view");
            }
            other => panic!("expected the default deny, got {other:?}"),
        }
        // Both are decided by the same rule; only the arguments differ.
        assert_eq!(translatable.deciding, untranslatable.deciding);
    }

    #[test]
    fn one_verb_is_rewritten_for_an_integer_operand_and_denied_for_a_word_operand() {
        let p = policy(PR_VIEW);
        assert_eq!(
            rewrite_target(&decide(&p, "gh", &["pr", "view", "12"])),
            "legion pr view"
        );
        let word = decide(&p, "gh", &["pr", "view", "my-branch"]);
        assert!(deny_reason(&word).contains("`my-branch`"));
    }

    #[test]
    fn one_verb_is_rewritten_within_its_operand_count_and_denied_beyond_it() {
        let p = policy(PR_VIEW);
        // Fewer operands than declared is still covered: nothing is dropped.
        assert_eq!(
            rewrite_target(&decide(&p, "gh", &["pr", "view"])),
            "legion pr view"
        );
        let extra = decide(&p, "gh", &["pr", "view", "12", "13"]);
        assert!(deny_reason(&extra).contains("`13`"));
    }

    #[test]
    fn a_valued_flag_translates_in_both_spellings_and_needs_its_value() {
        let p = policy(PR_VIEW);
        for words in [
            vec!["pr", "view", "12", "--json", "title"],
            vec!["pr", "view", "--json=title", "12"],
            vec!["pr", "view", "--comments", "12"],
        ] {
            assert_eq!(
                rewrite_target(&decide(&p, "gh", &words)),
                "legion pr view",
                "{words:?}"
            );
        }
        // `--json` as the last word has no value to translate.
        let dangling = decide(&p, "gh", &["pr", "view", "12", "--json"]);
        assert!(deny_reason(&dangling).contains("`--json`"));
        // Between two of the family's words, `--json`'s adjacent argument is a
        // subcommand word, not a value: the trailing operand is not swallowed.
        let mid_family = decide(&p, "gh", &["pr", "--json", "view", "my-branch"]);
        assert!(
            deny_reason(&mid_family).contains("`--json`"),
            "{:?}",
            mid_family.decision
        );
        let mid_family_integer = decide(&p, "gh", &["pr", "--json", "view", "12"]);
        assert!(deny_reason(&mid_family_integer).contains("`--json`"));
        // Before the whole subcommand sequence, the same holds.
        let leading = decide(&p, "gh", &["--json", "pr", "view", "12"]);
        assert!(
            deny_reason(&leading).contains("`--json`"),
            "{:?}",
            leading.decision
        );
        // `=` joined to a switch that takes no value is not the switch.
        let joined = decide(&p, "gh", &["pr", "view", "--comments=yes"]);
        assert!(deny_reason(&joined).contains("`--comments=yes`"));
    }

    #[test]
    fn option_words_the_spec_does_not_list_verbatim_are_untranslatable() {
        // A combined cluster, the end-of-options marker and a bare `-` would
        // each need per-binary knowledge to reinterpret: fail closed.
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"grep": {"rules": [
            {"id": "g", "outcome": {"kind": "rewrite", "target": "legion sym etc find-content",
             "reason": "r", "translatable": {"flags": ["-r", "-n"], "operands": ["any"]}}}
        ]}}}}}"#,
        );
        assert_eq!(
            rewrite_target(&decide(&p, "grep", &["-r", "-n", "foo"])),
            "legion sym etc find-content"
        );
        for (words, named) in [
            (vec!["-rn", "foo"], "`-rn`"),
            (vec!["-r", "--", "foo"], "`--`"),
            (vec!["-r", "-"], "`-`"),
        ] {
            let outcome = decide(&p, "grep", &words);
            assert!(deny_reason(&outcome).contains(named), "{words:?}");
        }
    }

    #[test]
    fn the_family_subcommand_words_are_governed_but_a_repeat_of_one_is_judged() {
        // `issue` and `list` are the family's words and translate by being the
        // target; a later `issue` is an operand the empty spec does not cover.
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"gh issue list": {"rules": [
            {"id": "l", "outcome": {"kind": "rewrite", "target": "legion issue list",
             "reason": "r", "translatable": {}}}
        ]}}}}}"#,
        );
        assert_eq!(
            rewrite_target(&decide(&p, "gh", &["issue", "list"])),
            "legion issue list"
        );
        let repeated = decide(&p, "gh", &["issue", "list", "issue"]);
        assert!(deny_reason(&repeated).contains("`issue`"));
        // A global option before the subcommand is still an argument: it is
        // not one of the family's words, so it must translate too.
        let global = decide(&p, "gh", &["--no-pager", "issue", "list"]);
        assert!(deny_reason(&global).contains("`--no-pager`"));
    }

    #[test]
    fn quoted_arguments_are_judged_by_the_value_the_shell_sees() {
        let p = policy(PR_VIEW);
        assert_eq!(
            rewrite_target(&decide(&p, "gh", &["pr", "view", "\"12\"", "'--comments'"])),
            "legion pr view"
        );
        let quoted = decide(&p, "gh", &["pr", "view", "'--web'"]);
        assert!(
            deny_reason(&quoted).contains("`'--web'`"),
            "names it as typed"
        );
    }

    #[test]
    fn a_declared_fallback_arm_applies_instead_of_the_default_deny() {
        // Same verb, different arguments: rewrite for the translatable ones,
        // the rule's declared `otherwise` (here: proxy) for the rest.
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"gh pr diff": {"rules": [
            {"id": "diff", "outcome": {"kind": "rewrite", "target": "legion pr diff",
             "reason": "r", "translatable": {"operands": ["integer"]},
             "otherwise": {"kind": "proxy", "reason": "full-patch"}}}
        ]}}}}}"#,
        );
        assert_eq!(
            rewrite_target(&decide(&p, "gh", &["pr", "diff", "7"])),
            "legion pr diff"
        );
        assert_eq!(
            decide(&p, "gh", &["pr", "diff", "7", "--patch"]).decision,
            Decision::Proxy {
                reason: ProxyReason::FullPatch
            }
        );
    }

    #[test]
    fn a_declared_ask_fallback_never_carries_the_operator_mark() {
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"gh pr merge": {"rules": [
            {"id": "merge", "outcome": {"kind": "rewrite", "target": "legion pr merge",
             "reason": "r", "translatable": {"operands": ["integer"]},
             "otherwise": {"kind": "ask", "question": "merge like this?", "reason": "unusual flags",
                           "needs_operator": true}}}
        ]}}}}}"#,
        );
        let outcome = decide(&p, "gh", &["pr", "merge", "7", "--admin"]);
        assert!(matches!(outcome.decision, Decision::Ask(_)));
        assert_eq!(
            outcome.deciding,
            Deciding::Rule {
                id: "merge".to_string(),
                needs_operator: false
            }
        );
    }

    #[test]
    fn a_fields_rewrite_needs_no_translatable_declaration() {
        // The shipped Explore rewrite's shape: lossless by construction.
        let p = policy(
            r#"{"tools": {"Task": {"rules": [
            {"id": "explore", "predicates": [{"kind": "field-equals", "field": "subagent_type", "any_of": ["explore"], "ignore_case": true}],
             "outcome": {"kind": "rewrite", "target": "legion:legion-explore", "reason": "r"}}
        ]}}}"#,
        );
        let outcome = decide_fields(
            &p,
            ToolKind::Task,
            &json(r#"{"subagent_type": "Explore", "prompt": "p", "description": "d"}"#),
            &Context::default(),
        );
        assert_eq!(rewrite_target(&outcome), "legion:legion-explore");
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
