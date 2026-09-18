use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{circuits, model::*, scheduler::Scheduler, store::engine_identity};

#[derive(Clone)]
struct App {
    jobs: Arc<Scheduler>,
    circuits: Arc<Vec<Circuit>>,
}

pub fn router(jobs: Arc<Scheduler>) -> anyhow::Result<Router> {
    let circuits = Arc::new(circuits::catalog()?);
    Ok(Router::new()
        .route("/api/health", get(health))
        .route("/api/circuits", get(circuit_catalog))
        .route("/api/records", get(records))
        .route("/api/records/{id}/ghost", get(ghost))
        .route("/api/runs", get(runs).post(create_run))
        .route("/api/runs/{id}", get(run))
        .route("/api/runs/{id}/stop", post(stop))
        .route("/api/runs/{id}/resume", post(resume))
        .route("/api/runs/{id}/generations", get(generations))
        .route(
            "/api/runs/{id}/generations/{generation}/replay",
            get(replay).post(request_replay),
        )
        .route(
            "/api/runs/{id}/generations/{generation}/replay/chunks/{index}",
            get(chunk),
        )
        .route(
            "/api/runs/{id}/generations/{generation}/cars/{car}/trace",
            get(trace),
        )
        .route("/api/events", get(events))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(App { jobs, circuits }))
}

struct ApiError {
    status: StatusCode,
    message: String,
}
impl ApiError {
    fn bad(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.to_string(),
        }
    }
    fn missing() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: "Not found".into(),
        }
    }
    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "GPU scheduler is unavailable".into(),
        }
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        let message = error.to_string();
        let status = if message.contains("not found") {
            StatusCode::NOT_FOUND
        } else if message.contains("queue is full") {
            StatusCode::TOO_MANY_REQUESTS
        } else if message.contains("incompatible")
            || message.contains("cannot be resumed")
            || message.contains("already belongs")
        {
            StatusCode::CONFLICT
        } else {
            tracing::error!(error=%format!("{error:#}"),"API operation failed");
            StatusCode::INTERNAL_SERVER_ERROR
        };
        Self {
            status,
            message: if status == StatusCode::INTERNAL_SERVER_ERROR {
                "Operation failed; see the server log".into()
            } else {
                message
            },
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error":self.message}))).into_response()
    }
}
type ApiResult<T> = Result<T, ApiError>;

fn available(app: &App) -> ApiResult<()> {
    if app.jobs.shutting_down() || app.jobs.error().is_some() {
        Err(ApiError::unavailable())
    } else {
        Ok(())
    }
}

async fn health(State(app): State<App>) -> Json<Health> {
    Json(Health {
        status: if app.jobs.shutting_down() {
            "stopping"
        } else if app.jobs.error().is_some() {
            "failed"
        } else {
            "ready"
        }
        .into(),
        device_name: app.jobs.device_name().into(),
        engine_version: engine_identity(app.jobs.device_name()),
    })
}

async fn circuit_catalog(State(app): State<App>) -> Json<Vec<Circuit>> {
    Json(app.circuits.as_ref().clone())
}

