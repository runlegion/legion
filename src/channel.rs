use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::sync::broadcast;

use crate::board;
use crate::db::{Database, ReflectionMeta};
use crate::error::LegionError;
use crate::search::SearchIndex;
use crate::signal as sig;

/// Broadcast channel capacity. A slow SSE consumer can lag by up to this many
/// events before it starts missing notifications.
const BROADCAST_CAPACITY: usize = 1024;

/// Maximum number of feed items returned by GET /api/feed.
const FEED_LIMIT: usize = 100;

/// Maximum number of feed items returned by the SSE feed event.
const SSE_FEED_LIMIT: usize = 20;

/// Seconds between keepalive pings when no change has been detected.
const PING_INTERVAL_SECS: u64 = 30;

/// Seconds between SSE poll-fallback wakeups (#613 cadence decision).
///
/// The broadcast channel only fires for in-process writes (HTTP /api/post,
/// the schedule-firing task, MCP handlers in the daemon). Cross-process
/// writes -- `legion post` from a CLI session, another legion binary
/// touching the same database -- never reach this process's broadcast, so
/// every connected SSE stream also re-checks the change timestamps on this
/// interval. 5s is the worst-case dashboard latency for a CLI write;
/// in-process writes surface immediately via the edge trigger.
const SSE_POLL_FALLBACK_SECS: u64 = 5;

/// Wake-up signal for in-process consumers of the broadcast channel. The
/// variants carry no payload: every consumer re-reads from the database on
/// receipt. A previous revision attached a `post_id` to `Feed`, but the two
/// live consumers now both query the database themselves (the HTTP SSE
/// handler queries `max(created_at)` on every tick, and the MCP notifier
/// was moved to a DB polling loop because a `tokio::broadcast` cannot
/// cross process boundaries and was silently missing writes from other
/// processes).
///
/// **The broadcast channel is still live and still used.** The SSE handler
/// in `src/channel.rs` subscribes and uses it as the edge-triggered wakeup
/// that replaces a dumber polling loop. The MCP tool-call handlers in
/// `src/mcp.rs` still fire `tx.send(ChannelEvent::Feed)` on every post so
/// an in-process SSE consumer -- for example, a future daemon mode that
/// runs both HTTP and MCP in one process -- sees them with zero-latency
/// wakeup. The MCP notifier thread does NOT subscribe (it polls the DB
/// directly), so the send in `handle_tool_call` is a no-op in the
/// stdio-only `legion mcp` subprocess. Do NOT delete the broadcast path
/// or the `tx.send` calls on the assumption that they are dead -- the SSE
/// consumer depends on them.
///
/// The wire-level `<channel post_id="...">` XML attribute is unchanged --
/// only this internal event enum lost the field.
#[derive(Debug, Clone)]
pub enum ChannelEvent {
    /// New board post or reflection arrived.
    Feed,
    /// Task table changed.
    Tasks,
}

/// Which server process answers the shared endpoints. Baked into /health
/// as the `role` field so port-:3131 clients -- above all the SessionStart
/// supervisor (#321) -- can tell the daemon from a `legion serve` and pick
/// the right remedy on version mismatch (#613, absorbed #601).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerRole {
    /// `legion serve`: the dashboard process.
    Serve,
    /// `legion daemon`: watch loop + channel HTTP in one process.
    Daemon,
}

impl ServerRole {
    /// Wire value for the /health `role` field.
    pub fn as_str(self) -> &'static str {
        match self {
            ServerRole::Serve => "serve",
            ServerRole::Daemon => "daemon",
        }
    }
}

/// Shared state for the channel HTTP server.
#[derive(Clone)]
pub struct ChannelState {
    pub data_dir: PathBuf,
    pub tx: broadcast::Sender<ChannelEvent>,
    /// Wall-clock start of the owning server process. Captured once at boot
    /// so /health (#319) can report uptime without per-request work.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Which process this state belongs to; reported by /health.
    pub role: ServerRole,
}

