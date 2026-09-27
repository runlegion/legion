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

use std::borrow::Cow;

use serde_json::Value;

use crate::decision::{Deciding, Decision, ProxyReason};
use crate::policy::{
    AliasReading, Family, Policy, Predicate, RewriteSpec, Rule, RuleOutcome, ToolKind, ToolRules,
};
use crate::splitter::{Invocation, Unreduced, UnreducedReason};
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
    /// The matched rule's own operator mark when this part is an ask
    /// (FR-CMD-006). It is not copied into `deciding`: route acts on it only
    /// once a confirmation in `Context` answers the ask (FR-CMD-026).
    pub operator_mark: bool,
    /// The argument words a rewrite carries into its target (FR-CMD-003,
    /// FR-CMD-008): the invocation's words after the family's subcommand
    /// words, in source order, as literal values. Empty for every other
    /// outcome. route returns them as [`crate::Facts::carried`].
    pub carried: Vec<String>,
}

/// Decides one Bash invocation against the policy (FR-CMD-011, FR-CMD-016).
///
/// An option before the subcommand that the binary's declared global options
/// do not name -> proxy opaque (#1294): route cannot say which word is the
/// subcommand, so it neither matches a family nor falls to the allow default.
/// A subcommand word naming an inline alias is decided under two readings,
/// folded by [`combine`] (#1298): the alias's words in its place -- or proxy
/// opaque when route cannot read the alias -- first, then the word as typed.
/// Git never runs an alias that shares a builtin's name, and route cannot
/// tell which names are builtins, so the stricter reading holds.
///
/// For each reading of words: no family matches the binary and its leading
/// operands -> allow (the command carries no managed binary). A family
/// matches but no rule in it resolves the arguments -> deny (a managed rule
/// that cannot resolve). A rule matches but its required recall or consult
/// result is [`Lookup::NotFetched`] -> deny. A rule matches and its lookups
/// are satisfied -> the rule's outcome. A rewrite rule carries the arguments
/// after the family's subcommand words into its target (FR-CMD-008), or
/// denies naming the one it cannot carry (see [`carry`]); whether the target
/// accepts them is the adapter's parse, not this function's.
///
/// `reads_pipe` is the invocation's [`Invocation::reads_pipe`]: a pipe-filter
/// sym job does not claim a stage that reads a pipe (see
/// [`select_bash_rule`]).
pub fn decide_bash_invocation(
    policy: &Policy,
    binary: &str,
    args: &[String],
    reads_pipe: bool,
    ctx: &Context,
) -> PartOutcome {
    combine(
        bash_readings(policy, binary, args)
            .iter()
            .map(|reading| match reading {
                BashReading::Words { args, start } => {
                    decide_bash_reading(policy, binary, args, *start, reads_pipe, ctx)
                }
                BashReading::Opaque => opaque_default(),
            })
            .collect(),
    )
}

/// One reading of a Bash invocation (#1298).
pub(crate) enum BashReading<'a> {
    /// The words route matches families on, with the index where their
    /// subcommand begins.
    Words {
        args: Cow<'a, [String]>,
        start: usize,
    },
    /// Words route cannot read, proxied opaque.
    Opaque,
}

/// Every reading route decides a Bash invocation under. An undeclared global
/// option (#1294) leaves only [`BashReading::Opaque`]. A subcommand word
/// naming an inline alias (#1298) is read first as the alias's expansion, or
/// as opaque when route cannot read the alias, then as typed. Shared by
/// [`decide_bash_invocation`] and the lookup pre-pass ([`crate::lookups`]) so
/// the two read the same commands.
pub(crate) fn bash_readings<'a>(
    policy: &Policy,
    binary: &str,
    args: &'a [String],
) -> Vec<BashReading<'a>> {
    let Some(start) = policy.subcommand_start(binary, args) else {
        return vec![BashReading::Opaque];
    };
    let typed = BashReading::Words {
        args: Cow::Borrowed(args),
        start,
    };
    match policy.inline_alias_reading(binary, args, start) {
        AliasReading::NotAlias => vec![typed],
        AliasReading::Opaque => vec![BashReading::Opaque, typed],
        AliasReading::Expanded {
            args: expanded,
            start: expanded_start,
        } => vec![
            BashReading::Words {
                args: Cow::Owned(expanded),
                start: expanded_start,
            },
            typed,
        ],
    }
}

