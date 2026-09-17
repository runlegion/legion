//! The one evaluator (FR-CMD-011): the only code in this crate that turns a
//! [`Policy`] into a [`Decision`]. `route` (in `route.rs`) is a thin
//! wrapper around [`evaluate`]; the adapter and the ledger call `route`,
//! never this module, so every routing branch lives here and nowhere else.

use std::collections::BTreeMap;

use crate::decision::{
    Context, DecidingEntry, Decision, Facts, Lookup, ProxyReason, Routed, ToolCall,
};
use crate::policy::{
    BinaryOptions, Family, MatchInput, Policy, RequiredLookup, Rule, SymJob, SymJobMatcher,
    ToolKind, ToolRules,
};
use crate::tokenizer::{self, Invocation, Opaque, Scan};

/// The message every FR-CMD-016 default carries, so the agent always sees
/// why a decision was made even when no specific rule matched.
mod default_messages {
    pub const NO_MANAGED_BINARY: &str =
        "no policy rule governs this command; it is outside legion-cmd's concern";
    pub const UNRESOLVED_MANAGED_RULE: &str =
        "this command's managed binary is governed, but no policy rule resolved it";
    pub const MISSING_LOOKUP: &str =
        "a policy rule matched but its required recall or consult result was not fetched";
    pub const EMPTY_POLICY: &str =
        "the policy is empty; every command is denied until it is populated";
    pub const DEFAULT_INSTEAD: &str =
        "the operator has not yet defined a policy replacement for this command";
}

/// Turns `call` and `ctx` into exactly one [`Routed`] decision, reading
/// `policy` as the single source of routing truth (FR-CMD-011). Pure: no
/// filesystem, network, database, or process access (NFR-CMD-001).
///
/// The no-go list (FR-CMD-025) is a named first step here once #1237 lands;
/// today this function starts directly at the empty-policy check below, so
/// that step has a fixed place to be inserted rather than a bare comment.
pub fn evaluate(policy: &Policy, call: &ToolCall, ctx: &Context) -> Routed {
    if call.tool == "Bash" {
        return evaluate_bash(policy, call, ctx);
    }
    evaluate_fields(policy, call, ctx)
}

// -- Bash: scan, sym-job precedence, per-invocation rules, then fold -------

fn evaluate_bash(policy: &Policy, call: &ToolCall, ctx: &Context) -> Routed {
    let command = call
        .input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    let scan = match tokenizer::scan(command) {
        Ok(scan) => scan,
        Err(err) => {
            // FR-CMD-006: a parse error routes ask unconditionally. This is
            // one of exactly two ways `evaluate` returns Ask, and it does
            // not depend on the policy's contents at all -- an empty
            // policy still asks here rather than denying, since there is
            // no command shape yet to apply the empty-policy default to.
            let message = err.to_string();
            // `err.to_string()` is a thiserror `#[error(...)]` display, so
            // it is never empty, and the reason is a non-empty constant --
            // both non-empty by construction, so the infallible
            // constructor is safe here without a panic path.
            let decision = Decision::ask_infallible(
                format!("the command could not be parsed: {message}"),
                "a malformed command cannot be routed safely",
            );
            return Routed {
                decision,
                facts: Facts::default(),
                entry: DecidingEntry::ParseError(message),
            };
        }
    };

    let facts = facts_from_scan(&scan);

    if policy.is_empty() {
        return routed_default_deny(default_messages::EMPTY_POLICY, facts);
    }

    // FR-CMD-007: a compound command whose job `legion sym` serves is
    // never allowed or proxied, regardless of what the rest of the
    // command's parts would otherwise decide. This is checked and
    // returned before the strictest-order fold below: a sym job always
    // denies naming the sym command (see `find_sym_job`'s doc for why it
    // never rewrites), which the fold would also produce on its own for
    // this part alone, but only deciding it first keeps "never allowed or
    // proxied" true regardless of what a sibling part's own decision is.
    if let Some((job_id, sym_command, matched_description)) =
        find_sym_job(&policy.sym_jobs, &scan.invocations, &scan.opaque)
    {
        // `sym_command` is non-empty because `parse_policy` already
        // rejects an empty one (`PolicyError::EmptySymCommand`), and the
        // reason is a non-empty format! -- both non-empty by construction.
        let decision = Decision::deny_infallible(
            format!("this command's job is one legion sym serves ({matched_description})"),
            sym_command,
        );
        return Routed {
            decision,
            facts,
            entry: DecidingEntry::Rule {
                id: job_id,
                needs_operator: false,
            },
        };
    }

    let (families, binaries) = bash_tool_rules(policy);
    let mut parts: Vec<(Decision, DecidingEntry)> = scan
        .invocations
        .iter()
        .map(|inv| evaluate_invocation(families, binaries, inv, ctx))
        .collect();
    parts.extend(scan.opaque.iter().map(|_| opaque_part()));

    gate_rewrite_on_whole_command(fold(parts, facts), scan.is_single_simple_command)
}

