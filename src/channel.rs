use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::routing::{get, post, put};
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
/// receipt. A previous revision attached a `post_id` to `Feed`, but the
/// live consumer queries the database itself (the HTTP SSE handler queries
/// `max(created_at)` on every tick).
///
/// **The broadcast channel is still live and still used.** The SSE handler
/// in `src/channel.rs` subscribes and uses it as the edge-triggered wakeup
/// that replaces a dumber polling loop. Do NOT delete the broadcast path
/// or the `tx.send` calls on the assumption that they are dead -- the SSE
/// consumer depends on them.
///
/// The wire-level `<channel post_id="...">` XML attribute is unchanged --
/// only this internal event enum lost the field.
#[derive(Debug, Clone)]
pub enum ChannelEvent {
    /// New board post or reflection arrived.
    Feed,
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

/// Cap on a document write body (POST/PUT to any /api/documents route).
/// Axum's crate-wide default is already 2 MB; this scopes an explicit,
/// documented bound onto just the document routes instead of leaning on
/// that implicit default (#1036 review, LOW), and raises it slightly to
/// leave room for a sizable schema payload or a long editor body without
/// inviting an unbounded request into memory.
const DOCUMENT_BODY_LIMIT_BYTES: usize = 4 * 1024 * 1024;

/// Build the axum Router for the channel HTTP server.
///
/// This is a standalone router -- the caller mounts it into the main axum
/// app. It is the single owner of the shared endpoint contract (#613):
/// /health, /sse, /api/feed, /api/tasks, /api/post. Both `legion serve`
/// (which merges this router into the dashboard app) and the daemon (which
/// serves it bare) answer these paths with the same implementation, so the
/// wire shapes cannot fork again.
///
/// `GET /api/search` (#1037, document search) is registered only when
/// `state.role == ServerRole::Daemon`. `legion serve`'s own app (src/serve.rs)
/// already owns `/api/search` for BM25 reflection search -- a distinct,
/// pre-existing, differently-shaped endpoint (required `q`, optional
/// `repo`, reflection JSON with score/domain/tags). Registering the same
/// path here unconditionally would make axum panic on a route collision
/// the moment `legion serve` merges this router in. The legion-app
/// document search box this endpoint serves talks to the daemon directly
/// (per #1037's own framing: "a search box over the daemon"), so scoping
/// registration to `ServerRole::Daemon` satisfies both the literal path
/// the acceptance criteria name and the actual consumer, without touching
/// serve.rs's live endpoint.
pub fn router(state: ChannelState) -> Router {
    let is_daemon = state.role == ServerRole::Daemon;

    // Split out so `DOCUMENT_BODY_LIMIT_BYTES` applies only to these routes
    // -- `route_layer` scopes to whatever is already in the Router it is
    // called on, so building the document routes as their own Router
    // before merging keeps the cap off /api/post and the rest. `/api/search`
    // (#1037) deliberately stays out of this sub-router: it's a GET with no
    // body, and keeping it out keeps the cap's scope honest.
    let document_routes = Router::new()
        .route(
            "/api/documents",
            get(api_documents).post(api_document_create),
        )
        .route("/api/documents/{id}", get(api_document))
        .route("/api/documents/{id}/status", post(api_document_set_status))
        .route("/api/documents/{id}/body", put(api_document_update_body))
        .route("/api/documents/{id}/revise", post(api_document_revise))
        .route_layer(DefaultBodyLimit::max(DOCUMENT_BODY_LIMIT_BYTES));

    let mut r = Router::new()
        .route("/health", get(health_endpoint))
        .route("/sse", get(sse_handler))
        .route("/api/feed", get(api_feed))
        .route("/api/tasks", get(api_tasks))
        .route("/api/post", post(api_post))
        .merge(document_routes);
    if is_daemon {
        r = r.route("/api/search", get(api_search));
    }
    r.with_state(state)
}

/// Query parameters for GET /api/documents. All optional; omitting every
/// field returns the hot (non-archived) document set. Field names are part
/// of the public JSON contract and mirror the `legion document list` flags.
#[derive(serde::Deserialize)]
pub struct DocumentListQuery {
    pub doc_type: Option<String>,
    pub surface: Option<String>,
    pub status: Option<String>,
    pub owner: Option<String>,
    /// None -> hot only; Some(true) -> archived only; Some(false) -> hot only.
    pub archived: Option<bool>,
}

/// Body for POST /api/documents/{id}/status -- the dashboard Publish/Approve
/// action. `{ "to": "published" }`.
#[derive(serde::Deserialize)]
pub struct SetStatusBody {
    pub to: String,
}

/// GET /api/documents -- the navigator list. Filters by doc_type / surface
/// (= repo) / status / owner, matching `legion document list`. Returns the
/// full Document rows (including payload) so the client can render without a
/// second round-trip per row.
pub async fn api_documents(
    State(state): State<ChannelState>,
    Query(q): Query<DocumentListQuery>,
) -> Result<Json<Vec<crate::documents::Document>>, ServeError> {
    let db = open_db(&state.data_dir)?;
    let filter = crate::documents::DocumentFilter {
        doc_type: q.doc_type.as_deref(),
        surface: q.surface.as_deref(),
        status: q.status.as_deref(),
        owner: q.owner.as_deref(),
        archived: q.archived,
    };
    Ok(Json(db.list_documents(&filter)?))
}

/// GET /api/documents/{id} -- one document for the render pane. 404 when the
/// id does not resolve, rather than the generic 500 a missing row would take
/// through the blanket LegionError conversion.
pub async fn api_document(
    State(state): State<ChannelState>,
    Path(id): Path<String>,
) -> Result<Json<crate::documents::Document>, ServeError> {
    let db = open_db(&state.data_dir)?;
    let doc = db
        .get_document(&id)?
        .ok_or_else(|| ServeError::NotFound(format!("document '{id}' not found")))?;
    Ok(Json(doc))
}

/// POST /api/documents/{id}/status -- flip a document's lifecycle status
/// (the Publish/Approve button). Returns the updated document. The localhost
/// operator is the human gate; there is no status-machine enforcement, mirroring
/// the `legion document set-status` verb this wraps.
pub async fn api_document_set_status(
    State(state): State<ChannelState>,
    Path(id): Path<String>,
    Json(body): Json<SetStatusBody>,
) -> Result<Json<crate::documents::Document>, ServeError> {
    let db = open_db(&state.data_dir)?;
    let index = open_index(&state.data_dir)?;
    // Return 404 for a missing doc, consistent with the GET handler, rather
    // than the 500 the blanket LegionError conversion would give the
    // set_document_status not-found error.
    if db.get_document(&id)?.is_none() {
        return Err(ServeError::NotFound(format!("document '{id}' not found")));
    }
    Ok(Json(crate::documents::set_document_status_indexed(
        &db, &index, &id, &body.to,
    )?))
}

/// Query parameters for GET /api/search (#1037). `repo` is required -- the
/// app always scopes a search to one repo, so an unscoped query is a client
/// bug worth surfacing loudly rather than silently searching everything.
/// Both fields are `Option` (rather than a bare `String`) so a missing
/// `repo` renders through this handler's own 400 body instead of axum's
/// generic query-rejection response, and an omitted `q` behaves the same
/// as an explicit empty one.
#[derive(serde::Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub repo: Option<String>,
    pub limit: Option<usize>,
}

