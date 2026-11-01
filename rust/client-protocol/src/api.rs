use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;

use crate::runtime::{AuthRuntime, AuthRuntimeError, TokenPersistence};

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiErrorKind {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Retryable,
    Unexpected,
}

#[derive(Clone, Deserialize, Default)]
pub struct ProblemDetails {
    pub title: Option<String>,
    pub status: Option<u16>,
    pub detail: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub instance: Option<String>,
}

pub struct ApiError {
    pub kind: ApiErrorKind,
    pub status: Option<u16>,
    pub problem: Option<ProblemDetails>,
    pub response_invalid: bool,
}

impl fmt::Debug for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiError")
            .field("kind", &self.kind)
            .field("status", &self.status)
            .field("response_invalid", &self.response_invalid)
            .finish()
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Backend API {:?} (status {:?}).", self.kind, self.status)
    }
}

impl std::error::Error for ApiError {}

pub fn classify_status(status: u16) -> ApiErrorKind {
    match status {
        400 => ApiErrorKind::BadRequest,
        401 => ApiErrorKind::Unauthorized,
        403 => ApiErrorKind::Forbidden,
        404 => ApiErrorKind::NotFound,
        408 | 429 | 500..=599 => ApiErrorKind::Retryable,
        _ => ApiErrorKind::Unexpected,
    }
}

#[derive(Clone, Deserialize)]
pub struct Transcript {
    pub transcript_id: String,
    pub recording_id: String,
    pub body: String,
    pub preview: String,
    #[serde(with = "time::serde::rfc3339")]
    pub completed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub character_count: usize,
    pub language: Option<String>,
    pub refine_model: Option<String>,
    #[serde(default = "recording_source")]
    pub source: String,
    pub raw_body: Option<String>,
    pub transcribe_model: Option<String>,
}

fn recording_source() -> String {
    "recording".into()
}

impl fmt::Debug for Transcript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Transcript(<redacted>)")
    }
}

#[derive(Clone, Deserialize)]
pub struct NegotiateResponse {
    pub url: String,
    pub hub: String,
    pub group: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

impl fmt::Debug for NegotiateResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NegotiateResponse(<redacted>)")
    }
}

pub struct ApiClient<P: TokenPersistence> {
    client: reqwest::Client,
    auth: Arc<AuthRuntime<P>>,
    base: url::Url,
    max_retries: usize,
    retry_delay: Duration,
}

impl<P: TokenPersistence> ApiClient<P> {
    pub fn auth_state(&self) -> tokio::sync::watch::Receiver<crate::state::AuthState> {
        self.auth.status()
    }

