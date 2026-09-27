//! Builds the replacement `tool_input` for a `Decision::Rewrite` (#1229,
//! FR-CMD-003, FR-CMD-008).
//!
//! route names the managed target and returns the `Facts` it extracted while
//! deciding; it never emits the replacement command string. This module is
//! the one place that turns those two into a `tool_input`, and it never
//! scans the command string (FR-CMD-017): a Bash replacement is built from
//! the words route carried (`Facts::carried`), never from `command`.
//!
//! Which field the target replaces depends on the tool (#1233): a Bash call's
//! `command`, or an Agent/Task spawn's `subagent_type` (the Explore rewrite
//! `plugin/hooks/no-harness-explore.sh` performed). [`rewritable_field`] is
//! the one place that choice is made. A spawn's rewrite patches the target
//! name and nothing more: it has no argument words.
//!
//! # A Bash replacement, judged by its target (FR-CMD-008 rev 3)
//!
//! A Bash rewrite is lossless only if the target runs the arguments the
//! agent typed as the agent meant them, and the target's own command-line
//! definition is the one authority on what it accepts. So the replacement is
//! assembled from the rule's target prefix and the carried words, then parsed
//! with the target's clap command tree before it is ever returned:
//!
//! 1. The prefix, its `{repo}` fill-in taken from the repo the adapter
//!    derived. A source flag the prefix also sets replaces the prefix's value
//!    in place (the first occurrence only; a repeat is left for the parse to
//!    judge), after any `reshape` exception has transformed its value.
//! 2. Each source positional the rule's `positional` exception names, as its
//!    named flag, in list order.
//! 3. Every other carried word, in source order.
//!
//! The parse accepts, and the words are shell-quoted and joined; or it
//! rejects, and the rewrite is refused naming what the target rejected. A
//! help or version request is refused too: it would run nothing the agent
//! asked for. Which word is a flag's value is read from the same tree -- the
//! target's arity, never a table here -- so no binary, flag or verb name
//! lives in this module (FR-CMD-011): the target's first word is matched to
//! the tree's own name.

use clap::error::ErrorKind;
use clap::{ColorChoice, Command};
use legion_cmd::{Facts, RewriteSpec};
use serde_json::{Map, Value};

/// The prefix word the adapter fills with the repo it derived (`Context::repo`).
const REPO_FILL: &str = "{repo}";

/// Why a rewrite's replacement could not be built. Each becomes a deny at the
/// adapter (FR-CMD-009: no Decision drops the command without a message).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ReplacementError {
    /// The target names nothing to run. `parse_policy` rejects an empty
    /// rewrite target, so this is only reachable from a `Routed` built by
    /// hand; it stays a refusal rather than an empty command.
    #[error("the rewrite target is empty; nothing to run in place of the command")]
    EmptyTarget,

    /// The original `tool_input` is not an object carrying a field a rewrite
    /// can replace (an Edit or Write call, or a malformed input). Inserting
    /// one would leave the fields the tool actually runs on untouched, and the
    /// adapter's `allow` would then grant the original call outright.
    #[error("the tool input has no command or subagent_type field to replace")]
    NoRewritableField,

    /// A Bash target whose first word is not the command tree the adapter
    /// judges with: its arguments cannot be checked, so it is not run.
    #[error(
        "the rewrite target `{target}` is not a `{tree}` command, so its arguments cannot be checked"
    )]
    UncheckableTarget { target: String, tree: String },

    /// The target prefix needs the repo, the command sets none, and the
    /// adapter derived none from `LEGION_REPO` or the working directory.
    #[error("the rewrite target `{target}` needs a repo and none is known; pass the repo flag")]
    NoRepo { target: String },

    /// A `reshape` exception could not read the value it transforms.
    #[error("`{flag} {value}` does not have the shape the rewrite's `{transform}` reshape reads")]
    Unreshapable {
        flag: String,
        value: String,
        transform: &'static str,
    },

    /// The target's own command-line definition rejected the assembled
    /// command (FR-CMD-008): `reason` is clap's error, naming the argument.
    #[error("the rewrite target `{target}` rejects this command ({reason})")]
    Rejected { target: String, reason: String },

    /// The carried words ask the target for its help or version, which runs
    /// nothing the agent asked the source command for.
    #[error("the rewrite to `{target}` would only print help or version, so it is not run")]
    HelpRequested { target: String },
}

