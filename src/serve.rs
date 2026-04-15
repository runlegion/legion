#![allow(clippy::manual_is_multiple_of)] // Use modulo for MSRV compatibility

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{Html, IntoResponse, Response, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_embed::Embed;
use tokio::signal;

use crate::db::{Database, ReflectionMeta};
use crate::error;
use crate::health::HealthSample;
use crate::search::SearchIndex;
use crate::signal as sig;
use crate::status;

#[derive(Embed)]
#[folder = "static/"]
struct StaticAssets;

#[derive(Clone)]
struct AppState {
    data_dir: PathBuf,
}

/// Open a database connection from the data directory.
///
/// Returns a 500 status code if the database cannot be opened.
fn open_db(data_dir: &Path) -> Result<Database, StatusCode> {
    Database::open(&data_dir.join("legion.db")).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// JSON error response helper.
fn json_error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "error": message });
    (status, Json(body)).into_response()
}

pub fn run_server(port: u16, data_dir: PathBuf) -> error::Result<()> {
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| error::LegionError::Server(format!("failed to create runtime: {e}")))?;

    runtime.block_on(async {
        let state = AppState { data_dir };

        let app = Router::new()
            .route("/", get(index_handler))
            .route("/sse", get(sse_handler))
            .route("/api/agents", get(api_agents))
            .route("/api/feed", get(api_feed))
            .route("/api/tasks", get(api_tasks))
            .route("/api/stats", get(api_stats))
            .route("/api/signals", get(api_signals))
            .route("/api/status", get(api_status))
            .route("/api/needs", get(api_needs))
            .route("/api/done", post(api_done))
            .route("/api/post", post(api_post))
            .route("/api/tasks/create", post(api_create_task))
            .route("/api/tasks/{id}/accept", post(api_task_accept))
            .route("/api/tasks/{id}/done", post(api_task_done))
            .route("/api/tasks/{id}/block", post(api_task_block))
            .route("/api/tasks/{id}/unblock", post(api_task_unblock))
            .route("/api/chat", get(api_chat))
            .route("/api/boost/{id}", post(api_boost))
            .route("/api/health", get(api_health))
            .route("/api/health/history", get(api_health_history))
            .route("/api/search", get(api_search))
            .route("/api/audit", get(api_audit))
            .route("/api/schedules", get(api_schedules))
            .route("/api/schedules/create", post(api_create_schedule))
            .route("/api/schedules/{id}/toggle", post(api_toggle_schedule))
            .route("/api/kanban", get(api_kanban))
            .route("/api/kanban/{id}/move", post(api_kanban_move))
            .route("/api/kanban/workloads", get(api_kanban_workloads))
            .route("/{*path}", get(static_handler))
            .with_state(state);

        let addr = format!("0.0.0.0:{port}");
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| error::LegionError::Server(format!("failed to bind {addr}: {e}")))?;

        eprintln!("[legion] dashboard at http://localhost:{port}");

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| error::LegionError::Server(format!("server error: {e}")))?;

        Ok(())
    })
}

