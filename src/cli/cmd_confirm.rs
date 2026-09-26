//! `legion cmd confirm --reason <why> -- <command>` (#1237, FR-CMD-026): how
//! an agent answers an ask. Records a one-time confirmation for exactly
//! `<command>` in the current session; exits non-zero and records nothing
//! when the reason is missing or empty, when the command matches a no-go
//! entry, when it does not parse, or when the record cannot be written.
//!
//! The command is exactly one argument, taken verbatim, as `legion cmd-check`
//! takes it (#1280). With several words the invoking shell has already
//! removed their quoting, and shell text rebuilt from them is not the command
//! the agent will run: a subscripted assignment prefix such as `arr[0]=x`
//! would stop reading as one, hiding the command behind it from the no-go
//! check, and a requoted `~` or `$VAR` would store a key the hook never
//! computes. So more than one word is a usage error, never rebuilt.

use chrono::Utc;
use clap::Subcommand;
use legion_cmd::{Policy, parse_policy};

use crate::cli::util::open_db;
use crate::cmd::confirm::{ConfirmError, ConfirmRequest, confirm};
use crate::cmd::hook::{LEGION_REPO_ENV, configured_policy_path, repo_for};
use crate::cmd::incident::{CONFIRMATION_TTL, IncidentLog, Origin, agent_for};
use crate::error;

/// The environment variable Claude Code sets in every Bash tool subprocess to
/// the session id -- the same session the hook payload's `session_id` names.
/// legion's shipped skills already read it for `legion uncertainty emit
/// --session-id` (plugin/skills/sd-*).
const SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

#[derive(Subcommand)]
pub(crate) enum CmdAction {
    /// Confirm a command legion-cmd asked about, with the reason it should
    /// run. The confirmation lets exactly that command run once in this
    /// session, within 10 minutes. A no-go command can never be confirmed.
    Confirm {
        /// Why this command should run. Required and non-empty.
        #[arg(long)]
        reason: Option<String>,
        /// The command, after `--`, as one quoted argument, taken verbatim.
        /// More than one word is a usage error (exit 2) and records nothing.
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
}

/// Dispatches `legion cmd`.
pub(crate) fn handle_cmd(action: CmdAction) -> error::Result<()> {
    match action {
        CmdAction::Confirm { reason, command } => handle_confirm(reason, &command),
    }
}

fn handle_confirm(reason: Option<String>, words: &[String]) -> error::Result<()> {
    // The usage check comes before any other work, so a refused form writes
    // no drop rows, no incident record, and no confirmation.
    let command: &str = match command_arg(words) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("[legion] error: {message}");
            return Err(error::LegionError::ExitWith(2));
        }
    };
    match run_confirm(reason, command) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("legion cmd confirm: {e}");
            Err(error::LegionError::ExitWith(1))
        }
    }
}

fn run_confirm(reason: Option<String>, command: &str) -> Result<(), ConfirmError> {
    let session_id: String = std::env::var(SESSION_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or(ConfirmError::NoSession(SESSION_ENV))?;
    let policy: Policy = read_policy()?;
    let cwd: String = std::env::current_dir()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    let legion_repo: Option<String> = std::env::var(LEGION_REPO_ENV).ok();
    let repo: String = repo_for(legion_repo.as_deref(), Some(&cwd)).unwrap_or_default();
    let request = ConfirmRequest {
        origin: Origin {
            command: command.to_string(),
            agent: agent_for(&repo),
            repo,
            session_id,
            cwd,
        },
        reason,
    };
    let db = open_db().map_err(|e| ConfirmError::Store(e.to_string()))?;
    let now = Utc::now();
    let stored = confirm(&request, &policy, &db, &IncidentLog::production(), now)?;
    println!(
        "confirmed for this session until {}: {}",
        (stored.recorded_at + CONFIRMATION_TTL).to_rfc3339(),
        request.origin.command
    );
    Ok(())
}

/// The policy whose no-go entries a confirmation is checked against. With no
/// policy file configured in this environment, the built-in entries alone
/// apply (they hold with an empty policy); a configured file that cannot be
/// read or parsed refuses the confirmation rather than checking less.
fn read_policy() -> Result<Policy, ConfirmError> {
    let Some(path) = configured_policy_path() else {
        return Ok(Policy::default());
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| ConfirmError::Policy(format!("{}: {e}", path.display())))?;
    parse_policy(&text).map_err(|e| ConfirmError::Policy(format!("{}: {e}", path.display())))
}

/// The one command argument after `--`, exactly as given. More than one word
/// is refused: see the module docs for why it is never rebuilt.
fn command_arg(words: &[String]) -> Result<&str, &'static str> {
    match words {
        [only] => Ok(only.as_str()),
        [] => Err("no command to confirm: pass it after `--` as one quoted argument"),
        _ => Err(MULTI_WORD_USAGE),
    }
}