/// FR-CMD-008: the harness that acts on a [`Decision::Rewrite`] replaces
/// the WHOLE command string with the rewrite's target, so a rewrite is
/// lossless only when the invocation it targets IS the whole command --
/// `scan.is_single_simple_command` is that fact. Wraps only the
/// strictest-order fold's result, since that is the one place a
/// [`Decision::Rewrite`] can come from: a sym job never rewrites (it
/// always denies naming the sym command, see `find_sym_job`'s doc) and
/// returns before this ever runs, so this gate governs family rewrites
/// alone, in route rather than needing a matching check in the adapter.
fn gate_rewrite_on_whole_command(routed: Routed, is_single_simple_command: bool) -> Routed {
    if is_single_simple_command {
        return routed;
    }
    match routed.decision {
        Decision::Rewrite { target, .. } => Routed {
            decision: Decision::deny_infallible(
                format!(
                    "the command is not just {}; a rewrite would replace the whole command and drop the rest of it",
                    target.as_str()
                ),
                target.as_str().to_string(),
            ),
            facts: routed.facts,
            entry: routed.entry,
        },
        _ => routed,
    }
}

fn bash_tool_rules(
    policy: &Policy,
) -> (&BTreeMap<String, Family>, &BTreeMap<String, BinaryOptions>) {
    static EMPTY_FAMILIES: BTreeMap<String, Family> = BTreeMap::new();
    static EMPTY_BINARIES: BTreeMap<String, BinaryOptions> = BTreeMap::new();
    match policy.tools.get(&ToolKind::Bash) {
        Some(ToolRules::Bash { families, binaries }) => (families, binaries),
        _ => (&EMPTY_FAMILIES, &EMPTY_BINARIES),
    }
}

/// The first non-flag argument in `inv.args` -- `binary`'s subcommand, if
/// any -- found by skipping `binary`'s own global options that take a
/// separate value word (e.g. `git -C <dir>`) so a global option's value
/// is never mistaken for the subcommand. `None` when no non-flag argument
/// remains (or exists at all): `binary` was invoked with no subcommand.
/// Also returns whether that resolution is doubtful: `true` when at least
/// one flag was skipped before settling on the verb that was not in
/// `binary`'s declared `global_value_options`, meaning an unlisted option
/// could have consumed the next word as its own value rather than that
/// word being the real subcommand.
fn subcommand_word<'a>(
    binaries: &BTreeMap<String, BinaryOptions>,
    inv: &'a Invocation,
) -> (Option<&'a str>, bool) {
    let value_options: &[String] = binaries
        .get(&inv.binary)
        .map(|b| b.global_value_options.as_slice())
        .unwrap_or(&[]);
    let mut index = 0;
    let mut doubtful = false;
    while index < inv.args.len() {
        let arg = &inv.args[index];
        if arg.starts_with('-') {
            if value_options.iter().any(|opt| opt == arg) {
                index += 2;
            } else {
                doubtful = true;
                index += 1;
            }
            continue;
        }
        return (Some(arg), doubtful);
    }
    (None, doubtful)
}

/// Finds the family governing `inv`, trying the verb-scoped two-word key
/// (`"git push"`) before falling back to the binary alone (`"git"`), so a
/// family can govern one subcommand of a binary without governing every
/// other subcommand (FR-CMD-011's `"git push"` example key). The
/// subcommand word is found by skipping `binary`'s own global value
/// options (e.g. `git -C /tmp push` resolves to `"git push"`, not the
/// undefined `"git -C"` a naive first-argument check would produce).
/// Also returns whether that resolution is doubtful (see
/// [`subcommand_word`]) -- the caller decides whether an unresolved,
/// doubtful call is worth failing closed over.
fn find_family<'p>(
    families: &'p BTreeMap<String, Family>,
    binaries: &BTreeMap<String, BinaryOptions>,
    inv: &Invocation,
) -> (Option<&'p Family>, bool) {
    let (verb, doubtful) = subcommand_word(binaries, inv);
    if let Some(verb) = verb {
        let two_word = format!("{} {verb}", inv.binary);
        if let Some(family) = families.get(&two_word) {
            return (Some(family), doubtful);
        }
    }
    (families.get(&inv.binary), doubtful)
}

/// True when `families` governs at least one subcommand of `inv.binary`
/// (a key of the form `"<binary> <verb>"`) and that verb literally
/// appears somewhere in `inv.args`. Consulted only when [`find_family`]'s
/// resolution came back both unresolved and doubtful (an unlisted global
/// option could have consumed the real verb as its own value, e.g.
/// `git --unlisted-opt value push`) -- an unresolved but *confident*
/// resolution (the verb was found with no doubt, e.g. `git branch -d
/// push`, a branch named `push`) is trusted and never fails closed on an
/// unrelated word (FR-CMD-016).
fn governed_subcommand_present(families: &BTreeMap<String, Family>, inv: &Invocation) -> bool {
    let prefix = format!("{} ", inv.binary);
    families.keys().any(|key| {
        key.strip_prefix(prefix.as_str())
            .is_some_and(|verb| inv.args.iter().any(|arg| arg == verb))
    })
}

fn evaluate_invocation(
    families: &BTreeMap<String, Family>,
    binaries: &BTreeMap<String, BinaryOptions>,
    inv: &Invocation,
    ctx: &Context,
) -> (Decision, DecidingEntry) {
    let (family, resolution_is_doubtful) = find_family(families, binaries, inv);
    if let Some(family) = family {
        return evaluate_rules(&family.rules, MatchInput::Args(&inv.args), ctx);
    }
    if resolution_is_doubtful && governed_subcommand_present(families, inv) {
        // FR-CMD-016: the subcommand resolver skipped an unlisted flag
        // before settling on a verb, so that verb could really be a
        // skipped option's value -- and a governed verb is plainly
        // present in the args anyway. Fail closed, deny, rather than
        // trust a resolution that was already in doubt.
        return (
            deny_default(default_messages::UNRESOLVED_MANAGED_RULE),
            DecidingEntry::Default,
        );
    }
    // FR-CMD-016: no managed binary and no matching rule -> allow,
    // surfaced to the agent. This also covers a confident resolution that
    // simply named an ungoverned verb (e.g. `git status`, or `push` as a
    // `git branch -d push` operand, not a subcommand) -- no reason to
    // second-guess a resolution nothing cast doubt on.
    (
        allow_default(default_messages::NO_MANAGED_BINARY),
        DecidingEntry::Default,
    )
}