async fn index_handler() -> impl IntoResponse {
    match StaticAssets::get("index.html") {
        Some(file) => Html(String::from_utf8_lossy(file.data.as_ref()).to_string()).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn static_handler(AxumPath(path): AxumPath<String>) -> Response {
    match StaticAssets::get(&path) {
        Some(file) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                file.data.to_vec(),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn sse_handler(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        let mut last_reflection_ts: Option<String> = None;
        let mut last_task_ts: Option<String> = None;
        let mut tick: u64 = 0;
        let poll_interval: Duration = Duration::from_secs(2);
        // Send a ping every 30s = every 15 poll ticks
        let ping_every: u64 = 15;

        loop {
            tokio::time::sleep(poll_interval).await;
            tick += 1;

            let db = match open_db(&state.data_dir) {
                Ok(db) => db,
                Err(_) => {
                    if tick % ping_every == 0 {
                        yield Ok(Event::default().event("ping").data("{}"));
                    }
                    continue;
                }
            };

            // Check for new reflections
            let current_reflection_ts = db.get_max_created_at().ok().flatten();
            if current_reflection_ts != last_reflection_ts && current_reflection_ts.is_some() {
                last_reflection_ts = current_reflection_ts;

                // Emit agents event
                if let Ok(agents_json) = build_agents_json(&db) {
                    yield Ok(Event::default().event("agents").data(agents_json));
                }

                // Emit feed event (last 20 team posts)
                if let Ok(feed_json) = build_feed_json(&db) {
                    yield Ok(Event::default().event("feed").data(feed_json));
                }
            }

            // Check for task changes
            let current_task_ts = db.get_max_task_updated_at().ok().flatten();
            if current_task_ts != last_task_ts && current_task_ts.is_some() {
                last_task_ts = current_task_ts;

                if let Ok(tasks) = db.get_all_tasks()
                    && let Ok(json) = serde_json::to_string(&tasks)
                {
                    yield Ok(Event::default().event("tasks").data(json));
                }
            }

            // Check for due schedules and fire them
            if let Ok(due) = db.get_due_schedules() {
                for schedule in &due {
                    // Post to bullpen
                    if let Ok(reflection) = db.insert_reflection_with_meta(
                        &schedule.repo,
                        &schedule.command,
                        "team",
                        &ReflectionMeta::default(),
                    ) {
                        // Best-effort add to search index
                        if let Ok(index) = SearchIndex::open(&state.data_dir.join("index"))
                            && let Err(e) = index.add(&reflection.id, &reflection.repo, &schedule.command)
                        {
                            eprintln!("[legion] search index add failed for schedule: {e}");
                        }
                        eprintln!("[legion] schedule fired: {}", schedule.name);
                    }
                    // Mark as run regardless of post success to avoid infinite retries
                    if let Err(e) = db.mark_schedule_run(&schedule.id) {
                        eprintln!("[legion] failed to mark schedule run: {e}");
                    }
                }
            }

            // Periodic ping keepalive
            if tick % ping_every == 0 {
                yield Ok(Event::default().event("ping").data("{}"));
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Build the agents JSON payload (same logic as api_agents).
fn build_agents_json(db: &Database) -> Result<String, error::LegionError> {
    let stats = db.get_dashboard_stats()?;
    let unread_map: HashMap<String, u64> = db
        .get_unread_counts_all()
        .unwrap_or_default()
        .into_iter()
        .collect();

    let agents: Vec<AgentInfo> = stats
        .into_iter()
        .map(|s| AgentInfo {
            unread: unread_map.get(&s.repo).copied().unwrap_or(0),
            repo: s.repo,
            reflection_count: s.reflection_count,
            boost_sum: s.boost_sum,
            team_post_count: s.team_post_count,
            last_activity: s.last_activity,
        })
        .collect();

    Ok(serde_json::to_string(&agents)?)
}

/// Build the feed JSON payload (last 20 team posts).
fn build_feed_json(db: &Database) -> Result<String, error::LegionError> {
    let posts = db.get_board_posts()?;
    let items: Vec<FeedItem> = posts
        .into_iter()
        .take(20)
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

/// Agent info returned by GET /api/agents.
#[derive(serde::Serialize)]
struct AgentInfo {
    repo: String,
    unread: u64,
    reflection_count: u64,
    boost_sum: i64,
    team_post_count: u64,
    last_activity: String,
}

/// GET /api/agents -- per-repo agent overview with unread counts.
async fn api_agents(State(state): State<AppState>) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    let stats = match db.get_dashboard_stats() {
        Ok(s) => s,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("query error: {e}"),
            );
        }
    };

    let unread_map: HashMap<String, u64> = match db.get_unread_counts_all() {
        Ok(counts) => counts.into_iter().collect(),
        Err(_) => HashMap::new(),
    };

    let agents: Vec<AgentInfo> = stats
        .into_iter()
        .map(|s| AgentInfo {
            unread: unread_map.get(&s.repo).copied().unwrap_or(0),
            repo: s.repo,
            reflection_count: s.reflection_count,
            boost_sum: s.boost_sum,
            team_post_count: s.team_post_count,
            last_activity: s.last_activity,
        })
        .collect();

    Json(agents).into_response()
}

/// Feed item returned by GET /api/feed.
#[derive(serde::Serialize)]
struct FeedItem {
    id: String,
    repo: String,
    text: String,
    created_at: String,
    is_signal: bool,
}

/// Query parameters for GET /api/feed.
#[derive(serde::Deserialize)]
struct FeedQuery {
    repo: Option<String>,
    filter: Option<String>,
    /// When set, return only posts unread by this reader repo AND atomically
    /// mark them as read. Used by the channel backlog fetch so agents only
    /// see each post once. The reader's own posts are excluded from the
    /// response regardless of other filters. Combining with `repo` narrows
    /// unread posts to that repo (not mutually exclusive).
    unread_for: Option<String>,
}

/// GET /api/feed -- bullpen posts with optional repo and signal/musing filter.
/// When `unread_for=<repo>` is set, returns only posts unread by that repo
/// and atomically marks them as read so the same post is never delivered twice.
async fn api_feed(State(state): State<AppState>, Query(params): Query<FeedQuery>) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    // When `unread_for` is set, use the atomic unread-and-mark query so the
    // channel backlog delivers each post exactly once across connections.
    let posts = if let Some(reader) = params.unread_for.as_deref() {
        match db.get_and_mark_unread_board_posts(reader) {
            Ok(p) => p,
            Err(e) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("query error: {e}"),
                );
            }
        }
    } else {
        match db.get_board_posts() {
            Ok(p) => p,
            Err(e) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("query error: {e}"),
                );
            }
        }
    };

    let repo_filter = params.repo.as_deref().unwrap_or("all");
    let type_filter = params.filter.as_deref().unwrap_or("all");
    let reader = params.unread_for.as_deref();

    let items: Vec<FeedItem> = posts
        .into_iter()
        // Exclude the reader's own posts from unread delivery.
        .filter(|p| reader.is_none_or(|r| p.repo != r))
        .filter(|p| repo_filter == "all" || p.repo == repo_filter)
        .filter(|p| match type_filter {
            "signals" => sig::is_signal(&p.text),
            "musings" => !sig::is_signal(&p.text),
            _ => true,
        })
        .take(100)
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

    Json(items).into_response()
}

