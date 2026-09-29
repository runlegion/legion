//! The one routing entry point (FR-CMD-001, FR-CMD-011, NFR-CMD-001, #1337).
//!
//! [`route`] takes a tool call and context. A Bash command is parsed once
//! and put through four lists: a command on the never-run list is denied; a
//! command on the ask list, or one whose rewritten form uses a power switch,
//! is asked about; each `git`, `gh`, `grep` and `rg` (the proxy list) gets
//! `legion ` inserted before its name, except the `worktree_agent_passthrough`
//! names in a worktree-isolated agent (#1358); everything else runs as typed. A
//! command matching more than one gets the strictest outcome: deny, then
//! ask, then insertion, then untouched. No job is recognized and no
//! interpreter body is inspected. A call to any other tool is decided by its
//! kind's rules. `route` performs no I/O (FR-CMD-014): its output depends
//! only on its inputs.

use serde_json::Value;

use crate::Context;
use crate::decision::{Deciding, Decision, Facts, ManagedTarget, Routed, ToolCall};
use crate::evaluate;
use crate::insert;
use crate::nogo::{self, CommandKey, NoGoEntry};
use crate::policy::{Policy, ToolKind};
use crate::splitter::{self, Invocation, Position, Scan};

/// The id an insertion is recorded under: the policy's `proxy` list.
pub const PROXY_ID: &str = "proxy";

/// Words that run the command after them: a never-run or ask command behind
/// one (`sudo rm -rf /`, `env FOO=1 curl x`, `xargs rm -rf`) is matched as if
/// it stood first. Grammar the parser does not model, not policy: the policy
/// holds only its four lists. Only the command the prefix word runs is read
/// ([`prefixed_starts`]), never a later argument of that command.
const PREFIX_WORDS: [&str; 13] = [
    "sudo", "doas", "env", "nice", "nohup", "timeout", "stdbuf", "xargs", "command", "exec", "npx",
    "pnpx", "bunx",
];

/// Shells whose `-c` argument is shell code. The payload is parsed and
/// checked against the never-run and ask lists the same way; nothing is
/// inserted inside it.
const SHELLS: [&str; 3] = ["sh", "bash", "zsh"];

/// The builtin whose arguments, joined, are shell code.
const EVAL: &str = "eval";

/// The question every ask and power-switch entry puts to the agent; the
/// entry's reason says why.
const ASK_QUESTION: &str = "this command needs the operator's approval: drop it or confirm it";

/// The one routing entry point. Pure (NFR-CMD-001): no filesystem, network, or
/// database, and its output depends only on `policy`, `call` and `ctx`.
pub fn route(policy: &Policy, call: &ToolCall, ctx: &Context) -> Routed {
    if call.tool == "Bash" {
        return route_bash(policy, bash_command(call), ctx);
    }
    let Some(kind) = ToolKind::parse(&call.tool) else {
        // A tool the policy structure does not model runs untouched.
        return untouched(None);
    };
    let outcome = evaluate::decide_fields(policy, kind, &call.input, ctx);
    Routed {
        decision: outcome.decision,
        facts: Facts::default(),
        deciding: outcome.deciding,
        confirmed: false,
    }
}

