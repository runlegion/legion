//! The channel notifier: recipient filtering for live delivery and
//! cold-boot replay, the notification XML payload, cursor seeding, and
//! the polling notifier thread with its heartbeat / health diagnostics.
//! Carved from mcp.rs (#612).

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::json;

use crate::db::Database;
use crate::error::Result;
use crate::signal as sig;

use super::log::{mcp_trace, mcp_verbose};

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

/// Replay-mode delivery filter (#400). Stricter than `should_notify`:
/// drops broadcasts and cross-repo musings, delivers only signals
/// directed at this recipient.
///
/// Used when a post predates the MCP subprocess boot timestamp -- the
/// agent was offline when it landed, and the only thing the channel
/// should backfill on cold boot is a directed signal someone meant for
/// them. Stale `@all` broadcasts and team musings are not worth the
/// flood when 24h of bullpen activity is replayed.
///
/// Shares `should_notify`'s recipient parsing -- `signal::is_addressed_to`,
/// the single addressing rule (#612) -- so `@kessel:`, `@kessel `, and
/// `@kessel\n` all match (the `:` trim and whitespace split). When
/// `client_repo` is unknown the function returns false -- without an
/// identity we cannot safely deliver any directed signal anyway.
pub fn replay_should_deliver(text: &str, client_repo: Option<&str>) -> bool {
    let Some(recipient) = client_repo else {
        return false;
    };
    sig::is_addressed_to(text, recipient)
}

/// Split a CDATA body around any literal `]]>` occurrences so the terminator
/// cannot escape the section. The standard XML trick is to replace every
/// `]]>` with `]]]]><![CDATA[>` -- close the current section after the first
/// `]]`, then reopen with `<![CDATA[` before the stray `>`. An agent post
/// containing the literal substring `]]>` (plausible in code snippets) would
/// otherwise terminate the block early and inject raw content into the XML.
fn escape_cdata(text: &str) -> String {
    text.replace("]]>", "]]]]><![CDATA[>")
}

/// Escape `"`, `<`, `>`, and `&` for use inside an XML attribute value.
/// The attribute values are short (post_id, repo, is_signal) and controlled,
/// but post_id comes from the DB and repo from the user; better to escape than
/// trust.
fn escape_xml_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Build the XML-like channel notification content string. Text goes inside a
/// CDATA block with `]]>` sequences neutralised; attribute values are
/// XML-escaped.
fn build_channel_content(post_id: &str, repo: &str, text: &str, is_signal: bool) -> String {
    let post_id_attr = escape_xml_attr(post_id);
    let repo_attr = escape_xml_attr(repo);
    let text_body = escape_cdata(text);
    format!(
        "<channel type=\"feed\" post_id=\"{post_id_attr}\" repo=\"{repo_attr}\" is_signal=\"{is_signal}\"><text><![CDATA[{text_body}]]></text></channel>"
    )
}

/// Default poll interval for the MCP notifier thread. Overridable via
/// `LEGION_MCP_POLL_MS` for integration tests that want a tighter loop.
const DEFAULT_MCP_POLL_MS: u64 = 500;

/// Maximum rows the notifier reads per poll tick. Bounds memory and stdout
/// mutex hold time if a burst of writes lands between ticks. Anything beyond
/// the cap is picked up on the next poll because the cursor advances to the
/// last delivered row.
const NOTIFIER_BATCH_LIMIT: usize = 100;