/// GET /api/tasks -- all tasks for kanban view.
async fn api_tasks(State(state): State<AppState>) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    match db.get_all_tasks() {
        Ok(tasks) => Json(tasks).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("query error: {e}"),
        ),
    }
}

/// GET /api/stats -- per-repo dashboard stats (same data as agents minus unread).
async fn api_stats(State(state): State<AppState>) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    match db.get_dashboard_stats() {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("query error: {e}"),
        ),
    }
}

/// A parsed signal with source metadata for the signals API.
#[derive(serde::Serialize)]
struct SignalItem {
    id: String,
    from_repo: String,
    to: String,
    verb: String,
    status: Option<String>,
    text: String,
    created_at: String,
}

/// GET /api/signals -- unresolved signals from the bullpen.
async fn api_signals(State(state): State<AppState>) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    let posts = match db.get_board_posts() {
        Ok(p) => p,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("query error: {e}"),
            );
        }
    };

    let items: Vec<SignalItem> = posts
        .into_iter()
        .filter(|p| sig::is_signal(&p.text))
        .filter_map(|p| {
            let parsed = sig::parse_signal(&p.text)?;
            Some(SignalItem {
                id: p.id,
                from_repo: p.repo,
                to: parsed.recipient,
                verb: parsed.verb,
                status: parsed.status,
                text: p.text,
                created_at: p.created_at,
            })
        })
        .collect();

    Json(items).into_response()
}

/// Query parameters for GET /api/status.
#[derive(serde::Deserialize)]
struct StatusQuery {
    repo: String,
}

/// GET /api/status?repo=<name> -- agent status overview.
async fn api_status(State(state): State<AppState>, Query(params): Query<StatusQuery>) -> Response {
    let repo = params.repo.trim();
    if repo.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "repo parameter is required");
    }

    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    match status::get_status(&db, repo) {
        Ok(output) => Json(output).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("status error: {e}"),
        ),
    }
}

