use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_embed::Embed;
use tokio::signal;

use crate::channel::{ServeError, open_db, open_index as open_search_index};
use crate::db::ReflectionMeta;
use crate::error;
use crate::health::HealthSample;
use crate::signal as sig;
use crate::status;

// The dashboard frontend is built from app/ (vanilla-TS web components ->
// vite -> app/dist) and embedded here. build.rs guarantees app/dist exists
// so a `cargo build` without a prior `pnpm -C app build` still compiles
// (it embeds a "not built" placeholder). The legacy hand-written dashboard
// in static/ is retained in-tree during the migration but no longer served.
#[derive(Embed)]
#[folder = "app/dist/"]
struct StaticAssets;

#[derive(Clone)]
struct AppState {
    data_dir: PathBuf,
}

/// Resolve the daemon pidfile path. Lives under XDG_STATE_HOME (same root
/// as bypass.jsonl and index-logs/) so the daemon supervisor (#321)
/// running from a SessionStart hook can find it without per-host config.
pub fn daemon_pid_path() -> PathBuf {
    if let Ok(state) = std::env::var("XDG_STATE_HOME")
        && !state.is_empty()
    {
        return PathBuf::from(state).join("legion").join("daemon.pid");
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(".local/state/legion/daemon.pid");
    }
    std::env::temp_dir().join("legion-daemon.pid")
}

/// Write `pid` to a specific path, creating parent dirs as needed. The
/// path-injectable form is the test seam; the env-resolving caller is
/// `write_daemon_pidfile` below.
fn write_daemon_pidfile_at(path: &Path, pid: u32) {
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!(
            "[legion] WARNING: failed to create pidfile dir {}: {e}",
            parent.display()
        );
        return;
    }
    if let Err(e) = std::fs::write(path, pid.to_string()) {
        eprintln!(
            "[legion] WARNING: failed to write pidfile {}: {e}",
            path.display()
        );
    }
}

/// Write `pid` to the resolved daemon pidfile. Best-effort: a write
/// failure is logged but the server still starts -- a missing pidfile
/// means the supervisor falls back to "no pidfile -> probe /health
/// instead," the same path it takes on a cold-boot fresh install.
fn write_daemon_pidfile(pid: u32) {
    write_daemon_pidfile_at(&daemon_pid_path(), pid);
}

/// Best-effort pidfile removal at shutdown. Quietly tolerates a missing
/// file -- the daemon may have been started without write permissions to
/// the XDG dir, or the file may have been cleaned up out-of-band.
fn remove_daemon_pidfile() {
    let _ = std::fs::remove_file(daemon_pid_path());
}

