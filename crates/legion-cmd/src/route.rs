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
    if let Ok(expanded) = &expanded
        && let Some(hit) = evaluate::decide_no_go(policy, &expanded.invocations)
    {
        let mut facts = extract_facts(&expanded.invocations, None);
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
        parts.push(evaluate::decide_bash_invocation(
            policy,
            &invocation.binary,
            &invocation.args,
            ctx,
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

    let winner = evaluate::combine(parts);
    let mut facts = extract_facts(&expanded.invocations, winner.verb.clone());
    facts.command_key = command_key;
    // A confirmation is used only when the command goes on to run or to the
    // operator prompt; a deny from another part leaves it unused.
    let confirmed = answered && !matches!(winner.decision, Decision::Deny(_));
    Routed {
        decision: winner.decision,
        facts,
        deciding: winner.deciding,
        confirmed,
    }
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
}

/// Splits `command` and re-enters every wrapper and interpreter payload the
/// policy names. This is the one scan route performs. The lookup pre-pass
/// ([`crate::lookups`]) calls this same function, so the rules it consults are
/// exactly the rules route will evaluate; no caller outside this crate splits
/// the command (FR-CMD-003, FR-CMD-017).
pub(crate) fn expand_command(policy: &Policy, command: &str) -> Result<Expanded, ScanError> {
    let scan = splitter::scan(command)?;
    let mut expanded = Expanded::default();
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

    if let Some(wrapper) = policy.matching_wrapper(&invocation.binary, &invocation.args) {
        let skip = usize::from(wrapper.required_subcommand.is_some());
        let payload: Vec<&String> = invocation.args.iter().skip(skip).collect();
        // A wrapper with no payload words (a bare `env`, `pnpm exec` with
        // nothing after it) wraps no command: there is nothing to route, so it
        // contributes no part and reaches the allow default -- not an opaque
        // proxy, which is reserved for a command route genuinely cannot read.
        if !payload.is_empty() {
            let text = payload
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            reenter_text(policy, &text, next_depth, out);
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

/// Re-enters `text` as a shell command. A payload the splitter rejects is
/// recorded `Unparsed` and proxies opaque -- a malformed wrapper payload is not
/// an ask (FR-CMD-007): re-entering it changes who called `scan_at`, not what
/// the region is.
fn reenter_text(policy: &Policy, text: &str, depth: u8, out: &mut Expanded) {
    match splitter::scan_at(text, depth) {
        Ok(scan) => expand(policy, scan.invocations, scan.unreduced, out),
        Err(_) => out.regions.push(Unreduced {
            text: text.to_string(),
            reason: UnreducedReason::Unparsed,
            depth,
        }),
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
        ] {
            assert_no_go(&route(&p, &bash(command), &Context::default()), command);
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
    fn an_entry_added_in_the_policy_file_is_refused_like_a_builtin_entry() {
        let p = policy(
            r#"{"no_go": [{"id": "shred-disk", "binaries": ["shred"],
                "predicates": [{"kind": "operand", "prefixes": ["/dev/"]}]}]}"#,
        );
        let routed = route(&p, &bash("sudo shred /dev/sda"), &Context::default());
        // `sudo` is not a wrapper in this policy, so only the direct form is
        // checked here; the built-in wrapper variants are covered above.
        assert!(!matches!(routed.deciding, Deciding::NoGo { .. }));
        let routed = route(&p, &bash("shred /dev/sda"), &Context::default());
        assert_no_go(&routed, "shred /dev/sda");
        assert_eq!(
            routed.deciding,
            Deciding::NoGo {
                id: "shred-disk".to_string()
            }
        );
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