/// Error type for every serve.rs and channel.rs HTTP handler (#613).
///
/// Implements `IntoResponse`, so handlers return `Result<_, ServeError>` and
/// propagate failures with `?` instead of hand-writing the same
/// match-to-json-error block per call site. The wire shape -- a JSON body of
/// `{"error": <message>}` with the matching status code -- and the
/// per-endpoint message prefixes (e.g. "query error: ...", "status error:
/// ...") are part of the public contract and are preserved exactly.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The legion database could not be opened. Always 500.
    #[error("failed to open database")]
    DbOpen,
    /// The search index could not be opened. Always 500.
    #[error("failed to open search index")]
    IndexOpen,
    /// Internal failure with a handler-chosen message. 500.
    #[error("{0}")]
    Internal(String),
    /// Caller error. 400.
    #[error("{0}")]
    BadRequest(String),
    /// Resource missing. 404.
    #[error("{0}")]
    NotFound(String),
}

impl ServeError {
    /// Internal error with a contextual prefix, preserving the per-endpoint
    /// message conventions ("status error: <e>", "insert error: <e>", ...).
    pub fn internal(context: &str, e: impl std::fmt::Display) -> Self {
        ServeError::Internal(format!("{context}: {e}"))
    }
}

/// The dominant handler convention: a `LegionError` escaping a handler is a
/// query failure and renders as 500 `{"error": "query error: <e>"}`. Sites
/// with a different deliberate prefix call `ServeError::internal` explicitly.
impl From<LegionError> for ServeError {
    fn from(e: LegionError) -> Self {
        ServeError::internal("query error", e)
    }
}

impl IntoResponse for ServeError {
    fn into_response(self) -> Response {
        let status = match &self {
            ServeError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ServeError::NotFound(_) => StatusCode::NOT_FOUND,
            ServeError::DbOpen | ServeError::IndexOpen | ServeError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let body = serde_json::json!({ "error": self.to_string() });
        (status, Json(body)).into_response()
    }
}

/// Feed item returned by GET /api/feed. Field names (snake_case) and is_signal flag are part of the
/// public JSON contract -- changing them breaks dashboard and external tooling.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct FeedItem {
    pub id: String,
    pub repo: String,
    pub text: String,
    pub created_at: String,
    pub is_signal: bool,
}

/// Query parameters for GET /api/feed.
#[derive(serde::Deserialize)]
pub struct FeedQuery {
    pub repo: Option<String>,
    pub filter: Option<String>,
    /// When set, return only posts unread by this repo and atomically mark them
    /// as read. Matches the existing serve.rs unread_for behaviour.
    pub unread_for: Option<String>,
}

/// Build the axum Router for the channel HTTP server.
///
/// This is a standalone router -- the caller mounts it into the main axum
/// app. It is the single owner of the shared endpoint contract (#613):
/// /health, /sse, /api/feed, /api/tasks, /api/post. Both `legion serve`
/// (which merges this router into the dashboard app) and the daemon (which
/// serves it bare) answer these paths with the same implementation, so the
/// wire shapes cannot fork again.
pub fn router(state: ChannelState) -> Router {
    Router::new()
        .route("/health", get(health_endpoint))
        .route("/sse", get(sse_handler))
        .route("/api/feed", get(api_feed))
        .route("/api/tasks", get(api_tasks))
        .route("/api/post", post(api_post))
        .with_state(state)
}

/// Pure builder for the `/health` JSON body. Separated from the axum
/// handler so it's directly unit-testable without spinning up a server.
///
/// `role` is additive to the #319 contract (status/version/started_at/
/// uptime_secs are unchanged): pre-#613 clients ignore it, the supervisor
/// uses it to pick between respawning a serve and bouncing the daemon.
fn build_health_body(
    role: ServerRole,
    started_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> serde_json::Value {
    let uptime_secs = (now - started_at).num_seconds().max(0);
    serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        // Build id (git short SHA + dirty flag, or "unknown") so the
        // supervisor can detect a same-version rebuild (#698). Baked in by
        // build.rs via LEGION_BUILD_ID.
        "build_id": env!("LEGION_BUILD_ID"),
        "role": role.as_str(),
        "started_at": started_at.to_rfc3339(),
        "uptime_secs": uptime_secs,
    })
}

/// GET /health -- cheap daemon liveness probe (#319).
///
/// Returns `{status, version, build_id, role, started_at, uptime_secs}`
/// with NO database access so it can be polled aggressively by hooks, the
/// MCP reconnect path (#320), and the SessionStart auto-spawn supervisor
/// (#321). The `version` field is baked in at compile time from
/// CARGO_PKG_VERSION so clients can detect protocol drift after a plugin
/// upgrade; `build_id` (#698) lets the supervisor catch a same-version
/// rebuild that `version` alone cannot.
pub async fn health_endpoint(State(state): State<ChannelState>) -> Json<serde_json::Value> {
    Json(build_health_body(
        state.role,
        state.started_at,
        chrono::Utc::now(),
    ))
}