/// The Bash command a tool call carries, or `""` when `input` has no string
/// `command` field.
pub(crate) fn bash_command(call: &ToolCall) -> &str {
    call.input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn route_bash(policy: &Policy, command: &str, ctx: &Context) -> Routed {
    let command_key: Option<CommandKey> = nogo::command_key(command).ok();
    if command.trim().is_empty() {
        return untouched(command_key);
    }
    let typed: Scan = match splitter::scan(command) {
        Ok(scan) => scan,
        Err(err) => {
            return refused(
                format!("the shell parser rejected this command: {err}"),
                None,
                Deciding::ParseError,
            );
        }
    };

    // The never-run list, built-ins first, over the command as typed. No
    // confirmation, operator prompt or other entry can override a match.
    if let Some(entry) = first_match(&policy.never_run_entries(), &typed) {
        return refused(
            entry.reason.clone(),
            command_key,
            Deciding::NoGo {
                id: entry.id.clone(),
            },
        );
    }

    let rewritten: Option<String> = match insert::proxy_insertion(command, &proxied(policy, ctx)) {
        Ok(rewritten) => rewritten,
        Err(err) => return refused(err.to_string(), command_key, proxy_rule()),
    };

    // The ask list over the command as typed; the power switches over the
    // command as it will run, so `git push --force` is matched as `legion
    // git push --force`.
    let asked: Option<NoGoEntry> = match first_match(&policy.ask, &typed) {
        Some(entry) => Some(entry.clone()),
        None => match splitter::scan(rewritten.as_deref().unwrap_or(command)) {
            Ok(scan) => first_match(&policy.power_switches, &scan).cloned(),
            Err(err) => {
                return refused(
                    format!("the rewritten command did not parse: {err}"),
                    command_key,
                    proxy_rule(),
                );
            }
        },
    };

    if let Some(entry) = asked {
        return ask(&entry, command_key, rewritten, ctx);
    }
    match rewritten {
        Some(rewritten) => Routed {
            decision: Decision::Rewrite {
                target: ManagedTarget::new("legion"),
                reason: "each proxied name runs through its legion proxy".to_string(),
            },
            facts: Facts {
                command_key,
                rewritten: Some(rewritten),
            },
            deciding: proxy_rule(),
            confirmed: false,
        },
        None => untouched(command_key),
    }
}

/// The names that get `legion ` inserted for this call: the policy's proxy
/// list, less its `worktree_agent_passthrough` names when the call comes from
/// a worktree-isolated agent (#1358). Claude Code's worktree guard refuses
/// `legion git ...` there, so the typed command is the form that runs. Only
/// insertion reads this: the never-run, ask and power-switch lists are
/// matched the same in every agent.
fn proxied(policy: &Policy, ctx: &Context) -> Vec<String> {
    policy
        .proxy
        .iter()
        .filter(|name| !ctx.worktree_isolated || !policy.worktree_agent_passthrough.contains(name))
        .cloned()
        .collect()
}

/// A refused Bash command: a deny with `reason` and no command to run
/// instead, decided by `deciding`.
fn refused(
    reason: impl Into<String>,
    command_key: Option<CommandKey>,
    deciding: Deciding,
) -> Routed {
    Routed {
        decision: Decision::refuse(reason),
        facts: Facts {
            command_key,
            rewritten: None,
        },
        deciding,
        confirmed: false,
    }
}

/// The deciding entry of an insertion, or of a refused one: the `proxy` list.
fn proxy_rule() -> Deciding {
    Deciding::Rule {
        id: PROXY_ID.to_string(),
        needs_operator: false,
    }
}

/// The ask for `entry` (FR-CMD-006, FR-CMD-026). Unconfirmed, the agent is
/// asked, with the entry's reason. Once the agent has confirmed the typed
/// command (`legion cmd confirm`), the ask goes to the operator with the
/// agent's reason: every ask and power-switch entry needs the operator.
fn ask(
    entry: &NoGoEntry,
    command_key: Option<CommandKey>,
    rewritten: Option<String>,
    ctx: &Context,
) -> Routed {
    let confirmation: Option<&String> = command_key
        .as_ref()
        .and_then(|key| ctx.confirmations.get(key));
    let reason: &str = confirmation.map_or(entry.reason.as_str(), String::as_str);
    let decision: Decision = Decision::ask(ASK_QUESTION, reason)
        .unwrap_or_else(|_| Decision::refuse("this command needs the operator's approval"));
    Routed {
        decision,
        facts: Facts {
            command_key,
            rewritten,
        },
        deciding: Deciding::Rule {
            id: entry.id.clone(),
            needs_operator: confirmation.is_some(),
        },
        confirmed: confirmation.is_some(),
    }
}

fn untouched(command_key: Option<CommandKey>) -> Routed {
    Routed {
        decision: Decision::Allow { note: None },
        facts: Facts {
            command_key,
            rewritten: None,
        },
        deciding: Deciding::Default,
        confirmed: false,
    }
}

/// The first entry, in list order, that matches any command `scan` holds,
/// read at every position the parser exposes, behind a prefix word, and
/// inside a shell payload.
fn first_match<'a>(entries: &'a [NoGoEntry], scan: &Scan) -> Option<&'a NoGoEntry> {
    if entries.is_empty() {
        return None;
    }
    let mut commands: Vec<Candidate> = Vec::new();
    for invocation in &scan.invocations {
        candidates(invocation, &mut commands);
    }
    entries.iter().find(|entry| {
        commands
            .iter()
            .any(|c| entry.matches(&c.name, &c.args, c.position))
    })
}

/// One command an entry is matched against: a name, its arguments, and the
/// grammar position of the invocation it came from.
struct Candidate {
    name: String,
    args: Vec<String>,
    position: Position,
}