async fn create_run(
    State(app): State<App>,
    Json(request): Json<CreateRunRequest>,
) -> ApiResult<(StatusCode, Json<RunDetail>)> {
    available(&app)?;
    request.config.validate().map_err(ApiError::bad)?;
    Uuid::parse_str(&request.request_id).map_err(ApiError::bad)?;
    if request.name.trim().is_empty() || request.name.chars().count() > 200 {
        return Err(ApiError::bad("Name must contain 1–200 characters"));
    }
    let stages = circuits::tour(&app.circuits, &request.config).map_err(ApiError::bad)?;
    let detail = app
        .jobs
        .store
        .create_run(&request, &stages, app.jobs.device_name())
        .await?;
    app.jobs.wake();
    Ok((StatusCode::ACCEPTED, Json(detail)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecordQuery {
    run_id: String,
}

async fn records(
    State(app): State<App>,
    Query(query): Query<RecordQuery>,
) -> ApiResult<Json<Vec<CircuitRecord>>> {
    Ok(Json(app.jobs.store.records(&query.run_id).await?))
}

async fn ghost(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Response> {
    let data = app
        .jobs
        .store
        .ghost(&id)
        .await?
        .ok_or_else(ApiError::missing)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (
                header::CACHE_CONTROL,
                "private, max-age=31536000, immutable",
            ),
        ],
        data,
    )
        .into_response())
}

async fn runs(State(app): State<App>) -> ApiResult<Json<Vec<RunSummary>>> {
    Ok(Json(app.jobs.store.runs().await?))
}
async fn run(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<RunDetail>> {
    Ok(Json(
        app.jobs
            .store
            .run(&id)
            .await?
            .ok_or_else(ApiError::missing)?,
    ))
}
async fn stop(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<RunDetail>> {
    Ok(Json(app.jobs.stop(&id).await?))
}
async fn resume(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<RunDetail>> {
    available(&app)?;
    let detail = app.jobs.store.resume(&id, app.jobs.device_name()).await?;
    app.jobs.wake();
    Ok(Json(detail))
}
async fn generations(
    State(app): State<App>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<GenerationSummary>>> {
    app.jobs
        .store
        .run(&id)
        .await?
        .ok_or_else(ApiError::missing)?;
    Ok(Json(app.jobs.store.generations(&id).await?))
}

async fn request_replay(
    State(app): State<App>,
    Path((id, generation)): Path<(String, u32)>,
    Json(request): Json<ReplayRequest>,
) -> ApiResult<(StatusCode, Json<ReplayManifest>)> {
    available(&app)?;
    app.jobs.validate_generation(&id, generation).await?;
    let manifest = app
        .jobs
        .store
        .request_replay(&id, generation, request.follow)
        .await?;
    app.jobs.wake();
    Ok((
        if manifest.status == ReplayStatus::Ready {
            StatusCode::OK
        } else {
            StatusCode::ACCEPTED
        },
        Json(manifest),
    ))
}
async fn replay(
    State(app): State<App>,
    Path((id, generation)): Path<(String, u32)>,
) -> ApiResult<Json<ReplayManifest>> {
    Ok(Json(
        app.jobs
            .store
            .replay(&id, generation)
            .await?
            .ok_or_else(ApiError::missing)?,
    ))
}
async fn chunk(
    State(app): State<App>,
    Path((id, generation, index)): Path<(String, u32, u32)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let data = app
        .jobs
        .store
        .chunk(&id, generation, index)
        .await?
        .ok_or_else(ApiError::missing)?;
    let etag = format!("\"{:x}\"", Sha256::digest(&data));
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(&etag)
    {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (
                header::CACHE_CONTROL,
                "private, max-age=31536000, immutable".to_owned(),
            ),
            (header::ETAG, etag),
        ],
        data,
    )
        .into_response())
}
async fn trace(
    State(app): State<App>,
    Path((id, generation, car)): Path<(String, u32, u32)>,
) -> ApiResult<Response> {
    available(&app)?;
    app.jobs.validate_generation(&id, generation).await?;
    if let Some(trace) = app.jobs.store.request_trace(&id, generation, car).await? {
        Ok(Json(trace).into_response())
    } else {
        app.jobs.wake();
        Ok((StatusCode::ACCEPTED, Json(json!({"status":"queued"}))).into_response())
    }
}

#[derive(Deserialize)]
struct EventQuery {
    after: Option<i64>,
}

async fn events(
    State(app): State<App>,
    Query(query): Query<EventQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let mut cursor = if let Some(value) = headers.get("last-event-id") {
        value
            .to_str()
            .map_err(ApiError::bad)?
            .parse::<i64>()
            .map_err(ApiError::bad)?
    } else {
        query.after.unwrap_or(0)
    };
    if cursor < 0 {
        return Err(ApiError::bad("Event cursor must be nonnegative"));
    }
    let latest = app.jobs.store.latest_event_id().await?;
    let reset = cursor > latest;
    if reset {
        cursor = latest;
    }
    let mut changed = app.jobs.store.changed.subscribe();
    let mut previews = app.jobs.preview.subscribe();
    let stream = async_stream::stream! {
        if reset {
            yield Ok::<Event,Infallible>(Event::default().event("reset").id(cursor.to_string()).data(json!({"cursor":cursor}).to_string()));
        }
        let initial=previews.borrow_and_update().clone();
        if let Some(snapshot)=initial
            && let Ok(event)=Event::default().event("preview").json_data(&snapshot){yield Ok::<Event,Infallible>(event);
        }
        loop{
            if app.jobs.shutting_down(){break}
            let _=*changed.borrow_and_update();
            match app.jobs.store.events(cursor).await{
                Ok(batch) if !batch.is_empty()=>{
                    for event in batch{
                        if app.jobs.shutting_down(){break}
                        cursor=event.id;
                        yield Ok(Event::default().id(event.id.to_string()).event(event.kind).data(event.data));
                    }
                    continue;
                }
                Ok(_)=>{},
                Err(error)=>{tracing::error!(%error,"SSE database read failed");break}
            }
            tokio::select!{
                result=changed.changed()=>{if result.is_err(){break}},
                result=previews.changed()=>{
                    if result.is_err(){break}
                    let snapshot=previews.borrow_and_update().clone();
                    if let Some(snapshot)=snapshot
                        && let Ok(event)=Event::default().event("preview").json_data(&snapshot){yield Ok(event);
                    }
                },
                _=tokio::time::sleep(Duration::from_millis(500))=>{}
            }
        }
    };
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}
