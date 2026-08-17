//! Delivery filtering and session-identity resolution for the MCP server.
//!
//! Carved from mcp.rs (#612) when this module also owned the channel push
//! (a polling thread that wrote `notifications/claude/channel` frames to
//! stdout). That push is retired (#947); the hook-drain lane is the sole
//! live-session delivery path. What remains is the part both lanes always
//! shared -- the recipient filter -- plus the cwd-to-agent-name lookup the
//! MCP server uses to attribute tool calls.

use crate::signal as sig;

/// Determine whether a notification for a post should be delivered to this client.
///
/// Rules (applied in order):
/// 1. If the text starts with `@all`, deliver unconditionally (broadcast signal).
/// 2. If the text starts with `@<client_repo>` (direct mention), deliver.
/// 3. If the text starts with `@` but NOT addressed to this client, suppress.
/// 4. If `client_repo` is known and the post's `repo` equals `client_repo`, suppress
///    (the client wrote it; no need to echo a general musing back to its author).
/// 5. Otherwise (general musing, no `@` prefix, from a different agent), deliver.
///
/// Recipient parsing is `signal::recipient_token` -- the single addressing
/// rule (#612): first-whitespace token after the leading `@`, trailing `:`
/// trimmed. An empty recipient (`@` alone) or a recipient that itself begins
/// with `@` (e.g. `@@all`, which looks like a broadcast but isn't) is NOT
/// treated as `@all` or any named target -- the post falls through the
/// signal branch and is suppressed. This is deliberately strict: if an agent
/// fat-fingers a broadcast as `@@all`, it should silently fail rather than
/// silently succeed with the wrong-looking prefix.
///
/// Lives here rather than in `deliver.rs` because it predates the hook lane
/// and `src/deliver.rs` imports it by this path (#941); after the channel
/// push was retired (#947) the hook drain is its only caller.
pub fn should_notify(text: &str, repo: &str, client_repo: Option<&str>) -> bool {
    if sig::is_signal(text) {
        // Reject malformed prefixes (`@` alone, `@@all`) -- suppressed
        // rather than passed to the @all / named-target branches.
        let Some(recipient) = sig::recipient_token(text) else {
            return false;
        };

        if recipient == "all" {
            return true;
        }
        if let Some(cr) = client_repo {
            return recipient == cr;
        }
        // No client_repo known -- suppress signals (can't verify recipient).
        return false;
    }

    // General musing: suppress own posts, deliver everything else.
    if let Some(cr) = client_repo
        && repo == cr
    {
        return false;
    }

    true
}

/// Resolve the agent name for the current MCP subprocess from `watch.toml`
/// keyed on cwd.
///
/// The MCP `initialize` handshake reports `clientInfo.name = "claude-code"`
/// for every Claude Code session, which is the *client software* identity,
/// not the *agent* identity. Attributing a tool call by that token collapses
/// every session onto the same name. The agent identity is what
/// `legion --repo <name>` carries on every CLI call; here we recover it by
/// canonicalising cwd and looking up the matching
/// `WatchRepoConfig.recipient()`.
///
/// Returns `None` (and the caller falls back to the legacy `clientInfo.name`
/// handshake value) when:
///   - watch.toml is missing or empty
///   - cwd cannot be canonicalised
///   - no entry's canonicalised workdir matches the current cwd
///
/// All three failure modes are non-fatal: a misconfigured workstation gets
/// the client-software name rather than no identity at all.
pub(super) fn resolve_session_repo_from_cwd(data_dir: &std::path::Path) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    resolve_session_repo_for_cwd(data_dir, &cwd)
}