/// Walks `rules` in declared order, returning the first whose predicate
/// matches and whose required lookups are satisfied. A rule whose
/// predicate matches but whose required lookup is missing counts as
/// resolved-but-blocked (FR-CMD-016's missing-lookup default), not as a
/// non-match that falls through to the next rule.
///
/// A matched rule whose decision is [`Decision::Rewrite`] and whose
/// `exact_args` is declared (FR-CMD-008) is checked against `input`'s
/// arguments before its decision is trusted: the invocation's arguments
/// must equal `exact_args` exactly, or it denies naming the rewrite's
/// target instead of rewriting. `exact_args` is `None` for a Fields
/// rule's Rewrite (`parse_policy` forbids it there), so this never runs
/// against `MatchInput::Json`.
fn evaluate_rules(
    rules: &[Rule],
    input: MatchInput<'_>,
    ctx: &Context,
) -> (Decision, DecidingEntry) {
    let Some(rule) = first_predicate_match(rules, input) else {
        // FR-CMD-016: a managed rule (this binary/field set is governed by
        // a family or a Fields rule list) that cannot resolve -> deny.
        return (
            deny_default(default_messages::UNRESOLVED_MANAGED_RULE),
            DecidingEntry::Default,
        );
    };
    if missing_required_lookup(&rule.requires, ctx) {
        return (
            deny_default(default_messages::MISSING_LOOKUP),
            DecidingEntry::Default,
        );
    }
    if let (Decision::Rewrite { target, .. }, MatchInput::Args(args), Some(exact_args)) =
        (&rule.decision, input, &rule.exact_args)
        && let Some(bad_arg) = first_differing_arg(exact_args, args)
    {
        let decision = Decision::deny_infallible(
            format!(
                "argument {bad_arg:?} has no lossless translation to {}",
                target.as_str()
            ),
            target.as_str().to_string(),
        );
        return (
            decision,
            DecidingEntry::Rule {
                id: rule.id.clone(),
                needs_operator: false,
            },
        );
    }
    (
        rule.decision.clone(),
        DecidingEntry::Rule {
            id: rule.id.clone(),
            needs_operator: rule.needs_operator,
        },
    )
}

/// The first rule in `rules` (declared order) whose predicate matches
/// `input`, ignoring lookup availability and the `exact_args` lossless
/// check entirely. Factored out of [`evaluate_rules`] so
/// [`candidate_rules`] can name the same rule a real `evaluate` call
/// would reach for, without a second implementation of "which rule
/// matches" that could silently diverge from this one.
fn first_predicate_match<'p>(rules: &'p [Rule], input: MatchInput<'_>) -> Option<&'p Rule> {
    rules.iter().find(|rule| rule.predicate.matches(input))
}

/// Every [`Rule`] a real `evaluate(policy, call, ctx)` call would consult
/// while deciding `call`, for ANY `ctx` -- i.e. the predicate-matched rule
/// for each Bash invocation (or the single matched rule for a Fields
/// call), found through the same sym-job precedence and family/global-
/// value-option resolution `evaluate_bash`/`evaluate_fields` use,
/// entirely before any lookup-availability or `exact_args` check is
/// applied.
///
/// This is `route`'s own candidate-rule step, exposed so
/// [`crate::lookups`]'s pure pre-pass (#1229) can pre-fetch exactly the
/// lookups those rules require, rather than re-implementing rule
/// selection a second time and risking it disagreeing with `route`. It is
/// not a second decision path: nothing here reads `ctx`, builds a
/// `Decision`, or folds severities -- it only names which rules are in
/// play.
///
/// Empty when: the tool is Bash and the command cannot be tokenized; the
/// policy is empty; a sym job matches first (sym jobs carry no `requires`
/// at all, and matching one means `evaluate_bash` returns before any
/// family rule is ever considered); no Bash invocation resolves to a
/// governed family; or a non-Bash tool has no vocabulary or no `Fields`
/// rule list in the policy.
pub(crate) fn candidate_rules<'p>(policy: &'p Policy, call: &ToolCall) -> Vec<&'p Rule> {
    if call.tool == "Bash" {
        return candidate_bash_rules(policy, call);
    }
    candidate_field_rules(policy, call)
}

fn candidate_bash_rules<'p>(policy: &'p Policy, call: &ToolCall) -> Vec<&'p Rule> {
    if policy.is_empty() {
        return Vec::new();
    }
    let command = call
        .input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let Ok(scan) = tokenizer::scan(command) else {
        return Vec::new();
    };
    if find_sym_job(&policy.sym_jobs, &scan.invocations, &scan.opaque).is_some() {
        return Vec::new();
    }
    let (families, binaries) = bash_tool_rules(policy);
    scan.invocations
        .iter()
        .filter_map(|inv| {
            let (family, _doubtful) = find_family(families, binaries, inv);
            first_predicate_match(&family?.rules, MatchInput::Args(&inv.args))
        })
        .collect()
}

