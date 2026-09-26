//! The one routing entry point (FR-CMD-001, FR-CMD-011, NFR-CMD-001).
//!
//! [`route`] takes a tool call and context, splits a Bash command with
//! [`crate::splitter::scan`], re-enters the payloads the policy says are
//! wrapped, evaluates every invocation and unreduced region through the one
//! evaluator, folds them into a single Decision, and returns it with the facts
//! it extracted. It performs no I/O and delegates to no process or engine
//! outside legion-cmd (FR-CMD-014): its output depends only on its inputs.

use serde_json::Value;

use crate::Context;
use crate::decision::{Deciding, Decision, Facts, Routed, ToolCall};
use crate::evaluate::{self, PartOutcome};
use crate::nogo;
use crate::policy::{BodyLanguage, Policy, ToolKind};
use crate::splitter::{self, Invocation, ScanError, Unreduced, UnreducedReason};

/// The one routing entry point. Pure (NFR-CMD-001): no filesystem, network, or
/// database, and its output depends only on `policy`, `call` and `ctx`.
pub fn route(policy: &Policy, call: &ToolCall, ctx: &Context) -> Routed {
    match call.tool.as_str() {
        "Bash" => route_bash(policy, call, ctx),
        other => {
            // An empty or unparsable policy denies every command (FR-CMD-016).
            // An unparsable policy never becomes a `Policy`, so the adapter
            // denies before calling route; an empty one is caught here.
            if policy.is_empty() {
                return empty_policy_deny();
            }
            route_fields(policy, other, call, ctx)
        }
    }
}

