use crate::error::Result;

/// Run the init command.
///
/// Hooks ship with the legion Claude Code plugin and are registered by Claude Code
/// directly, so init no longer writes hook scripts or edits settings.json -- doing so
/// would duplicate and conflict with the plugin-provided hook suite. init now only
/// points the operator/agent at the next onboarding step: registering a repo with
/// `legion watch add`.
///
/// `force` is retained for backward compatibility. There is no longer a destructive
/// write to confirm, so it is accepted and ignored.
pub fn init(_force: bool) -> Result<()> {
    eprintln!("[legion] Hooks are provided by the legion plugin -- init writes nothing.");
    eprintln!("[legion] To watch a repo on this node, register it with:");
    eprintln!("[legion]   legion watch add <path> --name <repo> --agent <agent>");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_writes_nothing_and_succeeds() {
        // init no longer touches the filesystem; it only prints guidance to stderr.
        assert!(init(false).is_ok());
        assert!(init(true).is_ok());
    }
}
