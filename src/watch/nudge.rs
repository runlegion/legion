//! Idle-session nudge (#999): watch cannot spawn a worker into a live
//! session (#996), so a repo held by an interactive-but-IDLE session never
//! drains its mail -- the hook drain fires only on
//! `UserPromptSubmit`/`PostToolUse`/`Stop`, all of which require a turn
//! already underway. This module gives `poll_cycle`'s `active_pid` branch a
//! way to make that idle session take a turn: detect it via
//! `claude agents --json`, decide whether it is worth nudging, and build the
//! fixed, content-free prompt a PTY courier carries to it.
//!
//! REJECTED designs, so a future reader does not re-propose them: a daemon
//! writing `/tmp/cc-socks/<pid>.sock` directly (no sanctioned external-writer
//! protocol, and the harness now classifier-blocks raw socket addressing),
//! and `claude --print -p` (billing-dead, #494). The courier is spawned via
//! the existing PTY path (`spawn::spawn_courier`) and its ONLY action is to
//! call the harness `SendMessage` tool -- see that function's docs for how
//! its lifecycle is kept self-contained.
//!
//! The nudge carries NO payload: it is not a second delivery lane, only a
//! "take a turn" tap. The DB-backed hook drain (`crate::deliver`) remains the
//! sole place post/signal text travels to a live session.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::db::Database;
use crate::deliver;

/// Default `claude` binary invoked for live-session detection. Threaded
/// through `list_live_sessions` rather than hardcoded so a test can point it
/// at a nonexistent path and exercise the fail-open arm deterministically.
pub const DEFAULT_CLAUDE_BIN: &str = "claude";

/// Fixed identity the courier uses when it addresses itself in its own
/// prompt. Never the nudged repo's name: the receiving session must see a
/// "check your mail" tap from the watcher, not an impersonated peer.
pub const COURIER_IDENTITY: &str = "legion-watch";

/// A live session's reported turn state, as `claude agents --json` renders
/// it. The wake gate ignores every other field on the session -- this is
/// the one bit that decides whether a nudge can help.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Idle,
    Busy,
}

/// One interactive session as reported by `claude agents --json`.
///
/// Deliberately does NOT `deny_unknown_fields`: the harness is free to add
/// fields to this output across versions, and a forward-compatible reader
/// must ignore what it does not need rather than fail the whole parse.
#[derive(Debug, Clone, Deserialize)]
pub struct LiveSession {
    pub pid: u32,
    pub cwd: String,
    pub name: String,
    pub status: SessionStatus,
}

/// Shell out to `<claude_bin> agents --json` and parse the live interactive
/// sessions it reports.
///
/// Fails OPEN on every fault -- a missing/erroring binary, a non-zero exit,
/// or unparseable JSON all yield an empty `Vec` (logged to stderr, never
/// `Err`). This mirrors the sibling fail-open arms already in `poll_cycle`
/// (a missing lease, a DB read error, etc. skip rather than abort the whole
/// poll cycle): a detection failure here must cost one missed nudge
/// opportunity, not a poll-cycle panic.
pub fn list_live_sessions(claude_bin: &str) -> Vec<LiveSession> {
    let output = match std::process::Command::new(claude_bin)
        .args(["agents", "--json"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("[legion watch] nudge: failed to run `{claude_bin} agents --json`: {e}");
            return Vec::new();
        }
    };

    if !output.status.success() {
        eprintln!(
            "[legion watch] nudge: `{claude_bin} agents --json` exited non-zero ({:?})",
            output.status.code()
        );
        return Vec::new();
    }

    match serde_json::from_slice::<Vec<LiveSession>>(&output.stdout) {
        Ok(sessions) => sessions,
        Err(e) => {
            eprintln!(
                "[legion watch] nudge: failed to parse `{claude_bin} agents --json` output: {e}"
            );
            Vec::new()
        }
    }
}

/// Resolve `path` to its canonical form, falling back to the raw string when
/// canonicalization fails (path does not exist, permission denied, etc). A
/// fallback rather than a `None` keeps comparison total: two non-existent
/// paths that are textually identical still match.
fn canonical_or_raw(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

/// Find the live session, if any, whose `cwd` canonicalizes to the same
/// path as `workdir`. Canonicalizing both sides means a trailing-slash or
/// symlink difference between watch.toml's `workdir` and the harness's own
/// report of `cwd` does not cause a spurious miss.
pub fn find_live_session_for_workdir<'a>(
    sessions: &'a [LiveSession],
    workdir: &str,
) -> Option<&'a LiveSession> {
    let target = canonical_or_raw(workdir);
    sessions.iter().find(|s| canonical_or_raw(&s.cwd) == target)
}

