//! Owner runtime routes: chat/training, feedback, teaching, ingest, outbound
//! questions/requests, live config, mind snapshots, and the SSE event stream.
//! Body limit, timeout, and rate limit are read from live config per request.

use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aster_core::OutboundStatus;
use aster_runtime::{
    Envelope, Event, Mode, Runtime, RuntimeError,
    config::{parse_cli_value, restart_required},
};
use aster_storage::StorageError;
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, Request, State, rejection::JsonRejection},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::Value;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use uuid::Uuid;

use super::{AppState, RuntimeSlot};

#[derive(Default)]
struct Limiter {
    hits: Mutex<VecDeque<Instant>>,
}

impl Limiter {
    fn allow(&self, per_minute: u32) -> bool {
        if per_minute == 0 {
            return true;
        }
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        while hits
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
        {
            hits.pop_front();
        }
        if hits.len() >= per_minute as usize {
            return false;
        }
        hits.push_back(now);
        true
    }
}

#[derive(Clone)]
struct Guard {
    runtime: RuntimeSlot,
    limiter: Arc<Limiter>,
}

pub(super) fn router(runtime: Option<Arc<Runtime>>) -> Router<AppState> {
    let guard = Guard {
        runtime: RuntimeSlot(runtime),
        limiter: Arc::new(Limiter::default()),
    };
    let bounded = Router::new()
        .route("/v1/chat", post(chat))
        .route("/v1/turns", get(turns))
        .route("/v1/thoughts/{turn_id}", get(thought))
        .route("/v1/turns/{turn_id}/prefer", post(prefer))
        .route(
            "/v1/turns/{turn_id}/candidates/{index}/rate",
            post(rate_candidate),
        )
        .route(
            "/v1/turns/{turn_id}/recalled/{index}/irrelevant",
            post(irrelevant),
        )
        .route("/v1/turns/{turn_id}/reconsider", post(reconsider))
        .route("/v1/turns/{turn_id}/signals", get(signals))
        .route("/v1/feedback", post(feedback))
        .route("/v1/teach", post(teach))
        .route("/v1/ingest", post(ingest))
        .route("/v1/ingest/files", get(ingest_files))
        .route("/v1/ingest/upload", post(upload))
        .route("/v1/read", post(read))
        .route("/v1/logs/{category}", get(log_tail))
        .route("/v1/outbound", get(outbound))
        .route("/v1/outbound/{id}/answer", post(answer))
        .route("/v1/outbound/{id}/approve", post(approve))
        .route("/v1/outbound/{id}/deny", post(deny))
        .route("/v1/runtime", get(runtime_status))
        .route("/v1/runtime/pause", post(pause))
        .route("/v1/runtime/resume", post(resume))
        .route("/v1/state/history", get(history))
        .route("/v1/config", get(config).patch(patch_config))
        .route("/v1/mind/snapshots", get(snapshots).post(save_snapshot))
        .route("/v1/mind/rollback", post(rollback))
        .route("/v1/mind/corpus", get(corpus))
        .route("/v1/mind/training", get(training))
        .layer(DefaultBodyLimit::disable())
        .route_layer(middleware::from_fn_with_state(guard.clone(), limits));
    // The event stream is long-lived: no timeout, but still requires a runtime.
    let streaming = Router::new()
        .route("/v1/events", get(events))
        .route_layer(middleware::from_fn_with_state(guard, require_runtime));
    bounded.merge(streaming)
}

fn error(status: StatusCode, code: &str, detail: Option<String>) -> Response {
    let mut body = serde_json::json!({ "error": code });
    if let Some(detail) = detail {
        body["detail"] = Value::String(detail);
    }
    (status, Json(body)).into_response()
}

fn failure(err: RuntimeError) -> Response {
    match err {
        RuntimeError::Storage(StorageError::NotInitialized) | RuntimeError::NotInitialized => {
            error(StatusCode::SERVICE_UNAVAILABLE, "not_initialized", None)
        }
        RuntimeError::Storage(error_value) => {
            tracing::warn!(error = %error_value, "runtime storage failure");
            error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable", None)
        }
        RuntimeError::Invalid(detail) => {
            error(StatusCode::BAD_REQUEST, "invalid_input", Some(detail))
        }
        RuntimeError::NotFound => error(StatusCode::NOT_FOUND, "not_found", None),
        RuntimeError::Conflict(detail) => error(StatusCode::CONFLICT, "conflict", Some(detail)),
        RuntimeError::Snapshot => {
            error(StatusCode::INTERNAL_SERVER_ERROR, "snapshot_invalid", None)
        }
        RuntimeError::Config(e) => error(
            StatusCode::BAD_REQUEST,
            "invalid_config",
            Some(e.to_string()),
        ),
    }
}

