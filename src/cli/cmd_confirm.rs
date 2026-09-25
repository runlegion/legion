//! `legion cmd confirm --reason <why> -- <command>` (#1237, FR-CMD-026): how
//! an agent answers an ask. Records a one-time confirmation for exactly
//! `<command>` in the current session; exits non-zero and records nothing
//! when the reason is missing or empty, when the command matches a no-go
//! entry, when it does not parse, or when the record cannot be written.

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
        /// The command, after `--`. Pass it as one quoted word to keep its
        /// quoting exactly; several words are re-joined as shell words.
        #[arg(last = true, required = true)]
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
    match run_confirm(reason, words) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("legion cmd confirm: {e}");
            Err(error::LegionError::ExitWith(1))
        }
    }
}

fn run_confirm(reason: Option<String>, words: &[String]) -> Result<(), ConfirmError> {
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
            command: join_command(words),
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

/// The command as one string. One word is taken exactly as given; several
/// are re-joined, each word that needs it single-quoted, so the command
/// parses back to the same words.
fn join_command(words: &[String]) -> String {
    if let [only] = words {
        return only.clone();
    }
    words
        .iter()
        .map(|word| {
            let plain = !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c));
            if plain {
                word.clone()
            } else {
                format!("'{}'", word.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_word_is_taken_exactly_and_several_are_rejoined_as_shell_words() {
        assert_eq!(
            join_command(&["git push origin 'a b'".to_string()]),
            "git push origin 'a b'"
        );
        let joined = join_command(&["echo".to_string(), "a b".to_string(), "it's".to_string()]);
        assert_eq!(joined, r"echo 'a b' 'it'\''s'");
        assert_eq!(
            legion_cmd::command_key(&joined).expect("key"),
            legion_cmd::command_key(r#"echo "a b" "it's""#).expect("key")
        );
    }
}