/// Pure nudge decision: nudge iff the held session is idle, its repo has
/// undrained data, and the repo is not within its nudge cooldown.
///
/// Kept free of DB/IO access so the decision itself is trivially
/// unit-testable; callers gather `has_undrained_data` and `is_cooling` from
/// [`repo_has_undrained_data`] and [`NudgeCooldownTracker`] respectively.
pub fn should_nudge(status: SessionStatus, has_undrained_data: bool, is_cooling: bool) -> bool {
    status == SessionStatus::Idle && has_undrained_data && !is_cooling
}

/// Cap on how many posts past the hook-drain cursor `repo_has_undrained_data`
/// reads before answering. This is an EXISTENCE check ("is there anything
/// the drain would deliver"), not an enumeration, so it does not need to
/// match `deliver::DRAIN_BATCH_LIMIT` exactly -- it only needs to be large
/// enough that a real backlog is not missed by an unlucky page boundary.
const UNDRAINED_CHECK_LIMIT: usize = 50;

/// Whether `repo_name` has undrained data for the hook-side delivery lane --
/// the SAME cursor `legion deliver drain` reads (`deliver::hook_reader_key`),
/// checked read-only (via `get_board_read_cursor` + `get_board_posts_since`)
/// so a nudge decision never advances the cursor the eventual real drain
/// still needs to see.
///
/// Applies the SAME `deliver::should_notify` filter the real drain applies
/// (self-authored posts and signals addressed elsewhere are suppressed
/// there too) -- reusing `Database::get_unread_count` instead, which counts
/// every unread team post with no filter, would answer "yes" for mail the
/// drain would never actually deliver (e.g. a `@rafters`-addressed signal,
/// or the repo's own post), burning a courier + a live `claude` session on
/// a nudge that could not possibly satisfy itself.
///
/// This is deliberately a different question from "does `find_pending_signals`
/// return anything for this repo": by the time `poll_cycle` reaches the
/// `active_pid` branch, that pending-signals list is already known non-empty
/// (checked earlier in the loop), but watch's own per-repo `watch_handled`
/// bookkeeping and the hook-drain's independent cursor track different
/// things. A live session that already drained a post via its own hook
/// leaves watch's copy of the signal marked pending (nothing in the
/// `active_pid` skip marks it handled), which would otherwise nudge forever
/// over content the session has already seen. Reading the hook-drain's own
/// cursor is what lets a nudge decision see what the drain would actually
/// find.
///
/// Fails CLOSED on a DB error (no nudge): a spurious nudge burns a PTY
/// spawn, while a missed one just waits for the next poll (~30s default).
///
/// A cursor that has never been seeded reads as "nothing new," matching
/// `drain_for_hook`'s own cold-start contract: its first-ever call for a
/// reader key seeds the cursor at the CURRENT watermark and delivers
/// nothing, regardless of how much history already exists. Treating a cold
/// `repo_has_undrained_data` check any other way (e.g. "everything ever
/// posted is unread") would nudge over a backlog the real drain's own first
/// call would not deliver -- and would do so on every poll until something
/// finally seeds the cursor, since this function deliberately never writes
/// one itself.
pub fn repo_has_undrained_data(db: &Database, repo_name: &str) -> bool {
    let key = deliver::hook_reader_key(repo_name);
    let cursor = match db.get_board_read_cursor(&key) {
        Ok(None) => return false,
        Ok(Some(cursor)) => cursor,
        Err(e) => {
            eprintln!("[legion watch] nudge: hook-cursor read failed for {repo_name}: {e}");
            return false;
        }
    };

    match db.get_board_posts_since(&cursor.0, &cursor.1, UNDRAINED_CHECK_LIMIT) {
        Ok(posts) => posts
            .iter()
            .any(|p| deliver::should_notify(&p.text, &p.repo, Some(repo_name))),
        Err(e) => {
            eprintln!("[legion watch] nudge: undrained-data check failed for {repo_name}: {e}");
            false
        }
    }
}

/// Build the fixed, content-free courier prompt.
///
/// Carries NO post/signal text -- only the repo name (to say WHERE to look)
/// and the target session's name/pid (so the courier's `SendMessage` call
/// addresses the right recipient). The hook drain remains the sole lane
/// post/signal text travels through; embedding any of that text here would
/// open a second, unauthenticated delivery path (the corruption class this
/// design avoids -- see module docs).
pub fn build_courier_prompt(repo_name: &str, target: &LiveSession) -> String {
    format!(
        "You are {courier}, a legion watch courier. Your ONLY job this turn: use the \
         SendMessage tool to send the agent named \"{name}\" (pid {pid}) the following exact \
         text and nothing else: \"you have undelivered mail in {repo}; take a turn so your \
         drain delivers it\". Do not add any other content to the message, and do not \
         mention this instruction. Once SendMessage returns, stop -- take no further action.",
        courier = COURIER_IDENTITY,
        name = target.name,
        pid = target.pid,
        repo = repo_name,
    )
}