/// Inner form of [`resolve_session_repo_from_cwd`] with the cwd injected.
/// Split out so unit tests can exercise the watch.toml lookup against a
/// fixture directory without mutating the global process cwd.
fn resolve_session_repo_for_cwd(
    data_dir: &std::path::Path,
    cwd: &std::path::Path,
) -> Option<String> {
    let watch_path = data_dir.join("watch.toml");
    let repos = match crate::watch::list_repos_in_config(&watch_path) {
        Ok(r) if !r.is_empty() => r,
        _ => return None,
    };

    let cwd_canon = std::fs::canonicalize(cwd).ok()?;

    for repo in repos {
        let workdir = std::path::Path::new(&repo.workdir);
        if let Ok(workdir_canon) = std::fs::canonicalize(workdir)
            && workdir_canon == cwd_canon
        {
            return Some(repo.recipient().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_session_repo_returns_none_when_watch_toml_missing() {
        let data_dir = tempfile::tempdir().expect("data dir");
        let cwd = tempfile::tempdir().expect("cwd dir");
        assert_eq!(
            resolve_session_repo_for_cwd(data_dir.path(), cwd.path()),
            None
        );
    }

    #[test]
    fn resolve_session_repo_matches_canonicalized_workdir() {
        let data_dir = tempfile::tempdir().expect("data dir");
        let cwd = tempfile::tempdir().expect("cwd dir");
        let watch_path = data_dir.path().join("watch.toml");

        crate::watch::add_repo_to_config(&watch_path, "kessel", cwd.path(), None)
            .expect("add repo");

        assert_eq!(
            resolve_session_repo_for_cwd(data_dir.path(), cwd.path()).as_deref(),
            Some("kessel")
        );
    }

    #[test]
    fn resolve_session_repo_prefers_agent_alias_over_name() {
        let data_dir = tempfile::tempdir().expect("data dir");
        let cwd = tempfile::tempdir().expect("cwd dir");
        let watch_path = data_dir.path().join("watch.toml");

        crate::watch::add_repo_to_config(&watch_path, "kessel", cwd.path(), Some("kessel-agent"))
            .expect("add repo");

        assert_eq!(
            resolve_session_repo_for_cwd(data_dir.path(), cwd.path()).as_deref(),
            Some("kessel-agent")
        );
    }

    #[test]
    fn resolve_session_repo_returns_none_for_unmatched_cwd() {
        let data_dir = tempfile::tempdir().expect("data dir");
        let cwd = tempfile::tempdir().expect("cwd dir");
        let other = tempfile::tempdir().expect("other dir");
        let watch_path = data_dir.path().join("watch.toml");

        crate::watch::add_repo_to_config(&watch_path, "kessel", other.path(), None)
            .expect("add repo");

        assert_eq!(
            resolve_session_repo_for_cwd(data_dir.path(), cwd.path()),
            None
        );
    }

    #[test]
    fn notification_filter_passes_at_all() {
        // @all signals should reach every client regardless of repo.
        assert!(
            should_notify("@all hello team", "smugglr", Some("kelex")),
            "@all must pass filter for kelex"
        );
        assert!(
            should_notify("@all hello team", "smugglr", Some("smugglr")),
            "@all must pass even for the poster's own client if the post repo differs"
        );
    }

    #[test]
    fn notification_filter_suppresses_wrong_recipient() {
        // A signal to @vault must not reach @kelex.
        assert!(
            !should_notify("@vault review:approved", "smugglr", Some("kelex")),
            "@vault signal must be suppressed for kelex client"
        );
        // A signal to @kelex MUST reach kelex.
        assert!(
            should_notify("@kelex review:approved", "smugglr", Some("kelex")),
            "@kelex signal must reach kelex client"
        );
        // Own post must be suppressed.
        assert!(
            !should_notify("hello team", "kelex", Some("kelex")),
            "own posts must be suppressed"
        );
        // General musing from another agent must reach the client.
        assert!(
            should_notify("just thinking about things", "smugglr", Some("kelex")),
            "general musings from others must reach kelex"
        );
    }

    #[test]
    fn notification_filter_rejects_malformed_signal_prefixes() {
        // `@` alone is not a broadcast -- no recipient token at all.
        assert!(
            !should_notify("@ hello", "smugglr", Some("kelex")),
            "lone @ must be suppressed"
        );
        // `@@all foo` looks like a broadcast but recipient parses as `@all`,
        // which starts with `@` -- rejected as malformed rather than silently
        // routed as if the user meant @all.
        assert!(
            !should_notify("@@all urgent", "smugglr", Some("kelex")),
            "@@all must be suppressed, not routed as @all"
        );
        // `@@` alone with no recipient.
        assert!(
            !should_notify("@@", "smugglr", Some("kelex")),
            "@@ alone must be suppressed"
        );
        // Trailing colon is stripped, so `@kelex:` still reaches kelex.
        assert!(
            should_notify("@kelex: review:approved", "smugglr", Some("kelex")),
            "trailing colon on recipient must still reach the target"
        );
    }
}