    fn auth_failure(error: AuthRuntimeError<P::Error>) -> ApiError {
        let kind = match error {
            AuthRuntimeError::Network(_) => ApiErrorKind::Retryable,
            AuthRuntimeError::Storage(_) | AuthRuntimeError::Worker(_) => ApiErrorKind::Unexpected,
            _ => ApiErrorKind::Unauthorized,
        };
        Self::failure(
            kind,
            (kind == ApiErrorKind::Unauthorized).then_some(401),
            false,
        )
    }
    pub fn new(base: url::Url, auth: Arc<AuthRuntime<P>>) -> Result<Self, ApiError> {
        if base.scheme() != "https"
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(Self::failure(ApiErrorKind::BadRequest, None, false));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Self::failure(ApiErrorKind::Unexpected, None, false))?;
        Ok(Self {
            client,
            auth,
            base,
            max_retries: 3,
            retry_delay: Duration::from_millis(250),
        })
    }

    fn failure(kind: ApiErrorKind, status: Option<u16>, response_invalid: bool) -> ApiError {
        ApiError {
            kind,
            status,
            problem: None,
            response_invalid,
        }
    }

    pub async fn negotiate(&self, version: &str) -> Result<NegotiateResponse, ApiError> {
        let payload = serde_json::json!({"client":{"platform":"windows","app_version":version}});
        self.send_json(
            Method::POST,
            "/v1/realtime/negotiate",
            Some(payload.to_string().into_bytes()),
            None,
        )
        .await
    }

    pub async fn transcript(&self, id: &str) -> Result<Transcript, ApiError> {
        if id.is_empty() || id.len() > 128 {
            return Err(Self::failure(ApiErrorKind::BadRequest, None, false));
        }
        let mut uri = self.base.clone();
        uri.set_path("/v1/transcripts/");
        uri.path_segments_mut()
            .map_err(|_| Self::failure(ApiErrorKind::BadRequest, None, false))?
            .pop_if_empty()
            .push(id);
        let path = uri.path().to_owned();
        let transcript: Transcript = self.send_json(Method::GET, &path, None, None).await?;
        if transcript.transcript_id != id || transcript.expires_at <= transcript.completed_at {
            return Err(Self::failure(ApiErrorKind::Unexpected, Some(200), true));
        }
        Ok(transcript)
    }

    async fn delay(&self, attempt: usize) {
        let multiplier = 1u32.checked_shl(attempt.min(16) as u32).unwrap_or(u32::MAX);
        tokio::time::sleep(
            self.retry_delay
                .saturating_mul(multiplier)
                .min(Duration::from_secs(4)),
        )
        .await;
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        retry_limit: Option<usize>,
    ) -> Result<T, ApiError> {
        let uri = self
            .base
            .join(path)
            .map_err(|_| Self::failure(ApiErrorKind::BadRequest, None, false))?;
        if uri.origin() != self.base.origin() {
            return Err(Self::failure(ApiErrorKind::BadRequest, None, false));
        }
        let mut refreshed = false;
        let mut attempt = 0;
        let limit = retry_limit.unwrap_or(self.max_retries);
        loop {
            let token = self
                .auth
                .backend_id_token()
                .await
                .map_err(Self::auth_failure)?
                .filter(|value| !value.is_empty())
                .ok_or_else(|| Self::failure(ApiErrorKind::Unauthorized, Some(401), false))?;
            let mut request = self
                .client
                .request(method.clone(), uri.clone())
                .bearer_auth(token);
            if let Some(body) = &body {
                request = request
                    .header("Content-Type", "application/json")
                    .body(body.clone());
            }
            let response = request.send().await;
            let mut response = match response {
                Ok(response) => response,
                Err(_) if attempt < limit => {
                    attempt += 1;
                    self.delay(attempt).await;
                    continue;
                }
                Err(_) => return Err(Self::failure(ApiErrorKind::Retryable, None, false)),
            };
            let status = response.status().as_u16();
            if status == 401 && !refreshed {
                refreshed = true;
                if self
                    .auth
                    .force_refresh()
                    .await
                    .map_err(Self::auth_failure)?
                    .is_some()
                {
                    continue;
                }
            }
            if !(200..300).contains(&status)
                && classify_status(status) == ApiErrorKind::Retryable
                && attempt < limit
            {
                attempt += 1;
                self.delay(attempt).await;
                continue;
            }
            if response
                .content_length()
                .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
            {
                return Err(Self::failure(ApiErrorKind::Unexpected, Some(status), true));
            }
            let mut data = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| Self::failure(ApiErrorKind::Retryable, Some(status), false))?
            {
                if chunk.len() > MAX_RESPONSE_BYTES - data.len() {
                    return Err(Self::failure(ApiErrorKind::Unexpected, Some(status), true));
                }
                data.extend_from_slice(&chunk);
            }
            if (200..300).contains(&status) {
                return serde_json::from_slice(&data)
                    .map_err(|_| Self::failure(ApiErrorKind::Unexpected, Some(status), true));
            }
            let problem = serde_json::from_slice::<ProblemDetails>(&data);
            return Err(ApiError {
                kind: classify_status(status),
                status: Some(status),
                response_invalid: problem.is_err(),
                problem: problem.ok(),
            });
        }
    }
}