/// Decides one reading of a Bash invocation: `args` with its subcommand
/// beginning at `start`.
fn decide_bash_reading(
    policy: &Policy,
    binary: &str,
    args: &[String],
    start: usize,
    reads_pipe: bool,
    ctx: &Context,
) -> PartOutcome {
    let Some(selection) = select_bash_rule(policy, binary, args, start, reads_pipe) else {
        return allow_default();
    };

    match selection.rule {
        // The family's own subcommand words are what the target replaces;
        // a rewrite carries the words after them. The arguments go through
        // whole, with the governed positions beside them, so a word before or
        // among the subcommand words is seen and refused, never dropped.
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
            operator_mark: false,
            carried: Vec::new(),
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
    /// words, as the family match consumed them. A rewrite carries the words
    /// after the last of them, so a later argument that happens to repeat a
    /// subcommand word is still carried.
    pub(crate) governed: Vec<usize>,
}

/// The rule that governs one reading of a Bash invocation (see
/// [`bash_readings`]), with the verb its family names; `start` is where the
/// reading's subcommand begins. `None` when no family matches the binary (an
/// unmanaged command); a selection whose `rule` is `None` when a family
/// matches but no rule in it resolves the arguments. The one rule-selection
/// step for Bash, shared by [`decide_bash_invocation`] and the lookup
/// pre-pass ([`crate::lookups`]) so the two cannot disagree about which rule
/// applies.
///
/// When `reads_pipe` is true (the invocation is a pipeline stage after the
/// first), a rule routing to a sym job the policy marks
/// [`crate::SymJob::pipe_filter`] is passed over: the stage is filtering the
/// previous command's output, which sym cannot serve (FR-CMD-007 rev 13).
/// A later rule whose predicates hold still governs; when such a sym rule
/// was the only one that held, the invocation is not managed there and this
/// returns `None`, the no-match default.
pub(crate) fn select_bash_rule<'a>(
    policy: &'a Policy,
    binary: &str,
    args: &[String],
    start: usize,
    reads_pipe: bool,
) -> Option<BashSelection<'a>> {
    let ToolRules::Bash { families } = policy.tools.get(&ToolKind::Bash)? else {
        return None;
    };
    let (verb, family, governed) = most_specific_family(families, binary, args, start)?;
    let mut passed_over = false;
    let rule = family.rules.iter().find(|rule| {
        if !predicates_hold(&rule.predicates, args) {
            return false;
        }
        if reads_pipe && is_pipe_filter_sym(policy, rule) {
            passed_over = true;
            return false;
        }
        true
    });
    if rule.is_none() && passed_over {
        return None;
    }
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
    opaque_default()
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
/// part wins, so this is not made command-faithful; confirmations key on the
/// whole command's `CommandKey`, never on `deciding.id` (#1237). An empty part list is the
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