fn candidate_field_rules<'p>(policy: &'p Policy, call: &ToolCall) -> Vec<&'p Rule> {
    if policy.is_empty() {
        return Vec::new();
    }
    let Some(kind) = ToolKind::from_tool_name(&call.tool) else {
        return Vec::new();
    };
    let rules: &[Rule] = match policy.tools.get(&kind) {
        Some(ToolRules::Fields { rules }) => rules,
        _ => return Vec::new(),
    };
    first_predicate_match(rules, MatchInput::Json(&call.input))
        .into_iter()
        .collect()
}

/// The first argument where `args` differs from `exact_args`, or `None`
/// when they are exactly equal (FR-CMD-008): an extra or mismatched
/// argument is named directly; an argument `exact_args` expects but
/// `args` is too short to carry is named as `(missing "<word>")`, so the
/// deny built from it always names something concrete.
fn first_differing_arg(exact_args: &[String], args: &[String]) -> Option<String> {
    if args == exact_args {
        return None;
    }
    let len = args.len().max(exact_args.len());
    (0..len).find_map(|i| match (args.get(i), exact_args.get(i)) {
        (Some(a), Some(e)) if a == e => None,
        (Some(a), _) => Some(a.clone()),
        (None, Some(e)) => Some(format!("(missing {e:?})")),
        (None, None) => None,
    })
}

fn missing_required_lookup(requires: &[RequiredLookup], ctx: &Context) -> bool {
    requires.iter().any(|req| {
        let lookup = match req {
            RequiredLookup::Recall => &ctx.recall,
            RequiredLookup::Consult => &ctx.consult,
        };
        matches!(lookup, Lookup::NotFetched)
    })
}

/// Every non-interpreter opaque region is proxied with reason opaque
/// (FR-CMD-004, FR-CMD-007): the scanner cannot see into it, so it is
/// recorded coverage-unknown rather than silently allowed.
fn opaque_part() -> (Decision, DecidingEntry) {
    (
        Decision::Proxy {
            reason: ProxyReason::Opaque,
        },
        DecidingEntry::Opaque,
    )
}

/// Checks every sym job against the whole command, in declared order,
/// returning the first one that matches: its id, its `sym_command`, and a
/// description of what matched, for the deny reason built from it. An
/// [`crate::policy::SymJobMatcher::Invocation`] job is checked against
/// every resolved [`Invocation`] (any position or depth, so `sh -c` and
/// pipelines count); an [`crate::policy::SymJobMatcher::InterpreterPatterns`]
/// job is checked against every [`Opaque::Interpreter`] body, requiring
/// all of its patterns to appear. Declared order matters for both: a
/// policy author lists a job's more specific match before a broader one
/// so the more specific job wins when both would otherwise match the
/// same command.
///
/// A sym job never rewrites (FR-CMD-008): identifying a search invocation
/// requires at least one argument (a recursive flag, a search pattern),
/// and nothing carries an argument into a rewrite target today -- so no
/// declaration could ever cover a real match. Rewriting a sym-served
/// search needs an argument-carrying template, which does not exist yet;
/// until it does, every match here denies naming `sym_command`.
fn find_sym_job(
    sym_jobs: &[SymJob],
    invocations: &[Invocation],
    opaque: &[Opaque],
) -> Option<(String, String, String)> {
    for job in sym_jobs {
        match &job.matcher {
            SymJobMatcher::Invocation { binary, predicate } => {
                let matched = invocations.iter().find(|inv| {
                    &inv.binary == binary && predicate.matches(MatchInput::Args(&inv.args))
                });
                if matched.is_some() {
                    let description = format!("invocation of {binary}");
                    return Some((job.id.clone(), job.sym_command.clone(), description));
                }
            }
            SymJobMatcher::InterpreterPatterns(patterns) => {
                // `parse_policy` already rejects a sym job with an empty
                // `interpreter_patterns` list (`PolicyError::EmptySymPatterns`),
                // so every job reaching here has at least one pattern to
                // check.
                let matched = opaque.iter().any(|region| {
                    let Opaque::Interpreter { body, .. } = region else {
                        return false;
                    };
                    patterns
                        .iter()
                        .all(|pattern| body.contains(pattern.as_str()))
                });
                if matched {
                    let description = patterns.join(", ");
                    return Some((job.id.clone(), job.sym_command.clone(), description));
                }
            }
        }
    }
    None
}

/// Combines every part of a compound command into one [`Decision`], taking
/// the strictest in the order deny, ask, proxy, rewrite, allow
/// (FR-CMD-007). An empty command (no invocations, no opaque regions)
/// falls through to the no-managed-binary allow default, the same as any
/// other command nothing in the policy governs.
fn fold(parts: Vec<(Decision, DecidingEntry)>, facts: Facts) -> Routed {
    let Some((decision, entry)) = parts
        .into_iter()
        .min_by_key(|(decision, _)| severity(decision))
    else {
        return Routed {
            decision: allow_default(default_messages::NO_MANAGED_BINARY),
            facts,
            entry: DecidingEntry::Default,
        };
    };
    Routed {
        decision,
        facts,
        entry,
    }
}

fn severity(decision: &Decision) -> u8 {
    match decision {
        Decision::Deny(_) => 0,
        Decision::Ask(_) => 1,
        Decision::Proxy { .. } => 2,
        Decision::Rewrite { .. } => 3,
        Decision::Allow { .. } => 4,
    }
}

// -- Fields tools: Edit/Write/Read/Grep/Agent route on their own fields ----