/// GET /api/needs?repo=<name> -- team help opportunities for an agent.
async fn api_needs(State(state): State<AppState>, Query(params): Query<StatusQuery>) -> Response {
    let repo = params.repo.trim();
    if repo.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "repo parameter is required");
    }

    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    match status::get_needs(&db, repo) {
        Ok(items) => Json(items).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("needs error: {e}"),
        ),
    }
}

/// Request body for POST /api/done.
#[derive(serde::Deserialize)]
struct DoneRequest {
    repo: String,
    text: String,
}

/// POST /api/done -- announce completed work and notify blocked agents.
async fn api_done(State(state): State<AppState>, Json(body): Json<DoneRequest>) -> Response {
    let repo = body.repo.trim();
    let text = body.text.trim();
    if repo.is_empty() || text.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "repo and text are required");
    }

    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    let index = match open_search_index(&state.data_dir) {
        Ok(idx) => idx,
        Err(_) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to open search index",
            );
        }
    };

    let announcement = format!("{repo} completed: {text}");
    let reflection = match db.insert_reflection_with_meta(
        repo,
        &announcement,
        "team",
        &ReflectionMeta::default(),
    ) {
        Ok(r) => r,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("insert error: {e}"),
            );
        }
    };

    let _ = index.add(&reflection.id, &reflection.repo, &announcement);

    let blocked_agents = status::find_blocked_agents(&db, repo).unwrap_or_default();
    let mut notified: Vec<String> = Vec::new();
    for agent in &blocked_agents {
        let notify_text = format!(
            "@{agent} announce from {repo} -- {repo} completed: {text}. Your blocker may be cleared."
        );
        if let Ok(r) =
            db.insert_reflection_with_meta(repo, &notify_text, "team", &ReflectionMeta::default())
        {
            let _ = index.add(&r.id, &r.repo, &notify_text);
            notified.push(agent.clone());
        }
    }

    Json(status::DoneResult {
        announcement,
        notified,
    })
    .into_response()
}

/// Request body for POST /api/post.
#[derive(serde::Deserialize)]
struct PostRequest {
    repo: String,
    text: String,
}

/// Open the search index from the data directory.
fn open_search_index(data_dir: &Path) -> Result<SearchIndex, StatusCode> {
    SearchIndex::open(&data_dir.join("index")).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// POST /api/post -- broadcast a message to the bullpen.
async fn api_post(State(state): State<AppState>, Json(body): Json<PostRequest>) -> Response {
    let trimmed = body.text.trim();
    if trimmed.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "text is required");
    }

    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    let index = match open_search_index(&state.data_dir) {
        Ok(idx) => idx,
        Err(_) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to open search index",
            );
        }
    };

    let reflection = match db.insert_reflection_with_meta(
        &body.repo,
        trimmed,
        "team",
        &ReflectionMeta::default(),
    ) {
        Ok(r) => r,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("insert error: {e}"),
            );
        }
    };

    // Best-effort add to search index; post is already in DB.
    if let Err(e) = index.add(&reflection.id, &reflection.repo, trimmed) {
        eprintln!("[legion] search index add failed: {e}");
    }

    Json(reflection).into_response()
}

/// POST /api/boost/:id -- boost a reflection's recall count.
async fn api_boost(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    match db.boost_reflection(&id) {
        Ok(true) => Json(serde_json::json!({"ok": true})).into_response(),
        Ok(false) => json_error(StatusCode::NOT_FOUND, "reflection not found"),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("boost error: {e}"),
        ),
    }
}

/// Request body for POST /api/tasks/create.
#[derive(serde::Deserialize)]
struct CreateTaskRequest {
    from: String,
    to: String,
    text: String,
    priority: String,
    context: Option<String>,
}

/// POST /api/tasks/create -- create a new task from the dashboard.
async fn api_create_task(
    State(state): State<AppState>,
    Json(body): Json<CreateTaskRequest>,
) -> Response {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "text is required");
    }
    if body.to.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "to is required");
    }
    if !["low", "med", "high"].contains(&body.priority.as_str()) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "priority must be low, med, or high",
        );
    }

    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    let context_ref = body.context.as_deref().filter(|c| !c.trim().is_empty());

    let id = match db.insert_task(
        &body.from,
        body.to.trim(),
        &text,
        context_ref,
        &body.priority,
    ) {
        Ok(id) => id,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("insert error: {e}"),
            );
        }
    };

    let task = match db.get_task_by_id(&id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "task created but not found",
            );
        }
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("fetch error: {e}"),
            );
        }
    };

    (StatusCode::CREATED, Json(task)).into_response()
}