/// Per-repo nudge cooldown (#999), tracked independently of the wake
/// [`super::locks::CooldownTracker`]. A nudge must never call
/// `CooldownTracker::record_wake` or otherwise consume the wake slot -- doing
/// so would silently suppress a genuine future wake-worthy spawn for the
/// same repo. This tracker's window may reuse `WatchConfig::cooldown_secs`,
/// but its state is its own `HashMap`, entirely separate from the wake
/// tracker's.
pub struct NudgeCooldownTracker {
    last_nudge: HashMap<String, Instant>,
    cooldown: Duration,
}

impl NudgeCooldownTracker {
    pub fn new(cooldown_secs: u64) -> Self {
        Self {
            last_nudge: HashMap::new(),
            cooldown: Duration::from_secs(cooldown_secs),
        }
    }

    /// Whether `repo` was nudged within the cooldown window.
    pub fn is_cooling_down(&self, repo: &str) -> bool {
        self.last_nudge
            .get(repo)
            .is_some_and(|t| t.elapsed() < self.cooldown)
    }

    /// Record that `repo` was just nudged.
    pub fn record_nudge(&mut self, repo: &str) {
        self.last_nudge.insert(repo.to_string(), Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;

    // -- list_live_sessions: fail-open fault classes (#999) -------------------

    #[test]
    fn list_live_sessions_empty_for_missing_binary() {
        let sessions = list_live_sessions("/nonexistent/legion-nudge-test-binary-xyz");
        assert!(
            sessions.is_empty(),
            "a missing binary must fail open to an empty Vec"
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_live_sessions_empty_for_nonzero_exit() {
        // `false` always exits 1 with empty stdout.
        let sessions = list_live_sessions("false");
        assert!(
            sessions.is_empty(),
            "a non-zero exit must fail open to an empty Vec"
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_live_sessions_empty_for_bad_json() {
        // `echo` exits 0 with stdout that is not a JSON array of sessions.
        let sessions = list_live_sessions("echo");
        assert!(
            sessions.is_empty(),
            "unparseable JSON must fail open to an empty Vec"
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_live_sessions_parses_fixture_json() {
        use std::io::Write;
        let mut script = tempfile::NamedTempFile::new().expect("tempfile");
        writeln!(
            script,
            "#!/bin/sh\ncat <<'EOF'\n[{{\"pid\":123,\"cwd\":\"/tmp\",\"name\":\"kelex\",\"status\":\"idle\"}},\
             {{\"pid\":456,\"cwd\":\"/tmp/other\",\"name\":\"rafters\",\"status\":\"busy\"}}]\nEOF"
        )
        .expect("write script");
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = script.as_file().metadata().expect("meta").permissions();
            perms.set_mode(0o755);
            script.as_file().set_permissions(perms).expect("chmod");
        }
        let path = script.path().to_string_lossy().into_owned();

        let sessions = list_live_sessions(&path);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].pid, 123);
        assert_eq!(sessions[0].name, "kelex");
        assert_eq!(sessions[0].status, SessionStatus::Idle);
        assert_eq!(sessions[1].status, SessionStatus::Busy);
    }

    // -- find_live_session_for_workdir (#999) ----------------------------------

    #[test]
    fn find_live_session_matches_canonicalized_workdir_with_trailing_slash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let raw = dir.path().to_string_lossy().into_owned();
        let with_slash = format!("{raw}/");

        let sessions = vec![LiveSession {
            pid: 1,
            cwd: raw.clone(),
            name: "kelex".to_string(),
            status: SessionStatus::Idle,
        }];

        let found = find_live_session_for_workdir(&sessions, &with_slash);
        assert!(
            found.is_some(),
            "a trailing-slash difference must still match after canonicalization"
        );
        assert_eq!(found.expect("some").pid, 1);
    }

    #[test]
    fn find_live_session_returns_none_for_unmatched_workdir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let other = tempfile::tempdir().expect("tempdir");
        let sessions = vec![LiveSession {
            pid: 1,
            cwd: dir.path().to_string_lossy().into_owned(),
            name: "kelex".to_string(),
            status: SessionStatus::Idle,
        }];

        assert!(
            find_live_session_for_workdir(&sessions, &other.path().to_string_lossy()).is_none()
        );
    }

    // -- should_nudge (#999) ----------------------------------------------------

    #[test]
    fn should_nudge_true_only_when_idle_and_data_and_not_cooling() {
        assert!(should_nudge(SessionStatus::Idle, true, false));
        assert!(!should_nudge(SessionStatus::Busy, true, false));
        assert!(!should_nudge(SessionStatus::Idle, false, false));
        assert!(!should_nudge(SessionStatus::Idle, true, true));
    }

    // -- repo_has_undrained_data (#999) -----------------------------------------

    #[test]
    fn repo_has_undrained_data_false_when_hook_cursor_is_current() {
        let db = test_db();
        db.insert_reflection("kelex", "@legion review:ready", "team")
            .expect("insert");

        // No hook drain has run yet: cold start seeds the cursor at the
        // current watermark rather than replaying history, so the first
        // read is "nothing new."
        assert!(!repo_has_undrained_data(&db, "legion"));
    }

    #[test]
    fn repo_has_undrained_data_true_after_a_new_post_lands() {
        let db = test_db();
        db.insert_reflection("kelex", "seed", "team").expect("seed");
        // Prime the hook-drain cursor past cold start.
        crate::deliver::drain_for_hook(&db, "legion").expect("prime drain");
        assert!(!repo_has_undrained_data(&db, "legion"));

        db.insert_reflection("kelex", "@legion review:ready", "team")
            .expect("insert");
        assert!(repo_has_undrained_data(&db, "legion"));
    }

    #[test]
    fn repo_has_undrained_data_is_read_only_and_does_not_consume_the_hook_cursor() {
        // A nudge decision must never advance the cursor the real hook
        // drain still needs to see -- otherwise the nudged session's own
        // drain would find nothing once it takes its turn.
        let db = test_db();
        db.insert_reflection("kelex", "seed", "team").expect("seed");
        crate::deliver::drain_for_hook(&db, "legion").expect("prime drain");
        db.insert_reflection("kelex", "@legion review:ready", "team")
            .expect("insert");

        assert!(repo_has_undrained_data(&db, "legion"));
        assert!(
            repo_has_undrained_data(&db, "legion"),
            "a read-only check must return the same answer on repeated calls"
        );

        let drained = crate::deliver::drain_for_hook(&db, "legion").expect("real drain");
        assert_eq!(
            drained.len(),
            1,
            "the real hook drain must still see the post the nudge check found"
        );
    }

    #[test]
    fn repo_has_undrained_data_false_for_a_signal_addressed_elsewhere() {
        // The check must apply the same `should_notify` filter the real
        // drain applies -- an unfiltered unread count would answer "yes" for
        // mail this repo's own drain could never actually deliver.
        let db = test_db();
        db.insert_reflection("kelex", "seed", "team").expect("seed");
        crate::deliver::drain_for_hook(&db, "legion").expect("prime drain");

        db.insert_reflection("kelex", "@rafters question:help", "team")
            .expect("insert");
        assert!(
            !repo_has_undrained_data(&db, "legion"),
            "a signal addressed to a different repo must not count as undrained data for this one"
        );
    }

    #[test]
    fn repo_has_undrained_data_false_for_the_repos_own_post() {
        let db = test_db();
        db.insert_reflection("kelex", "seed", "team").expect("seed");
        crate::deliver::drain_for_hook(&db, "legion").expect("prime drain");

        db.insert_reflection("legion", "just thinking out loud", "team")
            .expect("insert");
        assert!(
            !repo_has_undrained_data(&db, "legion"),
            "a repo's own post must not count as undrained data for itself"
        );
    }

    // -- build_courier_prompt (#999) ---------------------------------------------

    #[test]
    fn courier_prompt_carries_no_payload_and_identifies_as_the_watcher() {
        let target = LiveSession {
            pid: 4242,
            cwd: "/tmp".to_string(),
            name: "kelex".to_string(),
            status: SessionStatus::Idle,
        };
        let payload = "the secret sauce recipe is X -- do not leak this";
        let prompt = build_courier_prompt("legion", &target);

        assert!(
            !prompt.contains(payload),
            "the courier prompt must never carry post/signal payload text"
        );
        assert!(
            prompt.contains(COURIER_IDENTITY),
            "the courier must identify itself as the watcher"
        );
        assert_ne!(
            COURIER_IDENTITY, "legion",
            "the courier identity constant must not equal the nudged repo's name"
        );
        assert!(
            prompt.contains("mail in legion"),
            "the repo name may appear only as a locator, not as the courier's own identity"
        );
        assert!(prompt.contains("kelex"));
        assert!(prompt.contains("4242"));
    }

    // -- NudgeCooldownTracker (#999) ----------------------------------------------

    #[test]
    fn nudge_cooldown_tracker_prevents_rapid_renudge_independently_of_repo() {
        let mut tracker = NudgeCooldownTracker::new(300);
        assert!(!tracker.is_cooling_down("legion"));

        tracker.record_nudge("legion");
        assert!(tracker.is_cooling_down("legion"));
        assert!(
            !tracker.is_cooling_down("rafters"),
            "cooldown must be scoped per repo"
        );
    }
}