fn evaluate_fields(policy: &Policy, call: &ToolCall, ctx: &Context) -> Routed {
    let facts = Facts::default();

    if policy.is_empty() {
        return routed_default_deny(default_messages::EMPTY_POLICY, facts);
    }

    let Some(kind) = ToolKind::from_tool_name(&call.tool) else {
        // A tool the policy has no vocabulary for at all is simply outside
        // its concern -- the same allow default as an unmanaged binary.
        return Routed {
            decision: allow_default(default_messages::NO_MANAGED_BINARY),
            facts,
            entry: DecidingEntry::Default,
        };
    };

    let rules: &[Rule] = match policy.tools.get(&kind) {
        Some(ToolRules::Fields { rules }) => rules,
        Some(ToolRules::Bash { .. }) | None => {
            return Routed {
                decision: allow_default(default_messages::NO_MANAGED_BINARY),
                facts,
                entry: DecidingEntry::Default,
            };
        }
    };

    let (decision, entry) = evaluate_rules(rules, MatchInput::Json(&call.input), ctx);
    Routed {
        decision,
        facts,
        entry,
    }
}

// -- Shared default builders -------------------------------------------

fn allow_default(message: &str) -> Decision {
    Decision::Allow {
        note: Some(message.to_string()),
    }
}

fn deny_default(message: &str) -> Decision {
    // `message` is always one of the `default_messages` constants above,
    // and `DEFAULT_INSTEAD` is a fixed non-empty constant -- both
    // non-empty by construction, so the infallible constructor is safe.
    Decision::deny_infallible(message, default_messages::DEFAULT_INSTEAD)
}

fn routed_default_deny(message: &str, facts: Facts) -> Routed {
    Routed {
        decision: deny_default(message),
        facts,
        entry: DecidingEntry::Default,
    }
}

// -- Facts extraction (FR-CMD-003) ---------------------------------------

/// Extracts the facts `route` promises the caller so it never re-parses
/// the command (FR-CMD-003): the primary verb (the binary at the scan's
/// first position, if any), every argument that looks like a path, every
/// `#<digits>` issue reference in an argument or an interpreter body, and
/// every other non-flag argument as a keyword.
fn facts_from_scan(scan: &Scan) -> Facts {
    let verb = scan
        .invocations
        .iter()
        .find(|inv| inv.position == tokenizer::Position::First)
        .map(|inv| inv.binary.clone());

    let mut paths = Vec::new();
    let mut keywords = Vec::new();
    let mut issue_numbers = Vec::new();

    for inv in &scan.invocations {
        for arg in &inv.args {
            if arg.starts_with('-') {
                continue;
            }
            issue_numbers.extend(extract_issue_numbers(arg));
            if looks_like_path(arg) {
                paths.push(arg.clone());
            } else {
                keywords.push(arg.clone());
            }
        }
    }

    for region in &scan.opaque {
        if let Opaque::Interpreter { body, .. } = region {
            issue_numbers.extend(extract_issue_numbers(body));
        }
    }

    Facts {
        paths,
        verb,
        issue_numbers,
        keywords,
    }
}

fn looks_like_path(arg: &str) -> bool {
    arg.contains('/') || arg.starts_with('.')
}