/// Optional request body for task state transitions.
#[derive(serde::Deserialize, Default)]
struct TaskTransitionBody {
    note: Option<String>,
}

/// POST /api/tasks/:id/accept -- accept a pending task.
async fn api_task_accept(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };
    match crate::task::accept_task(&db, &id) {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => json_error(StatusCode::BAD_REQUEST, &format!("{e}")),
    }
}

/// POST /api/tasks/:id/done -- complete an accepted task.
async fn api_task_done(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<TaskTransitionBody>>,
) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };
    let note = body.and_then(|b| b.note.clone());
    match crate::task::complete_task(&db, &id, note.as_deref()) {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => json_error(StatusCode::BAD_REQUEST, &format!("{e}")),
    }
}

/// POST /api/tasks/:id/block -- block an accepted task.
async fn api_task_block(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<TaskTransitionBody>>,
) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };
    let reason = body.and_then(|b| b.note.clone());
    match crate::task::block_task(&db, &id, reason.as_deref()) {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => json_error(StatusCode::BAD_REQUEST, &format!("{e}")),
    }
}

/// POST /api/tasks/:id/unblock -- unblock a blocked task.
async fn api_task_unblock(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };
    match crate::task::unblock_task(&db, &id) {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => json_error(StatusCode::BAD_REQUEST, &format!("{e}")),
    }
}

/// Query parameters for GET /api/chat.
#[derive(serde::Deserialize)]
struct ChatQuery {
    agent: String,
}

/// A chat message for the conversation view.
#[derive(serde::Serialize)]
struct ChatMessage {
    id: String,
    repo: String,
    text: String,
    created_at: String,
}

/// GET /api/chat?agent=<name> -- filtered conversation between meatbag and an agent.
async fn api_chat(State(state): State<AppState>, Query(params): Query<ChatQuery>) -> Response {
    let agent = params.agent.trim().to_lowercase();
    if agent.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "agent parameter is required");
    }

    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    let posts = match db.get_board_posts() {
        Ok(p) => p,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("query error: {e}"),
            );
        }
    };

    let at_agent = format!("@{agent}");
    let at_meatbag = "@meatbag";
    let at_all = "@all";

    let mut messages: Vec<ChatMessage> = posts
        .into_iter()
        .filter(|p| {
            let text_lower = p.text.to_lowercase();
            let repo_lower = p.repo.to_lowercase();
            // meatbag posts mentioning the agent
            let from_meatbag = repo_lower == "meatbag" && text_lower.contains(&at_agent);
            // agent posts mentioning meatbag or all
            let from_agent = repo_lower == agent
                && (text_lower.contains(at_meatbag) || text_lower.contains(at_all));
            from_meatbag || from_agent
        })
        .map(|p| ChatMessage {
            id: p.id,
            repo: p.repo,
            text: p.text,
            created_at: p.created_at,
        })
        .collect();

    // Reverse to chronological order (oldest first) since board posts come newest-first
    messages.reverse();

    // Limit to last 50
    if messages.len() > 50 {
        let start = messages.len() - 50;
        messages = messages.split_off(start);
    }

    Json(messages).into_response()
}

/// GET /api/schedules -- list all schedules.
async fn api_schedules(State(state): State<AppState>) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    match db.list_schedules() {
        Ok(schedules) => Json(schedules).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("query error: {e}"),
        ),
    }
}

/// Request body for POST /api/schedules/create.
#[derive(serde::Deserialize)]
struct CreateScheduleRequest {
    name: String,
    cron: String,
    command: String,
    repo: String,
    active_start: Option<String>,
    active_end: Option<String>,
}

