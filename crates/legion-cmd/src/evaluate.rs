//! The one evaluator (FR-CMD-011): the only code in this crate that turns a
//! [`Policy`] into a [`Decision`]. `route` (in `route.rs`) is a thin
//! wrapper around [`evaluate`]; the adapter and the ledger call `route`,
//! never this module, so every routing branch lives here and nowhere else.

use std::collections::BTreeMap;

use crate::decision::{
    Context, DecidingEntry, Decision, Facts, Lookup, ProxyReason, Routed, ToolCall,
};
use crate::policy::{Family, MatchInput, Policy, RequiredLookup, Rule, ToolKind, ToolRules};
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
    // returned before the strictest-order fold below, deliberately: today
    // every sym job yields Deny, which the fold would also produce on its
    // own, but #1228 turns the lossless cases into Rewrite -- weaker than
    // Proxy in fold order -- so once that lands, a sym-job Rewrite folded
    // alongside an unrelated Proxy part would wrongly lose to the Proxy.
    // Deciding the sym job first, outside the fold, keeps "never allowed
    // or proxied" true regardless of what #1228 changes.
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

    let families = bash_families(policy);
    let mut parts: Vec<(Decision, DecidingEntry)> = scan
        .invocations
        .iter()
        .map(|inv| evaluate_invocation(families, inv, ctx))
        .collect();
    parts.extend(scan.opaque.iter().map(|_| opaque_part()));

    fold(parts, facts)
}

fn bash_families(policy: &Policy) -> &BTreeMap<String, Family> {
    static EMPTY: BTreeMap<String, Family> = BTreeMap::new();
    match policy.tools.get(&ToolKind::Bash) {
        Some(ToolRules::Bash { families }) => families,
        _ => &EMPTY,
    }
}

/// Finds the family governing `inv`, trying the verb-scoped two-word key
/// (`"git push"`) before falling back to the binary alone (`"git"`), so a
/// family can govern one subcommand of a binary without governing every
/// other subcommand (FR-CMD-011's `"git push"` example key).
fn find_family<'p>(families: &'p BTreeMap<String, Family>, inv: &Invocation) -> Option<&'p Family> {
    if let Some(first) = inv.args.first()
        && !first.starts_with('-')
    {
        let two_word = format!("{} {first}", inv.binary);
        if let Some(family) = families.get(&two_word) {
            return Some(family);
        }
    }
    families.get(&inv.binary)
}

fn evaluate_invocation(
    families: &BTreeMap<String, Family>,
    inv: &Invocation,
    ctx: &Context,
) -> (Decision, DecidingEntry) {
    let Some(family) = find_family(families, inv) else {
        // FR-CMD-016: no managed binary and no matching rule -> allow,
        // surfaced to the agent.
        return (
            allow_default(default_messages::NO_MANAGED_BINARY),
            DecidingEntry::Default,
        );
    };
    evaluate_rules(&family.rules, MatchInput::Args(&inv.args), ctx)
}

/// Walks `rules` in declared order, returning the first whose predicate
/// matches and whose required lookups are satisfied. A rule whose
/// predicate matches but whose required lookup is missing counts as
/// resolved-but-blocked (FR-CMD-016's missing-lookup default), not as a
/// non-match that falls through to the next rule.
fn evaluate_rules(
    rules: &[Rule],
    input: MatchInput<'_>,
    ctx: &Context,
) -> (Decision, DecidingEntry) {
    for rule in rules {
        if !rule.predicate.matches(input) {
            continue;
        }
        if missing_required_lookup(&rule.requires, ctx) {
            return (
                deny_default(default_messages::MISSING_LOOKUP),
                DecidingEntry::Default,
            );
        }
        return (
            rule.decision.clone(),
            DecidingEntry::Rule {
                id: rule.id.clone(),
                needs_operator: rule.needs_operator,
            },
        );
    }
    // FR-CMD-016: a managed rule (this binary/field set is governed by a
    // family or a Fields rule list) that cannot resolve -> deny.
    (
        deny_default(default_messages::UNRESOLVED_MANAGED_RULE),
        DecidingEntry::Default,
    )
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
        DecidingEntry::Default,
    )
}

/// Checks every sym job against the whole command, in declared order,
/// returning the first one that matches. An [`crate::policy::SymJobMatcher::Invocation`]
/// job is checked against every resolved [`Invocation`] (any position or
/// depth, so `sh -c` and pipelines count); an
/// [`crate::policy::SymJobMatcher::InterpreterPatterns`] job is checked
/// against every [`Opaque::Interpreter`] body, requiring all of its
/// patterns to appear. Declared order matters for both: a policy author
/// lists a job's more specific match before a broader one so the more
/// specific job wins when both would otherwise match the same command.
fn find_sym_job(
    sym_jobs: &[crate::policy::SymJob],
    invocations: &[Invocation],
    opaque: &[Opaque],
) -> Option<(String, String, String)> {
    for job in sym_jobs {
        match &job.matcher {
            crate::policy::SymJobMatcher::Invocation { binary, predicate } => {
                let matched = invocations.iter().find(|inv| {
                    &inv.binary == binary && predicate.matches(MatchInput::Args(&inv.args))
                });
                if matched.is_some() {
                    let description = format!("invocation of {binary}");
                    return Some((job.id.clone(), job.sym_command.clone(), description));
                }
            }
            crate::policy::SymJobMatcher::InterpreterPatterns(patterns) => {
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
                                    "target": "legion issue list",
                                    "reason": "gh issue list duplicates legion's issue tracking surface"
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
            Decision::Rewrite { target, .. } => assert_eq!(target.as_str(), "legion issue list"),
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