/// Scans `text` for `#<digits>` references (e.g. `#1227`), a plain byte
/// walk with no quoting awareness -- the same bounded approach #1142's
/// `extract_issues` used, acceptable here since the tokenizer, not this
/// helper, owns quoting semantics.
fn extract_issue_numbers(text: &str) -> Vec<u64> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > start
                && let Ok(number) = text[start..end].parse::<u64>()
            {
                out.push(number);
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::ToolCall;
    use crate::policy::parse_policy;

    fn bash_call(command: &str) -> ToolCall {
        ToolCall {
            tool: "Bash".to_string(),
            input: serde_json::json!({"command": command}),
        }
    }

    fn tool_call(tool: &str, input: serde_json::Value) -> ToolCall {
        ToolCall {
            tool: tool.to_string(),
            input,
        }
    }

    const POLICY: &str = r#"{
        "tools": {
            "Bash": {
                "kind": "bash",
                "families": {
                    "git push": {
                        "rules": [
                            {
                                "id": "git-push-force",
                                "predicate": {"arg_equals": "--force"},
                                "outcome": {
                                    "kind": "deny",
                                    "reason": "force-push rewrites shared history",
                                    "instead": "git push --force-with-lease"
                                }
                            },
                            {
                                "id": "git-push-default",
                                "predicate": "always",
                                "outcome": {"kind": "allow", "note": null}
                            }
                        ]
                    },
                    "chmod": {
                        "rules": [
                            {
                                "id": "chmod-777",
                                "predicate": {"operand_contains": "777"},
                                "outcome": {
                                    "kind": "deny",
                                    "reason": "777 opens the tree to every user",
                                    "instead": "chmod 755"
                                }
                            }
                        ]
                    },
                    "gh": {
                        "rules": [
                            {
                                "id": "gh-recall-gated",
                                "predicate": {"arg_equals": "--recall-gated"},
                                "requires": ["recall"],
                                "outcome": {
                                    "kind": "allow",
                                    "note": "cleared by recall"
                                }
                            }
                        ]
                    },
                    "gh issue": {
                        "rules": [
                            {
                                "id": "gh-issue-list",
                                "predicate": {"arg_equals": "list"},
                                "outcome": {
                                    "kind": "rewrite",
                                    "target": "legion issue list --repo {repo}",
                                    "reason": "gh issue list duplicates legion's issue tracking surface",
                                    "exact_args": ["issue", "list"]
                                }
                            }
                        ]
                    },
                    "gh pr": {
                        "rules": [
                            {
                                "id": "gh-pr-merge-admin",
                                "predicate": {"all": [
                                    {"arg_equals": "merge"},
                                    {"arg_equals": "--admin"}
                                ]},
                                "outcome": {
                                    "kind": "ask",
                                    "question": "merge bypassing branch protection?",
                                    "reason": "--admin skips required checks",
                                    "needs_operator": true
                                }
                            }
                        ]
                    }
                }
            },
            "Edit": {
                "kind": "fields",
                "rules": [
                    {
                        "id": "edit-dotenv",
                        "predicate": {"field_contains": {"path": "file_path", "contains": ".env"}},
                        "outcome": {
                            "kind": "ask",
                            "question": "edit a secrets file?",
                            "reason": ".env holds credentials",
                            "needs_operator": true
                        }
                    }
                ]
            }
        },
        "sym_jobs": [
            {
                "id": "py-rglob-read-text",
                "sym_command": "legion sym etc find-content",
                "interpreter_patterns": ["rglob(", "read_text()"]
            },
            {
                "id": "py-rglob-list",
                "sym_command": "legion sym etc find-file",
                "interpreter_patterns": ["rglob("]
            }
        ]
    }"#;

    fn policy() -> Policy {
        parse_policy(POLICY).expect("test policy is well-formed")
    }

    // -- Every one of the five decision arms has a test (NFR-CMD-002) -----

    #[test]
    fn allow_arm_for_an_unmanaged_command() {
        let routed = evaluate(&policy(), &bash_call("ls -la"), &Context::default());
        assert!(matches!(routed.decision, Decision::Allow { .. }));
        assert_eq!(routed.entry, DecidingEntry::Default);
    }

    #[test]
    fn rewrite_arm_from_a_family_rule() {
        let routed = evaluate(&policy(), &bash_call("gh issue list"), &Context::default());
        match routed.decision {
            Decision::Rewrite { target, .. } => {
                assert_eq!(target.as_str(), "legion issue list --repo {repo}")
            }
            other => panic!("expected Rewrite, got {other:?}"),
        }
        assert_eq!(
            routed.entry,
            DecidingEntry::Rule {
                id: "gh-issue-list".to_string(),
                needs_operator: false
            }
        );
    }

    #[test]
    fn family_rewrite_with_an_untranslatable_flag_denies_naming_the_target() {
        // FR-CMD-008 applies to every rewrite, not only sym jobs: "--repo"
        // (the gh flag, distinct from the target's own `{repo}`
        // placeholder) is not one of the "gh-issue-list" rule's
        // `exact_args`, so it denies naming the target instead of
        // silently dropping the flag.
        let routed = evaluate(
            &policy(),
            &bash_call("gh issue list --repo other/org"),
            &Context::default(),
        );
        match &routed.decision {
            Decision::Deny(details) => {
                assert_eq!(details.instead(), "legion issue list --repo {repo}");
                assert!(
                    details.reason().contains("\"--repo\""),
                    "reason should name the untranslatable flag: {}",
                    details.reason()
                );
            }
            other => panic!("expected Deny naming the target, got {other:?}"),
        }
    }

    #[test]
    fn family_rewrite_composed_with_another_command_denies_instead_of_rewriting_the_whole_command()
    {
        // FR-CMD-008: the harness replaces the WHOLE command string, so a
        // family rewrite that would otherwise be lossless still does not
        // rewrite when it is not the whole command.
        let routed = evaluate(
            &policy(),
            &bash_call("gh issue list | head"),
            &Context::default(),
        );
        match &routed.decision {
            Decision::Deny(details) => {
                assert_eq!(details.instead(), "legion issue list --repo {repo}")
            }
            other => panic!("expected Deny naming the target, got {other:?}"),
        }
    }

    #[test]
    fn family_rewrite_shorter_than_exact_args_names_the_missing_word() {
        // first_differing_arg's (None, Some(_)) arm: the invocation is
        // shorter than the rule's exact_args, so the deny names the
        // expected word the invocation never supplied.
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "gh-two-words", "predicate": "always",
             "outcome": {"kind": "rewrite", "target": "legion gh", "reason": "why",
                         "exact_args": ["a", "b"]}}
        ]}}}}}"#;
        let short_policy = parse_policy(text).expect("valid test policy");
        let routed = evaluate(&short_policy, &bash_call("gh a"), &Context::default());
        match &routed.decision {
            Decision::Deny(details) => {
                assert_eq!(details.instead(), "legion gh");
                assert!(
                    details.reason().contains("missing"),
                    "reason should name the missing expected word: {}",
                    details.reason()
                );
                assert!(
                    details.reason().contains('b'),
                    "reason should name the specific missing word: {}",
                    details.reason()
                );
            }
            other => panic!("expected Deny naming the target, got {other:?}"),
        }
    }

    #[test]
    fn proxy_arm_for_an_opaque_region() {
        let routed = evaluate(&policy(), &bash_call("./deploy.sh"), &Context::default());
        assert_eq!(
            routed.decision,
            Decision::Proxy {
                reason: ProxyReason::Opaque
            }
        );
    }

    #[test]
    fn opaque_region_is_tagged_deciding_entry_opaque_not_default() {
        // An opaque Proxy part is recorded as DecidingEntry::Opaque, not
        // DecidingEntry::Default, so the ledger can tell a genuinely
        // opaque region apart from an FR-CMD-016 default that happens to
        // produce the same Proxy decision.
        let routed = evaluate(&policy(), &bash_call("./deploy.sh"), &Context::default());
        assert_eq!(routed.entry, DecidingEntry::Opaque);
    }

    #[test]
    fn deny_arm_from_a_family_rule() {
        let routed = evaluate(
            &policy(),
            &bash_call("git push --force"),
            &Context::default(),
        );
        match &routed.decision {
            Decision::Deny(details) => assert_eq!(details.instead(), "git push --force-with-lease"),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn ask_arm_from_a_family_rule() {
        let routed = evaluate(
            &policy(),
            &bash_call("gh pr merge --admin"),
            &Context::default(),
        );
        match &routed.decision {
            Decision::Ask(details) => assert_eq!(details.reason(), "--admin skips required checks"),
            other => panic!("expected Ask, got {other:?}"),
        }
        assert_eq!(
            routed.entry,
            DecidingEntry::Rule {
                id: "gh-pr-merge-admin".to_string(),
                needs_operator: true
            }
        );
    }

    // -- Visibility paths (NFR-CMD-002): pipe, env-prefix, inline wrapper --

    #[test]
    fn managed_binary_after_a_pipe_routes_identically_to_first_position() {
        let first = evaluate(&policy(), &bash_call("chmod 777 x"), &Context::default());
        let piped = evaluate(
            &policy(),
            &bash_call("ls | chmod 777 x"),
            &Context::default(),
        );
        assert_eq!(first.decision, piped.decision);
        assert!(matches!(piped.decision, Decision::Deny(_)));
    }

    #[test]
    fn managed_binary_behind_an_env_prefix_routes_identically() {
        let routed = evaluate(
            &policy(),
            &bash_call("FOO=1 chmod 777 x"),
            &Context::default(),
        );
        assert!(matches!(routed.decision, Decision::Deny(_)));
    }

    #[test]
    fn managed_binary_behind_an_inline_wrapper_routes_identically() {
        let routed = evaluate(
            &policy(),
            &bash_call("env chmod 777 x"),
            &Context::default(),
        );
        assert!(matches!(routed.decision, Decision::Deny(_)));

        let sh_wrapped = evaluate(
            &policy(),
            &bash_call("sh -c 'chmod 777 x'"),
            &Context::default(),
        );
        assert!(matches!(sh_wrapped.decision, Decision::Deny(_)));
    }

    // -- FR-CMD-007: compound commands ---------------------------------

    #[test]
    fn strictest_decision_wins_among_a_pipeline_of_parts() {
        // chmod 777 (deny) piped into an unmanaged command (allow): deny wins.
        let routed = evaluate(
            &policy(),
            &bash_call("chmod 777 x | wc -l"),
            &Context::default(),
        );
        assert!(matches!(routed.decision, Decision::Deny(_)));
    }

    #[test]
    fn ask_beats_proxy_in_a_pipeline() {
        let routed = evaluate(
            &policy(),
            &bash_call("gh pr merge --admin | ./notify.sh"),
            &Context::default(),
        );
        assert!(matches!(routed.decision, Decision::Ask(_)));
    }

    #[test]
    fn python_search_one_liner_routes_to_sym_find_content() {
        let command = r#"python3 -c "import pathlib; [print(f) for f in pathlib.Path('.').rglob('*.rs') if 'foo' in f.read_text()]""#;
        let routed = evaluate(&policy(), &bash_call(command), &Context::default());
        match &routed.decision {
            Decision::Deny(details) => {
                assert_eq!(details.instead(), "legion sym etc find-content")
            }
            other => panic!("expected Deny naming sym, got {other:?}"),
        }
    }

    #[test]
    fn python_traversal_without_content_read_routes_to_sym_find_file() {
        let command =
            r#"python3 -c "import pathlib; print(list(pathlib.Path('.').rglob('*.rs')))""#;
        let routed = evaluate(&policy(), &bash_call(command), &Context::default());
        match &routed.decision {
            Decision::Deny(details) => {
                assert_eq!(details.instead(), "legion sym etc find-file")
            }
            other => panic!("expected Deny naming sym, got {other:?}"),
        }
    }

    #[test]
    fn python_one_liner_that_only_reads_one_file_is_not_a_sym_job() {
        // The negative case: read_text() alone, with no traversal, is not
        // a search -- it must not match find-content (which requires
        // rglob( too) or find-file (which requires rglob( at all).
        let command = r#"python3 -c "import pathlib; print(pathlib.Path('x').read_text())""#;
        let routed = evaluate(&policy(), &bash_call(command), &Context::default());
        // Not a sym job: falls through to the ordinary opaque-interpreter
        // proxy, never a deny naming a sym command.
        assert_eq!(
            routed.decision,
            Decision::Proxy {
                reason: ProxyReason::Opaque
            }
        );
    }

    #[test]
    fn sym_job_decision_is_never_overridden_by_a_weaker_or_stronger_sibling_part() {
        // The sym job is decided before the fold, so a sibling opaque
        // region (which would also proxy) cannot change the outcome.
        let command = r#"python3 -c "import pathlib; [print(f) for f in pathlib.Path('.').rglob('*.rs') if 'foo' in f.read_text()]" | ./other.sh"#;
        let routed = evaluate(&policy(), &bash_call(command), &Context::default());
        match &routed.decision {
            Decision::Deny(details) => {
                assert_eq!(details.instead(), "legion sym etc find-content")
            }
            other => panic!("expected the sym job's Deny to win, got {other:?}"),
        }
        assert!(matches!(routed.entry, DecidingEntry::Rule { .. }));
    }

    // -- Parse errors route ask (FR-CMD-006, FR-CMD-007) --------------------

    #[test]
    fn unparseable_command_routes_ask() {
        let routed = evaluate(
            &policy(),
            &bash_call("echo 'unterminated"),
            &Context::default(),
        );
        assert!(matches!(routed.decision, Decision::Ask(_)));
        assert!(matches!(routed.entry, DecidingEntry::ParseError(_)));
    }

    // -- FR-CMD-016 defaults --------------------------------------------

    #[test]
    fn no_managed_binary_and_no_matching_rule_yields_allow() {
        let routed = evaluate(&policy(), &bash_call("wc -l file.txt"), &Context::default());
        assert!(matches!(routed.decision, Decision::Allow { note: Some(_) }));
    }

    #[test]
    fn managed_rule_that_cannot_resolve_yields_deny() {
        // "chmod" is a managed family, but its only rule requires "777";
        // "chmod 644 x" matches no rule in the family.
        let routed = evaluate(&policy(), &bash_call("chmod 644 x"), &Context::default());
        match &routed.decision {
            Decision::Deny(details) => {
                assert_eq!(details.reason(), default_messages::UNRESOLVED_MANAGED_RULE)
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn missing_required_recall_yields_deny_not_the_rules_own_outcome() {
        let routed = evaluate(
            &policy(),
            &bash_call("gh --recall-gated"),
            &Context::default(),
        );
        match &routed.decision {
            Decision::Deny(details) => {
                assert_eq!(details.reason(), default_messages::MISSING_LOOKUP)
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn present_recall_lets_the_rules_own_outcome_apply() {
        let ctx = Context {
            recall: Lookup::Found(vec!["hit".to_string()]),
            ..Context::default()
        };
        let routed = evaluate(&policy(), &bash_call("gh --recall-gated"), &ctx);
        assert!(matches!(routed.decision, Decision::Allow { .. }));
    }

    #[test]
    fn empty_policy_denies_every_command() {
        let empty = parse_policy("{}").expect("empty policy parses");
        let routed = evaluate(&empty, &bash_call("ls -la"), &Context::default());
        match &routed.decision {
            Decision::Deny(details) => {
                assert_eq!(details.reason(), default_messages::EMPTY_POLICY)
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn empty_policy_still_asks_on_a_parse_error() {
        let empty = parse_policy("{}").expect("empty policy parses");
        let routed = evaluate(
            &empty,
            &bash_call("echo 'unterminated"),
            &Context::default(),
        );
        assert!(matches!(routed.decision, Decision::Ask(_)));
    }

    // -- Fields tools (Edit/Write/Read/Grep/Agent) --------------------------

    #[test]
    fn fields_tool_rule_matches_a_json_field() {
        let routed = evaluate(
            &policy(),
            &tool_call(
                "Edit",
                serde_json::json!({"file_path": "config/.env.local"}),
            ),
            &Context::default(),
        );
        assert!(matches!(routed.decision, Decision::Ask(_)));
    }

    #[test]
    fn fields_tool_governed_but_unresolved_denies() {
        // Edit is a governed tool kind (it has a Fields rule list), but no
        // rule in that list matches "src/main.rs" -- the same
        // unresolvable-managed-rule default a Bash family gives when none
        // of its rules match (FR-CMD-016).
        let routed = evaluate(
            &policy(),
            &tool_call("Edit", serde_json::json!({"file_path": "src/main.rs"})),
            &Context::default(),
        );
        match &routed.decision {
            Decision::Deny(details) => {
                assert_eq!(details.reason(), default_messages::UNRESOLVED_MANAGED_RULE)
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn unmanaged_tool_kind_yields_allow() {
        let routed = evaluate(
            &policy(),
            &tool_call(
                "WebFetch",
                serde_json::json!({"url": "https://example.com"}),
            ),
            &Context::default(),
        );
        assert!(matches!(routed.decision, Decision::Allow { .. }));
    }

    #[test]
    fn ungoverned_tool_kind_yields_allow() {
        // "Read" is a real ToolKind, but the policy has no entry for it at
        // all -- unlike Edit above, this is the no-managed-rule allow
        // default, not the unresolved-managed-rule deny default.
        let routed = evaluate(
            &policy(),
            &tool_call("Read", serde_json::json!({"file_path": "x"})),
            &Context::default(),
        );
        assert!(matches!(routed.decision, Decision::Allow { .. }));
    }

    // -- Facts extraction (FR-CMD-003) ---------------------------------

    #[test]
    fn facts_are_populated_from_a_real_command() {
        // "#1227" is quoted: unquoted, bash would read a bare `#` as the
        // start of a comment and never hand it to the process as an
        // argument at all.
        let routed = evaluate(
            &policy(),
            &bash_call(r#"grep foo src/main.rs "relates to #1227""#),
            &Context::default(),
        );
        assert_eq!(routed.facts.verb.as_deref(), Some("grep"));
        assert!(routed.facts.paths.contains(&"src/main.rs".to_string()));
        assert!(routed.facts.keywords.contains(&"foo".to_string()));
        assert_eq!(routed.facts.issue_numbers, vec![1227]);
    }

    #[test]
    fn empty_command_falls_through_to_allow_default() {
        let routed = evaluate(&policy(), &bash_call(""), &Context::default());
        assert!(matches!(routed.decision, Decision::Allow { .. }));
    }
}