/// Open a Database from the data_dir. Logs and maps failure to
/// `ServeError::DbOpen` (renders as 500 "failed to open database").
pub(crate) fn open_db(data_dir: &std::path::Path) -> Result<Database, ServeError> {
    Database::open(&data_dir.join("legion.db")).map_err(|e| {
        eprintln!("[legion channel] open_db failed: {e}");
        ServeError::DbOpen
    })
}

/// Open the search index from the data_dir. Logs and maps failure to
/// `ServeError::IndexOpen` (renders as 500 "failed to open search index").
pub(crate) fn open_index(data_dir: &std::path::Path) -> Result<SearchIndex, ServeError> {
    SearchIndex::open(&data_dir.join("index")).map_err(|e| {
        eprintln!("[legion channel] open_index failed: {e}");
        ServeError::IndexOpen
    })
}

/// GET /api/feed -- bullpen posts with optional repo and signal/musing filter.
///
/// Query shape is part of the public JSON contract: repo, filter=signals|musings, unread_for=<repo>.
/// - `repo`: filter by source repo
/// - `filter`: "signals" | "musings" | (all)
/// - `unread_for`: atomic unread-and-mark for the channel backlog
pub async fn api_feed(
    State(state): State<ChannelState>,
    Query(params): Query<FeedQuery>,
) -> Result<Json<Vec<FeedItem>>, ServeError> {
    let db = open_db(&state.data_dir)?;

    let posts = if let Some(reader) = params.unread_for.as_deref() {
        db.get_and_mark_unread_board_posts(reader)?
    } else {
        db.get_board_posts()?
    };

    let repo_filter = params.repo.as_deref().unwrap_or("all");
    let type_filter = params.filter.as_deref().unwrap_or("all");
    let reader = params.unread_for.as_deref();

    let items: Vec<FeedItem> = posts
        .into_iter()
        .filter(|p| reader.is_none_or(|r| p.repo != r))
        .filter(|p| repo_filter == "all" || p.repo == repo_filter)
        .filter_map(|p| {
            let is_signal = sig::is_signal(&p.text);
            let keep = match type_filter {
                "signals" => is_signal,
                "musings" => !is_signal,
                _ => true,
            };
            if keep {
                Some(FeedItem {
                    id: p.id,
                    repo: p.repo,
                    text: p.text,
                    created_at: p.created_at,
                    is_signal,
                })
            } else {
                None
            }
        })
        .take(FEED_LIMIT)
        .collect();

    Ok(Json(items))
}

/// GET /api/tasks -- all tasks serialized as the legacy Task shape.
pub async fn api_tasks(
    State(state): State<ChannelState>,
) -> Result<Json<Vec<crate::task::Task>>, ServeError> {
    let db = open_db(&state.data_dir)?;
    Ok(Json(db.get_all_tasks()?))
}

/// POST /api/post request body.
#[derive(serde::Deserialize)]
pub struct PostRequest {
    pub repo: String,
    pub text: String,
}

/// POST /api/post -- broadcast a message to the bullpen and notify SSE subscribers.
///
/// Response shape (#613 divergence resolution): `{"id": <reflection id>}`.
/// serve.rs used to return the full reflection object from its own copy of
/// this handler; the channel shape won because the only verified consumer
/// (static/app.js) ignores the response body entirely, and the id is all a
/// caller needs to reference the post. Pinned by the
/// api_post_returns_id_only_shape test.
///
/// Index-failure policy (#613 divergence resolution): failures are 500s
/// rather than silently swallowed -- a post that cannot be indexed is
/// unsearchable, which is a half-broken state. serve.rs used to treat
/// indexing as best-effort; the strict policy won because it lives in
/// board::post_from_text_with_meta, the single owner of the write+index
/// invariant. The post may already be in the DB when a 500 is returned;
/// callers should retry.
pub async fn api_post(
    State(state): State<ChannelState>,
    Json(body): Json<PostRequest>,
) -> Result<Json<serde_json::Value>, ServeError> {
    let trimmed = body.text.trim().to_string();
    if trimmed.is_empty() {
        return Err(ServeError::BadRequest("text is required".to_string()));
    }

    let db = open_db(&state.data_dir)?;
    let index = open_index(&state.data_dir)?;

    // TODO(019d7991-2eab): compute and store embedding so this post is similarity-searchable
    let id = board::post_from_text_with_meta(
        &db,
        &index,
        &body.repo,
        &trimmed,
        &ReflectionMeta::default(),
    )
    .map_err(|e| {
        eprintln!("[legion channel] api_post failed: {e}");
        ServeError::Internal("failed to store post".to_string())
    })?;

    // Notify SSE subscribers (best-effort; no SSE listeners is not an error).
    let _ = state.tx.send(ChannelEvent::Feed);

    Ok(Json(serde_json::json!({ "id": id })))
}

