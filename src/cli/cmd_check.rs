//! `legion cmd-check`: the CLI surface over the legion-cmd hook adapter
//! (#1229): `--hook` answers one PreToolUse payload, and `--deny-patterns`
//! prints the no-go permissions mirror (#1237). The operator and scripting
//! mode ships in the cmd-check issue.

use std::process::ExitCode;

use crate::cmd::hook::run_hook;
use crate::error;

/// Dispatches `legion cmd-check`. `--hook` reads one PreToolUse payload on
/// stdin and writes one hook response on stdout; any other invocation is
/// refused as not yet implemented rather than silently doing nothing.
pub(crate) fn handle_cmd_check(hook: bool, deny_patterns: bool) -> error::Result<()> {
    if deny_patterns {
        println!("{}", deny_patterns_json()?);
        return Ok(());
    }
    if !hook {
        return Err(error::LegionError::NotImplemented {
            feature:
                "legion cmd-check without --hook (the operator and scripting mode ships separately)"
                    .to_string(),
        });
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    // `run_hook` answers every payload with a response and exits 0
    // (FR-CMD-009); the code it returns is always success, so there is
    // nothing to map onto `LegionError::ExitWith` here.
    let _always_success: ExitCode = run_hook(stdin.lock(), stdout.lock());
    Ok(())
}

/// The permissions.deny patterns mirroring every built-in no-go entry
/// (FR-CMD-025), as one JSON array in entry order, for plugin setup
/// (`plugin/hooks/lib/deny-mirror.sh`) to merge into the harness settings.
fn deny_patterns_json() -> error::Result<String> {
    let patterns: Vec<&str> = legion_cmd::BUILTIN_DENY_PATTERNS
        .iter()
        .flat_map(|(_, patterns)| patterns.iter().copied())
        .collect();
    Ok(serde_json::to_string(&patterns)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deny_patterns_are_one_json_array_covering_every_builtin_entry() {
        let text = deny_patterns_json().expect("serializes");
        let patterns: Vec<String> = serde_json::from_str(&text).expect("a JSON string array");
        for (id, entry_patterns) in legion_cmd::BUILTIN_DENY_PATTERNS {
            assert!(
                entry_patterns
                    .iter()
                    .all(|p| patterns.contains(&p.to_string())),
                "{id} missing"
            );
        }
        assert_eq!(
            legion_cmd::BUILTIN_DENY_PATTERNS.len(),
            legion_cmd::builtin_no_go().len()
        );
    }
}