/// Read the notifier poll interval from the environment, falling back to the
/// default. Invalid values (non-numeric, zero) fall back silently -- the
/// failure mode is "notifier ticks at the default rate", not crash.
fn mcp_poll_interval() -> std::time::Duration {
    let ms = std::env::var("LEGION_MCP_POLL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MCP_POLL_MS);
    std::time::Duration::from_millis(ms)
}

/// Replay window applied at cold boot when this recipient has no
/// `board_reads` cursor yet -- a fresh agent picks up directed signals filed
/// in the last 24 hours instead of starting from the live watermark and
/// silently swallowing them. Bounded so the first boot after a long absence
/// is not a flood.
const NOTIFIER_COLD_BOOT_REPLAY: chrono::Duration = chrono::Duration::hours(24);

/// Resolve the agent name for the current MCP subprocess from `watch.toml`
/// keyed on cwd.
///
/// The MCP `initialize` handshake reports `clientInfo.name = "claude-code"`
/// for every Claude Code session, which is the *client software* identity,
/// not the *agent* identity. Routing channel notifications by that token
/// breaks every directed signal because every session collides on the same
/// name. The agent identity is what `legion --repo <name>` carries on every
/// CLI call; here we recover it by canonicalising cwd and looking up the
/// matching `WatchRepoConfig.recipient()`.
///
/// Returns `None` (and the caller falls back to the legacy `clientInfo.name`
/// handshake value) when:
///   - watch.toml is missing or empty
///   - cwd cannot be canonicalised
///   - no entry's canonicalised workdir matches the current cwd
///
/// All three failure modes are non-fatal: a misconfigured workstation gets
/// the pre-fix behaviour (broadcasts only) rather than no channel at all.
fn resolve_session_repo_from_cwd(data_dir: &std::path::Path) -> Option<String> {
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

/// Compute the initial `(last_seen_at, last_seen_id)` cursor for the
/// notifier thread.
///
/// Three-way resolution (#400):
///
///   1. **Known recipient with a `board_reads` cursor** -- seed from
///      that timestamp. The cursor advances on every successful delivery
///      so subsequent boots see only what arrived since the last delivery.
///   2. **Known recipient, no cursor yet** -- seed at
///      `now - NOTIFIER_COLD_BOOT_REPLAY` so a fresh agent picks up the
///      recent past instead of starting at the live watermark and
///      silently swallowing offline-window posts. `should_notify` and
///      `resolved_at IS NULL` keep replay narrow.
///   3. **Unknown recipient** (no watch.toml entry for cwd) -- fall back
///      to the pre-#400 watermark. The notifier cannot route directed
///      signals in that state anyway, so behaviour matches the prior
///      version: live posts only, no replay.
///
/// In case 1 the id comes from `board_reads.last_read_id`, written
/// alongside the timestamp on every successful delivery, so the
/// strict-`>` comparator in `get_board_posts_since` excludes the
/// already-delivered row even when its `created_at` collides with a
/// neighbour. In case 2 the id is empty (no prior delivery exists), and
/// in case 3 the id comes from the watermark row.
fn seed_notifier_cursor(db: &Database, client_repo: Option<&str>) -> Result<(String, String)> {
    if let Some(recipient) = client_repo {
        match db.get_board_read_cursor(recipient)? {
            Some((ts, id)) => {
                mcp_trace(
                    "notifier.cursor.seed",
                    &[
                        ("at", &ts),
                        ("id", &id),
                        ("source", "board_reads"),
                        ("recipient", recipient),
                    ],
                );
                Ok((ts, id))
            }
            None => {
                let backstop = (chrono::Utc::now() - NOTIFIER_COLD_BOOT_REPLAY).to_rfc3339();
                mcp_trace(
                    "notifier.cursor.seed",
                    &[
                        ("at", &backstop),
                        ("source", "cold_boot_replay"),
                        ("recipient", recipient),
                    ],
                );
                Ok((backstop, String::new()))
            }
        }
    } else {
        match db.get_board_cursor_watermark()? {
            Some((ts, id)) => {
                mcp_trace(
                    "notifier.cursor.seed",
                    &[("at", &ts), ("id", &id), ("source", "watermark")],
                );
                Ok((ts, id))
            }
            None => {
                let now = chrono::Utc::now().to_rfc3339();
                mcp_trace(
                    "notifier.cursor.seed",
                    &[("at", &now), ("source", "now_empty_table")],
                );
                Ok((now, String::new()))
            }
        }
    }
}

/// Body of the notifier thread spawned by `run_stdio_loop`.
///
/// Polls the `reflections` table for new bullpen rows and writes a
/// `notifications/claude/channel` JSON-RPC frame to the shared stdout writer
/// for each row that passes the recipient filter. Shared state (stdout,
/// client_repo cell) is passed in by the caller so the thread can be tested
/// independently from stdio wiring.
///
/// Exits cleanly on a stdout write failure (client hung up, EPIPE) or on a
/// poisoned stdout mutex. Transient database errors are logged and the loop
/// continues -- the notifier is a best-effort push channel, not a strict
/// delivery guarantee. The `last_seen_at` cursor advances only when a poll
/// succeeds, so a transient failure does not lose events on recovery.
/// Heartbeat sentinel for the notifier thread (#391). The notifier loop
/// updates `last_poll_unix_secs` at the top of every iteration, regardless
/// of whether new posts were emitted -- "still polling" is what the
/// diagnostic needs, not "still emitting." `0` means "no tick yet"
/// (initial state before the loop runs).
///
/// Why atomic, not Mutex<Instant>: this value is written from one thread
/// (the notifier) and read from another (the dispatch handler for
/// `legion/notifier_health`). An `AtomicI64` is lock-free and avoids any
/// chance of the dispatch path blocking on a poisoned mutex held by a
/// dying notifier -- which is exactly the failure mode this issue fixes.
#[derive(Debug, Default)]
pub struct NotifierHeartbeat {
    last_poll_unix_secs: AtomicI64,
}

impl NotifierHeartbeat {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn touch(&self) {
        let now = chrono::Utc::now().timestamp();
        self.last_poll_unix_secs.store(now, Ordering::Relaxed);
    }

    fn last_poll(&self) -> i64 {
        self.last_poll_unix_secs.load(Ordering::Relaxed)
    }
}

/// Notifier liveness check result. Surfaced verbatim through the
/// `legion/notifier_health` JSON-RPC method. The `state` field is the load-
/// bearing summary; the numeric fields let an operator (or smarter watchdog)
/// reason about how stale "stale" actually is.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NotifierHealth {
    /// Notifier has not yet ticked. Either the thread is still starting,
    /// or it died before the first poll (rare but possible -- DB open
    /// failure exits the thread before the loop is reached).
    Unknown,
    Alive {
        last_tick_secs_ago: i64,
    },
    Stale {
        last_tick_secs_ago: i64,
        threshold_secs: i64,
    },
}

/// Compute the current health by comparing the heartbeat against the
/// configured poll interval. Threshold is 3x the poll interval -- one
/// missed tick is fine under load, three in a row means the thread is
/// gone. Pure-function over `(now, last_poll, poll_interval)` so the
/// boundary behavior is unit-testable without a running notifier.
pub fn classify_notifier_health(
    now_unix_secs: i64,
    hb: &NotifierHeartbeat,
    poll_interval: std::time::Duration,
) -> NotifierHealth {
    let last = hb.last_poll();
    if last == 0 {
        return NotifierHealth::Unknown;
    }
    let last_tick_secs_ago = (now_unix_secs - last).max(0);
    // ceil(poll_interval * 3) + 1 so a 1500ms poll yields ceil(4.5)+1 = 6s.
    // The previous `as_secs_f64() as i64 * 3 + 1` truncated sub-second
    // intervals to 0 before multiplying, collapsing the threshold to 1s
    // and masking the real boundary. The +1 adds a whole-second slack so
    // a notifier landing exactly at 3x interval registers Alive.
    let threshold_secs = (poll_interval.as_secs_f64() * 3.0).ceil() as i64 + 1;
    if last_tick_secs_ago > threshold_secs {
        NotifierHealth::Stale {
            last_tick_secs_ago,
            threshold_secs,
        }
    } else {
        NotifierHealth::Alive { last_tick_secs_ago }
    }
}

fn run_notifier_loop(
    data_dir: PathBuf,
    out: Arc<Mutex<std::io::BufWriter<std::io::Stdout>>>,
    client_repo_cell: Arc<OnceLock<String>>,
    heartbeat: Arc<NotifierHeartbeat>,
) {
    let db = match Database::open(&data_dir.join("legion.db")) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[legion mcp notif] failed to open db: {e}; notifier thread exiting");
            return;
        }
    };

    let (mut last_seen_at, mut last_seen_id): (String, String) = match seed_notifier_cursor(
        &db,
        client_repo_cell.get().map(String::as_str),
    ) {
        Ok(seed) => seed,
        Err(e) => {
            mcp_trace("notifier.seed.failed", &[("err", &e.to_string())]);
            eprintln!(
                "[legion mcp notif] failed to seed cursor: {e}; notifier thread exiting (channel push is now inoperative for this session)"
            );
            return;
        }
    };

    // Boot timestamp partitions posts into replay (older than boot) vs
    // live (newer). Replay is narrowed to directed signals for this
    // recipient (#400): a fresh-boot agent should pick up signals they
    // missed while offline, but should NOT be flooded by 24h of cross-repo
    // musings or `@all` broadcasts the team has long since moved past.
    // Live posts run through the full `should_notify` filter unchanged,
    // so once boot is past the agent receives the same flow they always
    // did.
    let boot_at = chrono::Utc::now().to_rfc3339();

    mcp_trace(
        "notifier.start",
        &[(
            "poll_interval_ms",
            &format!("{}", mcp_poll_interval().as_millis()),
        )],
    );

    let poll_interval = mcp_poll_interval();
    let mut consecutive_cap_hits: u32 = 0;
    // #393: a transient stdout write/flush blip used to kill the notifier
    // thread permanently, leaving the MCP responsive to JSON-RPC but unable
    // to push any further notifications -- the symptom is "agents miss every
    // post except the one immediately after their fresh session start."
    // Track consecutive write failures and only exit after the configured
    // threshold so a brief EPIPE or back-pressure event does not silently
    // dark the channel forever.
    let mut consecutive_write_failures: u32 = 0;
    const MAX_CONSECUTIVE_WRITE_FAILURES: u32 = 5;

    loop {
        std::thread::sleep(poll_interval);

        // Heartbeat for #391: mark the tick BEFORE any work so a stuck
        // DB query or stdout write does not silently freeze the
        // diagnostic in a stale state.
        heartbeat.touch();

        let new_posts =
            match db.get_board_posts_since(&last_seen_at, &last_seen_id, NOTIFIER_BATCH_LIMIT) {
                Ok(posts) => posts,
                Err(e) => {
                    mcp_trace("notifier.poll.failed", &[("err", &e.to_string())]);
                    eprintln!("[legion mcp notif] db poll failed: {e}; continuing");
                    continue;
                }
            };

        if mcp_verbose() {
            mcp_trace(
                "notifier.poll",
                &[
                    ("cursor_at", &last_seen_at),
                    ("cursor_id", &last_seen_id),
                    ("returned", &new_posts.len().to_string()),
                ],
            );
        }

        if new_posts.is_empty() {
            consecutive_cap_hits = 0;
            continue;
        }

        // Surface a breadcrumb when the notifier hits the batch cap on
        // back-to-back ticks. Hitting the cap occasionally is expected
        // under normal activity bursts; hitting it repeatedly means the
        // notifier is falling behind (a misbehaving spammer, or a batch
        // import). This is the only diagnostic for "delivery is minutes
        // behind because we are saturated," which would otherwise be
        // indistinguishable from "the team is quiet."
        if new_posts.len() == NOTIFIER_BATCH_LIMIT {
            consecutive_cap_hits = consecutive_cap_hits.saturating_add(1);
            if consecutive_cap_hits >= 3 {
                eprintln!(
                    "[legion mcp notif] hit NOTIFIER_BATCH_LIMIT ({}) on {} consecutive polls; delivery may be lagging real time",
                    NOTIFIER_BATCH_LIMIT, consecutive_cap_hits
                );
            }
        } else {
            consecutive_cap_hits = 0;
        }

        // Advance the cursor to the newest row we saw. Rows are ordered
        // ascending by `(created_at, id)`, so the last element is the
        // newest. This must happen unconditionally regardless of whether
        // individual rows are delivered or suppressed, or a suppressed
        // post (own-post, wrong signal target) would be re-scanned
        // forever.
        if let Some(last) = new_posts.last() {
            last_seen_at = last.created_at.clone();
            last_seen_id = last.id.clone();
        }

        let client_repo = client_repo_cell.get().map(String::as_str);

        for post in new_posts {
            let is_signal = crate::signal::is_signal(&post.text);

            // Log the "named signal suppressed because client_repo is
            // unknown" case exactly once per post, so that a stuck
            // initialize (or a client that omitted clientInfo.name) is
            // visible in the breadcrumb log instead of manifesting as
            // silent delivery failures. Other suppression cases (own post,
            // signal to a different agent) are expected and not logged.
            if client_repo.is_none() && is_signal && !post.text.starts_with("@all") {
                eprintln!(
                    "[legion mcp notif] suppressing signal {} -- client_repo unknown (initialize handshake missing or clientInfo.name absent)",
                    post.id
                );
            }

            // Replay window narrows delivery to directed-to-this-recipient
            // signals only (#400). A post strictly older than `boot_at`
            // means we are catching up after the recipient was offline;
            // delivering generic musings or `@all` broadcasts from that
            // window would flood the session with stale content the team
            // has already metabolised. Live posts (created at or after
            // boot) use the unchanged `should_notify` rule so the steady
            // state matches what the channel always delivered.
            let is_replay = post.created_at < boot_at;
            let deliver = if is_replay {
                replay_should_deliver(&post.text, client_repo)
            } else {
                should_notify(&post.text, &post.repo, client_repo)
            };
            if mcp_verbose() {
                let preview: String = post.text.chars().take(40).collect();
                mcp_trace(
                    "notifier.decision",
                    &[
                        ("post_id", &post.id),
                        ("from_repo", &post.repo),
                        ("client_repo", client_repo.unwrap_or("<unset>")),
                        ("is_signal", &is_signal.to_string()),
                        ("is_replay", &is_replay.to_string()),
                        ("deliver", &deliver.to_string()),
                        ("text_prefix", &preview.replace('\n', " ")),
                    ],
                );
            }
            if !deliver {
                continue;
            }

            let content = build_channel_content(&post.id, &post.repo, &post.text, is_signal);
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "notifications/claude/channel",
                "params": {
                    "content": content
                }
            });

            let Ok(s) = serde_json::to_string(&notification) else {
                eprintln!("[legion mcp notif] failed to serialize notification");
                continue;
            };

            // Mutex poisoning here is catastrophic, not recoverable. The
            // same `Arc<Mutex<BufWriter<Stdout>>>` is shared with the
            // request loop running on the main thread; a poisoned mutex
            // means every subsequent `out.lock()` on EITHER side returns
            // Err, which would leave the MCP subprocess alive (still
            // accepting requests on stdin) but silently unable to write
            // any response or notification. That is strictly worse than
            // a dead subprocess: Claude Code can recover from a dead MCP
            // server by respawning, but it cannot detect a server that
            // accepts initialize, accepts tool calls, and quietly drops
            // every response. Abort the process so the client gets a
            // clean disconnect and can respawn.
            let Ok(mut locked) = out.lock() else {
                eprintln!(
                    "[legion mcp notif] stdout mutex poisoned; aborting process so claude code can respawn the mcp subprocess"
                );
                std::process::abort();
            };

            // A write or flush failure on stdout is usually EPIPE (client
            // hung up) but can also be a transient back-pressure event. The
            // historical behaviour was to exit the notifier on the first
            // failure, which silently darked the channel for the rest of
            // the session even when stdout recovered. Track consecutive
            // failures and only exit after MAX_CONSECUTIVE_WRITE_FAILURES
            // -- a long-dead pipe still gets us out of the loop, while a
            // single hiccup no longer kills delivery permanently. The
            // mutex-poisoned case above stays as `abort()` because that
            // one is genuinely unrecoverable.
            //
            // The cursor was already advanced for this batch (at the top
            // of the for-loop's enclosing scope), so failed posts are not
            // retried -- accept the loss in exchange for keeping the
            // thread alive. Loss-tolerant beats dead-tolerant.
            let write_ok = writeln!(locked, "{s}").is_ok() && locked.flush().is_ok();
            drop(locked);
            if write_ok {
                consecutive_write_failures = 0;
                // Advance the per-recipient delivery cursor so the next cold
                // boot resumes from this post rather than replaying it. The
                // upsert is forward-only; concurrent writers (e.g. the HTTP
                // backlog path's mark_board_read) cannot move the cursor
                // backwards and race us into re-delivery. Best-effort: a
                // failure here is logged but does not kill the loop -- worst
                // case is one redundant replay on the next boot.
                // Persisted cursor advances only on delivered rows.
                // Skipped rows are re-filtered on the next cold boot
                // (idempotent), and the cross-process race a stale
                // persisted cursor could mask -- a directed signal
                // arriving at the same `created_at` from another
                // process between boots -- stays narrow this way.
                // The in-memory `last_seen_at` already passes skipped
                // rows within this running loop; that's a separate
                // single-process advance, not the same invariant.
                if let Some(recipient) = client_repo
                    && let Err(e) =
                        db.advance_board_read_cursor(recipient, &post.created_at, &post.id)
                {
                    mcp_trace(
                        "notifier.cursor.advance.failed",
                        &[
                            ("recipient", recipient),
                            ("post_id", &post.id),
                            ("err", &e.to_string()),
                        ],
                    );
                }
            } else {
                consecutive_write_failures = consecutive_write_failures.saturating_add(1);
                eprintln!(
                    "[legion mcp notif] stdout write failed ({}/{}) for post {}",
                    consecutive_write_failures, MAX_CONSECUTIVE_WRITE_FAILURES, post.id
                );
                if consecutive_write_failures >= MAX_CONSECUTIVE_WRITE_FAILURES {
                    eprintln!(
                        "[legion mcp notif] {} consecutive write failures; notifier thread exiting (channel push is now inoperative for this session)",
                        consecutive_write_failures
                    );
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::testutil::mcp_test_dir;

    #[test]
    fn notifier_health_unknown_before_first_tick() {
        let hb = NotifierHeartbeat::new();
        let health = classify_notifier_health(1000, &hb, std::time::Duration::from_secs(1));
        assert!(matches!(health, NotifierHealth::Unknown));
    }

    #[test]
    fn notifier_health_alive_within_threshold() {
        let hb = NotifierHeartbeat::new();
        hb.last_poll_unix_secs.store(1000, Ordering::Relaxed);
        // 2s since last tick, threshold = 3*1 + 1 = 4s -> Alive.
        let health = classify_notifier_health(1002, &hb, std::time::Duration::from_secs(1));
        match health {
            NotifierHealth::Alive { last_tick_secs_ago } => {
                assert_eq!(last_tick_secs_ago, 2);
            }
            other => panic!("expected Alive, got {other:?}"),
        }
    }

    #[test]
    fn notifier_health_threshold_handles_sub_second_poll() {
        // 500ms poll should yield threshold = ceil(1.5) + 1 = 3s, not the
        // 1s the previous `as i64 * 3 + 1` would have produced after
        // truncating 0.5 to 0.
        let hb = NotifierHeartbeat::new();
        hb.last_poll_unix_secs.store(1000, Ordering::Relaxed);
        // 2s since last tick: under 3s threshold -> Alive.
        let health = classify_notifier_health(1002, &hb, std::time::Duration::from_millis(500));
        assert!(matches!(health, NotifierHealth::Alive { .. }));
        // 4s since last tick: over 3s threshold -> Stale.
        let health = classify_notifier_health(1004, &hb, std::time::Duration::from_millis(500));
        match health {
            NotifierHealth::Stale { threshold_secs, .. } => {
                assert_eq!(threshold_secs, 3);
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test]
    fn notifier_health_stale_past_threshold() {
        let hb = NotifierHeartbeat::new();
        hb.last_poll_unix_secs.store(1000, Ordering::Relaxed);
        // 10s since last tick, threshold = 3*1 + 1 = 4s -> Stale.
        let health = classify_notifier_health(1010, &hb, std::time::Duration::from_secs(1));
        match health {
            NotifierHealth::Stale {
                last_tick_secs_ago,
                threshold_secs,
            } => {
                assert_eq!(last_tick_secs_ago, 10);
                assert_eq!(threshold_secs, 4);
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test]
    fn notifier_health_clamps_negative_skew_to_zero() {
        // Clock jumped backwards (NTP correction). last_tick_secs_ago must
        // not underflow; report 0 and let the watchdog re-evaluate next call.
        let hb = NotifierHeartbeat::new();
        hb.last_poll_unix_secs.store(1010, Ordering::Relaxed);
        let health = classify_notifier_health(1000, &hb, std::time::Duration::from_secs(1));
        match health {
            NotifierHealth::Alive { last_tick_secs_ago } => {
                assert_eq!(last_tick_secs_ago, 0);
            }
            other => panic!("expected Alive (clamped), got {other:?}"),
        }
    }

    #[test]
    fn notifier_health_touch_updates_timestamp() {
        let hb = NotifierHeartbeat::new();
        assert_eq!(hb.last_poll(), 0);
        hb.touch();
        let after = hb.last_poll();
        assert!(after > 0);
        // touch is monotonic via wall clock; two consecutive calls within
        // the same second can produce the same value, so we don't assert
        // strict-greater here.
    }

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
    fn seed_notifier_cursor_unknown_client_uses_watermark_when_table_empty() {
        let (db, _dir) = mcp_test_dir();
        let (ts, id) = seed_notifier_cursor(&db, None).expect("seed");
        // Empty board -> seed at now() with empty id.
        assert!(id.is_empty());
        // Cannot equality-test `now`, but it parses as RFC3339.
        chrono::DateTime::parse_from_rfc3339(&ts).expect("valid rfc3339");
    }

    #[test]
    fn seed_notifier_cursor_known_recipient_no_history_uses_cold_boot_replay() {
        let (db, _dir) = mcp_test_dir();
        let (ts, id) = seed_notifier_cursor(&db, Some("kessel")).expect("seed");
        assert!(id.is_empty());
        let parsed = chrono::DateTime::parse_from_rfc3339(&ts).expect("valid rfc3339");
        let age = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
        // Should be roughly 24h old; allow 1h slack for slow runners.
        let lo = NOTIFIER_COLD_BOOT_REPLAY - chrono::Duration::hours(1);
        let hi = NOTIFIER_COLD_BOOT_REPLAY + chrono::Duration::hours(1);
        assert!(
            age >= lo && age <= hi,
            "expected ~24h backstop, got {}",
            age
        );
    }

    #[test]
    fn seed_notifier_cursor_known_recipient_with_history_uses_board_reads() {
        let (db, _dir) = mcp_test_dir();
        let pinned_ts = "2026-04-01T12:00:00+00:00";
        let pinned_id = "019dabcd-0000-7000-8000-000000000001";
        db.advance_board_read_cursor("kessel", pinned_ts, pinned_id)
            .unwrap();
        let (ts, id) = seed_notifier_cursor(&db, Some("kessel")).expect("seed");
        assert_eq!(ts, pinned_ts);
        assert_eq!(id, pinned_id);
    }

    #[test]
    fn replay_should_deliver_directed_signal_to_this_recipient() {
        assert!(replay_should_deliver(
            "@kessel ping:open hello",
            Some("kessel")
        ));
    }

    #[test]
    fn replay_should_deliver_handles_colon_suffix() {
        assert!(replay_should_deliver(
            "@kessel: please review",
            Some("kessel")
        ));
    }

    #[test]
    fn replay_should_deliver_drops_broadcast_all() {
        // @all is a live-time courtesy, not worth replaying when stale.
        assert!(!replay_should_deliver("@all team standup", Some("kessel")));
    }

    #[test]
    fn replay_should_deliver_drops_directed_to_other_agent() {
        assert!(!replay_should_deliver(
            "@huttspawn check this",
            Some("kessel")
        ));
    }

    #[test]
    fn replay_should_deliver_drops_general_musing() {
        // Plain post with no @ prefix is a musing -- skip on replay.
        assert!(!replay_should_deliver(
            "thinking about color tokens",
            Some("kessel")
        ));
    }

    #[test]
    fn replay_should_deliver_returns_false_when_recipient_unknown() {
        assert!(!replay_should_deliver("@kessel ping", None));
    }

    #[test]
    fn replay_should_deliver_rejects_at_at_prefix() {
        // Same edge case as should_notify: `@@all` is a fat-finger, not a broadcast.
        assert!(!replay_should_deliver("@@all hi", Some("kessel")));
    }

    #[test]
    fn cold_boot_replay_picks_up_offline_signal_then_advance_prevents_redelivery() {
        // End-to-end #400: signal filed while kessel is offline lands on
        // first poll; advance_board_read_cursor on delivery means second
        // boot does not re-replay the same row.
        let (db, _dir) = mcp_test_dir();

        // Pretend a directed signal was filed an hour ago, before kessel boots.
        let post = db
            .insert_reflection("legion", "@kessel ping:open from a test", "team")
            .expect("insert");
        let post_id = post.id.clone();
        // Sanity: board_reads is empty for kessel.
        assert!(db.get_board_read_cursor("kessel").unwrap().is_none());

        // First boot: cold replay seed should be in the past, so the post is
        // visible via get_board_posts_since.
        let (seed_at, seed_id) = seed_notifier_cursor(&db, Some("kessel")).expect("seed");
        let visible = db
            .get_board_posts_since(&seed_at, &seed_id, NOTIFIER_BATCH_LIMIT)
            .expect("posts");
        let found = visible.iter().any(|p| p.id == post_id);
        assert!(found, "cold-boot replay must surface the offline signal");

        // Simulate successful delivery: advance the cursor to the post's
        // (created_at, id).
        let delivered = visible
            .iter()
            .find(|p| p.id == post_id)
            .expect("post present");
        db.advance_board_read_cursor("kessel", &delivered.created_at, &delivered.id)
            .unwrap();

        // Second boot: seed comes from board_reads now, equal to the post's
        // created_at. Strict-`>` comparator means the post is NOT re-emitted.
        let (seed_at_2, seed_id_2) = seed_notifier_cursor(&db, Some("kessel")).expect("seed");
        let visible_2 = db
            .get_board_posts_since(&seed_at_2, &seed_id_2, NOTIFIER_BATCH_LIMIT)
            .expect("posts");
        assert!(
            !visible_2.iter().any(|p| p.id == post_id),
            "advanced cursor must not re-replay an already-delivered post"
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

    #[test]
    fn build_channel_content_escapes_cdata_terminator() {
        // A post text containing the CDATA terminator `]]>` would otherwise
        // close the CDATA block early and leak raw content into the XML.
        // escape_cdata splits the terminator across a close/reopen using the
        // canonical `]]]]><![CDATA[>` pattern. An XML parser then sees the
        // original `]]>` in the reassembled CDATA content.
        let content = build_channel_content(
            "019d-test-id",
            "legion",
            "here is the literal terminator ]]> in a code example",
            false,
        );
        assert!(
            content.contains("]]]]><![CDATA[>"),
            "CDATA escape should split ]]> across a close-and-reopen; got: {content}"
        );
        // The legitimate final closer is still `]]></text></channel>`. That
        // is the ONE allowed occurrence of the `]]>` sequence -- and it
        // must close a balanced pair of CDATA opens.
        assert!(
            content.ends_with("]]></text></channel>"),
            "content must end with the correct closer; got: {content}"
        );
        let cdata_opens = content.matches("<![CDATA[").count();
        let cdata_closes = content.matches("]]>").count();
        assert_eq!(
            cdata_opens, cdata_closes,
            "CDATA opens/closes must balance after escape (opens={cdata_opens}, closes={cdata_closes}); got: {content}"
        );
    }

    #[test]
    fn build_channel_content_escapes_xml_attributes() {
        // Post id / repo go into attribute positions. A post from a repo
        // named with a literal quote or ampersand would otherwise break the
        // attribute quoting. Not expected in practice, but cheap to enforce.
        let content = build_channel_content("id\"with'quote", "repo&name", "plain text body", true);
        assert!(
            content.contains("id&quot;with"),
            "post_id attribute must be XML-escaped; got: {content}"
        );
        assert!(
            content.contains("repo&amp;name"),
            "repo attribute must be XML-escaped; got: {content}"
        );
    }
}