/// The `tool_input` field a rewrite target replaces: `command` for a Bash
/// call, `subagent_type` for an Agent or Task spawn. `None` for any other
/// shape, which the rewrite refuses rather than inventing a field.
pub(crate) fn rewritable_field(original: &Value) -> Option<&'static str> {
    let Value::Object(map) = original else {
        return None;
    };
    ["command", "subagent_type"]
        .into_iter()
        .find(|field| map.contains_key(*field))
}

/// Builds the replacement `tool_input` for a rewrite under `spec`: the
/// replacement patched into `original`'s [`rewritable_field`], so every
/// sibling field survives -- a Bash call's `description`, `timeout`,
/// `run_in_background`; a spawn's `prompt` and `description`. `updatedInput`
/// replaces the whole `tool_input`, so rebuilding it from the command alone
/// would turn a background command into a foreground one and drop a raised
/// timeout (the bug `emit.sh`'s `emit_rewrite` already paid for).
///
/// A Bash `command` becomes [`replacement_command`] over the carried facts;
/// a spawn's `subagent_type` becomes the target name.
pub(crate) fn build_replacement(
    spec: &RewriteSpec,
    facts: &Facts,
    repo: Option<&str>,
    tree: &Command,
    original: &Value,
) -> Result<Value, ReplacementError> {
    if spec.target.as_str().trim().is_empty() {
        return Err(ReplacementError::EmptyTarget);
    }
    let (Some(field), Value::Object(map)) = (rewritable_field(original), original) else {
        return Err(ReplacementError::NoRewritableField);
    };
    let value: String = if field == "command" {
        replacement_command(spec, &facts.carried, repo, tree)?
    } else {
        spec.target.as_str().to_string()
    };
    let mut patched: Map<String, Value> = map.clone();
    patched.insert(field.to_string(), Value::String(value));
    Ok(Value::Object(patched))
}

/// One carried word, read by the target's arity.
enum Carried {
    /// An option word, with its value: the next word when the target says
    /// the option takes one, or the text after `=` (`joined`).
    Flag {
        name: String,
        value: Option<String>,
        joined: bool,
    },
    /// Any other word: an operand, or `--` and everything after it.
    Word(String),
}

/// The Bash replacement command for a rewrite under `spec`, assembled from
/// its prefix and the `carried` words and accepted by the target's clap
/// `tree` (see the module doc for the order). Every word is shell-quoted.
pub(crate) fn replacement_command(
    spec: &RewriteSpec,
    carried: &[String],
    repo: Option<&str>,
    tree: &Command,
) -> Result<String, ReplacementError> {
    let target: &str = spec.target.as_str();
    let mut words: Vec<String> = target.split_whitespace().map(str::to_string).collect();
    if words.is_empty() {
        return Err(ReplacementError::EmptyTarget);
    }
    if words[0] != tree.get_name() {
        return Err(ReplacementError::UncheckableTarget {
            target: target.to_string(),
            tree: tree.get_name().to_string(),
        });
    }
    let path: Vec<&Command> = command_path(tree, &words[1..]);

    let mut mapped: Vec<String> = Vec::new();
    let mut rest: Vec<String> = Vec::new();
    let mut positionals = spec.positional.iter();
    let mut replaced: Vec<String> = Vec::new();
    for item in read_carried(carried, &path) {
        match item {
            Carried::Flag {
                name,
                value,
                joined,
            } => {
                let value: Option<String> = match (spec.reshape.get(&name), value) {
                    (Some(transform), Some(value)) => {
                        Some(transform.apply(&value).ok_or_else(|| {
                            ReplacementError::Unreshapable {
                                flag: name.clone(),
                                value: value.clone(),
                                transform: transform.as_str(),
                            }
                        })?)
                    }
                    (_, value) => value,
                };
                let slot: Option<usize> = (!replaced.contains(&name))
                    .then(|| prefix_value_slot(&words, &name))
                    .flatten();
                match (slot, value) {
                    (Some(slot), Some(value)) => {
                        words[slot] = value;
                        replaced.push(name);
                    }
                    (_, Some(value)) if joined => rest.push(format!("{name}={value}")),
                    (_, Some(value)) => rest.extend([name, value]),
                    (_, None) => rest.push(name),
                }
            }
            Carried::Word(word) => match positionals.next() {
                Some(flag) if word != "--" => mapped.extend([flag.clone(), word]),
                _ => rest.push(word),
            },
        }
    }

    for word in &mut words {
        if word == REPO_FILL {
            *word = repo
                .ok_or_else(|| ReplacementError::NoRepo {
                    target: target.to_string(),
                })?
                .to_string();
        }
    }
    words.extend(mapped);
    words.extend(rest);

    judge(tree, &words, target)?;
    Ok(words
        .iter()
        .map(|word| shell_quote(word))
        .collect::<Vec<String>>()
        .join(" "))
}