fn bad_json(rejection: JsonRejection) -> Response {
    match rejection.status() {
        StatusCode::PAYLOAD_TOO_LARGE => {
            error(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", None)
        }
        StatusCode::UNSUPPORTED_MEDIA_TYPE => error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            None,
        ),
        _ => error(StatusCode::BAD_REQUEST, "invalid_json", None),
    }
}

fn respond<T: serde::Serialize>(result: Result<T, RuntimeError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(err) => failure(err),
    }
}

macro_rules! runtime {
    ($slot:expr) => {
        match $slot.0.clone() {
            Some(runtime) => runtime,
            None => {
                return error(StatusCode::SERVICE_UNAVAILABLE, "runtime_unavailable", None);
            }
        }
    };
}

macro_rules! body {
    ($input:expr) => {
        match $input {
            Ok(Json(value)) => value,
            Err(rejection) => return bad_json(rejection),
        }
    };
}

async fn require_runtime(State(guard): State<Guard>, request: Request, next: Next) -> Response {
    let _ = runtime!(guard.runtime);
    next.run(request).await
}

async fn limits(State(guard): State<Guard>, request: Request, next: Next) -> Response {
    let runtime = runtime!(guard.runtime);
    let cfg = runtime.config().get();
    let server = cfg.server.clone();
    // Uploads carry a whole file; reading a page waits on another server.
    // Uploads carry a whole file. Reading a page (directly, or by approving
    // a request or answering with a link) waits on another server.
    let path = request.uri().path();
    let reads = path == "/v1/read"
        || (path.starts_with("/v1/outbound/")
            && (path.ends_with("/approve") || path.ends_with("/answer")));
    let body_limit = if path == "/v1/ingest/upload" {
        server.body_limit_bytes.max(
            usize::try_from(server.ingest_max_bytes)
                .unwrap_or(usize::MAX)
                .saturating_mul(2)
                .saturating_add(4096),
        )
    } else {
        server.body_limit_bytes
    };
    let timeout_ms = if reads {
        server
            .request_timeout_ms
            .max(cfg.web.timeout_secs * 1000 + 5_000)
    } else {
        server.request_timeout_ms
    };
    // Reads (dashboard resyncs, TUI refreshes) don't spend the write budget.
    let read = request.method() == axum::http::Method::GET;
    if !read && !guard.limiter.allow(server.rate_limit_per_minute) {
        return error(StatusCode::TOO_MANY_REQUESTS, "rate_limited", None);
    }
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|len| len > body_limit) {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", None);
    }
    let (parts, body) = request.into_parts();
    let body = Body::new(http_body_util::Limited::new(body, body_limit));
    let request = Request::from_parts(parts, body);
    match tokio::time::timeout(Duration::from_millis(timeout_ms), next.run(request)).await {
        Ok(response) => response,
        Err(_) => error(StatusCode::GATEWAY_TIMEOUT, "request_timeout", None),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatInput {
    text: String,
    #[serde(default)]
    mode: Mode,
    /// Thread this message under an earlier turn.
    reply_to: Option<Uuid>,
}

async fn chat(
    State(slot): State<RuntimeSlot>,
    input: Result<Json<ChatInput>, JsonRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let input = body!(input);
    respond(runtime.reply(&input.text, input.mode, input.reply_to).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitQuery {
    limit: Option<i64>,
    status: Option<String>,
}

async fn turns(State(slot): State<RuntimeSlot>, Query(query): Query<LimitQuery>) -> Response {
    let runtime = runtime!(slot);
    respond(runtime.recent_turns(query.limit.unwrap_or(50)).await)
}

async fn thought(State(slot): State<RuntimeSlot>, Path(turn_id): Path<Uuid>) -> Response {
    let runtime = runtime!(slot);
    respond(runtime.thought(turn_id).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferInput {
    candidate: usize,
}

async fn prefer(
    State(slot): State<RuntimeSlot>,
    Path(turn_id): Path<Uuid>,
    input: Result<Json<PreferInput>, JsonRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let input = body!(input);
    respond(runtime.prefer(turn_id, input.candidate).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RateInput {
    rating: i8,
}

async fn rate_candidate(
    State(slot): State<RuntimeSlot>,
    Path((turn_id, index)): Path<(Uuid, usize)>,
    input: Result<Json<RateInput>, JsonRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let input = body!(input);
    respond(runtime.rate_candidate(turn_id, index, input.rating).await)
}

async fn irrelevant(
    State(slot): State<RuntimeSlot>,
    Path((turn_id, index)): Path<(Uuid, usize)>,
) -> Response {
    let runtime = runtime!(slot);
    respond(runtime.mark_irrelevant(turn_id, index).await)
}

async fn reconsider(State(slot): State<RuntimeSlot>, Path(turn_id): Path<Uuid>) -> Response {
    let runtime = runtime!(slot);
    respond(runtime.reconsider(turn_id).await)
}

async fn signals(State(slot): State<RuntimeSlot>, Path(turn_id): Path<Uuid>) -> Response {
    let runtime = runtime!(slot);
    respond(runtime.turn_signals(turn_id).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedbackInput {
    turn_id: Uuid,
    rating: i8,
    correction: Option<String>,
}

async fn feedback(
    State(slot): State<RuntimeSlot>,
    input: Result<Json<FeedbackInput>, JsonRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let input = body!(input);
    respond(
        runtime
            .feedback(input.turn_id, input.rating, input.correction.as_deref())
            .await,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TeachInput {
    prompt: String,
    reply: String,
}

async fn teach(
    State(slot): State<RuntimeSlot>,
    input: Result<Json<TeachInput>, JsonRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let input = body!(input);
    respond(runtime.teach(&input.prompt, &input.reply).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IngestInput {
    path: String,
}

async fn ingest(
    State(slot): State<RuntimeSlot>,
    input: Result<Json<IngestInput>, JsonRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let input = body!(input);
    respond(runtime.ingest(&input.path).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadInput {
    name: String,
    text: String,
}

async fn upload(
    State(slot): State<RuntimeSlot>,
    input: Result<Json<UploadInput>, JsonRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let input = body!(input);
    respond(runtime.upload(&input.name, &input.text).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    url: String,
}

async fn read(
    State(slot): State<RuntimeSlot>,
    input: Result<Json<ReadInput>, JsonRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let input = body!(input);
    respond(runtime.read_url(&input.url, "owner").await)
}

async fn outbound(State(slot): State<RuntimeSlot>, Query(query): Query<LimitQuery>) -> Response {
    let runtime = runtime!(slot);
    let status = match query.status.as_deref() {
        None | Some("all") => None,
        Some(value) => match OutboundStatus::parse(value) {
            Some(status) => Some(status),
            None => return error(StatusCode::BAD_REQUEST, "invalid_status", None),
        },
    };
    respond(runtime.outbound(status, query.limit.unwrap_or(50)).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerInput {
    text: String,
}

async fn answer(
    State(slot): State<RuntimeSlot>,
    Path(id): Path<Uuid>,
    input: Result<Json<AnswerInput>, JsonRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let input = body!(input);
    respond(runtime.answer(id, &input.text).await)
}

async fn approve(State(slot): State<RuntimeSlot>, Path(id): Path<Uuid>) -> Response {
    let runtime = runtime!(slot);
    respond(runtime.decide(id, true).await)
}

async fn deny(State(slot): State<RuntimeSlot>, Path(id): Path<Uuid>) -> Response {
    let runtime = runtime!(slot);
    respond(runtime.decide(id, false).await)
}

async fn runtime_status(State(slot): State<RuntimeSlot>) -> Response {
    let runtime = runtime!(slot);
    Json(runtime.status().await).into_response()
}

async fn pause(State(slot): State<RuntimeSlot>) -> Response {
    let runtime = runtime!(slot);
    respond(
        runtime
            .set_paused(true)
            .await
            .map(|()| serde_json::json!({ "paused": true })),
    )
}

async fn resume(State(slot): State<RuntimeSlot>) -> Response {
    let runtime = runtime!(slot);
    respond(
        runtime
            .set_paused(false)
            .await
            .map(|()| serde_json::json!({ "paused": false })),
    )
}

async fn history(State(slot): State<RuntimeSlot>, Query(query): Query<LimitQuery>) -> Response {
    let runtime = runtime!(slot);
    respond(runtime.state_history(query.limit.unwrap_or(100)).await)
}

async fn config(State(slot): State<RuntimeSlot>) -> Response {
    let runtime = runtime!(slot);
    let handle = runtime.config();
    Json(serde_json::json!({
        "path": handle.path().display().to_string(),
        "config": *handle.get(),
        "restart_required_prefixes": aster_runtime::config::RESTART_REQUIRED,
    }))
    .into_response()
}

/// Body: `{"section.key": value, ...}`. String values that look like TOML
/// literals are not reinterpreted; send JSON numbers/booleans directly.
async fn patch_config(
    State(slot): State<RuntimeSlot>,
    input: Result<Json<serde_json::Map<String, Value>>, JsonRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let input = body!(input);
    if input.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_input",
            Some("no keys given".into()),
        );
    }
    let updates: Vec<(String, Value)> = input
        .into_iter()
        .map(|(key, value)| match value {
            // Console sends raw text; accept "{\"raw\": \"...\"}" style strings too.
            Value::String(raw) if raw.starts_with('=') => (key, parse_cli_value(&raw[1..])),
            other => (key, other),
        })
        .collect();
    match runtime.config().set(&updates).await {
        Ok(keys) => {
            let restart = restart_required(&keys);
            Json(serde_json::json!({ "changed": keys, "restart_required": restart }))
                .into_response()
        }
        Err(err) => failure(RuntimeError::Config(err)),
    }
}

async fn snapshots(State(slot): State<RuntimeSlot>, Query(query): Query<LimitQuery>) -> Response {
    let runtime = runtime!(slot);
    respond(runtime.snapshots(query.limit.unwrap_or(20)).await)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SnapshotInput {
    reason: Option<String>,
}

async fn save_snapshot(
    State(slot): State<RuntimeSlot>,
    input: Option<Json<SnapshotInput>>,
) -> Response {
    let runtime = runtime!(slot);
    let reason = input
        .and_then(|Json(i)| i.reason)
        .filter(|r| !r.trim().is_empty() && r.len() <= 200)
        .unwrap_or_else(|| "manual".into());
    respond(runtime.snapshot(&reason).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RollbackInput {
    version: i64,
}

async fn rollback(
    State(slot): State<RuntimeSlot>,
    input: Result<Json<RollbackInput>, JsonRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let input = body!(input);
    respond(runtime.rollback(input.version).await)
}

async fn corpus(State(slot): State<RuntimeSlot>) -> Response {
    let runtime = runtime!(slot);
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        runtime.corpus().await,
    )
        .into_response()
}

async fn ingest_files(State(slot): State<RuntimeSlot>) -> Response {
    let runtime = runtime!(slot);
    respond(runtime.ingest_files().await)
}

async fn log_tail(
    State(slot): State<RuntimeSlot>,
    Path(category): Path<String>,
    query: Result<Query<LimitQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let runtime = runtime!(slot);
    let limit = query.ok().and_then(|q| q.0.limit).unwrap_or(100).max(1) as usize;
    respond(runtime.log_tail(&category, limit).await)
}

async fn training(State(slot): State<RuntimeSlot>) -> Response {
    let runtime = runtime!(slot);
    Json(runtime.training().await).into_response()
}

fn sse(envelope: &Envelope) -> Option<SseEvent> {
    SseEvent::default().json_data(envelope).ok()
}

async fn events(State(slot): State<RuntimeSlot>) -> Response {
    let runtime = runtime!(slot);
    let receiver = runtime.subscribe();
    let hello = Envelope::now(Event::Levels {
        state: runtime.levels(),
        cause: "connected".into(),
    });
    let initial = tokio_stream::iter(sse(&hello));
    let live = BroadcastStream::new(receiver).filter_map(|item| match item {
        Ok(envelope) => sse(&envelope),
        // A slow client skips missed events rather than disconnecting.
        Err(_) => None,
    });
    let stream = initial.chain(live).map(Ok::<_, Infallible>);
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_enforces_per_minute_budget() {
        let limiter = Limiter::default();
        assert!(limiter.allow(2));
        assert!(limiter.allow(2));
        assert!(!limiter.allow(2));
        assert!(limiter.allow(0));
    }
}
