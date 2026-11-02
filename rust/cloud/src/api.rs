use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::ws::{CloseFrame, Message};
use axum::extract::{Path, Request, State, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::StreamExt;

use crate::auth::{GoogleVerifier, User};
use crate::models;
use crate::problem::Problem;
use crate::services::{ChunkUpload, Services};

#[derive(Clone)]
enum Verifier {
    Google(Arc<GoogleVerifier>),
    #[cfg(test)]
    Static {
        token: String,
        user: User,
    },
}

#[derive(Clone)]
pub struct ApiState {
    pub services: Arc<Services>,
    verifier: Verifier,
    pub ready: Arc<AtomicBool>,
    pub realtime: Option<Arc<crate::gateway::WebPubSub>>,
    pub ai: Option<Arc<crate::ai::AzureAi>>,
    dictation_pool: Arc<crate::ai::ExecutionPool>,
    refinement_pool: Arc<crate::ai::ExecutionPool>,
    pub streaming: Option<Arc<crate::streaming::MaiStreamingProvider>>,
    stream_slots: Arc<tokio::sync::Semaphore>,
    pub shutdown: tokio::sync::watch::Sender<bool>,
}
impl ApiState {
    pub fn new(services: Arc<Services>, verifier: Arc<GoogleVerifier>) -> Self {
        Self {
            services,
            verifier: Verifier::Google(verifier),
            ready: Arc::new(AtomicBool::new(false)),
            realtime: None,
            ai: None,
            dictation_pool: Arc::new(crate::ai::ExecutionPool::new(
                4,
                std::time::Duration::from_secs(20),
            )),
            refinement_pool: Arc::new(crate::ai::ExecutionPool::new(
                2,
                std::time::Duration::from_secs(20),
            )),
            streaming: None,
            stream_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            shutdown: tokio::sync::watch::channel(false).0,
        }
    }
    async fn user(&self, headers: &HeaderMap) -> Result<User, Problem> {
        let bearer = headers
            .get("Authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer") && !token.is_empty())
            .map(|(_, token)| token)
            .ok_or_else(Problem::unauthorized)?;
        match &self.verifier {
            Verifier::Google(verifier) => verifier.verify(bearer).await,
            #[cfg(test)]
            Verifier::Static { token, user } => {
                if bearer == token {
                    Ok(user.clone())
                } else {
                    Err(Problem::unauthorized())
                }
            }
        }
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route(
            "/health/live",
            get(|| async { Json(serde_json::json!({"status":"ok"})) }),
        )
        .route("/health/ready", get(ready))
        .route("/v1/recordings", post(create_recording))
        .route("/v1/recordings/{recording_id}", get(recording))
        .route("/v1/recordings/{recording_id}/chunks/{index}", put(upload))
        .route("/v1/recordings/{recording_id}/complete", post(complete))
        .route("/v1/transcripts", get(list_transcripts))
        .route("/v1/transcripts/{transcript_id}", get(transcript))
        .route("/v1/dictation/transcripts/{session_id}", put(archive))
        .route("/v1/realtime/negotiate", post(negotiate))
        .route("/v1/dictation/transcribe", post(transcribe))
        .route("/v1/dictation/refine", post(refine))
        .route("/v1/dictation/stream", get(stream))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn(trace))
        .with_state(state)
}
async fn ready(State(state): State<ApiState>) -> Response {
    if state.ready.load(Ordering::Acquire) {
        Json(serde_json::json!({"status":"ok"})).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"status":"not_ready"})),
        )
            .into_response()
    }
}
async fn trace(mut request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get("X-Request-Id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 1024)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            crate::services::uuid().unwrap_or_else(|error| {
                eprintln!("Trace ID generation failed: {error}");
                "trace-id-unavailable".into()
            })
        });
    let dictation = request.uri().path().starts_with("/v1/dictation/");
    request.extensions_mut().insert(id.clone());
    // Keep an unread rejected body alive until the response's connection policy is decided.
    let (request, pending) = crate::request_body::PendingBody::retain(request);
    let mut response = next.run(request).await;
    if !pending.finish().await {
        response
            .headers_mut()
            .insert("Connection", HeaderValue::from_static("close"));
    }
    if let Ok(header) = HeaderValue::from_str(&id) {
        response.headers_mut().insert("X-Request-Id", header);
    }
    if dictation {
        response
            .headers_mut()
            .insert("Cache-Control", HeaderValue::from_static("no-store"));
    }
    response
}
fn failure(request: &Request, problem: Problem) -> Response {
    if problem.status >= 500 {
        eprintln!("API failure: {problem}");
    }
    problem.response(
        request.uri().path(),
        request
            .extensions()
            .get::<String>()
            .map_or("", String::as_str),
    )
}
fn failure_at(path: &str, trace: &str, problem: Problem) -> Response {
    if problem.status >= 500 {
        eprintln!("API failure: {problem}");
    }
    problem.response(path, trace)
}
fn info(request: &Request) -> (String, String) {
    (
        request.uri().path().into(),
        request
            .extensions()
            .get::<String>()
            .cloned()
            .unwrap_or_default(),
    )
}
async fn body(request: Request, maximum: usize, mime: &str) -> Result<Vec<u8>, Problem> {
    let actual = request
        .headers()
        .get("Content-Type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if actual != mime {
        return Err(Problem::new(
            415,
            "unsupported-media-type",
            "Unsupported media type",
            format!("Request requires {mime}."),
        ));
    }
    read_body(request, maximum).await
}
async fn read_body(request: Request, maximum: usize) -> Result<Vec<u8>, Problem> {
    let oversized = || {
        Problem::new(
            413,
            "payload-too-large",
            "Payload too large",
            "Request body exceeded supported bounds.",
        )
    };
    if request
        .headers()
        .get("Content-Length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(oversized());
    }
    let mut data = Vec::new();
    let mut stream = request.into_body().into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            eprintln!("API request body was interrupted.");
            Problem::new(
                400,
                "bad-request",
                "Bad request",
                "Request body was interrupted.",
            )
        })?;
        if chunk.len() > maximum - data.len() {
            return Err(oversized());
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}
fn encoded(request: &Request) -> bool {
    request
        .headers()
        .get("Content-Encoding")
        .is_some_and(|value| {
            value
                .to_str()
                .map_or(true, |value| !value.eq_ignore_ascii_case("identity"))
        })
}
async fn json<T: serde::de::DeserializeOwned>(
    request: Request,
    maximum: usize,
) -> Result<T, Problem> {
    let data = body(request, maximum, "application/json").await?;
    serde_json::from_slice(&data)
        .map_err(|_| Problem::validation("One or more fields are invalid."))
}
fn fields_json<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, Problem> {
    let mut deserializer = serde_json::Deserializer::from_slice(data);
    let value = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        let path = error.path().to_string();
        let message = error.inner().to_string();
        let quoted = message.split('`').nth(1);
        let field = if message.starts_with("missing field") || message.starts_with("unknown field")
        {
            let name = quoted.unwrap_or("");
            if path == "." || path.is_empty() {
                name.into()
            } else if path.rsplit('.').next() == Some(name) {
                path
            } else {
                format!("{path}.{name}")
            }
        } else if path == "." {
            String::new()
        } else {
            path
        };
        let label = if message.starts_with("missing field") {
            "Field required"
        } else if message.starts_with("unknown field") {
            "Extra inputs are not permitted"
        } else {
            "Invalid field value"
        };
        Problem::field("One or more fields are invalid.", field, label)
    })?;
    deserializer
        .end()
        .map_err(|_| Problem::field("One or more fields are invalid.", "", "Invalid JSON input"))?;
    Ok(value)
}
async fn json_fields<T: serde::de::DeserializeOwned>(
    request: Request,
    maximum: usize,
) -> Result<T, Problem> {
    let mime = request
        .headers()
        .get("Content-Type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !mime.is_empty()
        && mime != "application/json"
        && !(mime.starts_with("application/") && mime.ends_with("+json"))
    {
        return Err(Problem::field(
            "One or more fields are invalid.",
            "",
            "Input should be a valid dictionary or object to extract fields from",
        ));
    }
    fields_json(&read_body(request, maximum).await?)
}
fn optional_header<T>(
    headers: &HeaderMap,
    name: &str,
    message: &str,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Result<Option<T>, Problem> {
    headers
        .get(name)
        .map(|value| {
            value.to_str().ok().and_then(parse).ok_or_else(|| {
                Problem::field(
                    "One or more fields are invalid.",
                    format!("header.{name}"),
                    message,
                )
            })
        })
        .transpose()
}
async fn not_found(request: Request) -> Response {
    failure(&request, Problem::not_found("Not Found"))
}
async fn method_not_allowed(request: Request) -> Response {
    failure(
        &request,
        Problem::new(
            405,
            "method-not-allowed",
            "Method not allowed",
            "Method Not Allowed",
        ),
    )
}
async fn create_recording(State(state): State<ApiState>, request: Request) -> Response {
    if let Err(error) = state.user(request.headers()).await {
        return failure(&request, error);
    }
    let (path, trace) = info(&request);
    let result = async {
        let payload: models::CreateRecording = json_fields(request, 1024 * 1024).await?;
        let (record, created) = state.services.create(payload).await?;
        let body = state.services.wire_recording(&record).await?;
        let mut response = (
            if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            Json(body),
        )
            .into_response();
        if created {
            response.headers_mut().insert(
                "Location",
                HeaderValue::from_str(&format!("/v1/recordings/{}", record.recording_id))
                    .map_err(|_| Problem::internal())?,
            );
        }
        Ok::<_, Problem>(response)
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => failure_at(&path, &trace, error),
    }
}
async fn recording(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    if let Err(error) = state.user(request.headers()).await {
        return failure(&request, error);
    }
    let result = async {
        let record = state.services.recording(&id).await?;
        Ok::<_, Problem>(Json(state.services.wire_recording(&record).await?).into_response())
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => failure(&request, error),
    }
}
async fn upload(
    State(state): State<ApiState>,
    Path((id, index)): Path<(String, String)>,
    request: Request,
) -> Response {
    if let Err(error) = state.user(request.headers()).await {
        return failure(&request, error);
    }
    let (path, trace) = info(&request);
    let headers = request.headers().clone();
    let result = async {
        let index = index.parse::<i64>().map_err(|_| {
            Problem::field(
                "One or more fields are invalid.",
                "path.index",
                "Input should be a valid integer",
            )
        });
        let integer_error = "Input should be a valid integer, unable to parse string as an integer";
        let duration_ms =
            optional_header(&headers, "X-Chunk-Duration-Ms", integer_error, |value| {
                value.trim().parse::<u32>().ok()
            });
        let overlap_ms = optional_header(&headers, "X-Chunk-Overlap-Ms", integer_error, |value| {
            value.trim().parse::<u32>().ok()
        });
        let started_at = optional_header(
            &headers,
            "X-Chunk-Started-At",
            "Input should be a valid datetime",
            |value| {
                time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                    .ok()
            },
        );
        let start_sample =
            optional_header(&headers, "X-Audio-Start-Sample", integer_error, |value| {
                value.trim().parse::<u64>().ok()
            });
        let sample_count =
            optional_header(&headers, "X-Audio-Sample-Count", integer_error, |value| {
                value.trim().parse::<u64>().ok()
            });
        let errors: Vec<_> = [
            index.as_ref().err(),
            duration_ms.as_ref().err(),
            overlap_ms.as_ref().err(),
            started_at.as_ref().err(),
            start_sample.as_ref().err(),
            sample_count.as_ref().err(),
        ]
        .into_iter()
        .flatten()
        .flat_map(|problem| problem.errors.iter().cloned())
        .collect();
        if !errors.is_empty() {
            let mut problem = Problem::validation("One or more fields are invalid.");
            problem.errors = errors;
            return Err(problem);
        }
        let index = index?;
        if index < 0 {
            return Err(Problem::field(
                "Chunk index must be non-negative.",
                "index",
                "must be >= 0",
            ));
        }
        let index = u32::try_from(index)
            .map_err(|_| Problem::validation("Chunk index exceeds supported bounds."))?;
        let wav = body(request, state.services.config.max_chunk_bytes, "audio/wav").await?;
        let checksum = crate::audio::content_digest(
            headers
                .get("Content-Digest")
                .and_then(|value| value.to_str().ok()),
            &wav,
        )?;
        voiceprompt_core::wav::validate_capture_wav(&wav, voiceprompt_core::wav::ParseMode::Legacy)
            .map_err(crate::audio::wav_problem)?;
        let upload = ChunkUpload {
            index,
            wav,
            checksum,
            duration_ms: duration_ms?,
            overlap_ms: overlap_ms?,
            started_at: started_at?,
            start_sample: start_sample?,
            sample_count: sample_count?,
        };
        let (accepted, status) = state.services.upload(&id, upload).await?;
        Ok::<_, Problem>(
            (
                StatusCode::from_u16(status).map_err(|_| Problem::internal())?,
                Json(accepted),
            )
                .into_response(),
        )
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => failure_at(&path, &trace, error),
    }
}
async fn complete(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    if let Err(error) = state.user(request.headers()).await {
        return failure(&request, error);
    }
    let (path, trace) = info(&request);
    let result = async {
        let payload: models::CompleteRecording = json_fields(request, 1024 * 1024).await?;
        if payload.chunk_count == 0 {
            return Err(Problem::field(
                "One or more fields are invalid.",
                "chunk_count",
                "Input should be greater than or equal to 1",
            ));
        }
        let (record, status) = state.services.complete(&id, payload.chunk_count).await?;
        Ok::<_, Problem>(
            (
                StatusCode::from_u16(status).map_err(|_| Problem::internal())?,
                Json(state.services.wire_recording(&record).await?),
            )
                .into_response(),
        )
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => failure_at(&path, &trace, error),
    }
}
async fn transcript(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    if let Err(error) = state.user(request.headers()).await {
        return failure(&request, error);
    }
    match state.services.transcript(&id).await {
        Ok(body) => Json(body).into_response(),
        Err(error) => failure(&request, error),
    }
}
async fn list_transcripts(State(state): State<ApiState>, request: Request) -> Response {
    if let Err(error) = state.user(request.headers()).await {
        return failure(&request, error);
    }
    let uri = url::Url::parse(&format!("https://fixture.invalid{}", request.uri()))
        .map_err(|_| Problem::validation("Invalid query."));
    let result = async {
        let uri = uri?;
        let mut limit = 20;
        let mut cursor = None;
        for (name, value) in uri.query_pairs() {
            if name == "limit" {
                limit = value.parse::<usize>().map_err(|_| {
                    Problem::field(
                        "One or more fields are invalid.",
                        "query.limit",
                        "Input should be a valid integer",
                    )
                })?;
            } else if name == "cursor" {
                cursor = Some(value.into_owned());
            }
            if !(1..=100).contains(&limit) {
                return Err(Problem::field(
                    "One or more fields are invalid.",
                    "query.limit",
                    if limit < 1 {
                        "Input should be greater than or equal to 1"
                    } else {
                        "Input should be less than or equal to 100"
                    },
                ));
            }
        }
        state
            .services
            .list_transcripts(limit, cursor.as_deref())
            .await
    }
    .await;
    match result {
        Ok(body) => Json(body).into_response(),
        Err(error) => failure(&request, error),
    }
}
async fn archive(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    if let Err(error) = state.user(request.headers()).await {
        return failure(&request, error);
    }
    if encoded(&request) {
        return failure(
            &request,
            Problem::new(
                415,
                "unsupported-media-type",
                "Unsupported media type",
                "Encoded audit bodies are not supported.",
            ),
        );
    }
    let (path, trace) = info(&request);
    let result = async {
        let payload: models::ArchiveDictation = json(request, 12 * 1024 * 1024 + 16384).await?;
        let saved = state.services.archive(&id, payload).await?;
        Ok::<_, Problem>(Json(models::summary(&saved)).into_response())
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => failure_at(&path, &trace, error),
    }
}

async fn negotiate(State(state): State<ApiState>, request: Request) -> Response {
    if let Err(error) = state.user(request.headers()).await {
        return failure(&request, error);
    }
    let (path, trace) = info(&request);
    let result = async {
        let bytes = read_body(request, 1024 * 1024).await?;
        if !bytes.is_empty() {
            let _: Option<models::NegotiateRequest> = fields_json(&bytes)?;
        }
        let gateway = state
            .realtime
            .ok_or_else(|| Problem::unavailable("Realtime is not configured."))?;
        gateway.negotiate().await
    }
    .await;
    match result {
        Ok(body) => Json(body).into_response(),
        Err(error) => failure_at(&path, &trace, error),
    }
}

async fn transcribe(State(state): State<ApiState>, request: Request) -> Response {
    if let Err(error) = state.user(request.headers()).await {
        return failure(&request, error);
    }
    if encoded(&request) {
        return failure(
            &request,
            Problem::new(
                415,
                "unsupported-media-type",
                "Unsupported media type",
                "Encoded dictation bodies are not supported.",
            ),
        );
    }
    let (path, trace) = info(&request);
    let uri = url::Url::parse(&format!("https://fixture.invalid{}", request.uri()));
    let result = async {
        let uri = uri.map_err(|_| Problem::validation("Invalid dictation query."))?;
        let mut language = "auto".to_owned();
        for (name, value) in uri.query_pairs() {
            if name == "language" {
                language = value.into_owned();
            }
        }
        if !matches!(language.as_str(), "auto" | "cs" | "en") {
            return Err(Problem::field(
                "One or more fields are invalid.",
                "query.language",
                "Input should be 'auto', 'cs' or 'en'",
            ));
        }
        let wav = body(request, 320000 + 4096, "audio/wav").await?;
        let properties = voiceprompt_core::wav::validate_capture_wav(
            &wav,
            voiceprompt_core::wav::ParseMode::Strict,
        )
        .map_err(crate::audio::wav_problem)?;
        if properties.data_bytes > 320000 {
            return Err(Problem::validation(
                "Dictation audio must be no longer than ten seconds.",
            ));
        }
        let ai = state
            .ai
            .ok_or_else(|| Problem::unavailable("Dictation AI is not configured."))?;
        state
            .dictation_pool
            .execute(async move {
                ai.transcribe(
                    wav,
                    crate::ai::TranscriptionHints {
                        language,
                        prompt: String::new(),
                        phrases: Vec::new(),
                    },
                    true,
                )
                .await
            })
            .await
    }
    .await;
    match result {
        Ok(text) => Json(serde_json::json!({"text":text})).into_response(),
        Err(error) => failure_at(&path, &trace, error),
    }
}

async fn refine(State(state): State<ApiState>, request: Request) -> Response {
    if let Err(error) = state.user(request.headers()).await {
        return failure(&request, error);
    }
    if encoded(&request) {
        return failure(
            &request,
            Problem::new(
                415,
                "unsupported-media-type",
                "Unsupported media type",
                "Encoded dictation bodies are not supported.",
            ),
        );
    }
    let (path, trace) = info(&request);
    let result = async {
        let request: models::RefineDictation = json(request, 32768).await?;
        if request.text.chars().all(crate::edits::python_whitespace)
            || request.text.chars().count() > 4000
            || request.previous_text.chars().count() > 8000
        {
            return Err(Problem::validation("Invalid dictation refinement window."));
        }
        let ai = state
            .ai
            .ok_or_else(|| Problem::unavailable("Refinement AI is not configured."))?;
        state
            .refinement_pool
            .execute(async move {
                ai.refine_dictation(request.text, request.previous_text)
                    .await
            })
            .await
    }
    .await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(error) => failure_at(&path, &trace, error),
    }
}

async fn stream(
    State(state): State<ApiState>,
    upgrade: WebSocketUpgrade,
    request: Request,
) -> Response {
    let authorized = state.user(request.headers()).await;
    let uri = url::Url::parse(&format!("https://fixture.invalid{}", request.uri()));
    let mut language = "auto".to_owned();
    let mut start = Some(0);
    if let Ok(uri) = uri {
        for (key, value) in uri.query_pairs() {
            match key.as_ref() {
                "language" => language = value.into_owned(),
                "start_byte" => {
                    start = value
                        .trim_matches(crate::edits::python_whitespace)
                        .parse::<i64>()
                        .ok()
                        .and_then(|value| u64::try_from(value).ok())
                }
                _ => {}
            }
        }
    } else {
        start = None;
    }
    upgrade
        .max_message_size(16384)
        .max_frame_size(16384)
        .on_upgrade(move |mut client| async move {
            let reject = match authorized {
                Err(error) => Some((
                    if error.status == 403 {
                        "forbidden"
                    } else {
                        "unauthorized"
                    },
                    if error.status == 403 { 4403 } else { 4401 },
                )),
                Ok(_)
                    if !matches!(language.as_str(), "auto" | "cs" | "en")
                        || !start
                            .is_some_and(|value| value <= 48 * 3600 * 32000 && value % 2 == 0) =>
                {
                    Some(("invalid_request", 4400))
                }
                Ok(_)
                    if state.streaming.is_none()
                        || !state.services.config.dictation_stream_enabled =>
                {
                    Some(("unavailable", 4503))
                }
                Ok(_) => None,
            };
            if let Some((code, close_code)) = reject {
                reject_stream(&mut client, code, close_code).await;
                return;
            }
            let Ok(_permit) = state.stream_slots.clone().try_acquire_owned() else {
                reject_stream(&mut client, "too_many_requests", 4429).await;
                return;
            };
            let Some(provider) = state.streaming else {
                reject_stream(&mut client, "unavailable", 4503).await;
                return;
            };
            let Some(start) = start else {
                reject_stream(&mut client, "invalid_request", 4400).await;
                return;
            };
            let mut shutdown=state.shutdown.subscribe();
            let result={
            let work = async {
                let mut upstream = provider.session(&language).await?;
                let result = async {
                    crate::relay::send(
                        &mut client,
                        serde_json::json!({"type":"ready","start_byte":start,"protocol":1}),
                    )
                    .await?;
                    crate::relay::relay(&mut client, &mut upstream, start).await
                }
                .await;
                if upstream.close().await.is_err() {
                    eprintln!("MAI streaming close was interrupted.");
                }
                result
            };
            if *shutdown.borrow(){Err(Problem::unavailable("Server is shutting down."))}else{
                tokio::select!{result=work=>result,_=shutdown.changed()=>Err(Problem::unavailable("Server is shutting down."))}
            }
            };
            if result.is_err() {
                eprintln!("Dictation stream failed; saved audio remains replayable.");
                if crate::relay::send(
                    &mut client,
                    serde_json::json!({"type":"error","code":"stream_failed",
                "message":"Streaming interrupted. Saved audio can be replayed."}),
                )
                .await
                .is_err()
                {
                    eprintln!("Dictation stream error delivery failed.");
                }
                close_stream(&mut client, 1011).await;
            } else {
                close_stream(&mut client, 1000).await;
            }
        })
}
async fn close_stream(client: &mut axum::extract::ws::WebSocket, code: u16) {
    if !matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            client.send(Message::Close(Some(CloseFrame {
                code,
                reason: "".into()
            })))
        )
        .await,
        Ok(Ok(()))
    ) {
        eprintln!("Dictation stream close delivery failed.");
    }
}
async fn reject_stream(client: &mut axum::extract::ws::WebSocket, code: &str, close_code: u16) {
    if crate::relay::send(client, serde_json::json!({"type":"error","code":code}))
        .await
        .is_err()
    {
        eprintln!("Dictation stream rejection delivery failed.");
    }
    close_stream(client, close_code).await;
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