pub fn run_server(port: u16, data_dir: PathBuf) -> error::Result<()> {
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| error::LegionError::Server(format!("failed to create runtime: {e}")))?;

    runtime.block_on(async {
        let state = AppState {
            data_dir: data_dir.clone(),
        };

        // One broadcast channel is shared by the schedule-firing task
        // (sender) and the channel SSE handler (subscribers), so an
        // in-process write wakes connected dashboards immediately.
        let (tx, _rx) = crate::channel::new_broadcast();

        // The shared endpoint contract (/health, /sse, /api/feed,
        // /api/tasks, /api/post) is owned by channel::router (#613) --
        // one implementation whether the daemon or `legion serve`
        // answers the port. This router holds only the serve-specific
        // surface: the embedded dashboard assets and the endpoints the
        // daemon does not need.
        let channel_state = crate::channel::ChannelState {
            data_dir: data_dir.clone(),
            tx: tx.clone(),
            started_at: chrono::Utc::now(),
            role: crate::channel::ServerRole::Serve,
        };

        let app = Router::new()
            .route("/", get(index_handler))
            .route("/api/agents", get(api_agents))
            .route("/api/stats", get(api_stats))
            .route("/api/signals", get(api_signals))
            .route("/api/status", get(api_status))
            .route("/api/needs", get(api_needs))
            .route("/api/done", post(api_done))
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
            .route("/api/telemetry/bypasses", get(api_telemetry_bypasses))
            .route("/api/kanban/{id}/move", post(api_kanban_move))
            .route("/api/kanban/workloads", get(api_kanban_workloads))
            .route("/{*path}", get(static_handler))
            .with_state(state)
            .merge(crate::channel::router(channel_state));

        let addr = format!("0.0.0.0:{port}");
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => return Err(bind_refusal(port, &data_dir, &addr, &e)),
        };

        eprintln!("[legion] dashboard at http://localhost:{port}");

        // Write the pidfile only after the port bind succeeded -- a
        // bind-failure path that wrote the pidfile would leave a stale
        // PID that the supervisor's "is this process alive" probe would
        // then have to disambiguate.
        write_daemon_pidfile(std::process::id());

        // One background task owns schedule firing (#613) -- previously it
        // ran inside the per-connection SSE stream body, so schedules only
        // fired while a dashboard was open and fired once per connected
        // client. Spawned only after the bind succeeded: a process that
        // failed to become the server must not produce side effects.
        let _firing = crate::channel::spawn_schedule_firing(data_dir.clone(), tx);

        let serve_result = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| error::LegionError::Server(format!("server error: {e}")));

        remove_daemon_pidfile();
        serve_result?;

        Ok(())
    })
}