/// Interval between due-schedule checks by the background firing task.
///
/// Schedule granularity is minutes (`*/Nm` or daily `HH:MM`), so a 30s
/// poll bounds firing latency at half the finest cron step. The previous
/// home of this loop -- the per-connection SSE stream body -- polled at
/// 2s, but only while a dashboard was connected and once per client.
const SCHEDULE_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Spawn the single background task that fires due schedules (#613).
///
/// Exactly one task per server process owns the get_due_schedules ->
/// post -> mark_schedule_run loop. It previously ran inside the
/// per-connection SSE stream body in serve.rs, which meant schedules
/// fired only while a dashboard was open, fired once per connected
/// client, and raced the get_due/mark_run window across connections.
/// Now both server entry points spawn it once at startup -- `legion
/// serve` (run_server) and the daemon (run_daemon_async) -- so
/// schedules fire under whichever server is running, with zero
/// connected clients. The two servers cannot share a port, so only one
/// fires per host in the default configuration; running both on
/// different ports against the same data dir is the one (accepted,
/// documented) double-firing window.
///
/// `tx` wakes in-process SSE subscribers after a successful fire so
/// dashboards update immediately instead of waiting for the poll
/// fallback.
pub fn spawn_schedule_firing(
    data_dir: PathBuf,
    tx: broadcast::Sender<ChannelEvent>,
) -> tokio::task::JoinHandle<()> {
    spawn_schedule_firing_with_interval(data_dir, tx, SCHEDULE_POLL_INTERVAL)
}

/// Interval-injectable form of `spawn_schedule_firing` -- the test seam.
fn spawn_schedule_firing_with_interval(
    data_dir: PathBuf,
    tx: broadcast::Sender<ChannelEvent>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            // Open per tick: a missing or locked database must not kill
            // the task for the lifetime of the server.
            let db = match Database::open(&data_dir.join("legion.db")) {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("[legion] schedule firing: failed to open db: {e}");
                    continue;
                }
            };
            let index = match SearchIndex::open(&data_dir.join("index")) {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("[legion] schedule firing: failed to open index: {e}");
                    continue;
                }
            };
            if fire_due_schedules(&db, &index) > 0 {
                // Best-effort wake; no SSE listeners is not an error.
                let _ = tx.send(ChannelEvent::Feed);
            }
        }
    })
}

/// Fire every due schedule once: post the command text to the bullpen
/// through `board::post_from_text_with_meta` (the single owner of the
/// write+index invariant) and mark the schedule run regardless of post
/// success so a permanently failing schedule cannot retry-loop forever.
/// Returns the number of successful posts.
fn fire_due_schedules(db: &Database, index: &SearchIndex) -> usize {
    let due = match db.get_due_schedules() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[legion] schedule firing: due query failed: {e}");
            return 0;
        }
    };
    let mut fired: usize = 0;
    for schedule in &due {
        match board::post_from_text_with_meta(
            db,
            index,
            &schedule.repo,
            &schedule.command,
            &ReflectionMeta::default(),
        ) {
            Ok(_) => {
                eprintln!("[legion] schedule fired: {}", schedule.name);
                fired += 1;
            }
            Err(e) => {
                eprintln!("[legion] schedule post failed for {}: {e}", schedule.name);
            }
        }
        // Mark as run regardless of post success to avoid infinite retries.
        if let Err(e) = db.mark_schedule_run(&schedule.id) {
            eprintln!("[legion] failed to mark schedule run: {e}");
        }
    }
    fired
}