/// The usage error for more than one word after `--`.
const MULTI_WORD_USAGE: &str =
    "pass the command as one quoted argument: legion cmd confirm --reason <why> -- '<command>'";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::confirm::live_confirmations;
    use crate::db::testutil::test_db;
    use legion_cmd::{CommandKey, Context, ToolCall, route};

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|w| w.to_string()).collect()
    }

    fn request(command: &str) -> ConfirmRequest {
        ConfirmRequest {
            origin: Origin {
                command: command.to_string(),
                agent: "legion".to_string(),
                repo: "legion".to_string(),
                session_id: "s1".to_string(),
                cwd: "/repo/legion".to_string(),
            },
            reason: Some("the operator asked for it".to_string()),
        }
    }

    fn log() -> (IncidentLog, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (IncidentLog::at(dir.path().join("cmd-incidents.jsonl")), dir)
    }

    fn bash(command: &str) -> ToolCall {
        ToolCall {
            tool: "Bash".to_string(),
            input: serde_json::json!({ "command": command }),
        }
    }

    #[test]
    fn one_argument_is_taken_verbatim() {
        let typed = "curl ~/notes $HOME/x 'a b' *.txt {a,b}";
        assert_eq!(command_arg(&words(&[typed])), Ok(typed));
    }

    #[test]
    fn more_than_one_word_is_a_usage_error_that_records_nothing() {
        let split = words(&["arr[0]=x", "git", "push", "--force", "origin", "main"]);
        assert_eq!(command_arg(&split), Err(MULTI_WORD_USAGE));
        // The refusal comes before the session, policy, store, or log is
        // touched: exit 2, nothing recorded.
        let refused = handle_confirm(Some("r".to_string()), &split).expect_err("refused");
        assert!(
            matches!(refused, error::LegionError::ExitWith(2)),
            "{refused:?}"
        );
    }

    /// The split-argv half of each regression: the words an unquoted shell
    /// would pass after `--` are refused as a usage error, exit 2, before the
    /// session, policy, store, or log is touched -- never rebuilt and routed.
    fn assert_split_argv_is_refused(split: &[&str]) {
        let argv: Vec<String> = words(split);
        assert_eq!(command_arg(&argv), Err(MULTI_WORD_USAGE), "{split:?}");
        let refused = handle_confirm(Some("r".to_string()), &argv).expect_err("refused");
        assert!(
            matches!(refused, error::LegionError::ExitWith(2)),
            "{split:?}: {refused:?}"
        );
    }

    #[test]
    fn a_subscripted_assignment_prefix_does_not_hide_a_no_go_command() {
        let db = test_db();
        let (log, _dir) = log();
        let now = Utc::now();
        let cases: [(&[&str], &str); 2] = [
            (
                &["arr[0]=x", "git", "push", "--force", "origin", "main"],
                "arr[0]=x git push --force origin main",
            ),
            (
                &["arr[i[0]]=x", "git", "push", "--force", "origin", "main"],
                "arr[i[0]]=x git push --force origin main",
            ),
        ];
        for (split, typed) in cases {
            // Split argv: a usage error, nothing confirmed.
            assert_split_argv_is_refused(split);

            // One quoted argument: routed as typed, refused as a no-go.
            let argv: Vec<String> = words(&[typed]);
            let command: &str = command_arg(&argv).expect("one argument");
            let err = confirm(&request(command), &Policy::default(), &db, &log, now)
                .expect_err("a no-go command is never confirmed");
            match err {
                ConfirmError::NoGo { entry } => assert_eq!(entry, "git-force-push-main", "{typed}"),
                other => panic!("`{typed}` expected NoGo, got {other:?}"),
            }
        }
        assert!(live_confirmations(&db, "s1", now).expect("read").is_empty());
        assert!(log.records().expect("read").is_empty());
    }

    #[test]
    fn a_confirmed_command_with_expansions_stores_the_key_the_hook_computes() {
        let db = test_db();
        let (log, _dir) = log();
        let now = Utc::now();
        let policy = legion_cmd::parse_policy(
            r#"{"tools": {"Bash": {"families": {
                "curl": {"rules": [{"id": "curl-ask", "outcome": {"kind": "ask",
                    "question": "fetch?", "reason": "network"}}]}
            }}}}"#,
        )
        .expect("policy");
        let cases: [(&[&str], &str); 2] = [
            (
                &["curl", "-o", "~/notes.txt", "$HOME/x"],
                "curl -o ~/notes.txt $HOME/x",
            ),
            (&["curl", "~/a", "*", "{a,b}"], "curl ~/a * {a,b}"),
        ];
        for (split, typed) in cases {
            // Split argv: a usage error, so no requoted key is ever stored.
            assert_split_argv_is_refused(split);

            // One quoted argument: the stored key is the hook's key.
            let argv: Vec<String> = words(&[typed]);
            let command: &str = command_arg(&argv).expect("one argument");
            let stored = confirm(&request(command), &policy, &db, &log, now).expect("confirmed");

            // The key route computes for the same command in the hook.
            let unconfirmed = route(&policy, &bash(typed), &Context::default());
            let hook_key: CommandKey = unconfirmed.facts.command_key.expect("key");
            assert_eq!(stored.command_key, hook_key.as_str(), "{typed}");

            // And route, given this session's confirmations, uses it.
            let ctx = Context {
                confirmations: live_confirmations(&db, "s1", now).expect("read"),
                ..Context::default()
            };
            assert!(route(&policy, &bash(typed), &ctx).confirmed, "{typed}");
        }
    }
}