/// Every command `invocation` runs: itself, the command behind each of its
/// prefix words, and the commands of its shell payload.
fn candidates(invocation: &Invocation, out: &mut Vec<Candidate>) {
    let depth: u8 = invocation.depth;
    let position: Position = invocation.position;
    let mut runs: Vec<(String, &[String])> = vec![(invocation.binary.clone(), &invocation.args)];
    // Follow a chain of prefix words (`sudo env FOO=1 rm ...`): each step
    // reads only the command the current prefix word runs.
    let mut frontier: Vec<(String, &[String])> =
        vec![(invocation.binary.clone(), &invocation.args)];
    while let Some((name, args)) = frontier.pop() {
        if !PREFIX_WORDS.contains(&name.as_str()) {
            continue;
        }
        for start in prefixed_starts(args) {
            let run: (String, &[String]) =
                (basename(&shell_value(&args[start])), &args[start + 1..]);
            frontier.push(run.clone());
            runs.push(run);
        }
    }
    for (name, args) in runs {
        payload_commands(&name, args, depth, out);
        out.push(Candidate {
            name,
            args: args.to_vec(),
            position,
        });
    }
}

/// Where the command a prefix word runs may start, among the prefix word's
/// `args`. The prefix word's own words come first: options (`-n`, `--user`,
/// `-I{}`), environment assignments (`FOO=1`), and numeric operands (a
/// `timeout` duration). A word right after a bare option may be that
/// option's value (`sudo -u deploy`), so it is a possible start and the
/// word after it is tried too. The first word that is none of these is the
/// command, and nothing after it is read: `xargs echo rm -rf /` runs `echo`.
/// A bare `--` ends the prefix word's options. No option table is needed:
/// every reading that could be the command is tried, and no later word is.
fn prefixed_starts(args: &[String]) -> Vec<usize> {
    let mut starts: Vec<usize> = Vec::new();
    let mut after_bare_option = false;
    for (index, raw) in args.iter().enumerate() {
        let word: String = shell_value(raw);
        if word == "--" {
            if index + 1 < args.len() {
                starts.push(index + 1);
            }
            break;
        }
        let is_option = word.len() > 1 && word.starts_with('-');
        if is_option {
            // `--name=value` and a clustered short option with its value
            // attached (`-I{}`, `-n5`) carry their own value.
            after_bare_option = !word.contains('=') && (word.starts_with("--") || word.len() == 2);
            continue;
        }
        let is_assignment = word.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        let is_numeric = word.starts_with(|c: char| c.is_ascii_digit());
        if is_assignment || is_numeric {
            after_bare_option = false;
            continue;
        }
        starts.push(index);
        if !after_bare_option {
            break;
        }
        after_bare_option = false;
    }
    starts
}

/// The commands of a shell payload: the `-c` argument of a shell, or the
/// joined arguments of `eval`, parsed as shell code. Nested payloads are
/// followed until the splitter's depth bound.
fn payload_commands(name: &str, args: &[String], depth: u8, out: &mut Vec<Candidate>) {
    let code: Option<String> = if name == EVAL {
        Some(
            args.iter()
                .map(|a| shell_value(a))
                .collect::<Vec<String>>()
                .join(" "),
        )
    } else if SHELLS.contains(&name) {
        shell_code_argument(args).map(|code| shell_value(code))
    } else {
        None
    };
    let Some(code) = code else {
        return;
    };
    if let Ok(scan) = splitter::scan_at(&code, depth.saturating_add(1)) {
        for invocation in &scan.invocations {
            candidates(invocation, out);
        }
    }
}

/// The argument a shell runs as code: the word after an option cluster
/// holding `c` (`-c`, `-lc`, `-xc`), among the options before the first
/// operand. A shell given a script file has no inline code.
fn shell_code_argument(args: &[String]) -> Option<&String> {
    for (index, raw) in args.iter().enumerate() {
        let word: String = shell_value(raw);
        if word == "--" || !word.starts_with('-') {
            return None;
        }
        // A long option (`--norc`) never carries the code.
        if word.starts_with("--") {
            continue;
        }
        if word[1..].contains('c') {
            return args.get(index + 1);
        }
    }
    None
}