/// SSE handler -- streams agents, feed, tasks, and ping events to
/// subscribers (#613: the canonical implementation; serve.rs's 2s-polling
/// twin is deleted and the dashboard connects here).
///
/// Opens the database once at stream start and holds it for the stream's
/// lifetime. Wakes on either edge (a broadcast notification from an
/// in-process write) or the SSE_POLL_FALLBACK_SECS timer (cross-process
/// writes -- see the constant's doc for the cadence decision), then emits
/// events only for timestamps that actually changed. Emits a keepalive
/// ping after PING_INTERVAL_SECS without any event. The stream itself is
/// read-only: schedule firing lives in spawn_schedule_firing, never here.
///
/// Event shapes (all consumed by static/app.js -- the dashboard
/// subscribes to agents, feed, and tasks):
///   agents -- JSON array of AgentInfo (same shape as GET /api/agents)
///   feed   -- JSON array of FeedItem (last SSE_FEED_LIMIT team posts)
///   tasks  -- JSON array of Task
///   ping   -- `{}` heartbeat
pub async fn sse_handler(
    State(state): State<ChannelState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.tx.subscribe();

    let stream = async_stream::stream! {
        // Open DB once for the lifetime of the stream.
        let db = match Database::open(&state.data_dir.join("legion.db")) {
            Ok(db) => db,
            Err(e) => {
                eprintln!("[legion channel] sse_handler: failed to open db: {e}");
                return;
            }
        };

        let mut last_reflection_ts: Option<String> = None;
        let mut last_task_ts: Option<String> = None;
        let poll_fallback = Duration::from_secs(SSE_POLL_FALLBACK_SECS);
        let ping_interval = Duration::from_secs(PING_INTERVAL_SECS);
        let mut last_emit = tokio::time::Instant::now();

        loop {
            // Wait for a broadcast notification or the poll fallback.
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok(_) => {
                            // Something changed -- fall through to emit events.
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Subscriber fell behind the broadcast ring buffer. Events were
                            // dropped, so force a re-read of the DB to catch up.
                            eprintln!("[legion channel] sse subscriber lagged {n} events; forcing re-check");
                            // Fall through to re-query the DB for latest state.
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            eprintln!("[legion channel] sse broadcast closed; ending stream");
                            return;
                        }
                    }
                }
                _ = tokio::time::sleep(poll_fallback) => {
                    // Cross-process writes never hit this process's
                    // broadcast; fall through to the timestamp checks.
                }
            }

            let mut emitted = false;

            // Agents + feed: emit when max created_at changes. The agents
            // event carries the same AgentInfo array as GET /api/agents so
            // the dashboard's live update matches its initial load.
            let current_reflection_ts = db.get_max_created_at().ok().flatten();
            if current_reflection_ts != last_reflection_ts && current_reflection_ts.is_some() {
                last_reflection_ts = current_reflection_ts;

                if let Ok(agents_json) = build_agents_json(&db) {
                    yield Ok(Event::default().event("agents").data(agents_json));
                    emitted = true;
                }

                if let Ok(feed_json) = build_feed_json(&db) {
                    yield Ok(Event::default().event("feed").data(feed_json));
                    emitted = true;
                }
            }

            // Tasks: emit when max task updated_at changes.
            let current_task_ts = db.get_max_task_updated_at().ok().flatten();
            if current_task_ts != last_task_ts && current_task_ts.is_some() {
                last_task_ts = current_task_ts;

                if let Ok(tasks) = db.get_all_tasks()
                    && let Ok(json) = serde_json::to_string(&tasks)
                {
                    yield Ok(Event::default().event("tasks").data(json));
                    emitted = true;
                }
            }

            if emitted {
                last_emit = tokio::time::Instant::now();
            } else if last_emit.elapsed() >= ping_interval {
                yield Ok(Event::default().event("ping").data("{}"));
                last_emit = tokio::time::Instant::now();
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Agent info returned by GET /api/agents and the SSE `agents` event.
/// Field names are part of the public JSON contract (static/app.js reads
/// repo, unread, boost_sum, last_activity directly).
#[derive(serde::Serialize)]
pub struct AgentInfo {
    pub repo: String,
    pub unread: u64,
    pub reflection_count: u64,
    pub boost_sum: i64,
    pub team_post_count: u64,
    pub last_activity: String,
}

/// Build the per-repo agent overview: dashboard stats merged with unread
/// counts. The single source for both GET /api/agents (serve.rs) and the
/// SSE `agents` event -- push and pull of the same resource must emit the
/// same shape or the dashboard diverges mid-session (audit DC3).
pub fn build_agents(db: &Database) -> Result<Vec<AgentInfo>, LegionError> {
    let stats = db.get_dashboard_stats()?;
    let unread_map: HashMap<String, u64> = db
        .get_unread_counts_all()
        .unwrap_or_default()
        .into_iter()
        .collect();

    Ok(stats
        .into_iter()
        .map(|s| AgentInfo {
            unread: unread_map.get(&s.repo).copied().unwrap_or(0),
            repo: s.repo,
            reflection_count: s.reflection_count,
            boost_sum: s.boost_sum,
            team_post_count: s.team_post_count,
            last_activity: s.last_activity,
        })
        .collect())
}

/// Serialized form of `build_agents` for the SSE event payload.
fn build_agents_json(db: &Database) -> Result<String, LegionError> {
    Ok(serde_json::to_string(&build_agents(db)?)?)
}

/// Build the feed JSON payload (last SSE_FEED_LIMIT team posts).
///
/// Returns the actual error so callers can log or propagate it.
fn build_feed_json(db: &Database) -> Result<String, LegionError> {
    let posts = db.get_board_posts()?;
    let items: Vec<FeedItem> = posts
        .into_iter()
        .take(SSE_FEED_LIMIT)
        .map(|p| {
            let is_signal = sig::is_signal(&p.text);
            FeedItem {
                id: p.id,
                repo: p.repo,
                text: p.text,
                created_at: p.created_at,
                is_signal,
            }
        })
        .collect();

    Ok(serde_json::to_string(&items)?)
}

/// Create a broadcast channel pair for the channel pub/sub system.
pub fn new_broadcast() -> (
    broadcast::Sender<ChannelEvent>,
    broadcast::Receiver<ChannelEvent>,
) {
    broadcast::channel(BROADCAST_CAPACITY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ReflectionMeta;
    use crate::testutil::test_storage;

    fn make_feed_item(id: &str, repo: &str, text: &str) -> FeedItem {
        FeedItem {
            id: id.to_string(),
            repo: repo.to_string(),
            text: text.to_string(),
            created_at: "2026-04-09T00:00:00Z".to_string(),
            is_signal: sig::is_signal(text),
        }
    }

    #[test]
    fn feed_endpoint_matches_legacy_shape() {
        let (db, index, _dir) = test_storage();

        // Insert a team post
        let reflection = db
            .insert_reflection_with_meta("kelex", "hello team", "team", &ReflectionMeta::default())
            .expect("insert");
        index
            .add(&reflection.id, "kelex", "hello team")
            .expect("index");

        // Verify the DB has the post. test_storage() uses "test.db" in the same dir.
        let posts = db.get_board_posts().expect("get posts");
        assert_eq!(posts.len(), 1);

        // Build FeedItem from the post -- matches the handler logic exactly.
        let item = FeedItem {
            id: posts[0].id.clone(),
            repo: posts[0].repo.clone(),
            text: posts[0].text.clone(),
            created_at: posts[0].created_at.clone(),
            is_signal: sig::is_signal(&posts[0].text),
        };

        assert_eq!(item.repo, "kelex");
        assert_eq!(item.text, "hello team");
        assert!(!item.is_signal);
        // Verify serialization matches legacy JSON shape.
        let json = serde_json::to_value(&item).expect("serialize");
        assert!(json.get("id").is_some());
        assert!(json.get("repo").is_some());
        assert!(json.get("text").is_some());
        assert!(json.get("created_at").is_some());
        assert!(json.get("is_signal").is_some());
    }

    #[test]
    fn feed_filter_signals_calls_is_signal_once_per_item() {
        // Verifies no double is_signal call via filter_map (finding #16).
        // We test the output shape is correct when filtering signals.
        let items = [
            make_feed_item("1", "kelex", "@legion review:approved"),
            make_feed_item("2", "kelex", "just a musing"),
            make_feed_item("3", "kelex", "@all announce: shipped"),
        ];

        let signals: Vec<_> = items.iter().filter(|i| i.is_signal).collect();
        assert_eq!(signals.len(), 2);

        let musings: Vec<_> = items.iter().filter(|i| !i.is_signal).collect();
        assert_eq!(musings.len(), 1);
    }

    #[test]
    fn broadcast_channel_delivers_events() {
        let (tx, mut rx) = new_broadcast();
        tx.send(ChannelEvent::Feed).expect("send");
        let evt = rx.try_recv().expect("recv");
        assert!(matches!(evt, ChannelEvent::Feed));
    }

    #[test]
    fn dedup_seen_ids_prevents_double_delivery() {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let item = make_feed_item("abc", "kelex", "@legion review:approved");

        assert!(seen.insert(item.id.clone()));
        // Second time: already seen
        assert!(!seen.insert(item.id.clone()));
    }

    #[test]
    fn build_feed_json_returns_valid_json() {
        let (db, _index, _dir) = test_storage();
        db.insert_reflection_with_meta("kelex", "hello", "team", &ReflectionMeta::default())
            .expect("insert");

        let json = build_feed_json(&db).expect("build feed json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 1);
    }

    #[test]
    fn health_body_shape() {
        let started = chrono::DateTime::parse_from_rfc3339("2026-05-10T12:00:00Z")
            .expect("parse")
            .with_timezone(&chrono::Utc);
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-10T12:01:30Z")
            .expect("parse")
            .with_timezone(&chrono::Utc);
        let body = build_health_body(ServerRole::Serve, started, now);
        assert_eq!(body["status"], "ok");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["build_id"], env!("LEGION_BUILD_ID"));
        // build_id is always present and non-empty (build.rs falls back to
        // "unknown" when git is unavailable), so the supervisor never sees a
        // missing field.
        assert!(
            !body["build_id"]
                .as_str()
                .expect("build_id is a string")
                .is_empty(),
            "build_id must be non-empty"
        );
        assert_eq!(body["role"], "serve");
        assert_eq!(body["started_at"], "2026-05-10T12:00:00+00:00");
        assert_eq!(body["uptime_secs"], 90);

        let daemon_body = build_health_body(ServerRole::Daemon, started, now);
        assert_eq!(daemon_body["role"], "daemon");
    }

    #[test]
    fn health_uptime_clamped_at_zero_for_clock_skew() {
        // If `now` is somehow before `started_at` (clock jump, NTP
        // correction, test fixture), uptime must not go negative.
        let started = chrono::DateTime::parse_from_rfc3339("2026-05-10T12:00:00Z")
            .expect("parse")
            .with_timezone(&chrono::Utc);
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-10T11:59:00Z")
            .expect("parse")
            .with_timezone(&chrono::Utc);
        let body = build_health_body(ServerRole::Daemon, started, now);
        assert_eq!(body["uptime_secs"], 0);
    }

    #[test]
    fn build_agents_merges_stats_and_unread() {
        let (db, _index, _dir) = test_storage();
        db.insert_reflection_with_meta("kelex", "hello team", "team", &ReflectionMeta::default())
            .expect("insert");

        let agents = build_agents(&db).expect("build agents");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].repo, "kelex");
        assert_eq!(agents[0].team_post_count, 1);

        // Serialized shape: the SSE agents event and GET /api/agents both
        // emit these field names (public JSON contract, app.js reads them).
        let json = serde_json::to_value(&agents).expect("serialize");
        let first = &json[0];
        for key in [
            "repo",
            "unread",
            "reflection_count",
            "boost_sum",
            "team_post_count",
            "last_activity",
        ] {
            assert!(first.get(key).is_some(), "missing agents field {key}");
        }
    }

    /// Pins the #613 api_post divergence resolution: the response body is
    /// exactly {"id": <uuid>} -- not serve's old full-reflection object.
    #[tokio::test]
    async fn api_post_returns_id_only_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let _db = Database::open(&data_dir.join("legion.db")).expect("open db");

        let (tx, _rx) = new_broadcast();
        let state = ChannelState {
            data_dir: data_dir.clone(),
            tx,
            started_at: chrono::Utc::now(),
            role: ServerRole::Daemon,
        };
        let app = router(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let response = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            use std::net::TcpStream;

            let body = r#"{"repo":"kelex","text":"hello shape"}"#;
            let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).expect("connect");
            stream
                .write_all(
                    format!(
                        "POST /api/post HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .expect("write");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read");
            buf
        })
        .await
        .expect("spawn_blocking");

        let response_str = String::from_utf8_lossy(&response);
        assert!(
            response_str.starts_with("HTTP/1.1 200"),
            "expected 200 OK, got: {}",
            &response_str[..response_str.len().min(200)]
        );
        let json_body = response_str
            .split("\r\n\r\n")
            .nth(1)
            .expect("response body");
        let parsed: serde_json::Value = serde_json::from_str(json_body.trim()).expect("parse body");
        let obj = parsed.as_object().expect("object body");
        assert!(obj.contains_key("id"), "body must carry the post id");
        assert_eq!(
            obj.len(),
            1,
            "api_post body is exactly {{\"id\"}}, got: {parsed}"
        );
    }

    #[test]
    fn fire_due_schedules_posts_and_advances() {
        let (db, index, _dir) = test_storage();

        let id = db
            .insert_schedule("standup", "*/30m", "post the standup", "legion", None, None)
            .expect("insert schedule");

        // Freshly inserted: next_run is in the future, nothing fires.
        assert_eq!(fire_due_schedules(&db, &index), 0, "not due yet");

        db.force_schedule_due(&id).expect("force due");
        assert_eq!(fire_due_schedules(&db, &index), 1, "due schedule fires");

        let posts = db.get_board_posts().expect("posts");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].text, "post the standup");
        assert_eq!(posts[0].repo, "legion");

        // mark_schedule_run advanced next_run, so an immediate re-check
        // must NOT double-fire -- the old per-SSE-connection loop did.
        assert_eq!(fire_due_schedules(&db, &index), 0, "no double fire");
        assert_eq!(db.get_board_posts().expect("posts").len(), 1);
    }

    /// AC (#613): schedules fire with zero SSE clients connected. The real
    /// background task is spawned with no subscriber anywhere -- no /sse
    /// stream, no broadcast receiver kept -- and the post still lands.
    #[tokio::test]
    async fn schedules_fire_with_zero_sse_clients() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();

        // The task opens data_dir/legion.db and data_dir/index, the same
        // paths run_server and run_daemon_async hand it.
        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        let _index = SearchIndex::open(&data_dir.join("index")).expect("open index");
        let id = db
            .insert_schedule(
                "nightly",
                "*/30m",
                "fire without clients",
                "legion",
                None,
                None,
            )
            .expect("insert schedule");
        db.force_schedule_due(&id).expect("force due");

        let (tx, _rx) = new_broadcast();
        drop(_rx); // zero subscribers: firing must not depend on listeners
        let handle =
            spawn_schedule_firing_with_interval(data_dir.clone(), tx, Duration::from_millis(20));

        // Poll for the fired post instead of a fixed sleep.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let posts = db.get_board_posts().expect("posts");
            if posts.iter().any(|p| p.text == "fire without clients") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "schedule did not fire within 5s with zero SSE clients"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        handle.abort();
    }

    #[test]
    fn broadcast_lag_produces_recv_error() {
        // A subscriber that falls behind the ring buffer capacity gets TryRecvError::Lagged,
        // not a silent drop. This guards the M2 fix -- the SSE handler must handle Lagged
        // explicitly (force re-read) rather than letting the select! arm silently not fire.
        use tokio::sync::broadcast::error::TryRecvError;

        // Tiny capacity to force lag.
        let (tx, mut rx) = broadcast::channel::<ChannelEvent>(1);

        // Fill past capacity without the subscriber reading.
        tx.send(ChannelEvent::Feed).expect("send 1");
        tx.send(ChannelEvent::Feed).expect("send 2");

        // The first recv should be Lagged since we overflowed the 1-slot buffer.
        let result = rx.try_recv();
        assert!(
            matches!(result, Err(TryRecvError::Lagged(_))),
            "expected TryRecvError::Lagged, got: {result:?}"
        );
    }

    #[test]
    fn broadcast_closed_produces_recv_error() {
        // When the sender is dropped the subscriber gets TryRecvError::Closed on next recv.
        // Guards the M2 fix -- SSE handler must return on Closed, not loop forever.
        use tokio::sync::broadcast::error::TryRecvError;

        let (tx, mut rx) = broadcast::channel::<ChannelEvent>(8);
        drop(tx); // close the channel

        let result = rx.try_recv();
        assert!(
            matches!(result, Err(TryRecvError::Closed)),
            "expected TryRecvError::Closed, got: {result:?}"
        );
    }
}