/// The Bash command a tool call carries, or `""` when `input` has no string
/// `command` field. Shared with the lookup pre-pass so both read the same
/// field the same way.
pub(crate) fn bash_command(call: &ToolCall) -> &str {
    call.input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn route_bash(policy: &Policy, call: &ToolCall, ctx: &Context) -> Routed {
    let command: &str = bash_command(call);
    let expanded = expand_command(policy, command);
    // The command's key, once: a no-go hit and a confirmation both carry it.
    let command_key = nogo::command_key(command).ok();

    // The no-go list is checked before any other policy entry (FR-CMD-025),
    // and before the empty-policy deny, so the built-in entries hold when the
    // policy file is empty. Nothing below can override a match: no
    // confirmation, operator prompt, or other entry is consulted.
    if let Some((hit, mut facts)) = no_go_hit(policy, command, expanded.as_ref().ok()) {
        facts.command_key = command_key;
        return Routed {
            decision: hit.decision,
            facts,
            deciding: hit.deciding,
            confirmed: false,
        };
    }

    // An empty policy denies every other command (FR-CMD-016).
    if policy.is_empty() {
        return empty_policy_deny();
    }
    if command.trim().is_empty() {
        return allow_routed();
    }

    let expanded = match expanded {
        Ok(expanded) => expanded,
        // A parse error of the command the agent ran routes ask (FR-CMD-006):
        // the command is refused with a question and a reason to the agent.
        Err(err) => {
            return Routed {
                decision: ask(
                    "this command could not be parsed; drop it or confirm it",
                    format!("the shell parser rejected the command: {err}"),
                ),
                facts: Facts::default(),
                deciding: Deciding::ParseError,
                confirmed: false,
            };
        }
    };

    let mut parts: Vec<PartOutcome> = Vec::new();
    for invocation in &expanded.invocations {
        let part =
            evaluate::decide_bash_invocation(policy, &invocation.binary, &invocation.args, ctx);
        parts.push(evaluate::refuse_rewrite_dropping_shell_words(
            part, invocation,
        ));
    }
    for region in &expanded.regions {
        parts.push(evaluate::decide_region(policy, region));
    }

    let confirmation: Option<&String> = command_key
        .as_ref()
        .and_then(|key| ctx.confirmations.get(key));
    let answered: bool = confirmation.is_some() && parts.iter().any(is_rule_ask);
    if let Some(reason) = confirmation {
        parts = answer_asks(parts, reason);
    }

    let mut winner = evaluate::combine(parts);
    // A rewrite replaces the whole command, so it is kept only for exactly one
    // simple command (FR-CMD-008); the shape comes from route's own parse, so
    // the adapter never scans the command (FR-CMD-017).
    if !expanded.single_simple {
        winner = evaluate::refuse_compound_rewrite(winner);
    }
    let mut facts = extract_facts(&expanded.invocations, winner.verb.clone());
    facts.command_key = command_key;
    // A confirmation is used only when the command goes on to run or to the
    // operator prompt; a deny -- from another part, or from the compound
    // refusal above -- leaves it unused.
    let confirmed = answered && !matches!(winner.decision, Decision::Deny(_));
    Routed {
        decision: winner.decision,
        facts,
        deciding: winner.deciding,
        confirmed,
    }
}

/// The no-go entry `command` hits, with the facts of the expansion it hit in.
/// Checked twice: over `expanded`, the policy's own expansion, and over the
/// expansion by [`Policy::no_go_resolver`], which resolves the built-in
/// wrappers and shell interpreters whether or not the policy file declares
/// them (FR-CMD-025). Either hit refuses, so a policy file can add wrapper
/// resolution but never take the built-in resolution away. The second scan
/// is skipped when the policy already declares every built-in resolver
/// first, as the shipped file does. Pure: at most two scans of the same
/// string, no I/O (NFR-CMD-001).
fn no_go_hit(
    policy: &Policy,
    command: &str,
    expanded: Option<&Expanded>,
) -> Option<(PartOutcome, Facts)> {
    let hit_in = |expanded: &Expanded| {
        evaluate::decide_no_go(policy, &expanded.invocations)
            .map(|hit| (hit, extract_facts(&expanded.invocations, None)))
    };
    expanded.and_then(hit_in).or_else(|| {
        let resolver: Policy = policy.no_go_resolver()?;
        let resolved: Expanded = expand_command(&resolver, command).ok()?;
        hit_in(&resolved)
    })
}

/// True when `part` is an ask a policy entry produced -- the only ask a
/// confirmation answers (a parse error has no key, so it is never confirmed).
fn is_rule_ask(part: &PartOutcome) -> bool {
    matches!(part.decision, Decision::Ask(_)) && matches!(part.deciding, Deciding::Rule { .. })
}

/// Treats every policy ask in `parts` as answered by the agent's confirmation
/// (FR-CMD-026, FR-CMD-006). An entry not marked as needing the operator
/// becomes allow, so the command earns whatever its other parts decide. A
/// marked entry becomes an ask with the operator mark set, carrying the
/// agent's `reason` for the operator prompt.
fn answer_asks(parts: Vec<PartOutcome>, reason: &str) -> Vec<PartOutcome> {
    parts
        .into_iter()
        .map(|part| {
            let (Decision::Ask(details), Deciding::Rule { id, .. }) =
                (&part.decision, &part.deciding)
            else {
                return part;
            };
            let (decision, needs_operator) = if part.operator_mark {
                (ask(details.question(), reason), true)
            } else {
                (Decision::Allow { note: None }, false)
            };
            PartOutcome {
                decision,
                deciding: Deciding::Rule {
                    id: id.clone(),
                    needs_operator,
                },
                ..part
            }
        })
        .collect()
}

fn route_fields(policy: &Policy, tool: &str, call: &ToolCall, ctx: &Context) -> Routed {
    let Some(kind) = ToolKind::ALL.into_iter().find(|k| k.as_str() == tool) else {
        // A tool the policy structure does not model routes to the allow
        // default: nothing managed applies to it.
        return allow_routed();
    };
    let outcome = evaluate::decide_fields(policy, kind, &call.input, ctx);
    Routed {
        decision: outcome.decision,
        facts: Facts::default(),
        deciding: outcome.deciding,
        confirmed: false,
    }
}

/// The invocations and unreduced regions a command reduces to, after every
/// wrapper and shell-interpreter payload the policy names has been re-entered.
#[derive(Default)]
pub(crate) struct Expanded {
    pub(crate) invocations: Vec<Invocation>,
    regions: Vec<Unreduced>,
    /// True when the command is exactly one simple command, optionally behind
    /// declared wrappers or inside a shell interpreter body that is itself
    /// exactly one simple command, the same rule applied at every re-entry.
    single_simple: bool,
}

/// Splits `command` and re-enters every wrapper and interpreter payload the
/// policy names. This is the one scan route performs. The lookup pre-pass
/// ([`crate::lookups`]) calls this same function, so the rules it consults are
/// exactly the rules route will evaluate; no caller outside this crate splits
/// the command (FR-CMD-003, FR-CMD-017).
pub(crate) fn expand_command(policy: &Policy, command: &str) -> Result<Expanded, ScanError> {
    let scan = splitter::scan(command)?;
    let mut expanded = Expanded {
        single_simple: scan.single_simple,
        ..Expanded::default()
    };
    expand(policy, scan.invocations, scan.unreduced, &mut expanded);
    Ok(expanded)
}

/// Re-enters wrapper and interpreter payloads (FR-CMD-007).
///
/// Each invocation is classified against the policy: a wrapper's payload and a
/// shell interpreter's body are re-entered through
/// [`crate::splitter::scan_at`], a foreign interpreter body and a script file
/// are recorded as unreduced regions, and everything else is a plain
/// invocation the evaluator decides.
///
/// Re-entry passes the invocation's depth plus one, never the depth unchanged:
/// [`scan_at`] tags the payload's top-level commands at exactly the depth it is
/// given, so passing the same depth would let `sh -c 'sh -c "..."'` re-enter
/// forever at one depth. Adding one makes the depth rise with each re-entry, so
/// the chain terminates at [`crate::MAX_DEPTH`], where the splitter returns the
/// region as `TooDeep` rather than an invocation (FR-CMD-007).
///
/// [`scan_at`]: crate::splitter::scan_at
fn expand(
    policy: &Policy,
    invocations: Vec<Invocation>,
    regions: Vec<Unreduced>,
    out: &mut Expanded,
) {
    out.regions.extend(regions);
    for invocation in invocations {
        classify(policy, invocation, out);
    }
}

fn classify(policy: &Policy, invocation: Invocation, out: &mut Expanded) {
    let next_depth = invocation.depth.saturating_add(1);
    let first_inner = out.invocations.len();

    if let Some(wrapper) = policy.matching_wrapper(&invocation.binary, &invocation.args) {
        // The wrapper's own options and operands are consumed by its
        // declaration (#1286). Words the declaration cannot account for leave
        // route unable to say where the wrapped command starts, so the
        // invocation is proxied opaque -- never allowed as unmanaged.
        let Some(start) = wrapper.payload_start(&invocation.args) else {
            out.regions.push(Unreduced {
                text: invocation.args.join(" "),
                reason: UnreducedReason::WrapperPayload,
                depth: invocation.depth,
            });
            return;
        };
        let payload = &invocation.args[start..];
        // A wrapper with no payload words (a bare `env`, `pnpm exec` with
        // nothing after it) wraps no command: there is nothing to route, so it
        // contributes no part and reaches the allow default -- not an opaque
        // proxy, which is reserved for a command route genuinely cannot read.
        if !payload.is_empty() {
            reenter_text(policy, &payload.join(" "), next_depth, out);
            inherit_shell_words(&invocation, &mut out.invocations[first_inner..]);
        }
        return;
    }

    // An interpreter with an inline body is handled here. An interpreter with
    // no inline body (the flag is absent) falls through: it may be running a
    // script file instead, which the script-carrier check below records.
    if let Some(interpreter) = policy.matching_interpreter(&invocation.binary)
        && let Some(body) = body_after_flag(&invocation.args, &interpreter.flag)
    {
        // The splitter hands back the -c argument with its outer quoting intact
        // (`'grep ...'`), but the value the interpreter receives is the content
        // between those quotes. Strip one matched outer pair so a shell body
        // re-enters as the command it is, not as one quoted word named after
        // the whole line.
        let body = evaluate::dequote_outer(body);
        match interpreter.body {
            BodyLanguage::Shell => {
                reenter_text(policy, body, next_depth, out);
                inherit_shell_words(&invocation, &mut out.invocations[first_inner..]);
            }
            BodyLanguage::Foreign => {
                out.regions.push(Unreduced {
                    text: body.to_string(),
                    reason: UnreducedReason::InterpreterBody,
                    depth: next_depth,
                });
            }
        }
        return;
    }

    if policy.matching_script_carrier(&invocation.binary).is_some()
        && invocation.args.iter().any(|a| !a.starts_with('-'))
    {
        out.regions.push(Unreduced {
            text: invocation.args.join(" "),
            reason: UnreducedReason::ScriptFile,
            depth: invocation.depth,
        });
        return;
    }

    out.invocations.push(invocation);
}

/// Marks every invocation re-entered from `outer` with the redirect and the
/// assignment prefix `outer` carries: `env gh pr list > out.txt` redirects the
/// command `env` runs, and `FOO=1 env gh pr list` hands it `FOO`. Each
/// invocation keeps whatever it already carries of its own.
fn inherit_shell_words(outer: &Invocation, inner: &mut [Invocation]) {
    for invocation in inner {
        invocation.redirected |= outer.redirected;
        invocation.assigned |= outer.assigned;
    }
}

/// Re-enters `text` as a shell command. A payload the splitter rejects is
/// recorded `Unparsed` and proxies opaque -- a malformed wrapper payload is not
/// an ask (FR-CMD-007): re-entering it changes who called `scan_at`, not what
/// the region is.
///
/// The command stays one simple command only if the re-entered text is itself
/// one simple command; anything else -- a list, an empty or assignment-only
/// body, a payload the splitter rejects -- makes it compound (FR-CMD-008).
fn reenter_text(policy: &Policy, text: &str, depth: u8, out: &mut Expanded) {
    match splitter::scan_at(text, depth) {
        Ok(scan) => {
            out.single_simple &= scan.single_simple;
            expand(policy, scan.invocations, scan.unreduced, out)
        }
        Err(_) => {
            out.single_simple = false;
            out.regions.push(Unreduced {
                text: text.to_string(),
                reason: UnreducedReason::Unparsed,
                depth,
            });
        }
    }
}

/// The argument immediately following `flag`, e.g. the body of `sh -c <body>`.
fn body_after_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// Extracts the facts route returns so no caller parses the command again
/// (FR-CMD-003): the verb the winning part named, the path-shaped operands, and
/// the issue numbers. Kept deliberately small; rules that need richer facts
/// extract them as they are added.
fn extract_facts(invocations: &[Invocation], verb: Option<String>) -> Facts {
    let mut paths = Vec::new();
    let mut issue_numbers = Vec::new();
    let mut keywords = Vec::new();
    for invocation in invocations {
        for raw in &invocation.args {
            // Compare against the value the shell sees, not the raw quoted text.
            let arg = evaluate::dequote_outer(raw);
            if arg.starts_with('-') {
                continue;
            }
            if let Some(number) = issue_number(arg) {
                issue_numbers.push(number);
            } else if arg.contains('/') {
                paths.push(arg.to_string());
            } else {
                keywords.push(arg.to_string());
            }
        }
    }
    Facts {
        paths,
        verb,
        issue_numbers,
        keywords,
        command_key: None,
    }
}

/// Parses a `#123` issue reference. The `#` prefix is required: a bare integer
/// is an ordinary operand (a `timeout 30` duration, a port number), not an
/// issue number, so it is not misread as one.
fn issue_number(arg: &str) -> Option<u64> {
    arg.strip_prefix('#')?.parse::<u64>().ok()
}

fn empty_policy_deny() -> Routed {
    Routed {
        decision: deny(
            "the routing policy is empty until it is populated",
            "populate the policy, then retry",
        ),
        facts: Facts::default(),
        deciding: Deciding::Default,
        confirmed: false,
    }
}

fn allow_routed() -> Routed {
    Routed {
        decision: Decision::Allow { note: None },
        facts: Facts::default(),
        deciding: Deciding::Default,
        confirmed: false,
    }
}

// The literals passed at every call site are non-empty, so these constructors
// never fail today. The fallback is `Proxy { Opaque }`, not `Allow`: if a
// future edit ever threaded an empty string through, the command would be
// recorded unreadable rather than silently permitted -- fail closed, matching
// the same helpers in evaluate.rs and FR-CMD-016's "never a silent allow".
fn deny(reason: &str, instead: &str) -> Decision {
    Decision::deny(reason, instead).unwrap_or(Decision::Proxy {
        reason: crate::decision::ProxyReason::Opaque,
    })
}

fn ask(question: &str, reason: impl Into<String>) -> Decision {
    Decision::ask(question, reason).unwrap_or(Decision::Proxy {
        reason: crate::decision::ProxyReason::Opaque,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lookup;
    use crate::decision::ProxyReason;
    use crate::parse_policy;

    fn policy(text: &str) -> Policy {
        parse_policy(text).expect("valid policy")
    }

    fn bash(command: &str) -> ToolCall {
        ToolCall {
            tool: "Bash".to_string(),
            input: serde_json::json!({ "command": command }),
        }
    }

    /// A policy that routes `git push` to ask, `grep` to a sym job, marks `curl`
    /// a proxy, and needs a recall result for `gh pr merge`. Enough to exercise
    /// every arm and every visibility path.
    fn sample_policy() -> Policy {
        policy(
            r#"{
            "sym_jobs": [
                {"id": "find-content", "sym_command": "legion sym find-content",
                 "interpreter_patterns": ["rglob", "read_text"]}
            ],
            "wrappers": [
                {"binary": "env"},
                {"binary": "pnpm", "required_subcommand": "exec"}
            ],
            "interpreters": [
                {"binary": "sh", "flag": "-c", "body": "shell"},
                {"binary": "python3", "flag": "-c", "body": "foreign"}
            ],
            "script_carriers": [{"binary": "bash"}],
            "tools": {"Bash": {"families": {
                "git push": {"rules": [
                    {"id": "git-push", "outcome": {"kind": "ask", "question": "push?",
                     "reason": "push rewrites history", "needs_operator": true}}
                ]},
                "grep": {"rules": [
                    {"id": "grep-search", "outcome": {"kind": "sym", "job": "find-content"}}
                ]},
                "curl": {"rules": [
                    {"id": "curl-proxy", "outcome": {"kind": "proxy", "reason": "binary"}}
                ]}
            }}}
        }"#,
        )
    }

    fn decide(command: &str) -> Routed {
        route(&sample_policy(), &bash(command), &Context::default())
    }

    #[test]
    fn empty_policy_denies_every_command() {
        let empty = policy("{}");
        let routed = route(&empty, &bash("echo hi"), &Context::default());
        assert!(matches!(routed.decision, Decision::Deny(_)));
    }

    #[test]
    fn a_command_with_no_managed_binary_is_allowed() {
        // FR-CMD-016: echo reaches the allow default.
        assert_eq!(
            decide("echo hello").decision,
            Decision::Allow { note: None }
        );
    }

    #[test]
    fn a_parse_error_routes_ask() {
        let routed = decide("grep 'unterminated");
        assert!(matches!(routed.decision, Decision::Ask(_)));
        assert_eq!(routed.deciding, Deciding::ParseError);
    }

    /// FR-CMD-007: a managed binary is routed identically wherever the splitter
    /// resolves it -- first position, after a pipe, behind an env prefix,
    /// inside an inline wrapper, inside sh -c. This one test covers three
    /// quoted criteria plus NFR-CMD-002's visibility paths.
    #[test]
    fn a_managed_binary_is_routed_identically_across_every_visibility_path() {
        let baseline = decide("grep -rn foo src").decision;
        assert!(matches!(baseline, Decision::Deny(_)), "grep routes to sym");
        for command in [
            "cat x | grep -rn foo src",
            "FOO=1 grep -rn foo src",
            "env grep -rn foo src",
            "sh -c 'grep -rn foo src'",
        ] {
            assert_eq!(
                decide(command).decision,
                baseline,
                "`{command}` must route identically to first position"
            );
        }
    }

    #[test]
    fn a_python_one_liner_that_searches_files_routes_to_sym() {
        let routed = decide(
            "python3 -c \"import pathlib; [print(f) for f in pathlib.Path('.').rglob('*.rs') if 'foo' in f.read_text()]\"",
        );
        match routed.decision {
            Decision::Deny(details) => assert_eq!(details.instead(), "legion sym find-content"),
            other => panic!("expected deny naming sym, got {other:?}"),
        }
    }

    #[test]
    fn a_sh_one_liner_that_searches_files_routes_to_sym() {
        let routed = decide("sh -c 'grep -rn foo src'");
        match routed.decision {
            Decision::Deny(details) => assert_eq!(details.instead(), "legion sym find-content"),
            other => panic!("expected deny naming sym, got {other:?}"),
        }
    }

    #[test]
    fn grep_piped_to_head_routes_to_sym() {
        // FR-CMD-007: `grep -rn foo . | head` is routed to sym.
        match decide("grep -rn foo . | head").decision {
            Decision::Deny(details) => assert_eq!(details.instead(), "legion sym find-content"),
            other => panic!("expected deny naming sym, got {other:?}"),
        }
    }

    #[test]
    fn a_managed_binary_through_a_js_runner_is_routed_like_a_wrapped_command() {
        // pnpm exec grep -> grep re-entered -> sym.
        match decide("pnpm exec grep -rn foo src").decision {
            Decision::Deny(details) => assert_eq!(details.instead(), "legion sym find-content"),
            other => panic!("expected deny naming sym, got {other:?}"),
        }
    }

    #[test]
    fn a_bare_pnpm_subcommand_is_an_ordinary_invocation() {
        // `pnpm grep` is not a runner: no exec/dlx word, so grep is pnpm's
        // argument, not a command. pnpm is unmanaged here -> allow.
        assert_eq!(decide("pnpm grep").decision, Decision::Allow { note: None });
    }

    #[test]
    fn a_compound_whose_job_sym_does_not_serve_takes_the_strictest_part() {
        // A pipeline with an ask part (git push) and a proxy part (curl) is
        // asked (FR-CMD-007 strictest order: ask outranks proxy).
        match decide("git push origin main | curl example.com").decision {
            Decision::Ask(_) => {}
            other => panic!("expected ask, got {other:?}"),
        }
    }

    #[test]
    fn deeply_nested_wrapper_re_entry_terminates_and_proxies_opaque() {
        // Re-entry must terminate, not loop. Nested `env` wrappers rise one
        // depth per re-entry (with no quote blowup); past MAX_DEPTH the
        // splitter returns the region as TooDeep, which is proxied opaque,
        // never allowed. `env` repeated well past 25 exercises the bound.
        let command = format!("{}grep -rn foo src", "env ".repeat(40));
        let routed = route(&sample_policy(), &bash(&command), &Context::default());
        assert_eq!(
            routed.decision,
            Decision::Proxy {
                reason: ProxyReason::Opaque
            }
        );
    }

    #[test]
    fn a_two_level_sh_c_re_entry_reaches_the_inner_binary() {
        // `sh -c 'sh -c "grep ..."'` re-enters twice and still routes the
        // inner grep to sym: depth rises but stays under the bound.
        match decide("sh -c 'sh -c \"grep -rn foo src\"'").decision {
            Decision::Deny(details) => assert_eq!(details.instead(), "legion sym find-content"),
            other => panic!("expected deny naming sym, got {other:?}"),
        }
    }

    #[test]
    fn an_ask_rule_marked_needing_operator_produces_an_unset_mark() {
        let routed = decide("git push origin main");
        assert!(matches!(routed.decision, Decision::Ask(_)));
        assert_eq!(
            routed.deciding,
            Deciding::Rule {
                id: "git-push".to_string(),
                needs_operator: false
            }
        );
    }

    #[test]
    fn a_missing_required_recall_result_denies() {
        let p = policy(
            r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
                {"id": "gh-recall", "requires_recall": true, "outcome": {"kind": "allow"}}
            ]}}}}}"#,
        );
        let routed = route(&p, &bash("gh issue list"), &Context::default());
        assert!(matches!(routed.decision, Decision::Deny(_)));

        let ctx = Context {
            recall: Lookup::Empty,
            ..Context::default()
        };
        let routed = route(&p, &bash("gh issue list"), &ctx);
        assert_eq!(routed.decision, Decision::Allow { note: None });
    }

    #[test]
    fn facts_carry_the_verb_and_paths() {
        let routed = decide("git push origin main -- src/a.rs");
        assert_eq!(routed.facts.verb.as_deref(), Some("push"));
        assert!(routed.facts.paths.contains(&"src/a.rs".to_string()));
    }

    #[test]
    fn issue_numbers_require_a_hash_prefix() {
        // A quoted `"#123"` is an issue number (a bare `#123` would be a shell
        // comment); a bare integer (a timeout, a port) is not misread as one.
        let hashed = decide("gh issue view \"#123\"");
        assert_eq!(hashed.facts.issue_numbers, vec![123]);
        let bare = decide("gh issue view 30");
        assert!(bare.facts.issue_numbers.is_empty());
    }

    #[test]
    fn a_quoted_managed_binary_is_still_routed() {
        // `git "push"` must reach the ask rule, not slip past a quoted word.
        assert!(matches!(
            decide("git \"push\" origin").decision,
            Decision::Ask(_)
        ));
    }

    #[test]
    fn a_valueless_global_option_before_the_subcommand_is_still_routed() {
        assert!(matches!(
            decide("git --no-pager push origin main").decision,
            Decision::Ask(_)
        ));
    }

    #[test]
    fn a_bare_wrapper_with_no_payload_is_allowed_not_proxied() {
        // `env` alone wraps no command: nothing to route -> allow, not opaque.
        assert_eq!(decide("env").decision, Decision::Allow { note: None });
    }

    /// Wrappers declaring their own options and operands the way the shipped
    /// policy does, plus deny families for the commands the review of #1283
    /// reached through them, so a wrapped command that slipped past its
    /// wrapper would show as an allow rather than pass vacuously (#1286).
    fn wrapper_args_policy() -> Policy {
        policy(
            r#"{
            "sym_jobs": [
                {"id": "find-content", "sym_command": "legion sym find-content"}
            ],
            "wrappers": [
                {"binary": "pnpm", "required_subcommand": "exec",
                 "flags": ["-r", "--recursive"], "value_options": ["-C", "--filter"]},
                {"binary": "timeout", "flags": ["--foreground", "-v"],
                 "value_options": ["-k", "--kill-after", "-s", "--signal"], "operands": 1},
                {"binary": "stdbuf", "value_options": ["-i", "-o", "-e", "--output"]},
                {"binary": "xargs", "flags": ["-0", "-r", "-t"],
                 "value_options": ["-I", "-n", "-P"]},
                {"binary": "sudo", "flags": ["-E", "-n"], "value_options": ["-u", "--user", "-g"]},
                {"binary": "env", "flags": ["-", "-i"], "value_options": ["-u"]}
            ],
            "tools": {"Bash": {"families": {
                "grep": {"rules": [
                    {"id": "grep-search", "outcome": {"kind": "sym", "job": "find-content"}}
                ]},
                "chmod": {"rules": [
                    {"id": "chmod-recursive", "predicates": [{"kind": "arg-present", "arg": "-R"}],
                     "outcome": {"kind": "deny", "reason": "recursive mode change", "instead": "name the paths"}}
                ]},
                "mkfs.ext4": {"rules": [
                    {"id": "mkfs", "outcome": {"kind": "deny", "reason": "formats a device", "instead": "do not"}}
                ]},
                "git push": {"rules": [
                    {"id": "git-push", "outcome": {"kind": "ask", "question": "push?",
                     "reason": "push publishes history", "needs_operator": true}}
                ]}
            }}}
        }"#,
        )
    }

    fn decide_wrapped(command: &str) -> Decision {
        route(&wrapper_args_policy(), &bash(command), &Context::default()).decision
    }

    #[test]
    fn a_command_behind_a_wrapper_with_its_own_arguments_routes_as_the_command_alone() {
        // #1286: each command the review of #1283 found allowed receives the
        // Decision the wrapped command alone receives -- here, the deny.
        for (wrapped, alone) in [
            ("timeout 5 chmod -R 777 /", "chmod -R 777 /"),
            (
                "timeout -s KILL 5 mkfs.ext4 /dev/sda1",
                "mkfs.ext4 /dev/sda1",
            ),
            ("stdbuf -oL mkfs.ext4 /dev/sda1", "mkfs.ext4 /dev/sda1"),
            ("xargs -I{} mkfs.ext4 {}", "mkfs.ext4 {}"),
            ("sudo -u root mkfs.ext4 /dev/sda1", "mkfs.ext4 /dev/sda1"),
            ("sudo -u root git push", "git push"),
            ("timeout --signal=KILL -k 1 5 mkfs.ext4 x", "mkfs.ext4 x"),
            ("xargs -0rt -n 1 mkfs.ext4", "mkfs.ext4"),
            (
                "sudo \"-u\" root mkfs.ext4 /dev/sda1",
                "mkfs.ext4 /dev/sda1",
            ),
            ("env -i -u HOME FOO=1 mkfs.ext4 x", "mkfs.ext4 x"),
            ("sudo -- mkfs.ext4 x", "mkfs.ext4 x"),
            // A runner's options precede its subcommand.
            ("pnpm -r exec grep foo .", "grep foo ."),
            ("pnpm -C pkg --filter web exec mkfs.ext4 x", "mkfs.ext4 x"),
        ] {
            let expected = decide_wrapped(alone);
            assert!(
                !matches!(expected, Decision::Allow { .. }),
                "`{alone}` must be managed for the comparison to mean anything"
            );
            assert_eq!(
                decide_wrapped(wrapped),
                expected,
                "`{wrapped}` must route as `{alone}` does"
            );
        }
    }

    #[test]
    fn a_wrapper_whose_own_words_the_declaration_cannot_consume_proxies_opaque() {
        // #1286: an option the wrapper does not declare, a value option with
        // no value, or a missing operand leaves route unable to say where the
        // wrapped command starts -- proxied opaque, never allowed as unmanaged.
        for command in [
            "timeout --bogus 5 mkfs.ext4 /dev/sda1",
            "sudo -Z mkfs.ext4 /dev/sda1",
            "xargs -0q mkfs.ext4",
            "sudo -u",
            "timeout",
            "timeout -s",
            "env -S 'mkfs.ext4 /dev/sda1'",
            "pnpm --bogus exec mkfs.ext4 x",
            // A `--` before a runner's subcommand is a shape the declaration
            // does not model: stricter (opaque), never the allow default.
            "pnpm -- exec grep foo .",
            "pnpm -r -- exec grep foo .",
        ] {
            assert_eq!(
                decide_wrapped(command),
                Decision::Proxy {
                    reason: ProxyReason::Opaque
                },
                "`{command}` must proxy opaque"
            );
        }
    }

    #[test]
    fn a_wrapper_whose_own_words_consume_to_nothing_wraps_no_command() {
        // A declared option with no command after it wraps nothing: the
        // allow default, as for a bare `env`.
        assert_eq!(decide_wrapped("sudo -n"), Decision::Allow { note: None });
        assert_eq!(decide_wrapped("env -i"), Decision::Allow { note: None });
    }

    #[test]
    fn a_runner_binary_without_its_subcommand_in_position_stays_ordinary() {
        // `pnpm grep` names no runner; in `pnpm run exec` the word `exec` is
        // run's script name, not pnpm's subcommand. Both stay ordinary pnpm
        // invocations, which nothing manages here.
        assert_eq!(decide_wrapped("pnpm grep"), Decision::Allow { note: None });
        assert_eq!(
            decide_wrapped("pnpm run exec"),
            Decision::Allow { note: None }
        );
    }

    #[test]
    fn a_script_file_is_recorded_opaque_not_missed() {
        // FR-CMD-007: a managed binary inside a script file is an opaque proxy.
        assert_eq!(
            decide("bash deploy.sh").decision,
            Decision::Proxy {
                reason: ProxyReason::Opaque
            }
        );
    }

    /// A rewrite rule for `gh pr list` whose target carries no arguments, so
    /// the bare command is lossless and reaches the rewrite, with `env` as a
    /// wrapper and `sh -c` as a shell interpreter to re-enter it through.
    fn gh_pr_list_policy() -> Policy {
        policy(
            r#"{"wrappers": [{"binary": "env"}],
            "interpreters": [{"binary": "sh", "flag": "-c", "body": "shell"}],
            "tools": {"Bash": {"families": {
                "gh pr list": {"rules": [
                    {"id": "pr-list", "outcome": {"kind": "rewrite", "target": "legion pr list",
                     "reason": "legion tracks PRs", "translatable": {}}}
                ]}
            }}}}"#,
        )
    }

    #[test]
    fn a_rewrite_without_a_redirect_or_assignment_routes_as_its_rule_says() {
        for command in ["gh pr list", "env gh pr list", "sh -c 'gh pr list'"] {
            let routed = route(&gh_pr_list_policy(), &bash(command), &Context::default());
            match routed.decision {
                Decision::Rewrite { target, .. } => {
                    assert_eq!(target.as_str(), "legion pr list", "`{command}`")
                }
                other => panic!("`{command}`: expected rewrite, got {other:?}"),
            }
        }
    }

    /// FR-CMD-008: a rewrite replaces the whole command, so a redirect or an
    /// environment-assignment prefix would be silently dropped. The command is
    /// refused instead, naming the rule and what would have been lost.
    #[test]
    fn a_rewrite_carrying_a_redirect_or_assignment_prefix_is_refused() {
        for (command, dropped) in [
            ("gh pr list > out.txt", "redirect"),
            ("gh pr list 2>/dev/null", "redirect"),
            ("gh pr list >&2", "redirect"),
            ("GIT_TRACE=1 gh pr list", "environment-assignment prefix"),
            // On an enclosing wrapper, interpreter, subshell or group: the
            // redirect or assignment applies to the command inside it.
            ("env gh pr list > out.txt", "redirect"),
            ("FOO=1 env gh pr list", "environment-assignment prefix"),
            ("env FOO=1 gh pr list", "environment-assignment prefix"),
            ("sh -c 'gh pr list' > out.txt", "redirect"),
            ("(gh pr list) > out.txt", "redirect"),
            ("{ gh pr list; } 2>/dev/null", "redirect"),
            ("gh pr list <<EOF\nx\nEOF", "redirect"),
            ("gh pr list 3>&1", "redirect"),
            ("FOO=1 sh -c 'gh pr list'", "environment-assignment prefix"),
            ("( { gh pr list; } ) > out.txt", "redirect"),
            ("{ gh pr list | head; } > out.txt", "redirect"),
            ("f() { gh pr list; } > out.txt", "redirect"),
        ] {
            let routed = route(&gh_pr_list_policy(), &bash(command), &Context::default());
            match &routed.decision {
                Decision::Deny(details) => {
                    let reason = details.reason();
                    assert!(reason.contains("rule 'pr-list'"), "`{command}`: {reason}");
                    assert!(
                        reason.contains(&format!("{dropped} would have been dropped")),
                        "`{command}`: {reason}"
                    );
                }
                other => panic!("`{command}` must be refused, got {other:?}"),
            }
            assert_eq!(
                routed.deciding,
                Deciding::Rule {
                    id: "pr-list".to_string(),
                    needs_operator: false
                },
                "`{command}`"
            );
        }
    }

    #[test]
    fn a_malformed_wrapper_payload_proxies_opaque_not_ask() {
        // The outer command parses; the sh -c body does not. That nested
        // failure is opaque, not a parse error of the command (FR-CMD-007).
        let routed = decide("env sh -c 'grep \"unterminated'");
        assert_eq!(
            routed.decision,
            Decision::Proxy {
                reason: ProxyReason::Opaque
            }
        );
    }

    #[test]
    fn route_is_pure_over_a_non_bash_tool() {
        let p = policy(
            r#"{"tools": {"Grep": {"rules": [
                {"id": "grep-tool", "predicates": [{"kind": "field-equals", "field": "pattern", "any_of": ["secret"]}],
                 "outcome": {"kind": "deny", "reason": "no secrets", "instead": "narrow it"}}
            ]}}}"#,
        );
        let call = ToolCall {
            tool: "Grep".to_string(),
            input: serde_json::json!({ "pattern": "secret" }),
        };
        assert!(matches!(
            route(&p, &call, &Context::default()).decision,
            Decision::Deny(_)
        ));
    }

    #[test]
    fn a_fields_tool_rewrite_names_its_target_and_the_deciding_rule() {
        // An Agent spawn of the built-in Explore agent is rewritten to the
        // policy's target; route names the target and the rule, and the facts
        // stay empty -- a Fields call has no command to extract them from.
        let p = policy(
            r#"{"tools": {"Agent": {"rules": [
                {"id": "explore", "predicates": [{"kind": "field-equals", "field": "subagent_type", "any_of": ["explore"], "ignore_case": true}],
                 "outcome": {"kind": "rewrite", "target": "legion:legion-explore", "reason": "sym over grep"}}
            ]}}}"#,
        );
        let call = ToolCall {
            tool: "Agent".to_string(),
            input: serde_json::json!({ "subagent_type": "Explore", "prompt": "map the FSM" }),
        };
        let routed = route(&p, &call, &Context::default());
        match routed.decision {
            Decision::Rewrite { target, .. } => {
                assert_eq!(target.as_str(), "legion:legion-explore")
            }
            other => panic!("expected rewrite, got {other:?}"),
        }
        assert_eq!(
            routed.deciding,
            Deciding::Rule {
                id: "explore".to_string(),
                needs_operator: false
            }
        );
        assert_eq!(routed.facts, Facts::default());
    }

    /// Rewrite rules for `git push`, `git add`, `git commit`, `gh pr view` and
    /// `gh pr list`, each covering the arguments the tests pass it, with `env`
    /// and `sudo` declared as wrappers as the shipped policy declares them and
    /// `sh`/`bash` as shell interpreters.
    fn rewrite_policy() -> Policy {
        policy(
            r#"{"wrappers": [{"binary": "env"}, {"binary": "sudo"}],
            "interpreters": [
                {"binary": "sh", "flag": "-c", "body": "shell"},
                {"binary": "bash", "flag": "-c", "body": "shell"}
            ],
            "tools": {"Bash": {"families": {
                "git push": {"rules": [
                    {"id": "push-rewrite", "outcome": {"kind": "rewrite", "target": "legion push",
                     "reason": "legion pushes", "translatable": {}}}
                ]},
                "git add": {"rules": [
                    {"id": "add-rewrite", "outcome": {"kind": "rewrite", "target": "legion add",
                     "reason": "legion stages", "translatable": {"flags": ["-A"]}}}
                ]},
                "git commit": {"rules": [
                    {"id": "commit-rewrite", "outcome": {"kind": "rewrite", "target": "legion commit",
                     "reason": "legion signs", "translatable": {"valued_flags": ["-m"]}}}
                ]},
                "gh pr view": {"rules": [
                    {"id": "pr-view", "outcome": {"kind": "rewrite", "target": "legion pr view",
                     "reason": "legion tracks PRs", "translatable": {"operands": ["integer"]}}}
                ]},
                "gh pr list": {"rules": [
                    {"id": "pr-list", "outcome": {"kind": "rewrite", "target": "legion pr list",
                     "reason": "legion tracks PRs", "translatable": {}}}
                ]},
                "gh issue view": {"rules": [
                    {"id": "issue-view", "outcome": {"kind": "rewrite", "target": "legion issue view",
                     "reason": "legion tracks issues", "translatable": {"operands": ["any"]}}}
                ]}
            }}}}"#,
        )
    }

    fn route_rewrite_policy(command: &str) -> Routed {
        route(&rewrite_policy(), &bash(command), &Context::default())
    }

    /// Exactly one simple command -- bare, behind a declared wrapper, or as the
    /// whole body of a shell interpreter -- is rewritten as its rule says.
    #[test]
    fn a_single_simple_command_rewrite_routes_as_its_rule_says() {
        for command in ["git push", "env git push", "sh -c 'git push'"] {
            let routed = route_rewrite_policy(command);
            match routed.decision {
                Decision::Rewrite { target, .. } => {
                    assert_eq!(target.as_str(), "legion push", "`{command}`")
                }
                other => panic!("`{command}`: expected rewrite, got {other:?}"),
            }
            assert_eq!(
                routed.deciding,
                Deciding::Rule {
                    id: "push-rewrite".to_string(),
                    needs_operator: false
                },
                "`{command}`"
            );
        }
        for command in ["gh pr list", "env gh pr list", "sh -c 'gh pr list'"] {
            match route_rewrite_policy(command).decision {
                Decision::Rewrite { target, .. } => {
                    assert_eq!(target.as_str(), "legion pr list", "`{command}`")
                }
                other => panic!("`{command}`: expected rewrite, got {other:?}"),
            }
        }
    }

    /// A line that is not one simple command but reaches no rewrite is not
    /// refused: the shape only matters when a rewrite wins.
    #[test]
    fn a_lone_command_with_no_rewrite_still_reaches_the_allow_default() {
        for command in [
            "X=1",
            "> out.txt",
            "[[ -f nope ]]",
            "(( 0 ))",
            "env",
            "sh -c ''",
        ] {
            let routed = route_rewrite_policy(command);
            assert_eq!(
                routed.decision,
                Decision::Allow { note: None },
                "`{command}`"
            );
            assert_eq!(routed.deciding, Deciding::Default, "`{command}`");
        }
    }

    /// FR-CMD-008: a rewrite replaces the whole command, so a command that is
    /// not exactly one simple command, and whose Decision would be a rewrite,
    /// is refused naming the rule rather than rewritten with the rest dropped.
    #[test]
    fn a_compound_command_whose_decision_is_a_rewrite_is_refused() {
        for (command, rule) in [
            ("git push && echo done", "push-rewrite"),
            ("git add -A && git commit -m x", "add-rewrite"),
            ("gh pr view 42 | head", "pr-view"),
            ("git commit -m x; gh pr view 42", "commit-rewrite"),
            // A bare wrapper is still a command the rewrite would drop.
            ("git push && env", "push-rewrite"),
            ("env && git push", "push-rewrite"),
            ("git push && sudo", "push-rewrite"),
            // A wrapper payload or shell body that is not one simple command.
            ("env X=1 && git push", "push-rewrite"),
            ("git push && env X=1", "push-rewrite"),
            ("sh -c '' && git push", "push-rewrite"),
            ("bash -c '# comment' && git push", "push-rewrite"),
            ("sh -c 'X=1 Y=2' && git push", "push-rewrite"),
            ("sh -c '' || git push", "push-rewrite"),
            ("sh -c 'git push && echo done'", "push-rewrite"),
            // List members with no command word, and compound constructs.
            ("X=1 || gh pr list", "pr-list"),
            ("X=1 Y=2 && gh pr list", "pr-list"),
            ("> out.txt && gh pr list", "pr-list"),
            ("> out.txt || gh pr list", "pr-list"),
            ("[[ -f nope ]] || gh pr list", "pr-list"),
            ("(( 0 )) && gh pr list", "pr-list"),
            ("(X=1) || gh pr list", "pr-list"),
            ("{ X=1; } || gh pr list", "pr-list"),
            // A substitution in an operand the rule translates runs something
            // the rewrite would drop -- here, truncating out.txt. Only the
            // compound rule catches these: no redirect, no assignment prefix.
            ("gh issue view \"$(> out.txt)\"", "issue-view"),
            ("gh issue view `> out.txt`", "issue-view"),
            ("gh issue view <(> out.txt)", "issue-view"),
            ("gh issue view ${!x}", "issue-view"),
        ] {
            let routed = route_rewrite_policy(command);
            match &routed.decision {
                Decision::Deny(details) => {
                    assert!(
                        details.reason().contains(&format!("rule '{rule}'")),
                        "`{command}`: {}",
                        details.reason()
                    );
                    assert!(
                        details.reason().contains("drop the rest"),
                        "`{command}`: {}",
                        details.reason()
                    );
                }
                other => panic!("`{command}` must be refused, got {other:?}"),
            }
            assert_eq!(
                routed.deciding,
                Deciding::Rule {
                    id: rule.to_string(),
                    needs_operator: false
                },
                "`{command}`"
            );
        }
    }

    /// A substitution carried by a redirect or an assignment prefix is refused
    /// by the redirect/assignment rule (#1281) before the compound rule sees
    /// it; either refusal names the rule, and the command is never rewritten.
    #[test]
    fn a_substitution_in_a_redirect_or_assignment_is_refused_either_way() {
        for command in [
            "FOO=$(> out.txt) gh pr list",
            "gh pr list > \"$(> out.txt)\"",
            "gh pr list <<< \"$(> out.txt)\"",
            "gh pr list > >(> out.txt)",
        ] {
            let routed = route_rewrite_policy(command);
            match &routed.decision {
                Decision::Deny(details) => assert!(
                    details.reason().contains("rule 'pr-list'"),
                    "`{command}`: {}",
                    details.reason()
                ),
                other => panic!("`{command}` must be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_tool_kind_the_policy_does_not_model_is_allowed() {
        let p = sample_policy();
        let call = ToolCall {
            tool: "NotebookEdit".to_string(),
            input: serde_json::json!({ "notebook_path": "a.ipynb" }),
        };
        let routed = route(&p, &call, &Context::default());
        assert_eq!(routed.decision, Decision::Allow { note: None });
        assert_eq!(routed.deciding, Deciding::Default);
    }

    // -- no-go list (FR-CMD-025) ------------------------------------------

    /// A policy whose own entries would allow, rewrite, proxy, or ask every
    /// no-go binary, so a no-go deny can only come from the no-go check.
    fn permissive_policy() -> Policy {
        policy(
            r#"{
            "wrappers": [{"binary": "env"}, {"binary": "sudo"}],
            "interpreters": [
                {"binary": "sh", "flag": "-c", "body": "shell"},
                {"binary": "python3", "flag": "-c", "body": "foreign"}
            ],
            "tools": {"Bash": {"families": {
                "rm": {"rules": [{"id": "rm-allow", "outcome": {"kind": "allow"}}]},
                "mkfs.ext4": {"rules": [{"id": "mkfs-proxy", "outcome": {"kind": "proxy", "reason": "binary"}}]},
                "dd": {"rules": [{"id": "dd-rewrite", "outcome": {"kind": "rewrite", "target": "legion x",
                    "reason": "r", "translatable": {"operands": ["any", "any"]}}}]},
                "git push": {"rules": [{"id": "push-ask", "outcome": {"kind": "ask", "question": "push?",
                    "reason": "pushes", "needs_operator": true}}]},
                "chmod": {"rules": [{"id": "chmod-allow", "outcome": {"kind": "allow"}}]}
            }}}
        }"#,
        )
    }

    fn assert_no_go(routed: &Routed, command: &str) {
        match &routed.decision {
            Decision::Deny(details) => {
                assert_eq!(details.instead(), crate::NO_GO_INSTEAD, "{command}")
            }
            other => panic!("`{command}` must be a no-go deny, got {other:?}"),
        }
        assert!(
            matches!(routed.deciding, Deciding::NoGo { .. }),
            "`{command}` must name a no-go entry, got {:?}",
            routed.deciding
        );
        assert!(!routed.confirmed);
    }

    #[test]
    fn every_builtin_entry_and_its_variants_is_refused_even_when_a_policy_entry_matches() {
        let p = permissive_policy();
        for command in [
            "rm -rf /",
            "rm -fr /",
            "rm -r -f /",
            "rm / -rf",
            "rm -rf /*",
            "rm -rf ~",
            "rm -rf $HOME",
            "mkfs.ext4 /dev/sda1",
            "dd if=/dev/zero of=/dev/sda",
            ":(){ :|:& };:",
            "chmod -R 777 /",
            "chown -R nobody /",
            "git push --force origin main",
            "git push -f origin master",
            "git push --force-with-lease origin main",
            "sudo rm -rf /",
            "env rm -rf /",
            "sh -c 'rm -rf /'",
            "echo hi | rm -rf /",
            "true && rm -rf /",
            "true; rm -rf /",
            "FOO=1 rm -rf /",
            "git push origin +main",
            "sudo git push origin +main",
            "sh -c 'git push origin +HEAD:main'",
            "echo hi | git push origin +main",
            "true && git push origin +master",
        ] {
            assert_no_go(&route(&p, &bash(command), &Context::default()), command);
        }
    }

    #[test]
    fn a_no_go_command_outranks_the_compound_and_redirect_rewrite_refusals() {
        // #1277 refuses a rewrite inside a compound command and #1282 one
        // carrying a redirect or assignment. A no-go part in the same command
        // must still get the no-go deny, never either rewrite refusal.
        for command in [
            "gh pr list && rm -rf /",
            "rm -rf / ; gh pr list",
            "gh pr list | sh -c 'rm -rf /'",
            "gh pr list > out.txt && git push origin +main",
            "GIT_TRACE=1 gh pr list || :(){ :|:& };:",
        ] {
            let routed = route(&gh_pr_list_policy(), &bash(command), &Context::default());
            assert_no_go(&routed, command);
        }
    }

    #[test]
    fn a_forced_push_to_another_branch_is_not_a_no_go_match() {
        let routed = route(
            &permissive_policy(),
            &bash("git push --force origin feature"),
            &Context::default(),
        );
        assert!(matches!(routed.decision, Decision::Ask(_)));
        assert!(!matches!(routed.deciding, Deciding::NoGo { .. }));
    }

    #[test]
    fn a_no_go_match_is_refused_even_with_a_confirmation_for_that_exact_command() {
        let command = "git push --force origin main";
        let key = nogo::command_key(command).expect("key");
        let ctx = Context {
            confirmations: [(key, "I need to".to_string())].into_iter().collect(),
            ..Context::default()
        };
        let routed = route(&permissive_policy(), &bash(command), &ctx);
        assert_no_go(&routed, command);
    }

    #[test]
    fn a_no_go_match_never_prompts_the_operator() {
        // The operator is prompted only by an ask carrying the operator mark;
        // a no-go match is a deny naming the no-go entry, never that.
        let routed = route(
            &permissive_policy(),
            &bash("git push -f origin main"),
            &Context::default(),
        );
        assert!(!matches!(
            routed.deciding,
            Deciding::Rule {
                needs_operator: true,
                ..
            }
        ));
        assert_no_go(&routed, "git push -f origin main");
    }

    #[test]
    fn the_builtin_entries_are_present_when_the_policy_file_is_absent_or_empty() {
        for p in [Policy::default(), policy("{}"), policy(r#"{"no_go": []}"#)] {
            assert_no_go(
                &route(&p, &bash("rm -rf /"), &Context::default()),
                "rm -rf /",
            );
            // Everything else still takes the empty-policy deny.
            match route(&p, &bash("echo hi"), &Context::default()).decision {
                Decision::Deny(details) => assert_ne!(details.instead(), crate::NO_GO_INSTEAD),
                other => panic!("expected the empty-policy deny, got {other:?}"),
            }
        }
    }

    #[test]
    fn wrapper_variants_are_refused_with_no_wrappers_in_the_policy() {
        // FR-CMD-025: the built-in entries hold when the policy file is absent
        // or empty, and wrapper variants hit the same entry. So the no-go
        // check resolves sudo, env, and sh/bash -c itself, without the policy
        // file's wrapper declarations.
        let mut missed: Vec<String> = Vec::new();
        for (name, p) in [("default", Policy::default()), ("{}", policy("{}"))] {
            for command in [
                "sudo rm -rf /",
                "env rm -rf /",
                "sh -c 'rm -rf /'",
                "bash -c 'rm -rf /'",
                "echo hi | sudo rm -rf /",
                "true && env rm -rf /",
                "sudo -u root rm -rf /",
                "env FOO=bar mkfs.ext4 /dev/sda1",
                "sudo mkfs.ext4 /dev/sda1",
                "sudo env FOO=1 sh -c 'dd if=/dev/zero of=/dev/sda'",
            ] {
                let routed = route(&p, &bash(command), &Context::default());
                if !matches!(routed.deciding, Deciding::NoGo { .. }) {
                    missed.push(format!("{name}: {command}"));
                    continue;
                }
                assert_no_go(&routed, command);
            }
        }
        assert!(missed.is_empty(), "not refused as no-go: {missed:#?}");
    }

    #[test]
    fn every_shipped_wrapper_and_shell_interpreter_form_is_refused_with_no_policy() {
        // FR-CMD-025's wrapper variants are the positions FR-CMD-007
        // resolves: every wrapper and shell interpreter the shipped policy
        // declares. Each one, around a built-in no-go, is refused under an
        // empty policy, where no file declares the wrapper.
        let shipped: Policy = parse_policy(include_str!("../../../plugin/legion-cmd/policy.json"))
            .expect("the shipped policy parses");
        let mut forms: Vec<String> = Vec::new();
        for wrapper in &shipped.wrappers {
            let mut words: Vec<String> = vec![wrapper.binary.clone()];
            words.extend(wrapper.required_subcommand.clone());
            words.extend(std::iter::repeat_n("5".to_string(), wrapper.operands));
            forms.push(format!("{} rm -rf /", words.join(" ")));
            forms.push(format!("{} mkfs.ext4 /dev/sda1", words.join(" ")));
            // A workspace selector pair before the subcommand (#1293).
            for selector in &wrapper.selectors {
                let mut selected: Vec<String> =
                    vec![wrapper.binary.clone(), selector.clone(), "web".to_string()];
                selected.extend(words.iter().skip(1).cloned());
                forms.push(format!("{} rm -rf /", selected.join(" ")));
            }
        }
        for interpreter in &shipped.interpreters {
            if interpreter.body == BodyLanguage::Shell {
                forms.push(format!(
                    "{} {} 'rm -rf /'",
                    interpreter.binary, interpreter.flag
                ));
            }
        }
        // Review reproductions. The device must be an argument route can
        // see: `echo /dev/sda1 | xargs -I{} mkfs.ext4 {}` hands it over
        // stdin, and the shipped policy does not refuse that form either.
        forms.push("timeout 5 mkfs.ext4 /dev/sda1".to_string());
        forms.push("nohup rm -rf /".to_string());
        forms.push("echo go | xargs -I{} mkfs.ext4 /dev/sda1".to_string());

        let missed: Vec<&String> = forms
            .iter()
            .filter(|command| {
                !matches!(
                    route(&Policy::default(), &bash(command), &Context::default()).deciding,
                    Deciding::NoGo { .. }
                )
            })
            .collect();
        assert!(missed.is_empty(), "not refused as no-go: {missed:#?}");
    }

    #[test]
    fn the_second_no_go_scan_is_skipped_only_when_the_policy_already_resolves_every_builtin() {
        let shipped: Policy = parse_policy(include_str!("../../../plugin/legion-cmd/policy.json"))
            .expect("the shipped policy parses");
        assert!(shipped.no_go_resolver().is_none());
        assert!(Policy::default().no_go_resolver().is_some());
        // A narrower sudo declared first would shadow the built-in one, so
        // the resolver still runs and the wrapped form is still refused.
        let narrow = policy(r#"{"wrappers": [{"binary": "sudo"}]}"#);
        assert!(narrow.no_go_resolver().is_some());
        assert_no_go(
            &route(&narrow, &bash("sudo -u root rm -rf /"), &Context::default()),
            "sudo -u root rm -rf /",
        );
    }

    #[test]
    fn builtin_wrapper_resolution_does_not_change_other_routing_under_an_empty_policy() {
        // Only the no-go match gains the built-in wrappers: every other
        // command under an empty policy still takes the empty-policy deny.
        for command in ["sudo echo hi", "env FOO=1 ls", "sh -c 'echo hi'"] {
            let routed = route(&Policy::default(), &bash(command), &Context::default());
            assert!(
                !matches!(routed.deciding, Deciding::NoGo { .. }),
                "{command}"
            );
            match routed.decision {
                Decision::Deny(details) => {
                    assert_ne!(details.instead(), crate::NO_GO_INSTEAD, "{command}")
                }
                other => panic!("`{command}` expected the empty-policy deny, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_entry_added_in_the_policy_file_is_refused_like_a_builtin_entry() {
        let p = policy(
            r#"{"no_go": [{"id": "shred-disk", "binaries": ["shred"],
                "predicates": [{"kind": "operand", "prefixes": ["/dev/"]}]}]}"#,
        );
        // `sudo` is not a wrapper in this policy, but the no-go check resolves
        // it itself (FR-CMD-025), so an added entry's wrapper variant is
        // refused like a built-in one's.
        for command in ["shred /dev/sda", "sudo shred /dev/sda"] {
            let routed = route(&p, &bash(command), &Context::default());
            assert_no_go(&routed, command);
            assert_eq!(
                routed.deciding,
                Deciding::NoGo {
                    id: "shred-disk".to_string()
                },
                "{command}"
            );
        }
    }

    #[test]
    fn a_policy_file_that_tries_to_override_a_builtin_entry_leaves_it_in_force() {
        // An added entry reusing the built-in id with a narrower match cannot
        // replace it: the built-in is checked first and still matches.
        let p = policy(
            r#"{"no_go": [{"id": "rm-recursive-force-root", "binaries": ["rm"],
                "predicates": [{"kind": "operand", "equals": ["/nothing"]}]}]}"#,
        );
        assert_no_go(
            &route(&p, &bash("rm -rf /"), &Context::default()),
            "rm -rf /",
        );
        // A key that tries to disable or remove entries is not policy data at
        // all: the file is rejected, and the adapter denies on an unparsable
        // policy (FR-CMD-016), so nothing is weakened.
        assert!(parse_policy(r#"{"no_go_disable": ["rm-recursive-force-root"]}"#).is_err());
        assert!(
            parse_policy(r#"{"no_go": [{"id": "x", "binaries": ["rm"], "enabled": false}]}"#)
                .is_err()
        );
    }

    #[test]
    fn a_no_go_command_inside_an_opaque_body_is_proxied_opaque_not_denied() {
        let routed = route(
            &permissive_policy(),
            &bash("python3 -c \"import os; os.system('rm -rf /')\""),
            &Context::default(),
        );
        assert_eq!(
            routed.decision,
            Decision::Proxy {
                reason: ProxyReason::Opaque
            }
        );
    }

    // -- confirmations (FR-CMD-026, FR-CMD-006) ----------------------------

    fn confirm_policy() -> Policy {
        policy(
            r#"{"tools": {"Bash": {"families": {
                "curl": {"rules": [{"id": "curl-ask", "outcome": {"kind": "ask",
                    "question": "fetch?", "reason": "network"}}]},
                "gh pr": {"rules": [{"id": "gh-pr", "outcome": {"kind": "ask",
                    "question": "touch the PR?", "reason": "PRs", "needs_operator": true}}]},
                "xxd": {"rules": [{"id": "xxd", "outcome": {"kind": "proxy", "reason": "binary"}}]},
                "shutdown": {"rules": [{"id": "shutdown", "outcome": {"kind": "deny",
                    "reason": "no", "instead": "ask the operator"}}]}
            }}}}"#,
        )
    }

    fn confirmed_ctx(command: &str, reason: &str) -> Context {
        Context {
            confirmations: [(nogo::command_key(command).expect("key"), reason.to_string())]
                .into_iter()
                .collect(),
            ..Context::default()
        }
    }

    #[test]
    fn without_a_confirmation_an_ask_carries_no_operator_mark() {
        for command in ["curl example.com", "gh pr merge 1"] {
            let routed = route(&confirm_policy(), &bash(command), &Context::default());
            assert!(matches!(routed.decision, Decision::Ask(_)), "{command}");
            assert!(matches!(
                routed.deciding,
                Deciding::Rule {
                    needs_operator: false,
                    ..
                }
            ));
            assert!(!routed.confirmed);
        }
    }

    #[test]
    fn a_confirmed_ask_not_needing_the_operator_is_allowed_and_marks_the_confirmation_used() {
        let ctx = confirmed_ctx("curl example.com", "fetching the release notes");
        let routed = route(&confirm_policy(), &bash("curl example.com"), &ctx);
        assert_eq!(routed.decision, Decision::Allow { note: None });
        assert!(routed.confirmed);
    }

    #[test]
    fn a_confirmed_ask_earns_what_the_rest_of_the_command_decides() {
        let ctx = confirmed_ctx("curl example.com | xxd", "inspect bytes");
        let routed = route(&confirm_policy(), &bash("curl example.com | xxd"), &ctx);
        assert_eq!(
            routed.decision,
            Decision::Proxy {
                reason: ProxyReason::Binary
            }
        );
        assert!(routed.confirmed);

        // A deny elsewhere in the command still wins, and the confirmation
        // is left unused.
        let ctx = confirmed_ctx("curl example.com; shutdown now", "why not");
        let routed = route(
            &confirm_policy(),
            &bash("curl example.com; shutdown now"),
            &ctx,
        );
        assert!(matches!(routed.decision, Decision::Deny(_)));
        assert!(!routed.confirmed);
    }

    #[test]
    fn a_confirmed_ask_needing_the_operator_prompts_with_the_agents_reason() {
        let ctx = confirmed_ctx("gh pr merge 1", "the review approved it");
        let routed = route(&confirm_policy(), &bash("gh pr merge 1"), &ctx);
        match &routed.decision {
            Decision::Ask(details) => assert_eq!(details.reason(), "the review approved it"),
            other => panic!("expected ask, got {other:?}"),
        }
        assert_eq!(
            routed.deciding,
            Deciding::Rule {
                id: "gh-pr".to_string(),
                needs_operator: true
            }
        );
        assert!(routed.confirmed);
    }

    #[test]
    fn a_confirmation_matches_on_parsed_arguments_not_the_literal_string() {
        let ctx = confirmed_ctx("curl example.com", "r");
        let same = route(&confirm_policy(), &bash("curl   'example.com'"), &ctx);
        assert_eq!(same.decision, Decision::Allow { note: None });
        let different = route(&confirm_policy(), &bash("curl example.org"), &ctx);
        assert!(matches!(different.decision, Decision::Ask(_)));
        assert!(!different.confirmed);
    }

    #[test]
    fn an_in_command_confirmation_marker_is_still_asked() {
        for command in [
            "LEGION_CONFIRMED=1 curl example.com",
            "curl example.com --confirmed",
            "curl example.com # confirmed: I need it",
        ] {
            let routed = route(&confirm_policy(), &bash(command), &Context::default());
            assert!(matches!(routed.decision, Decision::Ask(_)), "{command}");
            assert!(!routed.confirmed, "{command}");
        }
    }

    #[test]
    fn facts_carry_the_command_key() {
        let routed = route(
            &confirm_policy(),
            &bash("curl example.com"),
            &Context::default(),
        );
        assert_eq!(
            routed.facts.command_key,
            Some(nogo::command_key("curl example.com").expect("key"))
        );
    }
}
