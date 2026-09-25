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
use crate::policy::{BodyLanguage, Policy, ToolKind};
use crate::splitter::{self, Invocation, ScanError, Unreduced, UnreducedReason};

/// The one routing entry point. Pure (NFR-CMD-001): no filesystem, network, or
/// database, and its output depends only on `policy`, `call` and `ctx`.
pub fn route(policy: &Policy, call: &ToolCall, ctx: &Context) -> Routed {
    // FR-CMD-025's no-go list is checked here, before any rule -- an ordered
    // first step #1237 fills in. It is intentionally empty in this issue.

    // An empty or unparsable policy denies every command (FR-CMD-016). An
    // unparsable policy never becomes a `Policy`, so the adapter denies before
    // calling route; an empty one is caught here.
    if policy.is_empty() {
        return empty_policy_deny();
    }

    match call.tool.as_str() {
        "Bash" => route_bash(policy, call, ctx),
        other => route_fields(policy, other, call, ctx),
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
    if command.trim().is_empty() {
        return allow_routed();
    }

    let expanded = match expand_command(policy, command) {
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

    let mut winner = evaluate::combine(parts);
    // A rewrite replaces the whole command, so it is kept only for exactly one
    // simple command (FR-CMD-008); the shape comes from route's own parse, so
    // the adapter never scans the command (FR-CMD-017).
    if !expanded.single_simple {
        winner = evaluate::refuse_compound_rewrite(winner);
    }
    let facts = extract_facts(&expanded.invocations, winner.verb.clone());
    Routed {
        decision: winner.decision,
        facts,
        deciding: winner.deciding,
    }
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
    }
}

fn allow_routed() -> Routed {
    Routed {
        decision: Decision::Allow { note: None },
        facts: Facts::default(),
        deciding: Deciding::Default,
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
}