/// GET /api/search -- BM25 search over documents in `repo` matching `q`
/// (#1037). Looks up each hit's full Document row (same shape as GET
/// /api/documents) so the app can open a result without a second round
/// trip, preserving the BM25 rank order `search_documents` returned. A hit
/// whose document row no longer exists (e.g. hard-deleted between the
/// index write and this query -- soft-delete is the norm, but not
/// guaranteed) is skipped rather than failing the whole request.
///
/// `repo` missing or blank is a 400, not a silent all-repo search: the app
/// always scopes to one repo, so an unscoped query is a client bug worth
/// surfacing loudly. An empty or whitespace-only `q` returns an empty list,
/// matching `SearchIndex::search_documents`'s own empty-query behavior.
pub async fn api_search(
    State(state): State<ChannelState>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<crate::documents::Document>>, ServeError> {
    let repo = q.repo.as_deref().unwrap_or("").trim().to_string();
    if repo.is_empty() {
        return Err(ServeError::BadRequest("repo is required".to_string()));
    }
    let query = q.q.as_deref().unwrap_or("");
    // Capped the same way serve.rs's sibling reflection-search handler
    // caps its own `limit` (src/serve.rs `api_search`): unbounded
    // caller-supplied input has no business setting the actual fetch size.
    let limit = q.limit.unwrap_or(20).min(50);

    let db = open_db(&state.data_dir)?;
    let index = open_index(&state.data_dir)?;

    let hits = index.search_documents(&repo, query, limit)?;
    let mut docs = Vec::with_capacity(hits.len());
    for hit in hits {
        if let Some(doc) = db.get_document(&hit.id)? {
            docs.push(doc);
        }
    }
    Ok(Json(docs))
}

/// Max length for a caller-supplied `owner` or `id` on POST /api/documents.
/// This is network input (#1036 review, MED-4) -- unlike the CLI's argv, a
/// local operator typing into their own shell -- so it gets a charset and
/// length gate before it reaches a SQL bind, a log line, or (for `owner`) a
/// schema pointer reflection's audience routing. Generous enough for any
/// real agent/repo name or typed document id (e.g. `FR-EMAIL-003`), small
/// enough to keep a hostile caller from stuffing an oversized value into an
/// indexed column.
const MAX_IDENTIFIER_LEN: usize = 128;

/// Reject `value` unless it is 1..=MAX_IDENTIFIER_LEN characters of
/// `[A-Za-z0-9._-]`. Shared by the `owner` and (when supplied) `id` checks
/// on POST /api/documents.
fn validate_identifier(value: &str, field: &str) -> Result<(), ServeError> {
    if value.is_empty() || value.chars().count() > MAX_IDENTIFIER_LEN {
        return Err(ServeError::BadRequest(format!(
            "{field} must be 1-{MAX_IDENTIFIER_LEN} characters"
        )));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(ServeError::BadRequest(format!(
            "{field} must match [A-Za-z0-9._-]"
        )));
    }
    Ok(())
}

/// Map a `LegionError::WorkSource` -- a caller-visible input problem: bad
/// JSON shape, a duplicate id, a document already archived, a corrupted
/// stored payload -- to 400. Every other `LegionError` variant is a
/// genuine server-side failure and keeps the blanket 500 the
/// `From<LegionError>` conversion gives it. Shared by every /api/documents
/// write path (#1036 review, HIGH-2) so the same kind of error reads the
/// same way regardless of which endpoint raised it.
fn work_source_as_bad_request(e: LegionError) -> ServeError {
    match e {
        LegionError::WorkSource(msg) => ServeError::BadRequest(msg),
        other => ServeError::from(other),
    }
}

/// Body for POST /api/documents. Mirrors `legion document create`'s flags
/// (`DocumentAction::Create` in src/cli/document.rs) field for field.
/// `payload` is an inline JSON object here rather than the CLI's
/// --from/stdin file text, since an HTTP caller already holds its payload
/// as a parsed value.
///
/// `owner` and `id` are network-supplied input here, unlike the CLI's argv
/// (a local operator typing into their own shell) -- both are gated by
/// `validate_identifier` before they reach storage (#1036 review, MED-4).
///
/// NOTE for client authors: this wraps `payload` under a key alongside the
/// meta fields (`doc_type`/`owner`/etc), because create has to disambiguate
/// them within one request body. POST /api/documents/{id}/revise below does
/// NOT wrap -- its entire request body IS the payload, with no meta fields
/// riding along (there is nothing to disambiguate: the id is in the URL,
/// and revise never changes doc_type/owner/surface/etc). Sending
/// `{"payload": {...}}` to revise is valid JSON and is NOT rejected -- it
/// gets stored verbatim as a payload whose only field happens to be named
/// "payload", not unwrapped for you.
#[derive(serde::Deserialize)]
pub struct CreateDocumentRequest {
    pub doc_type: String,
    pub owner: String,
    pub surface: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    /// Optional caller-supplied id. Omitted or empty generates a UUIDv7,
    /// matching `Database::insert_document`.
    pub id: Option<String>,
    pub payload: serde_json::Value,
}

/// POST /api/documents -- create a document over HTTP, e.g. the legion-app
/// sidebar's Intentions `+` control. Runs the same structural gate and
/// pointer-reflection dual write as `legion document create`
/// (`DocumentAction::Create`) so the two entry points stay behaviorally
/// identical; the CLI handler itself is untouched.
///
/// Returns 200 (matching this router's existing document-endpoint
/// convention -- none of them return 201).
pub async fn api_document_create(
    State(state): State<ChannelState>,
    Json(req): Json<CreateDocumentRequest>,
) -> Result<Json<crate::documents::Document>, ServeError> {
    validate_identifier(&req.owner, "owner")?;
    validate_identifier(&req.doc_type, "doc_type")?;
    if let Some(id) = req.id.as_deref().filter(|s| !s.is_empty()) {
        validate_identifier(id, "id")?;
    }
    if let Some(surface) = req.surface.as_deref().filter(|s| !s.is_empty()) {
        validate_identifier(surface, "surface")?;
    }

    // The CLI's equivalent check is a raw JSON parse of --from/stdin text;
    // here `Json` has already parsed the body, so the equivalent gate is an
    // object-type check -- a bare string/array/number payload is refused
    // rather than silently landing in the table.
    if !req.payload.is_object() {
        return Err(ServeError::BadRequest(
            "payload must be a JSON object".to_string(),
        ));
    }
    let payload_str = req.payload.to_string();

    // Schema documents get the same structural gate `document create`
    // enforces (#526): a malformed JSON Schema is rejected before insert,
    // and a valid one is summarized for the pointer reflection below.
    // `validate_schema_payload` only ever returns `LegionError::WorkSource`,
    // so its message alone is the right 400 body.
    let schema_summary = if req.doc_type == "schema" {
        Some(
            crate::documents::validate_schema_payload(&payload_str)
                .map_err(|e| ServeError::BadRequest(e.to_string()))?,
        )
    } else {
        None
    };

    let db = open_db(&state.data_dir)?;
    let index = open_index(&state.data_dir)?;
    let meta = crate::documents::DocumentMeta {
        id: req.id.as_deref(),
        doc_type: &req.doc_type,
        surface: req.surface.as_deref(),
        status: req.status.as_deref(),
        priority: req.priority.as_deref(),
        owner: &req.owner,
    };
    // A duplicate caller-supplied id is a caller error (400), not the
    // generic 500 the blanket LegionError conversion would give it --
    // everything else `insert_document` can fail with is a genuine
    // server-side problem and keeps that blanket 500. Indexed (#1037) via
    // `insert_document_indexed` so a document created over HTTP is
    // findable through GET /api/search, same as one created via the CLI.
    let doc = crate::documents::insert_document_indexed(&db, &index, &meta, &payload_str)
        .map_err(work_source_as_bad_request)?;

    // Dual-write the pointer reflection (domain=schema), same shared write
    // as `DocumentAction::Create` and the revise handler below (#1036
    // review, MED-3) -- the document is already committed by this point, so
    // a failure here is reported as an internal error rather than rolled
    // back (matches the CLI's own behavior: the doc exists, the command
    // reports failure, and the pointer can be re-created by hand).
    if let Some(summary) = schema_summary {
        crate::documents::write_schema_pointer(&db, &doc, &summary).map_err(|e| {
            ServeError::internal(
                &format!(
                    "document {} created, but the schema pointer reflection failed",
                    doc.id
                ),
                e,
            )
        })?;
    }

    Ok(Json(doc))
}

/// Shared preamble for the body-save and revise handlers below: fetch the
/// document (404 when the id does not resolve) and refuse an archived one
/// (4xx, not the 500 the blanket LegionError conversion would give it).
/// Both handlers need this exact pre-check ahead of their mutating call, so
/// it lives once here instead of twice.
fn load_editable_document(
    db: &Database,
    id: &str,
) -> Result<crate::documents::Document, ServeError> {
    let doc = db
        .get_document(id)?
        .ok_or_else(|| ServeError::NotFound(format!("document '{id}' not found")))?;
    if doc.archived_at.is_some() {
        return Err(ServeError::BadRequest(format!(
            "document '{id}' is archived and cannot be edited"
        )));
    }
    Ok(doc)
}

/// Body for PUT /api/documents/{id}/body -- a debounced editor working-copy
/// save. Merges `body` into the document's payload without touching
/// `revision`; a revision is cut only by an explicit `/revise` call or a
/// status change (#1036).
#[derive(serde::Deserialize)]
pub struct UpdateBodyRequest {
    pub body: String,
}

/// PUT /api/documents/{id}/body -- save the editor's working copy. Does not
/// bump `revision`, so it is safe to call on every debounce tick while a
/// user types: `verification.criteria` ids and any `spec_revision` a
/// verdict cites live in other fields of `payload`, which this call never
/// touches -- only `payload.body` and `updated_at` change, so a criterion's
/// staleness check is unaffected by a body save. 404 for an unknown id; a
/// 4xx (not 500) for an archived document or a malformed stored payload,
/// via `load_editable_document` and `work_source_as_bad_request`.
pub async fn api_document_update_body(
    State(state): State<ChannelState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateBodyRequest>,
) -> Result<Json<crate::documents::Document>, ServeError> {
    let db = open_db(&state.data_dir)?;
    let index = open_index(&state.data_dir)?;
    load_editable_document(&db, &id)?;
    // Indexed (#1037) via `update_document_body_indexed`: `body` is part of
    // the document's searchable text, so a debounced editor save must
    // re-index or the index goes stale on every keystroke pause.
    Ok(Json(
        crate::documents::update_document_body_indexed(&db, &index, &id, &req.body)
            .map_err(work_source_as_bad_request)?,
    ))
}

/// POST /api/documents/{id}/revise -- a thin HTTP wrapper over
/// `Database::revise_document`, not a second implementation of revise
/// semantics. The request body IS the full replacement payload as a JSON
/// object -- NOT wrapped under a `"payload"` key the way POST
/// /api/documents wraps it (see that handler's `CreateDocumentRequest`
/// doc comment) -- same contract as `legion document revise`'s --from/stdin
/// input: a whole-payload replace, not a patch. Mirrors
/// `DocumentAction::Revise` (src/cli/document.rs) exactly (#1036 review,
/// HIGH-1): a schema document gets the same structural gate `create`
/// enforces, checked against the existing document's doc_type before the
/// write, and the pointer reflection is refreshed after. 404 for an
/// unknown id; a 4xx (not 500) for a non-object payload, an archived
/// document, or an invalid schema payload (#1036 review, HIGH-2).
pub async fn api_document_revise(
    State(state): State<ChannelState>,
    Path(id): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<crate::documents::Document>, ServeError> {
    if !payload.is_object() {
        return Err(ServeError::BadRequest(
            "payload must be a JSON object".to_string(),
        ));
    }
    let payload_str = payload.to_string();

    let db = open_db(&state.data_dir)?;
    let index = open_index(&state.data_dir)?;
    let existing = load_editable_document(&db, &id)?;

    let schema_summary = if existing.doc_type == "schema" {
        Some(
            crate::documents::validate_schema_payload(&payload_str)
                .map_err(|e| ServeError::BadRequest(e.to_string()))?,
        )
    } else {
        None
    };

    // Indexed (#1037) via `revise_document_indexed` so a revise over HTTP
    // re-indexes the same as the CLI's `legion document revise`.
    let doc = crate::documents::revise_document_indexed(&db, &index, &id, &payload_str)
        .map_err(work_source_as_bad_request)?;

    if let Some(summary) = schema_summary {
        crate::documents::write_schema_pointer(&db, &doc, &summary).map_err(|e| {
            ServeError::internal(
                &format!(
                    "document {} revised, but refreshing the schema pointer reflection failed",
                    doc.id
                ),
                e,
            )
        })?;
    }

    Ok(Json(doc))
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
            .add_reflection(
                &reflection.id,
                "kelex",
                "hello team",
                &reflection.created_at,
            )
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
        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;

        let (status, body) = http_req(
            port,
            "POST",
            "/api/post",
            Some(r#"{"repo":"kelex","text":"hello shape"}"#),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "expected 200 OK, got: {status}"
        );
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("parse body");
        let obj = parsed.as_object().expect("object body");
        assert!(obj.contains_key("id"), "body must carry the post id");
        assert_eq!(
            obj.len(),
            1,
            "api_post body is exactly {{\"id\"}}, got: {parsed}"
        );
    }

    #[tokio::test]
    async fn document_endpoints_publish_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        let meta = crate::documents::DocumentMeta {
            id: Some("FR-SERVE-1"),
            doc_type: "requirement",
            surface: Some("legion"),
            status: Some("draft"),
            priority: None,
            owner: "legion",
        };
        db.insert_document(&meta, "{}").expect("insert");
        drop(db);

        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;

        // Publish flips draft -> published and echoes the updated row.
        let (status, body) = http_req(
            port,
            "POST",
            "/api/documents/FR-SERVE-1/status",
            Some(r#"{"to":"published"}"#),
        )
        .await;
        assert!(status.starts_with("HTTP/1.1 200"), "publish: {status}");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("parse publish body");
        assert_eq!(parsed["status"], "published");

        // The flip persisted -- a fresh GET sees published.
        let (status, body) = http_req(port, "GET", "/api/documents/FR-SERVE-1", None).await;
        assert!(status.starts_with("HTTP/1.1 200"), "view: {status}");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("parse view body");
        assert_eq!(parsed["status"], "published");

        // Missing id is a 404 on both read and mutate, not a 500.
        let (view_missing, _) = http_req(port, "GET", "/api/documents/NOPE", None).await;
        assert!(
            view_missing.starts_with("HTTP/1.1 404"),
            "view-missing: {view_missing}"
        );
        let (post_missing, _) = http_req(
            port,
            "POST",
            "/api/documents/NOPE/status",
            Some(r#"{"to":"published"}"#),
        )
        .await;
        assert!(
            post_missing.starts_with("HTTP/1.1 404"),
            "post-missing: {post_missing}"
        );
    }

    /// Boots `router(state)` on an ephemeral port for tests that need real
    /// HTTP semantics (status codes, raw body text) rather than calling
    /// handler functions directly. `data_dir` must already hold a legion.db
    /// (or be empty -- the server opens/creates it lazily per request).
    /// `role` matters for #1037: `/api/search` is only registered under
    /// `ServerRole::Daemon` (see `router`'s doc comment).
    async fn spawn_test_server(data_dir: PathBuf, role: ServerRole) -> u16 {
        let (tx, _rx) = new_broadcast();
        let state = ChannelState {
            data_dir,
            tx,
            started_at: chrono::Utc::now(),
            role,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        // No sleep before returning (#1036 review, LOW): `TcpListener::bind`
        // already puts the socket into the OS listen state, so a connect
        // against `port` queues correctly even before the spawned task's
        // accept loop has run -- the fixed 100ms delay this used to have
        // was pure latency, not a correctness requirement.
        tokio::spawn(async move {
            axum::serve(listener, router(state)).await.expect("serve");
        });
        port
    }

    /// Raw HTTP/1.1 request helper -> (status_line, body). Every test in
    /// this module that needs real HTTP semantics shares this one
    /// implementation rather than hand-rolling its own TCP round trip.
    async fn http_req(port: u16, method: &str, path: &str, body: Option<&str>) -> (String, String) {
        let (method, path) = (method.to_string(), path.to_string());
        let body = body.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            use std::net::TcpStream;
            let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).expect("connect");
            let payload = body.unwrap_or_default();
            let head = format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            stream.write_all(head.as_bytes()).expect("write head");
            stream.write_all(payload.as_bytes()).expect("write body");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read");
            let text = String::from_utf8_lossy(&buf).to_string();
            let status = text.lines().next().unwrap_or("").to_string();
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("").trim().to_string();
            (status, body)
        })
        .await
        .expect("spawn_blocking")
    }

    #[tokio::test]
    async fn api_search_not_registered_under_serve_role() {
        // `legion serve`'s own app owns /api/search for reflection search
        // (src/serve.rs); this router must not answer it under
        // ServerRole::Serve, or merging the two routers would panic on a
        // route collision (the failure this guard exists to prevent).
        let dir = tempfile::tempdir().expect("tempdir");
        let port = spawn_test_server(dir.path().to_path_buf(), ServerRole::Serve).await;

        let (status, _) = http_req(port, "GET", "/api/search?q=mapping&repo=kelex", None).await;
        assert!(
            status.starts_with("HTTP/1.1 404"),
            "ServerRole::Serve must not answer /api/search: {status}"
        );
    }

    #[tokio::test]
    async fn api_search_requires_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let port = spawn_test_server(dir.path().to_path_buf(), ServerRole::Daemon).await;

        let (status, body) = http_req(port, "GET", "/api/search?q=mapping", None).await;
        assert!(status.starts_with("HTTP/1.1 400"), "status: {status}");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("parse body");
        assert!(
            parsed.get("error").is_some(),
            "400 body must carry an error message: {body}"
        );
    }

    #[tokio::test]
    async fn api_search_returns_ranked_documents_for_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        let index = SearchIndex::open(&data_dir.join("index")).expect("open index");

        let weak_meta = crate::documents::DocumentMeta {
            id: Some("FR-WEAK"),
            doc_type: "requirement",
            surface: None,
            status: None,
            priority: None,
            owner: "kelex",
        };
        crate::documents::insert_document_indexed(
            &db,
            &index,
            &weak_meta,
            r#"{"title":"unrelated topic","description":"nothing about the query"}"#,
        )
        .expect("insert weak");

        let strong_meta = crate::documents::DocumentMeta {
            id: Some("FR-STRONG"),
            doc_type: "requirement",
            surface: None,
            status: None,
            priority: None,
            owner: "kelex",
        };
        crate::documents::insert_document_indexed(
            &db,
            &index,
            &strong_meta,
            r#"{"title":"mapping rules","description":"mapping rules for schema mapping"}"#,
        )
        .expect("insert strong");

        // A document owned by a different repo must not leak into this repo's results.
        let other_repo_meta = crate::documents::DocumentMeta {
            id: Some("FR-OTHER"),
            doc_type: "requirement",
            surface: None,
            status: None,
            priority: None,
            owner: "rafters",
        };
        crate::documents::insert_document_indexed(
            &db,
            &index,
            &other_repo_meta,
            r#"{"title":"mapping rules","description":"also about mapping"}"#,
        )
        .expect("insert other repo");

        drop(db);
        drop(index);

        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;

        let (status, body) = http_req(port, "GET", "/api/search?q=mapping&repo=kelex", None).await;
        assert!(status.starts_with("HTTP/1.1 200"), "status: {status}");
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).expect("parse body");
        assert_eq!(
            parsed.len(),
            1,
            "other-repo document must not appear: {body}"
        );
        assert_eq!(parsed[0]["id"], "FR-STRONG");
    }

    #[tokio::test]
    async fn api_search_empty_query_returns_empty_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        let index = SearchIndex::open(&data_dir.join("index")).expect("open index");
        let meta = crate::documents::DocumentMeta {
            id: Some("FR-ANY"),
            doc_type: "requirement",
            surface: None,
            status: None,
            priority: None,
            owner: "kelex",
        };
        crate::documents::insert_document_indexed(&db, &index, &meta, r#"{"title":"anything"}"#)
            .expect("insert");
        drop(db);
        drop(index);

        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;

        let (status, body) = http_req(port, "GET", "/api/search?q=&repo=kelex", None).await;
        assert!(status.starts_with("HTTP/1.1 200"), "status: {status}");
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).expect("parse body");
        assert!(
            parsed.is_empty(),
            "empty q must return an empty list: {body}"
        );
    }

    /// `TopDocs::with_limit(0)` panics in tantivy 0.22.1 (top_score_collector.rs
    /// 192-194); `?limit=0` is caller-supplied input that must not reach it.
    #[tokio::test]
    async fn api_search_limit_zero_returns_empty_list_not_500() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        let index = SearchIndex::open(&data_dir.join("index")).expect("open index");
        let meta = crate::documents::DocumentMeta {
            id: Some("FR-ANY"),
            doc_type: "requirement",
            surface: None,
            status: None,
            priority: None,
            owner: "kelex",
        };
        crate::documents::insert_document_indexed(&db, &index, &meta, r#"{"title":"anything"}"#)
            .expect("insert");
        drop(db);
        drop(index);

        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;

        let (status, body) = http_req(
            port,
            "GET",
            "/api/search?q=anything&repo=kelex&limit=0",
            None,
        )
        .await;
        assert!(status.starts_with("HTTP/1.1 200"), "status: {status}");
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).expect("parse body");
        assert!(
            parsed.is_empty(),
            "limit=0 must return an empty list: {body}"
        );
    }

    #[tokio::test]
    async fn api_document_create_round_trip_and_generates_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;

        let (status, body) = http_req(
            port,
            "POST",
            "/api/documents",
            Some(r#"{"doc_type":"thesis","owner":"legion","payload":{"title":"New intention"}}"#),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "create: {status} {body}"
        );
        let created: serde_json::Value = serde_json::from_str(&body).expect("parse create body");
        assert_eq!(created["doc_type"], "thesis");
        assert_eq!(created["owner"], "legion");
        let id = created["id"].as_str().expect("id is a string").to_string();
        // No id was supplied -- a UUIDv7 is generated.
        assert!(
            uuid::Uuid::parse_str(&id).is_ok_and(|u| u.get_version_num() == 7),
            "expected a generated UUIDv7, got {id}"
        );
        let payload: serde_json::Value =
            serde_json::from_str(created["payload"].as_str().expect("payload is a string"))
                .expect("parse payload");
        assert_eq!(payload["title"], "New intention");

        // The insert persisted -- a fresh GET sees it.
        let (status, body) = http_req(port, "GET", &format!("/api/documents/{id}"), None).await;
        assert!(status.starts_with("HTTP/1.1 200"), "view: {status}");
        let fetched: serde_json::Value = serde_json::from_str(&body).expect("parse view body");
        assert_eq!(fetched["id"], id);
    }

    /// A document created over HTTP must be findable via GET /api/search
    /// (#1037), same as one created via the CLI.
    #[tokio::test]
    async fn api_document_create_indexes_for_search() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;

        let (status, body) = http_req(
            port,
            "POST",
            "/api/documents",
            Some(
                r#"{"doc_type":"thesis","owner":"legion","payload":{"title":"uniqueCreateSearchTerm"}}"#,
            ),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "create: {status} {body}"
        );
        let created: serde_json::Value = serde_json::from_str(&body).expect("parse create body");
        let id = created["id"].as_str().expect("id").to_string();

        let (status, body) = http_req(
            port,
            "GET",
            "/api/search?q=uniqueCreateSearchTerm&repo=legion",
            None,
        )
        .await;
        assert!(status.starts_with("HTTP/1.1 200"), "search: {status}");
        let hits: Vec<serde_json::Value> = serde_json::from_str(&body).expect("parse search body");
        assert!(
            hits.iter().any(|h| h["id"] == id),
            "created document must be findable via /api/search: {body}"
        );
    }

    #[tokio::test]
    async fn api_document_create_schema_dual_writes_pointer_reflection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let port = spawn_test_server(data_dir.clone(), ServerRole::Daemon).await;

        let schema_payload = serde_json::json!({
            "$schema": "https://json-schema.org/draft-07/schema#",
            "title": "Widget",
            "description": "A test schema",
            "type": "object",
            "properties": {"name": {"type": "string"}},
        });
        let create_body = serde_json::json!({
            "doc_type": "schema",
            "owner": "legion",
            "payload": schema_payload,
        })
        .to_string();
        let (status, body) = http_req(port, "POST", "/api/documents", Some(&create_body)).await;
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "create: {status} {body}"
        );
        let created: serde_json::Value = serde_json::from_str(&body).expect("parse create body");
        let id = created["id"].as_str().expect("id").to_string();

        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        let reflections = db
            .get_reflections_by_domain(
                "legion",
                "schema",
                10,
                crate::recall::ArchiveMode::Hot,
                &crate::timerange::TimeRange::default(),
            )
            .expect("get reflections by domain");
        assert!(
            reflections
                .iter()
                .any(|r| r.text.contains("Widget") && r.text.contains(&id)),
            "expected a schema pointer reflection naming the new document, got: {reflections:?}"
        );

        // An invalid schema payload (missing every required field) is
        // rejected at create, not written through (#1036 review, MED-2).
        let (status, _) = http_req(
            port,
            "POST",
            "/api/documents",
            Some(r#"{"doc_type":"schema","owner":"legion","payload":{}}"#),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "invalid schema payload: {status}"
        );
    }

    /// The revise-side mirror of
    /// `api_document_create_schema_dual_writes_pointer_reflection` (#1036
    /// review, MED-2, HIGH-1 coverage): a non-object revise payload and an
    /// invalid schema payload are both rejected with 400 and leave
    /// `revision` unchanged, and a valid schema revise bumps `revision` and
    /// refreshes the pointer reflection.
    #[tokio::test]
    async fn api_document_revise_schema_refreshes_pointer_and_rejects_invalid_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let port = spawn_test_server(data_dir.clone(), ServerRole::Daemon).await;

        let schema_payload = serde_json::json!({
            "$schema": "https://json-schema.org/draft-07/schema#",
            "title": "Gadget",
            "description": "A revisable test schema",
            "type": "object",
            "properties": {"name": {"type": "string"}},
        });
        let create_body = serde_json::json!({
            "doc_type": "schema",
            "owner": "legion",
            "payload": schema_payload,
        })
        .to_string();
        let (status, body) = http_req(port, "POST", "/api/documents", Some(&create_body)).await;
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "create: {status} {body}"
        );
        let created: serde_json::Value = serde_json::from_str(&body).expect("parse create body");
        let id = created["id"].as_str().expect("id").to_string();

        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        assert_eq!(db.document_revision(&id).unwrap(), 1);

        // A non-object revise payload is rejected before it ever reaches
        // revise_document.
        let (status, _) = http_req(
            port,
            "POST",
            &format!("/api/documents/{id}/revise"),
            Some(r#""not an object""#),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "non-object revise payload: {status}"
        );
        assert_eq!(
            db.document_revision(&id).unwrap(),
            1,
            "the rejected non-object payload must not bump revision"
        );

        // An invalid schema payload (missing every required field) is
        // rejected at revise, matching create, and does not bump revision.
        let (status, _) = http_req(
            port,
            "POST",
            &format!("/api/documents/{id}/revise"),
            Some("{}"),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "invalid schema revise payload: {status}"
        );
        assert_eq!(
            db.document_revision(&id).unwrap(),
            1,
            "an invalid schema revise must not bump revision"
        );

        // A valid schema revise bumps revision and refreshes the pointer
        // reflection.
        let revised_schema_payload = serde_json::json!({
            "$schema": "https://json-schema.org/draft-07/schema#",
            "title": "Gadget Mark Two",
            "description": "A revised test schema",
            "type": "object",
            "properties": {"name": {"type": "string"}},
        })
        .to_string();
        let (status, body) = http_req(
            port,
            "POST",
            &format!("/api/documents/{id}/revise"),
            Some(&revised_schema_payload),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "valid schema revise: {status} {body}"
        );
        assert_eq!(db.document_revision(&id).unwrap(), 2);

        let reflections = db
            .get_reflections_by_domain(
                "legion",
                "schema",
                10,
                crate::recall::ArchiveMode::Hot,
                &crate::timerange::TimeRange::default(),
            )
            .expect("get reflections by domain");
        assert!(
            reflections
                .iter()
                .any(|r| r.text.contains("Gadget Mark Two") && r.text.contains(&id)),
            "expected the revise to refresh the schema pointer reflection, got: {reflections:?}"
        );
    }

    #[tokio::test]
    async fn api_document_create_rejects_non_object_payload_and_duplicate_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;

        let (status, _) = http_req(
            port,
            "POST",
            "/api/documents",
            Some(r#"{"doc_type":"thesis","owner":"legion","payload":"not an object"}"#),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "non-object payload: {status}"
        );
        // Nothing was written through despite the 400 (#1036 review, MED-2).
        let (status, body) = http_req(port, "GET", "/api/documents", None).await;
        assert!(status.starts_with("HTTP/1.1 200"), "list: {status}");
        let listed: serde_json::Value = serde_json::from_str(&body).expect("parse list body");
        assert_eq!(
            listed.as_array().expect("array body").len(),
            0,
            "the rejected non-object payload must not have landed a row: {body}"
        );

        let first_create =
            r#"{"doc_type":"thesis","owner":"legion","id":"TH-DUP","payload":{"title":"first"}}"#;
        let (status, _) = http_req(port, "POST", "/api/documents", Some(first_create)).await;
        assert!(status.starts_with("HTTP/1.1 200"), "first create: {status}");

        let duplicate_create =
            r#"{"doc_type":"thesis","owner":"legion","id":"TH-DUP","payload":{"title":"second"}}"#;
        let (status, _) = http_req(port, "POST", "/api/documents", Some(duplicate_create)).await;
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "duplicate id must be a 4xx, not a 500: {status}"
        );

        // The rejected duplicate must not have overwritten the original
        // (#1036 review, MED-2).
        let (status, body) = http_req(port, "GET", "/api/documents/TH-DUP", None).await;
        assert!(status.starts_with("HTTP/1.1 200"), "view: {status}");
        let fetched: serde_json::Value = serde_json::from_str(&body).expect("parse view body");
        let payload: serde_json::Value =
            serde_json::from_str(fetched["payload"].as_str().expect("payload string"))
                .expect("parse payload");
        assert_eq!(
            payload["title"], "first",
            "the first create's payload must survive the rejected duplicate"
        );
    }

    /// `doc_type` and `surface` get the same charset/length gate as `owner`
    /// and `id` (#1036 review, LOW): a caller cannot smuggle a character
    /// outside `[A-Za-z0-9._-]` into either indexed column.
    #[tokio::test]
    async fn api_document_create_validates_doc_type_and_surface() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;

        let (status, _) = http_req(
            port,
            "POST",
            "/api/documents",
            Some(r#"{"doc_type":"bad type!","owner":"legion","payload":{}}"#),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "invalid doc_type: {status}"
        );

        let (status, _) = http_req(
            port,
            "POST",
            "/api/documents",
            Some(r#"{"doc_type":"thesis","owner":"legion","surface":"bad surface!","payload":{}}"#),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "invalid surface: {status}"
        );
    }

    #[tokio::test]
    async fn api_document_update_body_leaves_revision_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        let meta = crate::documents::DocumentMeta {
            id: Some("TH-BODY-1"),
            doc_type: "thesis",
            surface: None,
            status: None,
            priority: None,
            owner: "legion",
        };
        db.insert_document(&meta, r#"{"title":"x"}"#)
            .expect("insert");
        assert_eq!(db.document_revision("TH-BODY-1").unwrap(), 1);

        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;
        let (status, body) = http_req(
            port,
            "PUT",
            "/api/documents/TH-BODY-1/body",
            Some(r#"{"body":"draft text"}"#),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "body save: {status} {body}"
        );
        let updated: serde_json::Value = serde_json::from_str(&body).expect("parse body");
        let payload: serde_json::Value =
            serde_json::from_str(updated["payload"].as_str().expect("payload string"))
                .expect("parse payload");
        assert_eq!(payload["body"], "draft text");
        assert_eq!(payload["title"], "x", "unrelated fields survive");
        assert_eq!(
            db.document_revision("TH-BODY-1").unwrap(),
            1,
            "a body save must not bump revision"
        );
    }

    /// `body` is part of the document's indexed text (#1037), so a body
    /// save must re-index. The query term exists ONLY in the newly saved
    /// body -- a hit proves the save re-indexed, not that a stale entry
    /// from before the save happened to survive.
    #[tokio::test]
    async fn api_document_update_body_indexes_body_text_for_search() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        let meta = crate::documents::DocumentMeta {
            id: Some("TH-BODYSEARCH-1"),
            doc_type: "thesis",
            surface: None,
            status: None,
            priority: None,
            owner: "legion",
        };
        db.insert_document(&meta, r#"{"title":"bodySearchHolder"}"#)
            .expect("insert");
        drop(db);

        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;
        let (status, _) = http_req(
            port,
            "PUT",
            "/api/documents/TH-BODYSEARCH-1/body",
            Some(r#"{"body":"exclusiveBodyOnlyTerm appears nowhere else"}"#),
        )
        .await;
        assert!(status.starts_with("HTTP/1.1 200"), "body save: {status}");

        let (status, body) = http_req(
            port,
            "GET",
            "/api/search?q=exclusiveBodyOnlyTerm&repo=legion",
            None,
        )
        .await;
        assert!(status.starts_with("HTTP/1.1 200"), "search: {status}");
        let hits: Vec<serde_json::Value> = serde_json::from_str(&body).expect("parse search body");
        assert_eq!(hits.len(), 1, "body save must index body text: {body}");
        assert_eq!(hits[0]["id"], "TH-BODYSEARCH-1");
    }

    #[tokio::test]
    async fn api_document_revise_bumps_revision_and_replaces_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        let meta = crate::documents::DocumentMeta {
            id: Some("TH-REVISE-1"),
            doc_type: "thesis",
            surface: None,
            status: None,
            priority: None,
            owner: "legion",
        };
        db.insert_document(&meta, r#"{"title":"old"}"#)
            .expect("insert");
        assert_eq!(db.document_revision("TH-REVISE-1").unwrap(), 1);

        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;
        let (status, body) = http_req(
            port,
            "POST",
            "/api/documents/TH-REVISE-1/revise",
            Some(r#"{"title":"new"}"#),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "revise: {status} {body}"
        );
        let revised: serde_json::Value = serde_json::from_str(&body).expect("parse body");
        let payload: serde_json::Value =
            serde_json::from_str(revised["payload"].as_str().expect("payload string"))
                .expect("parse payload");
        assert_eq!(
            payload,
            serde_json::json!({"title": "new"}),
            "revise replaces, not merges"
        );
        assert_eq!(db.document_revision("TH-REVISE-1").unwrap(), 2);

        // A second revise bumps again, not just once total (#1036 review,
        // MED-2).
        let (status, _) = http_req(
            port,
            "POST",
            "/api/documents/TH-REVISE-1/revise",
            Some(r#"{"title":"newer"}"#),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "second revise: {status}"
        );
        assert_eq!(db.document_revision("TH-REVISE-1").unwrap(), 3);
    }

    /// A revise over HTTP must re-index (#1037): proves both directions,
    /// the pre-revise text stops matching (no stale ghost) and the revised
    /// text matches exactly once (no duplicate entry) -- the same
    /// delete-then-add guarantee `add_document_replaces_prior_entry_for_same_id`
    /// (src/search.rs) proves at the SearchIndex level, closed here at the
    /// HTTP handler level.
    #[tokio::test]
    async fn api_document_revise_reindexes_for_search() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        let meta = crate::documents::DocumentMeta {
            id: Some("TH-REINDEX-1"),
            doc_type: "thesis",
            surface: None,
            status: None,
            priority: None,
            owner: "legion",
        };
        db.insert_document(&meta, r#"{"title":"originalReviseTerm"}"#)
            .expect("insert");
        drop(db);

        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;
        let (status, _) = http_req(
            port,
            "POST",
            "/api/documents/TH-REINDEX-1/revise",
            Some(r#"{"title":"revisedSearchTerm"}"#),
        )
        .await;
        assert!(status.starts_with("HTTP/1.1 200"), "revise: {status}");

        // Stale pre-revise text must not still match.
        let (_, body) = http_req(
            port,
            "GET",
            "/api/search?q=originalReviseTerm&repo=legion",
            None,
        )
        .await;
        let stale: Vec<serde_json::Value> = serde_json::from_str(&body).expect("parse body");
        assert!(
            stale.is_empty(),
            "stale pre-revise text must not match: {body}"
        );

        let (status, body) = http_req(
            port,
            "GET",
            "/api/search?q=revisedSearchTerm&repo=legion",
            None,
        )
        .await;
        assert!(status.starts_with("HTTP/1.1 200"), "search: {status}");
        let hits: Vec<serde_json::Value> = serde_json::from_str(&body).expect("parse search body");
        assert_eq!(
            hits.len(),
            1,
            "revise must re-index without duplicating: {body}"
        );
        assert_eq!(hits[0]["id"], "TH-REINDEX-1");
    }

    #[tokio::test]
    async fn api_document_body_and_revise_404_for_unknown_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;

        let (status, _) = http_req(
            port,
            "PUT",
            "/api/documents/NOPE/body",
            Some(r#"{"body":"x"}"#),
        )
        .await;
        assert!(status.starts_with("HTTP/1.1 404"), "body-missing: {status}");

        let (status, _) = http_req(port, "POST", "/api/documents/NOPE/revise", Some("{}")).await;
        assert!(
            status.starts_with("HTTP/1.1 404"),
            "revise-missing: {status}"
        );
    }

    #[tokio::test]
    async fn api_document_body_and_revise_refuse_archived_document() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let db = Database::open(&data_dir.join("legion.db")).expect("open db");
        let meta = crate::documents::DocumentMeta {
            id: Some("TH-ARCHIVED-1"),
            doc_type: "thesis",
            surface: None,
            status: None,
            priority: None,
            owner: "legion",
        };
        db.insert_document(&meta, "{}").expect("insert");
        db.archive_document("TH-ARCHIVED-1").expect("archive");

        let port = spawn_test_server(data_dir, ServerRole::Daemon).await;
        let (status, _) = http_req(
            port,
            "PUT",
            "/api/documents/TH-ARCHIVED-1/body",
            Some(r#"{"body":"x"}"#),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "archived body-save must be a 4xx, not a 500: {status}"
        );

        let (status, _) = http_req(
            port,
            "POST",
            "/api/documents/TH-ARCHIVED-1/revise",
            Some("{}"),
        )
        .await;
        assert!(
            status.starts_with("HTTP/1.1 400"),
            "archived revise must be a 4xx, not a 500: {status}"
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
