//! `legion cmd-check`: the CLI surface over the legion-cmd adapter (#1229).
//!
//! Only `--hook` mode exists in this slice -- one PreToolUse payload on
//! stdin, one hook response on stdout, always exit 0 (`crate::cmd::hook`
//! owns the actual logic; this is the thin clap-to-adapter wire). The
//! operator and scripting modes (`legion cmd-check` with no `--hook`, for
//! testing a command by hand) ship in the cmd-check issue.

use crate::cmd::hook::run_hook;
use crate::error;

/// Dispatches `legion cmd-check`. `hook` selects the only mode this issue
/// implements; any other invocation is `LegionError::NotImplemented` rather
/// than silently doing nothing.
pub(crate) fn handle_cmd_check(hook: bool) -> error::Result<()> {
    if !hook {
        return Err(error::LegionError::NotImplemented {
            feature: "legion cmd-check without --hook (operator/scripting mode ships separately)"
                .to_string(),
        });
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    // `run_hook` always returns `ExitCode::SUCCESS` -- it turns every
    // failure it can observe into a deny JSON body rather than a non-zero
    // exit (FR-CMD-009), so there is nothing here to propagate as an
    // `Err`.
    let _ = run_hook(stdin.lock(), stdout.lock());
    Ok(())
}