/// POST /api/schedules/create -- create a new schedule.
async fn api_create_schedule(
    State(state): State<AppState>,
    Json(body): Json<CreateScheduleRequest>,
) -> Response {
    let name = body.name.trim();
    if name.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "name is required");
    }
    let command = body.command.trim();
    if command.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "command is required");
    }

    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    match db.insert_schedule(
        name,
        &body.cron,
        command,
        &body.repo,
        body.active_start.as_deref(),
        body.active_end.as_deref(),
    ) {
        Ok(id) => Json(serde_json::json!({"ok": true, "id": id})).into_response(),
        Err(e) => json_error(StatusCode::BAD_REQUEST, &format!("create error: {e}")),
    }
}

/// POST /api/schedules/:id/toggle -- toggle a schedule's enabled state.
async fn api_toggle_schedule(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    // Read current state to toggle it
    let schedules = match db.list_schedules() {
        Ok(s) => s,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("query error: {e}"),
            );
        }
    };

    let current = schedules.iter().find(|s| s.id == id);
    match current {
        None => json_error(StatusCode::NOT_FOUND, "schedule not found"),
        Some(s) => {
            let new_enabled = !s.enabled;
            match db.toggle_schedule(&id, new_enabled) {
                Ok(true) => {
                    Json(serde_json::json!({"ok": true, "enabled": new_enabled})).into_response()
                }
                Ok(false) => json_error(StatusCode::NOT_FOUND, "schedule not found"),
                Err(e) => json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("toggle error: {e}"),
                ),
            }
        }
    }
}

/// Query parameters for GET /api/health.
#[derive(serde::Deserialize)]
struct HealthQuery {
    /// Minutes of history to return (default 60).
    minutes: Option<u64>,
}

/// GET /api/health -- latest health samples per host + recent history.
async fn api_health(State(state): State<AppState>, Query(params): Query<HealthQuery>) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    let minutes = params.minutes.unwrap_or(60);
    let since = chrono::Utc::now() - chrono::Duration::minutes(minutes as i64);
    let since_str = since.to_rfc3339();

    let samples: Vec<HealthSample> = match db.get_health_all_hosts(&since_str) {
        Ok(s) => s,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("health query error: {e}"),
            );
        }
    };

    // Group by hostname, return latest + history
    let mut by_host: HashMap<String, Vec<&HealthSample>> = HashMap::new();
    for sample in &samples {
        by_host
            .entry(sample.hostname.clone())
            .or_default()
            .push(sample);
    }

    let hosts: Vec<serde_json::Value> = by_host
        .into_iter()
        .map(|(hostname, mut host_samples)| {
            host_samples.sort_by(|a, b| b.sampled_at.cmp(&a.sampled_at));
            let latest = host_samples.first().map(|s| {
                serde_json::json!({
                    "cpu_usage_pct": s.cpu_usage_pct,
                    "mem_usage_pct": s.mem_usage_pct,
                    "mem_total_bytes": s.mem_total_bytes,
                    "mem_used_bytes": s.mem_used_bytes,
                    "swap_total_bytes": s.swap_total_bytes,
                    "swap_used_bytes": s.swap_used_bytes,
                    "load_avg_1": s.load_avg_1,
                    "load_avg_5": s.load_avg_5,
                    "load_avg_15": s.load_avg_15,
                    "cpu_core_count": s.cpu_core_count,
                    "cpu_temp_celsius": s.cpu_temp_celsius,
                    "agents_active": s.agents_active,
                    "pressure": s.pressure,
                    "sampled_at": s.sampled_at,
                })
            });
            let history: Vec<serde_json::Value> = host_samples
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "sampled_at": s.sampled_at,
                        "cpu_usage_pct": s.cpu_usage_pct,
                        "mem_usage_pct": s.mem_usage_pct,
                        "pressure": s.pressure,
                        "agents_active": s.agents_active,
                    })
                })
                .collect();
            serde_json::json!({
                "hostname": hostname,
                "latest": latest,
                "history": history,
            })
        })
        .collect();

    Json(hosts).into_response()
}

/// Query parameters for GET /api/health/history.
#[derive(serde::Deserialize)]
struct HealthHistoryQuery {
    /// Hostname to filter by.
    hostname: String,
    /// Minutes of history to return (default 60).
    minutes: Option<u64>,
}