/// The value the shell passes for one argument word: quotes and escapes
/// resolved when the word is a plain literal, else one matched outer quote
/// pair removed.
fn shell_value(raw: &str) -> String {
    splitter::literal_word(raw).unwrap_or_else(|| {
        let bytes = raw.as_bytes();
        match (bytes.first(), bytes.last()) {
            (Some(first @ (b'\'' | b'"')), Some(last)) if bytes.len() >= 2 && first == last => {
                raw[1..raw.len() - 1].to_string()
            }
            _ => raw.to_string(),
        }
    })
}

/// The text after the last `/`: the command a path names.
fn basename(word: &str) -> String {
    word.rsplit('/').next().unwrap_or(word).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_policy;

    fn policy() -> Policy {
        parse_policy(include_str!("../../../plugin/legion-cmd/policy.json"))
            .expect("the shipped policy parses")
    }

    fn bash(command: &str) -> ToolCall {
        ToolCall {
            tool: "Bash".to_string(),
            input: serde_json::json!({ "command": command }),
        }
    }

    fn decide(command: &str) -> Routed {
        route(&policy(), &bash(command), &Context::default())
    }

    fn never_run_id(command: &str) -> Option<String> {
        match decide(command).deciding {
            Deciding::NoGo { id } => Some(id),
            _ => None,
        }
    }

    fn asked_id(routed: &Routed) -> Option<String> {
        match (&routed.decision, &routed.deciding) {
            (Decision::Ask(_), Deciding::Rule { id, .. }) => Some(id.clone()),
            _ => None,
        }
    }

    /// One command per never-run entry: the seven built-ins and the two the
    /// shipped policy adds.
    const NEVER_RUN: [(&str, &str); 9] = [
        ("rm-recursive-force-root", "rm -rf /"),
        ("mkfs-device", "mkfs.ext4 /dev/sda1"),
        ("dd-to-disk", "dd if=/dev/zero of=/dev/sda"),
        ("fork-bomb", ":(){ :|:& };:"),
        ("chmod-chown-recursive-root", "chmod -R 777 /"),
        ("git-force-push-main", "git push --force origin main"),
        ("git-force-refspec-main", "git push origin +main"),
        ("sqlite3-legion-db", "sqlite3 legion.db"),
        ("rm-recursive-force", "rm -rf build"),
    ];

    #[test]
    fn each_never_run_entry_is_denied_with_its_reason() {
        let entries = policy().never_run_entries();
        for (id, command) in NEVER_RUN {
            let routed = decide(command);
            assert_eq!(
                routed.deciding,
                Deciding::NoGo { id: id.to_string() },
                "{command}"
            );
            let reason = &entries.iter().find(|e| e.id == id).expect("entry").reason;
            let Decision::Deny(details) = routed.decision else {
                panic!("{command} must deny");
            };
            assert_eq!(details.reason(), reason);
            assert_eq!(details.instead(), crate::NO_GO_INSTEAD);
        }
    }

    #[test]
    fn each_never_run_entry_is_denied_at_every_position_behind_prefix_words_and_in_payloads() {
        for (id, command) in NEVER_RUN {
            // The fork bomb is a function definition; its positions are its
            // own and it is covered by the other shapes below.
            let shapes: Vec<String> = if id == "fork-bomb" {
                vec![
                    format!("echo a; {command}"),
                    format!("sh -c '{command}'"),
                    format!("bash -c '{command}'"),
                    format!("eval '{command}'"),
                ]
            } else {
                vec![
                    format!("echo a && {command}"),
                    format!("echo a | {command}"),
                    format!("echo a; {command}"),
                    format!("FOO=1 {command}"),
                    format!("(cd x && {command})"),
                    format!("echo $({command})"),
                    format!("if true; then {command}; fi"),
                    format!("f() {{ {command}; }}"),
                    format!("sudo {command}"),
                    format!("sudo -u root {command}"),
                    format!("env FOO=1 {command}"),
                    format!("nice -n 5 {command}"),
                    format!("xargs {command}"),
                    format!("timeout 5 {command}"),
                    format!("sh -c '{command}'"),
                    format!("bash -c '{command}'"),
                    format!("bash -lc '{command}'"),
                    format!("zsh -c '{command}'"),
                    format!("eval '{command}'"),
                    format!("sudo sh -c '{command}'"),
                ]
            };
            for shape in shapes {
                assert_eq!(never_run_id(&shape).as_deref(), Some(id), "{shape}");
            }
        }
    }

    #[test]
    fn the_builtin_entries_hold_with_an_empty_policy() {
        let empty = Policy::default();
        for (id, command) in &NEVER_RUN[..7] {
            let routed = route(&empty, &bash(command), &Context::default());
            assert_eq!(routed.deciding, Deciding::NoGo { id: id.to_string() });
        }
        // Nothing else is refused, and nothing is inserted, with no lists.
        let routed = route(&empty, &bash("git status"), &Context::default());
        assert_eq!(routed.decision, Decision::Allow { note: None });
        assert_eq!(routed.facts.rewritten, None);
    }

    #[test]
    fn curl_is_asked_and_its_confirmation_goes_to_the_operator() {
        let routed = decide("curl https://example.com");
        assert_eq!(asked_id(&routed).as_deref(), Some("curl-network"));
        assert!(!routed.confirmed);
        assert!(matches!(
            routed.deciding,
            Deciding::Rule {
                needs_operator: false,
                ..
            }
        ));
        for shape in ["sudo curl x", "sh -c 'curl x'", "ls | curl -d @- x"] {
            assert_eq!(
                asked_id(&decide(shape)).as_deref(),
                Some("curl-network"),
                "{shape}"
            );
        }

        let key = nogo::command_key("curl https://example.com").expect("key");
        let ctx = Context {
            confirmations: [(key, "fetching the release notes".to_string())].into(),
            ..Context::default()
        };
        let confirmed = route(&policy(), &bash("curl https://example.com"), &ctx);
        assert!(confirmed.confirmed);
        assert_eq!(
            confirmed.deciding,
            Deciding::Rule {
                id: "curl-network".to_string(),
                needs_operator: true
            }
        );
        let Decision::Ask(details) = confirmed.decision else {
            panic!("a confirmed ask goes to the operator");
        };
        assert_eq!(details.reason(), "fetching the release notes");
    }

    #[test]
    fn each_power_switch_is_asked_in_the_git_or_gh_form_and_the_legion_form() {
        for (id, commands) in [
            (
                "push-force",
                &[
                    "git push --force",
                    "git push -f origin feature",
                    "git push --force-with-lease origin feature",
                    "legion push --repo legion --force",
                    "legion git push --force",
                ][..],
            ),
            (
                "push-force-refspec",
                &[
                    "git push origin +feature",
                    "legion git push origin +feature",
                ][..],
            ),
            (
                "merge-despite-failures",
                &[
                    "gh pr merge 5 --merge-despite-failures",
                    "legion pr merge --repo legion --number 5 --merge-despite-failures",
                ][..],
            ),
            (
                "issue-close-force",
                &[
                    "gh issue close 5 --force",
                    "legion issue close --repo legion --number 5 --force --force-reason x",
                ][..],
            ),
        ] {
            for command in commands {
                assert_eq!(asked_id(&decide(command)).as_deref(), Some(id), "{command}");
            }
        }
    }

    #[test]
    fn a_confirmed_power_switch_carries_the_rewritten_command_to_the_operator() {
        let key = nogo::command_key("git push --force").expect("key");
        let ctx = Context {
            confirmations: [(key, "rebased onto main".to_string())].into(),
            ..Context::default()
        };
        let routed = route(&policy(), &bash("git push --force"), &ctx);
        assert!(routed.confirmed);
        assert!(matches!(routed.decision, Decision::Ask(_)));
        assert_eq!(
            routed.facts.rewritten.as_deref(),
            Some("legion git push --force")
        );
    }

    #[test]
    fn a_forced_push_to_main_is_still_denied() {
        for command in [
            "git push --force origin main",
            "git push -f origin master",
            "git push origin +main",
        ] {
            assert!(never_run_id(command).is_some(), "{command}");
        }
    }

    #[test]
    fn proxied_names_run_with_legion_inserted() {
        let routed = decide("git push -h");
        assert!(matches!(routed.decision, Decision::Rewrite { .. }));
        assert_eq!(
            routed.facts.rewritten.as_deref(),
            Some("legion git push -h")
        );
        assert_eq!(
            routed.deciding,
            Deciding::Rule {
                id: PROXY_ID.to_string(),
                needs_operator: false
            }
        );
        assert_eq!(
            decide("cd x && grep -rn foo src | rg bar")
                .facts
                .rewritten
                .as_deref(),
            Some("cd x && legion grep -rn foo src | legion rg bar")
        );
    }

    /// The shipped policy with `worktree_agent_passthrough` set as #1358's
    /// second release ships it.
    fn passthrough_policy() -> Policy {
        Policy {
            worktree_agent_passthrough: vec!["git".to_string(), "gh".to_string()],
            ..policy()
        }
    }

    fn decide_isolated(command: &str) -> Routed {
        let ctx = Context {
            worktree_isolated: true,
            ..Context::default()
        };
        route(&passthrough_policy(), &bash(command), &ctx)
    }

    #[test]
    fn with_no_passthrough_list_a_worktree_isolated_agent_gets_every_insertion() {
        let ctx = Context {
            worktree_isolated: true,
            ..Context::default()
        };
        let routed = route(
            &Policy {
                worktree_agent_passthrough: Vec::new(),
                ..policy()
            },
            &bash("git status"),
            &ctx,
        );
        assert_eq!(routed.facts.rewritten.as_deref(), Some("legion git status"));
    }

    #[test]
    fn a_worktree_isolated_agent_runs_git_and_gh_as_typed() {
        for command in [
            "git status --short",
            "gh pr view 1",
            "cd x && git log --oneline -3",
        ] {
            let routed = decide_isolated(command);
            assert_eq!(routed.decision, Decision::Allow { note: None }, "{command}");
            assert_eq!(routed.facts.rewritten, None, "{command}");
            assert_eq!(routed.deciding, Deciding::Default, "{command}");
        }
    }

    #[test]
    fn a_worktree_isolated_agent_still_gets_legion_grep_and_rg() {
        assert_eq!(
            decide_isolated("git status && grep -rn foo src | rg bar")
                .facts
                .rewritten
                .as_deref(),
            Some("git status && legion grep -rn foo src | legion rg bar")
        );
    }

    #[test]
    fn a_worktree_isolated_agent_is_denied_and_asked_as_elsewhere() {
        assert_eq!(
            decide_isolated("rm -rf /").deciding,
            Deciding::NoGo {
                id: "rm-recursive-force-root".to_string()
            }
        );
        for (command, id) in [
            ("git push --force", "push-force"),
            ("git push origin +feature", "push-force-refspec"),
            (
                "gh pr merge 5 --merge-despite-failures",
                "merge-despite-failures",
            ),
            ("gh issue close 5 --force", "issue-close-force"),
            ("curl https://example.com", "curl-network"),
        ] {
            let routed = decide_isolated(command);
            assert_eq!(asked_id(&routed).as_deref(), Some(id), "{command}");
            // The operator is asked about the command as typed: nothing is
            // inserted into it.
            assert_eq!(routed.facts.rewritten, None, "{command}");
        }
    }

    #[test]
    fn outside_a_worktree_isolated_agent_git_still_gets_legion() {
        for (command, rewritten) in [
            ("git status", "legion git status"),
            ("gh pr view 1", "legion gh pr view 1"),
        ] {
            let routed = route(&passthrough_policy(), &bash(command), &Context::default());
            assert_eq!(routed.facts.rewritten.as_deref(), Some(rewritten));
        }
    }

    #[test]
    fn a_command_matching_no_rule_runs_as_typed() {
        for command in [
            "echo hello",
            "ls -la",
            "python3 -c \"import glob; print(glob.glob('**/*.rs'))\"",
            "echo 'grep for it with rg'",
            "cargo test --bin legion",
            "legion recall --repo legion --context x",
        ] {
            let routed = decide(command);
            assert_eq!(routed.decision, Decision::Allow { note: None }, "{command}");
            assert_eq!(routed.deciding, Deciding::Default, "{command}");
            assert_eq!(routed.facts.rewritten, None, "{command}");
        }
    }

    #[test]
    fn a_parse_error_is_denied() {
        let routed = decide("grep 'unterminated");
        assert_eq!(routed.deciding, Deciding::ParseError);
        let Decision::Deny(details) = routed.decision else {
            panic!("a parse error denies");
        };
        assert!(details.reason().contains("shell parser"));
    }

    #[test]
    fn no_bash_decision_names_a_command_to_run_instead() {
        for command in [
            "rm -rf /",
            "grep 'unterminated",
            "curl x",
            "git push --force",
            "git status",
            "echo hi",
        ] {
            if let Decision::Deny(details) = decide(command).decision {
                assert_eq!(details.instead(), crate::NO_GO_INSTEAD, "{command}");
            }
        }
    }

    #[test]
    fn a_tool_the_policy_does_not_model_runs_untouched() {
        let call = ToolCall {
            tool: "NotebookEdit".to_string(),
            input: serde_json::json!({}),
        };
        let routed = route(&policy(), &call, &Context::default());
        assert_eq!(routed.decision, Decision::Allow { note: None });
    }
}