/// The commands the prefix names, root first: each prefix word after the
/// binary that names a subcommand of the one before it. An option a
/// subcommand reads may be declared on any command in the path (a global
/// flag lives on the root).
fn command_path<'t>(tree: &'t Command, prefix: &[String]) -> Vec<&'t Command> {
    let mut path: Vec<&Command> = vec![tree];
    for word in prefix {
        let Some(current) = path.last() else {
            break;
        };
        match current.find_subcommand(word) {
            Some(sub) => path.push(sub),
            None => break,
        }
    }
    path
}

/// Reads the carried words as the target reads them: an option word takes
/// the next word as its value only when the target declares that it takes
/// one; `--name=value` carries its own; `--` and every word after it is an
/// operand. An option the target does not know takes no value -- the parse
/// then rejects it by name.
fn read_carried(carried: &[String], path: &[&Command]) -> Vec<Carried> {
    let mut items: Vec<Carried> = Vec::new();
    let mut words = carried.iter();
    let mut operands_only = false;
    while let Some(word) = words.next() {
        if operands_only || word == "-" || !word.starts_with('-') {
            items.push(Carried::Word(word.clone()));
            continue;
        }
        if word == "--" {
            operands_only = true;
            items.push(Carried::Word(word.clone()));
            continue;
        }
        if let Some((name, value)) = word.split_once('=').filter(|_| word.starts_with("--")) {
            items.push(Carried::Flag {
                name: name.to_string(),
                value: Some(value.to_string()),
                joined: true,
            });
            continue;
        }
        let value: Option<String> = takes_value(path, word)
            .then(|| words.next().cloned())
            .flatten();
        items.push(Carried::Flag {
            name: word.clone(),
            value,
            joined: false,
        });
    }
    items
}

/// Whether the option `word` (`--name` or `-n`) takes a value, by the
/// target's own declaration on any command in `path`.
fn takes_value(path: &[&Command], word: &str) -> bool {
    let long: Option<&str> = word.strip_prefix("--");
    let short: Option<char> = match (long, word.strip_prefix('-')) {
        (None, Some(rest)) if rest.chars().count() == 1 => rest.chars().next(),
        _ => None,
    };
    path.iter()
        .flat_map(|command| command.get_arguments())
        .find(|arg| {
            long.is_some_and(|name| arg.get_long() == Some(name))
                || short.is_some_and(|c| arg.get_short() == Some(c))
        })
        .is_some_and(|arg| arg.get_action().takes_values())
}

/// The index of the value word after `flag` in the prefix, when the prefix
/// sets it as `flag <value>`.
fn prefix_value_slot(words: &[String], flag: &str) -> Option<usize> {
    let index: usize = words.iter().position(|word| word == flag)?;
    (index + 1 < words.len()).then_some(index + 1)
}