/// Shape the error for a failed `legion serve` bind (#613, absorbed #601).
///
/// Port-ownership decision: the daemon owns the port while it runs.
/// `legion serve` never takes the port over; it refuses with a pointer,
/// and `legion daemon-stop` is the explicit takeover path. When the data
/// dir's daemon pidfile names a live process, the error says so -- lsof
/// confirmation (when available) upgrades "likely owns" to "owns" --
/// instead of leaving the operator to map a bare EADDRINUSE to the
/// daemon by hand, which is exactly how the wake-storm recovery went
/// sideways (recall 019eb03a).
fn bind_refusal(port: u16, data_dir: &Path, addr: &str, e: &std::io::Error) -> error::LegionError {
    if let Some(pid) = crate::daemon::live_daemon_pid(data_dir) {
        let qualifier = if crate::daemon::port_listener_pids(port).contains(&pid) {
            "owns"
        } else {
            "is running and likely owns"
        };
        return error::LegionError::Server(format!(
            "failed to bind {addr}: {e}. The legion daemon (pid {pid}) {qualifier} port {port} -- \
             it serves the channel API and fires schedules while it runs. Stop it first with \
             `legion daemon-stop`, or run the dashboard on another port with --port."
        ));
    }
    error::LegionError::Server(format!("failed to bind {addr}: {e}"))
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

/// GET /api/agents -- per-repo agent overview with unread counts.
///
/// Shares its builder with the SSE `agents` event (channel.rs) so push
/// and pull of the agent list cannot diverge (audit DC3).
async fn api_agents(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::channel::AgentInfo>>, ServeError> {
    let db = open_db(&state.data_dir)?;
    Ok(Json(crate::channel::build_agents(&db)?))
}

/// GET /api/stats -- per-repo dashboard stats (same data as agents minus unread).
async fn api_stats(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::db::DashboardRepoStats>>, ServeError> {
    let db = open_db(&state.data_dir)?;
    Ok(Json(db.get_dashboard_stats()?))
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
async fn api_signals(State(state): State<AppState>) -> Result<Json<Vec<SignalItem>>, ServeError> {
    let db = open_db(&state.data_dir)?;
    let posts = db.get_board_posts()?;

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

    Ok(Json(items))
}

/// Query parameters for GET /api/status.
#[derive(serde::Deserialize)]
struct StatusQuery {
    repo: String,
}

/// GET /api/status?repo=<name> -- agent status overview.
async fn api_status(
    State(state): State<AppState>,
    Query(params): Query<StatusQuery>,
) -> Result<Response, ServeError> {
    let repo = params.repo.trim();
    if repo.is_empty() {
        return Err(ServeError::BadRequest(
            "repo parameter is required".to_string(),
        ));
    }

    let db = open_db(&state.data_dir)?;
    let output =
        status::get_status(&db, repo).map_err(|e| ServeError::internal("status error", e))?;
    Ok(Json(output).into_response())
}

/// GET /api/needs?repo=<name> -- team help opportunities for an agent.
async fn api_needs(
    State(state): State<AppState>,
    Query(params): Query<StatusQuery>,
) -> Result<Response, ServeError> {
    let repo = params.repo.trim();
    if repo.is_empty() {
        return Err(ServeError::BadRequest(
            "repo parameter is required".to_string(),
        ));
    }

    let db = open_db(&state.data_dir)?;
    let items = status::get_needs(&db, repo).map_err(|e| ServeError::internal("needs error", e))?;
    Ok(Json(items).into_response())
}

/// Request body for POST /api/done.
#[derive(serde::Deserialize)]
struct DoneRequest {
    repo: String,
    text: String,
}

/// POST /api/done -- announce completed work and notify blocked agents.
async fn api_done(
    State(state): State<AppState>,
    Json(body): Json<DoneRequest>,
) -> Result<Json<status::DoneResult>, ServeError> {
    let repo = body.repo.trim();
    let text = body.text.trim();
    if repo.is_empty() || text.is_empty() {
        return Err(ServeError::BadRequest(
            "repo and text are required".to_string(),
        ));
    }

    let db = open_db(&state.data_dir)?;
    let index = open_search_index(&state.data_dir)?;

    // Both posts route through board::post_from_text_with_meta so the
    // write+index invariant lives in one place (#613). Consequence: an
    // index failure now fails the post (the announcement returns 500, a
    // notification does not count its agent as notified) instead of the
    // old silently-unsearchable best effort.
    let announcement = format!("{repo} completed: {text}");
    crate::board::post_from_text_with_meta(
        &db,
        &index,
        repo,
        &announcement,
        &ReflectionMeta::default(),
    )
    .map_err(|e| ServeError::internal("insert error", e))?;

    let blocked_agents = status::find_blocked_agents(&db, repo).unwrap_or_default();
    let mut notified: Vec<String> = Vec::new();
    for agent in &blocked_agents {
        let notify_text = format!(
            "@{agent} announce from {repo} -- {repo} completed: {text}. Your blocker may be cleared."
        );
        if crate::board::post_from_text_with_meta(
            &db,
            &index,
            repo,
            &notify_text,
            &ReflectionMeta::default(),
        )
        .is_ok()
        {
            notified.push(agent.clone());
        }
    }

    Ok(Json(status::DoneResult {
        announcement,
        notified,
    }))
}

/// POST /api/boost/:id -- boost a reflection's recall count.
async fn api_boost(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, ServeError> {
    let db = open_db(&state.data_dir)?;

    match db.boost_reflection(&id) {
        Ok(true) => Ok(Json(serde_json::json!({"ok": true}))),
        Ok(false) => Err(ServeError::NotFound("reflection not found".to_string())),
        Err(e) => Err(ServeError::internal("boost error", e)),
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
) -> Result<(StatusCode, Json<crate::task::Task>), ServeError> {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return Err(ServeError::BadRequest("text is required".to_string()));
    }
    if body.to.trim().is_empty() {
        return Err(ServeError::BadRequest("to is required".to_string()));
    }
    if !["low", "med", "high"].contains(&body.priority.as_str()) {
        return Err(ServeError::BadRequest(
            "priority must be low, med, or high".to_string(),
        ));
    }

    let db = open_db(&state.data_dir)?;

    let context_ref = body.context.as_deref().filter(|c| !c.trim().is_empty());

    let id = db
        .insert_task(
            &body.from,
            body.to.trim(),
            &text,
            context_ref,
            &body.priority,
        )
        .map_err(|e| ServeError::internal("insert error", e))?;

    let task = match db.get_task_by_id(&id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Err(ServeError::Internal(
                "task created but not found".to_string(),
            ));
        }
        Err(e) => return Err(ServeError::internal("fetch error", e)),
    };

    Ok((StatusCode::CREATED, Json(task)))
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
) -> Result<Json<serde_json::Value>, ServeError> {
    let db = open_db(&state.data_dir)?;
    // Task-domain errors (unknown id, illegal transition) render as 400
    // with the bare error message -- a deliberate divergence from the
    // 500 "query error" convention, preserved from the original handlers.
    crate::task::accept_task(&db, &id).map_err(|e| ServeError::BadRequest(e.to_string()))?;
    Ok(Json(serde_json::json!({"ok": true})))
}

/// POST /api/tasks/:id/done -- complete an accepted task.
async fn api_task_done(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<TaskTransitionBody>>,
) -> Result<Json<serde_json::Value>, ServeError> {
    let db = open_db(&state.data_dir)?;
    let note = body.and_then(|b| b.note.clone());
    crate::task::complete_task(&db, &id, note.as_deref())
        .map_err(|e| ServeError::BadRequest(e.to_string()))?;
    Ok(Json(serde_json::json!({"ok": true})))
}

/// POST /api/tasks/:id/block -- block an accepted task.
async fn api_task_block(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<TaskTransitionBody>>,
) -> Result<Json<serde_json::Value>, ServeError> {
    let db = open_db(&state.data_dir)?;
    let reason = body.and_then(|b| b.note.clone());
    crate::task::block_task(&db, &id, reason.as_deref())
        .map_err(|e| ServeError::BadRequest(e.to_string()))?;
    Ok(Json(serde_json::json!({"ok": true})))
}

/// POST /api/tasks/:id/unblock -- unblock a blocked task.
async fn api_task_unblock(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, ServeError> {
    let db = open_db(&state.data_dir)?;
    crate::task::unblock_task(&db, &id).map_err(|e| ServeError::BadRequest(e.to_string()))?;
    Ok(Json(serde_json::json!({"ok": true})))
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
async fn api_chat(
    State(state): State<AppState>,
    Query(params): Query<ChatQuery>,
) -> Result<Json<Vec<ChatMessage>>, ServeError> {
    let agent = params.agent.trim().to_lowercase();
    if agent.is_empty() {
        return Err(ServeError::BadRequest(
            "agent parameter is required".to_string(),
        ));
    }

    let db = open_db(&state.data_dir)?;
    let posts = db.get_board_posts()?;

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

    Ok(Json(messages))
}

/// GET /api/schedules -- list all schedules.
async fn api_schedules(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::db::Schedule>>, ServeError> {
    let db = open_db(&state.data_dir)?;
    Ok(Json(db.list_schedules()?))
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
) -> Result<Json<serde_json::Value>, ServeError> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ServeError::BadRequest("name is required".to_string()));
    }
    let command = body.command.trim();
    if command.is_empty() {
        return Err(ServeError::BadRequest("command is required".to_string()));
    }

    let db = open_db(&state.data_dir)?;

    // Insert failures are caller errors (bad cron expression, bad time
    // window), so they render as 400 -- preserved from the original handler.
    let id = db
        .insert_schedule(
            name,
            &body.cron,
            command,
            &body.repo,
            body.active_start.as_deref(),
            body.active_end.as_deref(),
        )
        .map_err(|e| ServeError::BadRequest(format!("create error: {e}")))?;

    Ok(Json(serde_json::json!({"ok": true, "id": id})))
}

/// POST /api/schedules/:id/toggle -- toggle a schedule's enabled state.
async fn api_toggle_schedule(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, ServeError> {
    let db = open_db(&state.data_dir)?;

    // Read current state to toggle it
    let schedules = db.list_schedules()?;

    let current = schedules
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| ServeError::NotFound("schedule not found".to_string()))?;

    let new_enabled = !current.enabled;
    match db.toggle_schedule(&id, new_enabled) {
        Ok(true) => Ok(Json(
            serde_json::json!({"ok": true, "enabled": new_enabled}),
        )),
        Ok(false) => Err(ServeError::NotFound("schedule not found".to_string())),
        Err(e) => Err(ServeError::internal("toggle error", e)),
    }
}

/// Query parameters for GET /api/health.
#[derive(serde::Deserialize)]
struct HealthQuery {
    /// Minutes of history to return (default 60).
    minutes: Option<u64>,
}

/// GET /api/health -- latest health samples per host + recent history.
async fn api_health(
    State(state): State<AppState>,
    Query(params): Query<HealthQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ServeError> {
    let db = open_db(&state.data_dir)?;

    let minutes = params.minutes.unwrap_or(60);
    let since = chrono::Utc::now() - chrono::Duration::minutes(minutes as i64);
    let since_str = since.to_rfc3339();

    let samples: Vec<HealthSample> = db
        .get_health_all_hosts(&since_str)
        .map_err(|e| ServeError::internal("health query error", e))?;

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

    Ok(Json(hosts))
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
) -> Result<Json<Vec<HealthSample>>, ServeError> {
    let db = open_db(&state.data_dir)?;

    let minutes = params.minutes.unwrap_or(60);
    let since = chrono::Utc::now() - chrono::Duration::minutes(minutes as i64);
    let since_str = since.to_rfc3339();

    let samples = db
        .get_health_history(&params.hostname, &since_str)
        .map_err(|e| ServeError::internal("health history error", e))?;
    Ok(Json(samples))
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
async fn api_search(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ServeError> {
    let q = params.q.trim();
    if q.is_empty() {
        return Err(ServeError::BadRequest(
            "q parameter is required".to_string(),
        ));
    }

    let db = open_db(&state.data_dir)?;
    // Preserved wire message: this endpoint reported index-open failure as
    // "search index error", not the shared "failed to open search index".
    let index = open_search_index(&state.data_dir)
        .map_err(|_| ServeError::Internal("search index error".to_string()))?;

    let limit = params.limit.unwrap_or(10).min(50);

    let hits = match &params.repo {
        Some(repo) => index.search(repo, q, limit),
        None => index.search_all(q, limit),
    };
    let hits = hits.map_err(|e| ServeError::internal("search error", e))?;

    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    let reflections = db
        .get_reflections_by_ids(&ids)
        .map_err(|e| ServeError::internal("reflection lookup error", e))?;

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

    Ok(Json(results))
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
async fn api_audit(
    State(state): State<AppState>,
    Query(params): Query<AuditQuery>,
) -> Result<Response, ServeError> {
    let db = open_db(&state.data_dir)?;

    let limit = params.limit.unwrap_or(50).min(200);

    let entries = db
        .query_audit_log(params.agent.as_deref(), params.action.as_deref(), limit)
        .map_err(|e| ServeError::internal("audit query error", e))?;
    Ok(Json(entries).into_response())
}

/// GET /api/kanban -- all kanban cards for the board view.
async fn api_kanban(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::kanban::Card>>, ServeError> {
    let db = open_db(&state.data_dir)?;
    Ok(Json(crate::kanban::board_cards(&db)?))
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
) -> Result<Json<serde_json::Value>, ServeError> {
    let db = open_db(&state.data_dir)?;

    let new_status = body
        .status
        .parse::<crate::kanban::CardStatus>()
        .map_err(|e| ServeError::BadRequest(format!("invalid status: {e}")))?;

    crate::kanban::force_move(&db, &id, new_status, body.sort_order)
        .map_err(|e| ServeError::internal("move failed", e))?;
    Ok(Json(serde_json::json!({"ok": true})))
}

/// GET /api/kanban/workloads -- per-agent workload summary for the agent strip.
async fn api_kanban_workloads(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::kanban::AgentWorkload>>, ServeError> {
    let db = open_db(&state.data_dir)?;
    Ok(Json(crate::kanban::agent_workloads(&db)?))
}

/// Query parameters for GET /api/telemetry/bypasses.
#[derive(serde::Deserialize)]
struct TelemetryQuery {
    /// Duration string (e.g. `24h`, `7d`); rows older than this are dropped.
    since: Option<String>,
    /// Restrict to a single repo.
    repo: Option<String>,
    /// Top N rows by count. Default 20; 0 means all.
    top: Option<usize>,
}

/// GET /api/telemetry/bypasses -- summary of bypass volume by
/// (tool, repo, pattern). Feeds the dashboard surface and the
/// uncertainty engine consumer (#354). Reads bypass.jsonl directly;
/// the file is append-only, so even if the legion DB is unavailable
/// this endpoint still works.
async fn api_telemetry_bypasses(
    Query(params): Query<TelemetryQuery>,
) -> Result<Response, ServeError> {
    let since_dur = match params.since.as_deref() {
        Some(s) => Some(
            crate::telemetry::parse_duration(s)
                .map_err(|e| ServeError::BadRequest(format!("invalid since: {e}")))?,
        ),
        None => None,
    };
    let rows = crate::telemetry::list_bypasses(since_dur, params.repo.as_deref())
        .map_err(|e| ServeError::internal("read bypass log", e))?;
    let summary = crate::telemetry::summarize(&rows, params.top.unwrap_or(20));
    Ok(Json(summary).into_response())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_pid_path_honors_xdg_state_home() {
        // SAFETY: test mutates process env. Other tests in this module
        // do not read XDG_STATE_HOME, so isolation by `cargo test`
        // default parallelism is fine.
        let saved = std::env::var("XDG_STATE_HOME").ok();
        unsafe {
            std::env::set_var("XDG_STATE_HOME", "/tmp/legion-xdg-test");
        }
        let p = daemon_pid_path();
        assert_eq!(p, PathBuf::from("/tmp/legion-xdg-test/legion/daemon.pid"));
        unsafe {
            match saved {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }

    #[test]
    fn pidfile_write_at_roundtrip() {
        // Path-injectable form -- no env mutation, safe under parallel
        // test execution.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/legion/daemon.pid");
        write_daemon_pidfile_at(&path, 12345);
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "12345");
        // Idempotent overwrite.
        write_daemon_pidfile_at(&path, 67890);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "67890");
    }

    // Unix-only: liveness goes through watch::process_alive (`kill`-based,
    // always false on other platforms), so on Windows live_daemon_pid is
    // None by construction and the bare-error path below is the correct
    // behavior -- pinned cross-platform by
    // serve_bind_failure_without_daemon_stays_bare.
    #[cfg(unix)]
    #[test]
    fn serve_refuses_port_held_by_live_daemon_with_pointer() {
        let dir = tempfile::tempdir().unwrap();
        // A "live daemon": this test process itself, recorded in the
        // pidfile path the daemon writes (data_dir/daemon.pid).
        std::fs::write(
            dir.path().join("daemon.pid"),
            std::process::id().to_string(),
        )
        .unwrap();
        // Hold a port the way the daemon would.
        let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();

        let err = run_server(port, dir.path().to_path_buf()).expect_err("bind must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("legion daemon"),
            "error must name the daemon as the holder: {msg}"
        );
        assert!(
            msg.contains("daemon-stop"),
            "error must point at the takeover path: {msg}"
        );
        assert!(
            msg.contains("--port"),
            "error must offer the alternate-port escape: {msg}"
        );
    }

    #[test]
    fn serve_bind_failure_without_daemon_stays_bare() {
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();

        let err = run_server(port, dir.path().to_path_buf()).expect_err("bind must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("failed to bind"),
            "bare bind error expected: {msg}"
        );
        assert!(
            !msg.contains("legion daemon (pid"),
            "no daemon attribution without a live pidfile: {msg}"
        );
    }
}