/// GET /api/health/history -- time-series health samples for a single host.
async fn api_health_history(
    State(state): State<AppState>,
    Query(params): Query<HealthHistoryQuery>,
) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    let minutes = params.minutes.unwrap_or(60);
    let since = chrono::Utc::now() - chrono::Duration::minutes(minutes as i64);
    let since_str = since.to_rfc3339();

    match db.get_health_history(&params.hostname, &since_str) {
        Ok(samples) => Json(samples).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("health history error: {e}"),
        ),
    }
}

/// Query parameters for GET /api/search.
#[derive(serde::Deserialize)]
struct SearchQuery {
    /// Search query string.
    q: String,
    /// Optional repo filter (omit to search all).
    repo: Option<String>,
    /// Max results (default 10).
    limit: Option<usize>,
}

/// GET /api/search -- BM25-ranked reflection search.
async fn api_search(State(state): State<AppState>, Query(params): Query<SearchQuery>) -> Response {
    let q = params.q.trim();
    if q.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "q parameter is required");
    }

    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    let index = match open_search_index(&state.data_dir) {
        Ok(idx) => idx,
        Err(_) => {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "search index error");
        }
    };

    let limit = params.limit.unwrap_or(10).min(50);

    let hits = match &params.repo {
        Some(repo) => index.search(repo, q, limit),
        None => index.search_all(q, limit),
    };

    let hits = match hits {
        Ok(hits) => hits,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("search error: {e}"),
            );
        }
    };

    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    let reflections = match db.get_reflections_by_ids(&ids) {
        Ok(r) => r,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("reflection lookup error: {e}"),
            );
        }
    };

    // Build score map from search hits, then return reflections in score order
    let score_map: HashMap<&str, f32> = hits.iter().map(|h| (h.id.as_str(), h.score)).collect();
    let mut results: Vec<serde_json::Value> = reflections
        .into_iter()
        .map(|r| {
            let score = score_map.get(r.id.as_str()).copied().unwrap_or(0.0);
            serde_json::json!({
                "id": r.id,
                "repo": r.repo,
                "text": r.text,
                "score": score,
                "created_at": r.created_at,
                "domain": r.domain,
                "tags": r.tags,
            })
        })
        .collect();
    results.sort_by(|a, b| {
        let sa = a["score"].as_f64().unwrap_or(0.0);
        let sb = b["score"].as_f64().unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    Json(results).into_response()
}

/// Query parameters for GET /api/audit.
#[derive(serde::Deserialize)]
struct AuditQuery {
    /// Filter by agent name.
    agent: Option<String>,
    /// Filter by action type.
    action: Option<String>,
    /// Max results (default 50).
    limit: Option<usize>,
}

/// GET /api/audit -- recent audit log entries.
async fn api_audit(State(state): State<AppState>, Query(params): Query<AuditQuery>) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    let limit = params.limit.unwrap_or(50).min(200);

    match db.query_audit_log(params.agent.as_deref(), params.action.as_deref(), limit) {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("audit query error: {e}"),
        ),
    }
}

/// GET /api/kanban -- all kanban cards for the board view.
async fn api_kanban(State(state): State<AppState>) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    match crate::kanban::board_cards(&db) {
        Ok(cards) => Json(cards).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("query error: {e}"),
        ),
    }
}

/// Request body for POST /api/kanban/{id}/move.
#[derive(serde::Deserialize)]
struct KanbanMoveRequest {
    status: String,
    sort_order: Option<i32>,
}

/// POST /api/kanban/{id}/move -- drag-and-drop: force-move a card to a new status/position.
async fn api_kanban_move(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<KanbanMoveRequest>,
) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    let new_status = match body.status.parse::<crate::kanban::CardStatus>() {
        Ok(s) => s,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("invalid status: {e}")),
    };

    match crate::kanban::force_move(&db, &id, new_status, body.sort_order) {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("move failed: {e}"),
        ),
    }
}

/// GET /api/kanban/workloads -- per-agent workload summary for the agent strip.
async fn api_kanban_workloads(State(state): State<AppState>) -> Response {
    let db = match open_db(&state.data_dir) {
        Ok(db) => db,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to open database"),
    };

    match crate::kanban::agent_workloads(&db) {
        Ok(workloads) => Json(workloads).into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("query error: {e}"),
        ),
    }
}
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    eprintln!("[legion] shutting down");
}