/// Parses `words` with the target's clap `tree` (FR-CMD-008): accepted, or
/// the refusal naming what the target rejected. clap's message is its error
/// text before the usage block -- the line that names the unexpected
/// argument, plus the lines listing any missing required ones.
fn judge(tree: &Command, words: &[String], target: &str) -> Result<(), ReplacementError> {
    let Err(error) = tree
        .clone()
        .color(ColorChoice::Never)
        .try_get_matches_from(words)
    else {
        return Ok(());
    };
    match error.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        | ErrorKind::DisplayVersion => Err(ReplacementError::HelpRequested {
            target: target.to_string(),
        }),
        _ => {
            let rendered: String = error.to_string();
            let reason: String = rendered
                .lines()
                .take_while(|line| !line.trim().is_empty())
                .map(str::trim)
                .collect::<Vec<&str>>()
                .join(" ");
            Err(ReplacementError::Rejected {
                target: target.to_string(),
                reason,
            })
        }
    }
}

/// `word` as one shell word: unchanged when every character is one the shell
/// passes through literally, else single-quoted, closing and reopening the
/// quote around each embedded single quote (`'\''`). An empty word is `''`.
fn shell_quote(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        return word.to_string();
    }
    shell_single_quote(word)
}

/// Single-quotes `text` for a shell command line, closing and reopening the
/// quote around every embedded single quote (`'\''`), so a command carrying
/// quotes or metacharacters stays one word.
pub(crate) fn shell_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Arg, ArgAction};
    use legion_cmd::ManagedTarget;
    use std::collections::BTreeMap;

    /// A command tree standing in for the target's: `legion issue view
    /// --repo <R> --number <N> [--json]`, `legion push --repo <R> [--force]`,
    /// and a global `-v`.
    fn tree() -> Command {
        let repo = || Arg::new("repo").long("repo").required(true);
        Command::new("legion")
            .arg(
                Arg::new("verbose")
                    .short('v')
                    .long("verbose")
                    .global(true)
                    .action(ArgAction::SetTrue),
            )
            .subcommand(
                Command::new("issue").subcommand(
                    Command::new("view")
                        .arg(repo())
                        .arg(Arg::new("number").long("number").required(true))
                        .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
                ),
            )
            .subcommand(
                Command::new("push")
                    .arg(repo())
                    .arg(Arg::new("force").long("force").action(ArgAction::SetTrue))
                    .arg(Arg::new("message").short('m').long("message")),
            )
    }

    fn spec(target: &str) -> RewriteSpec {
        RewriteSpec {
            target: ManagedTarget::new(target),
            positional: Vec::new(),
            reshape: BTreeMap::new(),
            deny_flags: Vec::new(),
        }
    }

    fn issue_view() -> RewriteSpec {
        RewriteSpec {
            positional: vec!["--number".to_string()],
            reshape: BTreeMap::from([("--repo".to_string(), legion_cmd::Reshape::RepoName)]),
            ..spec("legion issue view --repo {repo}")
        }
    }

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    fn command(spec: &RewriteSpec, carried: &[&str]) -> Result<String, ReplacementError> {
        replacement_command(spec, &words(carried), Some("legion"), &tree())
    }

    fn facts(carried: &[&str]) -> Facts {
        Facts {
            carried: words(carried),
            ..Facts::default()
        }
    }

    #[test]
    fn the_prefix_fills_its_repo_and_carries_the_words() {
        assert_eq!(
            command(&spec("legion push --repo {repo}"), &[]),
            Ok("legion push --repo legion".to_string())
        );
        assert_eq!(
            command(&spec("legion push --repo {repo}"), &["-m", "a b"]),
            Ok("legion push --repo legion -m 'a b'".to_string())
        );
    }

    #[test]
    fn a_positional_becomes_its_named_flag_after_the_prefix() {
        assert_eq!(
            command(&issue_view(), &["1278"]),
            Ok("legion issue view --repo legion --number 1278".to_string())
        );
        // The source's flag between the prefix and the operand stays after
        // the mapped flag, in source order.
        assert_eq!(
            command(&issue_view(), &["--json", "1278"]),
            Ok("legion issue view --repo legion --number 1278 --json".to_string())
        );
    }

    #[test]
    fn a_source_flag_the_prefix_sets_replaces_its_value_in_place_after_reshape() {
        for carried in [
            vec!["1278", "--repo", "runlegion/legion"],
            vec!["--repo=runlegion/legion", "1278"],
            vec!["--repo", "legion", "1278"],
        ] {
            assert_eq!(
                command(&issue_view(), &carried),
                Ok("legion issue view --repo legion --number 1278".to_string()),
                "{carried:?}"
            );
        }
        // The source's repo wins over the derived one, and needs none.
        assert_eq!(
            replacement_command(
                &issue_view(),
                &words(&["7", "--repo", "acme/widgets"]),
                None,
                &tree()
            ),
            Ok("legion issue view --repo widgets --number 7".to_string())
        );
    }

    #[test]
    fn a_repeated_prefix_flag_is_left_for_the_target_to_reject() {
        let err = command(&issue_view(), &["1", "--repo", "a/x", "--repo", "b/y"])
            .expect_err("two repos are not one");
        match err {
            ReplacementError::Rejected { reason, .. } => {
                assert!(reason.contains("--repo"), "{reason}")
            }
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn an_argument_the_target_rejects_is_refused_naming_it() {
        for (carried, named) in [
            (vec!["1278", "--web"], "'--web'"),
            (vec!["1278", "extra"], "'extra'"),
            (vec!["1278", "-x"], "'-x'"),
        ] {
            match command(&issue_view(), &carried) {
                Err(ReplacementError::Rejected { target, reason }) => {
                    assert_eq!(target, "legion issue view --repo {repo}");
                    assert!(reason.contains(named), "{carried:?}: {reason}");
                    assert!(!reason.contains("Usage"), "{reason}");
                }
                other => panic!("{carried:?}: expected a rejection, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_missing_required_argument_is_refused_naming_it() {
        match command(&issue_view(), &[]) {
            Err(ReplacementError::Rejected { reason, .. }) => {
                assert!(reason.contains("--number"), "{reason}")
            }
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn a_help_or_version_request_is_refused() {
        for carried in [vec!["-h"], vec!["--help"]] {
            assert_eq!(
                command(&spec("legion push --repo {repo}"), &carried),
                Err(ReplacementError::HelpRequested {
                    target: "legion push --repo {repo}".to_string()
                }),
                "{carried:?}"
            );
        }
    }

    #[test]
    fn a_global_flag_of_the_target_is_accepted_from_any_level() {
        assert_eq!(
            command(&spec("legion push --repo {repo}"), &["-v"]),
            Ok("legion push --repo legion -v".to_string())
        );
    }

    #[test]
    fn a_value_is_read_by_the_target_arity() {
        // `-m` takes a value in the target, so the word after it is the
        // message, never read as an operand.
        assert_eq!(
            command(&spec("legion push --repo {repo}"), &["-m", "x", "--force"]),
            Ok("legion push --repo legion -m x --force".to_string())
        );
        // `--force` takes none, so the word after it is an operand the target
        // rejects.
        assert!(matches!(
            command(&spec("legion push --repo {repo}"), &["--force", "origin"]),
            Err(ReplacementError::Rejected { .. })
        ));
    }

    #[test]
    fn no_repo_to_fill_is_refused() {
        assert_eq!(
            replacement_command(&spec("legion push --repo {repo}"), &[], None, &tree()),
            Err(ReplacementError::NoRepo {
                target: "legion push --repo {repo}".to_string()
            })
        );
    }

    #[test]
    fn an_unreadable_reshape_value_is_refused() {
        assert_eq!(
            command(&issue_view(), &["1", "--repo", "a/b/c"]),
            Err(ReplacementError::Unreshapable {
                flag: "--repo".to_string(),
                value: "a/b/c".to_string(),
                transform: "repo_name",
            })
        );
    }

    #[test]
    fn a_target_outside_the_tree_is_refused() {
        assert_eq!(
            command(&spec("other push"), &[]),
            Err(ReplacementError::UncheckableTarget {
                target: "other push".to_string(),
                tree: "legion".to_string(),
            })
        );
    }

    #[test]
    fn every_carried_word_is_shell_quoted_only_when_it_needs_it() {
        for (word, quoted) in [
            ("plain-word_1.2/x:y=z", "plain-word_1.2/x:y=z"),
            ("a b", "'a b'"),
            ("", "''"),
            ("it's", r"'it'\''s'"),
            ("$HOME", "'$HOME'"),
            ("#1", "'#1'"),
            ("~/x", "'~/x'"),
            ("*", "'*'"),
            ("a\nb", "'a\nb'"),
        ] {
            assert_eq!(shell_quote(word), quoted, "{word:?}");
        }
    }

    #[test]
    fn the_replacement_is_built_from_the_facts_never_the_command_text() {
        // FR-CMD-017: the command field is replaced, never read.
        let spec = spec("legion push --repo {repo}");
        let facts = facts(&["-m", "a b"]);
        let build = |command: &str| {
            build_replacement(
                &spec,
                &facts,
                Some("legion"),
                &tree(),
                &serde_json::json!({"command": command}),
            )
        };
        let expected = serde_json::json!({"command": "legion push --repo legion -m 'a b'"});
        assert_eq!(build("git push -m 'a b'"), Ok(expected.clone()));
        assert_eq!(build("unrelated text entirely"), Ok(expected));
    }

    #[test]
    fn sibling_bash_fields_are_preserved() {
        let original = serde_json::json!({
            "command": "gh issue list",
            "description": "list issues",
            "timeout": 5000,
            "run_in_background": true
        });
        let replaced = build_replacement(
            &spec("legion push --repo {repo}"),
            &Facts::default(),
            Some("legion"),
            &tree(),
            &original,
        )
        .expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({
                "command": "legion push --repo legion",
                "description": "list issues",
                "timeout": 5000,
                "run_in_background": true
            })
        );
    }

    #[test]
    fn an_empty_target_is_refused() {
        let err = build_replacement(
            &spec(""),
            &Facts::default(),
            Some("legion"),
            &tree(),
            &serde_json::json!({"command": "gh issue list"}),
        )
        .expect_err("an empty target must not become an empty command");
        assert_eq!(err, ReplacementError::EmptyTarget);
    }

    #[test]
    fn an_agent_spawn_has_its_subagent_type_patched_and_keeps_its_prompt() {
        // The Explore rewrite (#1233): only subagent_type changes; prompt and
        // description ride through, as no-harness-explore.sh did. A spawn
        // carries no argument words, so no parse judges it.
        let original = serde_json::json!({
            "subagent_type": "Explore",
            "prompt": "map it",
            "description": "explore"
        });
        let replaced = build_replacement(
            &spec("legion:legion-explore"),
            &Facts::default(),
            None,
            &tree(),
            &original,
        )
        .expect("builds");
        assert_eq!(
            replaced,
            serde_json::json!({
                "subagent_type": "legion:legion-explore",
                "prompt": "map it",
                "description": "explore"
            })
        );
    }

    #[test]
    fn a_tool_input_with_no_rewritable_field_is_refused() {
        for original in [
            serde_json::json!({"file_path": ".env", "old_string": "KEY"}),
            Value::Null,
            serde_json::json!(["gh", "issue", "list"]),
            serde_json::json!("gh issue list"),
        ] {
            let err = build_replacement(
                &spec("legion push --repo {repo}"),
                &Facts::default(),
                Some("legion"),
                &tree(),
                &original,
            )
            .expect_err("a rewrite must not invent a field the tool ignores");
            assert_eq!(err, ReplacementError::NoRewritableField, "{original}");
        }
    }
}