/// Refuses a rewrite that won a command which is not exactly one simple
/// command (FR-CMD-008). A rewrite replaces the whole command, so keeping it
/// would drop every other command, operator, or construct the line carries;
/// the deny names the rule and says so, keeping the part's `deciding` and
/// `verb`. Any other outcome is returned unchanged.
pub(crate) fn refuse_compound_rewrite(part: PartOutcome) -> PartOutcome {
    let Decision::Rewrite { target, .. } = &part.decision else {
        return part;
    };
    let rule = rewrite_rule_label(&part.deciding);
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

/// How a refused rewrite's deny names the rule that produced it. Every rewrite
/// comes from `resolve_rule`, which always names its rule, so the fallback arm
/// cannot fire today; it keeps the deny readable if a rewrite is ever produced
/// without one.
fn rewrite_rule_label(deciding: &Deciding) -> String {
    match deciding {
        Deciding::Rule { id, .. } => format!("rule '{id}'"),
        _ => "a rewrite rule".to_string(),
    }
}

/// Resolves a matched rule into a [`PartOutcome`], applying the lookup gates
/// (FR-CMD-016), the sym action (FR-CMD-007), and a rewrite's carried words
/// (FR-CMD-008). `args` are the invocation's arguments and `governed` the
/// indices of the family's subcommand words among them -- both empty for a
/// Fields call, which carries nothing.
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

    let mut carried: Vec<String> = Vec::new();
    let (decision, is_sym) = match &rule.outcome {
        RuleOutcome::Allow { note } => (Decision::Allow { note: note.clone() }, false),
        RuleOutcome::Rewrite { spec, reason } => {
            let decision = match carry(spec, args, governed) {
                Ok(words) => {
                    carried = words;
                    Decision::Rewrite {
                        target: spec.target.clone(),
                        reason: reason.clone(),
                    }
                }
                Err(refusal) => refusal,
            };
            (decision, false)
        }
        RuleOutcome::Proxy { reason } => (Decision::Proxy { reason: *reason }, false),
        RuleOutcome::Deny { reason, instead } => (deny(reason, instead), false),
        // The operator mark stays unset here (FR-CMD-006); route sets it only
        // when a confirmation answers the ask (FR-CMD-026).
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

    // The rule's own operator mark, kept on the part for route's
    // confirmation step and never copied into `deciding` here.
    let operator_mark = matches!(
        rule.outcome,
        RuleOutcome::Ask {
            needs_operator: true,
            ..
        }
    ) && matches!(decision, Decision::Ask(_));
    PartOutcome {
        operator_mark,
        carried,
        ..rule_outcome(rule, decision, is_sym, verb)
    }
}

/// The no-go pre-check (FR-CMD-025), run before any other policy entry: the
/// first no-go entry -- built-in, then policy-added -- matching any resolved
/// invocation, as a deny carrying the fixed no-go instead. Only resolved
/// invocations are checked; a command inside an opaque region stays opaque
/// (FR-CMD-007), since route does not guess at what it cannot see.
pub fn decide_no_go(policy: &Policy, invocations: &[Invocation]) -> Option<PartOutcome> {
    let entries = policy.no_go_entries();
    let entry = crate::nogo::first_match(&entries, invocations)?;
    Some(PartOutcome {
        // Infallible: a no-go match is always a deny, never a fallback arm.
        decision: Decision::no_go_entry(&entry.id),
        deciding: Deciding::NoGo {
            id: entry.id.clone(),
        },
        is_sym: false,
        verb: None,
        operator_mark: false,
        carried: Vec::new(),
    })
}

/// The argument words a rewrite carries into its target, or the deny that
/// refuses it naming the one word it cannot carry (FR-CMD-003, FR-CMD-008).
///
/// Carried are the words after the family's subcommand words (the
/// `governed` indices), in source order, each as the literal value the shell
/// passes ([`crate::splitter::literal_word`]). Whether the target accepts
/// them is not judged here: the adapter parses the candidate command with the
/// target's own definition. Three things are judged here, because only the
/// words themselves can show them, and each denies naming the word as typed
/// (FR-CMD-005):
///
/// - a word before or among the subcommand words (a global option such as
///   `git -C <path>`): it is not after the verb, so it is not carried, and
///   the rewrite would drop it;
/// - a word that is not a plain literal (`"$MSG"`, a glob): the value the
///   command receives is decided at run time, so no carried text is it;
/// - a flag the rule's `deny_flags` names, as `--flag` or `--flag=value`,
///   before any `--`: its name collides with a target flag of different
///   meaning.
fn carry(spec: &RewriteSpec, args: &[String], governed: &[usize]) -> Result<Vec<String>, Decision> {
    let target = spec.target.as_str();
    let refuse = |raw: &str, why: &str| {
        deny(
            format!("`{raw}` {why}, so this command is not rewritten to `{target}`"),
            target,
        )
    };
    let verb_end: usize = governed.iter().max().map_or(0, |last| last + 1);
    if let Some(early) = (0..verb_end).find(|index| !governed.contains(index)) {
        return Err(refuse(
            &args[early],
            "comes before the subcommand the rewrite replaces and would be dropped",
        ));
    }
    let mut carried: Vec<String> = Vec::with_capacity(args.len() - verb_end);
    let mut options_ended = false;
    for raw in &args[verb_end..] {
        let Some(word) = crate::splitter::literal_word(raw) else {
            return Err(refuse(
                raw,
                "is not a plain literal word (the shell expands it when the command runs)",
            ));
        };
        if !options_ended && word == "--" {
            options_ended = true;
        }
        let name: &str = match word.strip_prefix("--").and_then(|w| w.split_once('=')) {
            Some((name, _)) => &word[..name.len() + 2],
            None => &word,
        };
        if !options_ended && spec.deny_flags.iter().any(|flag| flag == name) {
            return Err(refuse(
                raw,
                "names a flag the target spells with a different meaning",
            ));
        }
        carried.push(word);
    }
    Ok(carried)
}

/// Refuses a rewrite of an invocation that carries a redirect or an
/// environment-assignment prefix (FR-CMD-008: nothing is silently dropped).
/// A rewrite replaces the whole command with its target, so the redirect or
/// the assignment would not survive it. Both facts come from the
/// [`Invocation`] route already holds -- `redirected` and `assigned`, set on
/// the command itself or inherited from an enclosing group, wrapper or
/// interpreter -- so no caller scans the command for them (FR-CMD-017). The
/// deny keeps the part's `deciding` and `verb`; any other outcome is returned
/// unchanged.
pub fn refuse_rewrite_dropping_shell_words(
    part: PartOutcome,
    invocation: &Invocation,
) -> PartOutcome {
    let Decision::Rewrite { target, .. } = &part.decision else {
        return part;
    };
    let dropped = match (invocation.redirected, invocation.assigned) {
        (true, true) => "its redirect and environment-assignment prefix",
        (true, false) => "its redirect",
        (false, true) => "its environment-assignment prefix",
        (false, false) => return part,
    };
    let rule = rewrite_rule_label(&part.deciding);
    let decision = deny(
        format!(
            "{rule} rewrites this command to `{}`, and {dropped} would have been dropped, \
             so it is not rewritten",
            target.as_str()
        ),
        format!(
            "run `{}` without the redirect or assignment",
            target.as_str()
        ),
    );
    PartOutcome { decision, ..part }
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
        operator_mark: false,
        carried: Vec::new(),
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

/// True when `rule` routes to a sym job the policy marks as a pipe filter.
fn is_pipe_filter_sym(policy: &Policy, rule: &Rule) -> bool {
    match &rule.outcome {
        RuleOutcome::Sym { job } => policy.sym_job(job).is_some_and(|job| job.pipe_filter),
        _ => false,
    }
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
        operator_mark: false,
        carried: Vec::new(),
    }
}

fn allow_default() -> PartOutcome {
    PartOutcome {
        decision: Decision::Allow { note: None },
        deciding: Deciding::Default,
        is_sym: false,
        verb: None,
        operator_mark: false,
        carried: Vec::new(),
    }
}

/// The proxy-opaque part for a command route cannot read (FR-CMD-004): never
/// allowed, recorded coverage-unknown.
fn opaque_default() -> PartOutcome {
    PartOutcome {
        decision: Decision::Proxy {
            reason: ProxyReason::Opaque,
        },
        deciding: Deciding::Default,
        is_sym: false,
        verb: None,
        operator_mark: false,
        carried: Vec::new(),
    }
}

/// The most specific family whose binary and operand words match, with the
/// verb it names and the argument indices its subcommand words matched. A
/// family key is whitespace-separated: the first token is the
/// binary, any further tokens are subcommand words the invocation must carry as
/// its leading operands in order. The verb is those subcommand words, or the
/// binary when the key names none.
///
/// Operands are read from `start`, the index past the binary's declared global
/// options (#1294), so a global option's separate value (`git -C /tmp push`)
/// is consumed with its option and never read as the subcommand. From there,
/// option tokens (those starting with `-`) are removed rather than stopped at,
/// so an option of a binary that declares none (`gh --no-pager issue list`)
/// does not hide its subcommand either. Each argument is dequoted first, so a
/// quoted subcommand (`git "push"`) still matches. The matched indices are
/// indices into the whole of `args`.
fn most_specific_family<'a>(
    families: &'a std::collections::BTreeMap<String, Family>,
    binary: &str,
    args: &[String],
    start: usize,
) -> Option<(String, &'a Family, Vec<usize>)> {
    // Each operand with its index in `args`, so the matched subcommand words
    // can be reported by position.
    let operands: Vec<(usize, &str)> = args
        .iter()
        .map(|a| dequote_outer(a))
        .enumerate()
        .skip(start)
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
        let outcome = decide_bash_invocation(
            &p,
            "git",
            &args(&["stash", "push"]),
            false,
            &Context::default(),
        );
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
        let outcome =
            decide_bash_invocation(&p, "git", &args(&["push"]), false, &Context::default());
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
        let outcome = decide_bash_invocation(&p, "gh", &args(&[]), false, &Context::default());
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
            decide_bash_invocation(&p, "gh", &args(&[]), false, &Context::default()).decision,
            Decision::Deny(_)
        ));
        // Fetched (even Empty) -> the rule's own outcome.
        let ctx = Context {
            consult: Lookup::Empty,
            ..Context::default()
        };
        assert_eq!(
            decide_bash_invocation(&p, "gh", &args(&[]), false, &ctx).decision,
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
            decide_bash_invocation(&p, "git", &args(&["\"push\""]), false, &Context::default())
                .decision,
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
                false,
                &Context::default()
            )
            .decision,
            Decision::Deny(_)
        ));
    }

    #[test]
    fn a_valueless_global_option_before_the_subcommand_does_not_hide_it() {
        // `git --no-pager push` and `git --git-dir=x push` must still match the
        // `git push` family (the H2 unambiguous half), even with no global
        // options declared. A separated option value (`git -C /tmp push`)
        // needs the declaration: see the global-options tests below.
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
                    decide_bash_invocation(
                        &p,
                        "git",
                        &args(&args_list),
                        false,
                        &Context::default()
                    )
                    .decision,
                    Decision::Deny(_)
                ),
                "args {args_list:?} should match git push",
            );
        }
    }

    /// A `git push` deny family and a `git commit` allow family, with git's
    /// global options declared (#1294).
    const GLOBAL_OPTIONS: &str = r#"{
        "global_options": [{"binary": "git",
            "flags": ["--no-pager", "--bare"],
            "value_options": ["-C", "-c", "--git-dir"]}],
        "tools": {"Bash": {"families": {
            "git push": {"rules": [
                {"id": "push", "outcome": {"kind": "deny", "reason": "no push", "instead": "x"}}]},
            "git commit": {"rules": [
                {"id": "commit", "outcome": {"kind": "allow", "note": "committed"}}]}
        }}}
    }"#;

    #[test]
    fn declared_global_options_are_consumed_before_the_subcommand_is_matched() {
        let p = policy(GLOBAL_OPTIONS);
        for words in [
            vec!["-C", "/tmp", "push"],
            vec!["-C", "push", "push"],
            vec!["--git-dir", ".git", "push"],
            vec!["--git-dir=.git", "push"],
            vec!["--no-pager", "-C", "/tmp", "push", "origin"],
        ] {
            let outcome = decide(&p, "git", &words);
            assert!(
                matches!(outcome.decision, Decision::Deny(_)),
                "{words:?} must reach git push, got {:?}",
                outcome.decision
            );
            assert_eq!(outcome.verb.as_deref(), Some("push"), "{words:?}");
            assert_eq!(
                outcome.deciding,
                Deciding::Rule {
                    id: "push".to_string(),
                    needs_operator: false
                },
                "{words:?}"
            );
        }
        let commit = decide(&p, "git", &["-c", "user.name=x", "commit", "-m", "y"]);
        assert_eq!(
            commit.decision,
            Decision::Allow {
                note: Some("committed".to_string())
            }
        );
        assert_eq!(commit.verb.as_deref(), Some("commit"));
    }

    #[test]
    fn an_option_value_that_names_a_subcommand_is_not_the_subcommand() {
        // `-C push` is a path named `push`: the subcommand is `status`, which
        // no family manages.
        let p = policy(GLOBAL_OPTIONS);
        let outcome = decide(&p, "git", &["-C", "push", "status"]);
        assert_eq!(outcome.decision, Decision::Allow { note: None });
        assert_eq!(outcome.deciding, Deciding::Default);
    }

    #[test]
    fn an_undeclared_option_before_the_subcommand_is_proxied_opaque() {
        let p = policy(GLOBAL_OPTIONS);
        for words in [
            vec!["--bogus", "push"],
            vec!["--bogus", "status"],
            vec!["-C", "/tmp", "--bogus", "push"],
            // A value option with no value left cannot be read either.
            vec!["-C"],
        ] {
            let outcome = decide(&p, "git", &words);
            assert_eq!(
                outcome.decision,
                Decision::Proxy {
                    reason: ProxyReason::Opaque
                },
                "{words:?}"
            );
            assert_eq!(outcome.deciding, Deciding::Default, "{words:?}");
            assert_eq!(outcome.verb, None, "{words:?}");
        }
        // After the subcommand, an option is the subcommand's own: it is
        // judged by the family's rules, never by the global declaration.
        assert!(matches!(
            decide(&p, "git", &["push", "--bogus"]).decision,
            Decision::Deny(_)
        ));
        // A binary with no declaration keeps today's reading: its options
        // are skipped, never proxied.
        let gh = policy(
            r#"{"tools": {"Bash": {"families": {"gh issue": {"rules": [
            {"id": "issue", "outcome": {"kind": "deny", "reason": "r", "instead": "x"}}
        ]}}}}}"#,
        );
        assert!(matches!(
            decide(&gh, "gh", &["--bogus", "issue", "list"]).decision,
            Decision::Deny(_)
        ));
    }

    #[test]
    fn a_rewrite_refuses_a_global_option_it_cannot_carry() {
        // The global option and its value come before the subcommand, so they
        // are not carried; the rewrite is refused naming `-C` rather than
        // dropping it (FR-CMD-008).
        let p = policy(
            r#"{
            "global_options": [{"binary": "git", "value_options": ["-C"]}],
            "tools": {"Bash": {"families": {"git push": {"rules": [
                {"id": "push", "outcome": {"kind": "rewrite", "target": "legion push",
                 "reason": "r"}}
            ]}}}}
        }"#,
        );
        assert_eq!(rewrite_target(&decide(&p, "git", &["push"])), "legion push");
        let outcome = decide(&p, "git", &["-C", "/tmp", "push"]);
        assert!(deny_reason(&outcome).contains("`-C`"));
        assert_eq!(outcome.verb.as_deref(), Some("push"));
    }

    /// A `git push` deny family and a `git status` allow family, with git's
    /// inline alias declared (#1298).
    const INLINE_ALIAS: &str = r#"{
        "global_options": [{"binary": "git", "value_options": ["-c", "--config-env"],
            "inline_alias": {"options": ["-c"], "prefix": "alias."}}],
        "tools": {"Bash": {"families": {
            "git push": {"rules": [
                {"id": "push", "outcome": {"kind": "deny", "reason": "no push", "instead": "x"}}]},
            "git status": {"rules": [
                {"id": "status", "outcome": {"kind": "allow", "note": "status"}}]}
        }}}
    }"#;

    #[test]
    fn an_inline_alias_is_decided_by_the_stricter_of_its_two_readings() {
        let p = policy(INLINE_ALIAS);
        // The alias's reading decides and names its verb.
        let aliased = decide(&p, "git", &["-c", "alias.p=push", "p", "origin"]);
        assert!(matches!(aliased.decision, Decision::Deny(_)));
        assert_eq!(aliased.verb.as_deref(), Some("push"));
        // A builtin name keeps its own reading: git never runs an alias that
        // shares a builtin's name.
        let builtin = decide(&p, "git", &["-c", "alias.push=status", "push"]);
        assert!(matches!(builtin.decision, Decision::Deny(_)));
        assert_eq!(builtin.verb.as_deref(), Some("push"));
        // A tie goes to the alias's reading.
        let status = decide(&p, "git", &["-c", "alias.s=status", "s"]);
        assert_eq!(
            status.decision,
            Decision::Allow {
                note: Some("status".to_string())
            }
        );
        assert_eq!(status.verb.as_deref(), Some("status"));
        // An alias route cannot read is proxied opaque, never allowed.
        for words in [
            vec!["-c", "alias.p=!git push", "p"],
            vec!["--config-env=alias.p=X", "p"],
        ] {
            let outcome = decide(&p, "git", &words);
            assert_eq!(
                outcome.decision,
                Decision::Proxy {
                    reason: ProxyReason::Opaque
                },
                "{words:?}"
            );
            assert_eq!(outcome.deciding, Deciding::Default, "{words:?}");
        }
        // An unreadable alias on a builtin name keeps the builtin's reading.
        assert!(matches!(
            decide(&p, "git", &["-c", "alias.push=!x", "push"]).decision,
            Decision::Deny(_)
        ));
    }

    #[test]
    fn ask_rule_marked_needing_operator_still_produces_an_unset_mark() {
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
            {"id": "r", "outcome": {"kind": "ask", "question": "q?", "reason": "why",
             "needs_operator": true}}
        ]}}}}}"#,
        );
        let outcome = decide_bash_invocation(&p, "gh", &args(&[]), false, &Context::default());
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
            decide_bash_invocation(&p, "a", &[], false, &ctx).decision,
            Decision::Allow { .. }
        ));
        assert!(matches!(
            decide_bash_invocation(&p, "b", &[], false, &ctx).decision,
            Decision::Rewrite { .. }
        ));
        assert!(matches!(
            decide_bash_invocation(&p, "c", &[], false, &ctx).decision,
            Decision::Proxy { .. }
        ));
        assert!(matches!(
            decide_bash_invocation(&p, "d", &[], false, &ctx).decision,
            Decision::Deny(_)
        ));
        assert!(matches!(
            decide_bash_invocation(&p, "e", &[], false, &ctx).decision,
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
            operator_mark: false,
            carried: Vec::new(),
        };
        let ask = PartOutcome {
            decision: ask("q", "r"),
            deciding: Deciding::Rule {
                id: "r".to_string(),
                needs_operator: false,
            },
            is_sym: false,
            verb: None,
            operator_mark: false,
            carried: Vec::new(),
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
            operator_mark: false,
            carried: Vec::new(),
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

    // -- a rewrite carries the words after its verb (FR-CMD-008 rev 3) -------

    /// `gh pr view` rewrites to `legion pr view` with the shipped rule's
    /// exception shapes: a positional that becomes `--number`, a reshaped
    /// `--repo`, and `--json` denied for its different meaning.
    const PR_VIEW: &str = r#"{"tools": {"Bash": {"families": {"gh pr view": {"rules": [
        {"id": "pr-view", "outcome": {"kind": "rewrite", "target": "legion pr view --repo {repo}",
         "reason": "legion tracks PRs", "positional": ["--number"],
         "reshape": {"--repo": "repo_name"}, "deny_flags": ["--json"]}}
    ]}}}}}"#;

    fn decide(p: &Policy, binary: &str, words: &[&str]) -> PartOutcome {
        decide_bash_invocation(p, binary, &args(words), false, &Context::default())
    }

    fn rewrite_target(outcome: &PartOutcome) -> &str {
        match &outcome.decision {
            Decision::Rewrite { target, .. } => target.as_str(),
            other => panic!("expected rewrite, got {other:?}"),
        }
    }

    #[test]
    fn a_rewrite_carries_the_words_after_its_verb_as_literal_values() {
        let p = policy(PR_VIEW);
        let outcome = decide(
            &p,
            "gh",
            &[
                "pr",
                "view",
                "42",
                "--repo",
                "'runlegion/legion'",
                "\"a b\"",
            ],
        );
        assert_eq!(rewrite_target(&outcome), "legion pr view --repo {repo}");
        assert_eq!(outcome.verb.as_deref(), Some("pr view"));
        assert_eq!(
            outcome.carried,
            args(&["42", "--repo", "runlegion/legion", "a b"])
        );
    }

    #[test]
    fn a_rewrite_carries_an_argument_the_target_will_judge() {
        // No per-flag list decides here (FR-CMD-008 rev 3): `--web` is
        // carried, and the target's own parse in the adapter rejects it. The
        // two commands share a verb and differ only in the words carried.
        let p = policy(PR_VIEW);
        let bare = decide(&p, "gh", &["pr", "view", "42"]);
        let flagged = decide(&p, "gh", &["pr", "view", "42", "--web"]);
        assert_eq!(bare.verb, flagged.verb);
        assert_eq!(bare.deciding, flagged.deciding);
        assert_eq!(bare.carried, args(&["42"]));
        assert_eq!(flagged.carried, args(&["42", "--web"]));
    }

    #[test]
    fn a_word_that_is_not_a_plain_literal_denies_naming_it() {
        let p = policy(PR_VIEW);
        for word in ["\"$MSG\"", "$MSG", "$(date)", "*.rs", "~/x"] {
            let outcome = decide(&p, "gh", &["pr", "view", "42", word]);
            let reason = deny_reason(&outcome);
            assert!(reason.contains(&format!("`{word}`")), "{word}: {reason}");
            assert!(reason.contains("not a plain literal"), "{word}: {reason}");
            assert!(outcome.carried.is_empty(), "{word}");
            match &outcome.decision {
                Decision::Deny(details) => {
                    assert_eq!(details.instead(), "legion pr view --repo {repo}")
                }
                other => panic!("{word}: expected deny, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_denied_flag_denies_naming_it_in_both_spellings() {
        let p = policy(PR_VIEW);
        for (words, named) in [
            (vec!["pr", "view", "42", "--json", "title"], "`--json`"),
            (vec!["pr", "view", "42", "--json=title"], "`--json=title`"),
            (vec!["pr", "view", "42", "'--json'"], "`'--json'`"),
        ] {
            let outcome = decide(&p, "gh", &words);
            let reason = deny_reason(&outcome);
            assert!(reason.contains(named), "{words:?}: {reason}");
            assert!(reason.contains("different meaning"), "{words:?}: {reason}");
        }
        // After `--` the word is an operand, not the flag.
        let operand = decide(&p, "gh", &["pr", "view", "--", "--json"]);
        assert_eq!(operand.carried, args(&["--", "--json"]));
        // A flag that only starts with a denied name is a different flag.
        let other = decide(&p, "gh", &["pr", "view", "--jsonl"]);
        assert_eq!(other.carried, args(&["--jsonl"]));
    }

    #[test]
    fn a_word_before_or_among_the_verb_words_denies_naming_it() {
        // It is not after the verb, so it is not carried; rewriting would
        // drop it.
        let p = policy(PR_VIEW);
        for (words, named) in [
            (vec!["--no-pager", "pr", "view", "42"], "`--no-pager`"),
            (vec!["pr", "--json", "view", "42"], "`--json`"),
        ] {
            let outcome = decide(&p, "gh", &words);
            let reason = deny_reason(&outcome);
            assert!(reason.contains(named), "{words:?}: {reason}");
            assert!(reason.contains("would be dropped"), "{words:?}: {reason}");
        }
    }

    #[test]
    fn the_family_subcommand_words_are_governed_but_a_repeat_of_one_is_carried() {
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"gh issue list": {"rules": [
            {"id": "l", "outcome": {"kind": "rewrite", "target": "legion issue list --repo {repo}",
             "reason": "r"}}
        ]}}}}}"#,
        );
        let bare = decide(&p, "gh", &["issue", "list"]);
        assert_eq!(rewrite_target(&bare), "legion issue list --repo {repo}");
        assert!(bare.carried.is_empty());
        let repeated = decide(&p, "gh", &["issue", "list", "issue"]);
        assert_eq!(repeated.carried, args(&["issue"]));
    }

    #[test]
    fn only_a_rewrite_carries_words() {
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"gh pr merge": {"rules": [
            {"id": "m", "outcome": {"kind": "deny", "reason": "r", "instead": "i"}}
        ]}}}}}"#,
        );
        assert!(decide(&p, "gh", &["pr", "merge", "7"]).carried.is_empty());
    }

    #[test]
    fn a_fields_rewrite_needs_no_exception_declaration() {
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
            false,
            &Context::default(),
        );
        assert!(outcome.is_sym);
        match outcome.decision {
            Decision::Deny(details) => assert_eq!(details.instead(), "legion sym find-content"),
            other => panic!("expected deny naming sym, got {other:?}"),
        }
    }

    /// `grep` routes to a pipe-filter job; `find` to a job that is not one.
    const PIPE_FILTER_POLICY: &str = r#"{
        "sym_jobs": [
            {"id": "content", "sym_command": "legion sym etc find-content", "pipe_filter": true},
            {"id": "file", "sym_command": "legion sym etc find-file"}
        ],
        "tools": {"Bash": {"families": {
            "grep": {"rules": [{"id": "grep-to-sym", "outcome": {"kind": "sym", "job": "content"}}]},
            "find": {"rules": [{"id": "find-to-sym", "outcome": {"kind": "sym", "job": "file"}}]}
        }}}
    }"#;

    #[test]
    fn a_pipe_filter_sym_job_does_not_claim_a_stage_reading_a_pipe() {
        let p = policy(PIPE_FILTER_POLICY);
        let piped =
            decide_bash_invocation(&p, "grep", &args(&["-i", "x"]), true, &Context::default());
        // Not managed there: the no-match default, not a sym part.
        assert_eq!(piped.decision, Decision::Allow { note: None });
        assert_eq!(piped.deciding, Deciding::Default);
        assert!(!piped.is_sym);
        assert!(piped.verb.is_none());
        // The same invocation outside a pipe stage is still the sym job.
        let first =
            decide_bash_invocation(&p, "grep", &args(&["-i", "x"]), false, &Context::default());
        assert!(first.is_sym);
    }

    #[test]
    fn a_sym_job_that_is_not_a_pipe_filter_still_claims_a_stage_reading_a_pipe() {
        let p = policy(PIPE_FILTER_POLICY);
        let piped = decide_bash_invocation(
            &p,
            "find",
            &args(&[".", "-name", "x"]),
            true,
            &Context::default(),
        );
        assert!(piped.is_sym);
        match piped.decision {
            Decision::Deny(details) => assert_eq!(details.instead(), "legion sym etc find-file"),
            other => panic!("expected deny naming sym, got {other:?}"),
        }
    }

    #[test]
    fn a_later_rule_still_governs_a_stage_a_pipe_filter_rule_passes_over() {
        let p = policy(
            r#"{
            "sym_jobs": [{"id": "content", "sym_command": "legion sym etc find-content", "pipe_filter": true}],
            "tools": {"Bash": {"families": {"grep": {"rules": [
                {"id": "grep-to-sym", "outcome": {"kind": "sym", "job": "content"}},
                {"id": "grep-piped", "outcome": {"kind": "proxy", "reason": "binary"}}
            ]}}}}
        }"#,
        );
        let piped = decide_bash_invocation(&p, "grep", &args(&["x"]), true, &Context::default());
        assert_eq!(
            piped.deciding,
            Deciding::Rule {
                id: "grep-piped".to_string(),
                needs_operator: false
            }
        );
        assert!(!piped.is_sym);
    }
}
