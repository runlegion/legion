//! `legion cmd-check`: the CLI surface over the legion-cmd hook adapter
//! (#1229). Only `--hook` exists in this slice; the operator and scripting
//! mode ships in the cmd-check issue.

use std::process::ExitCode;

use crate::cmd::hook::run_hook;
use crate::error;

/// Dispatches `legion cmd-check`. `--hook` reads one PreToolUse payload on
/// stdin and writes one hook response on stdout; any other invocation is
/// refused as not yet implemented rather than silently doing nothing.
pub(crate) fn handle_cmd_check(hook: bool) -> error::Result<()> {
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
