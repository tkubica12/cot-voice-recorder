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
pub(crate) mod tests;

#[cfg(test)]
#[path = "dictation_http_tests.rs"]
mod dictation_tests;

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

#[derive(Clone, Deserialize)]
pub struct TranscriptSummary {
    pub transcript_id: String,
    pub recording_id: String,
    pub preview: String,
    #[serde(with = "time::serde::rfc3339")]
    pub completed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub character_count: usize,
    #[serde(default = "recording_source")]
    pub source: String,
}

impl fmt::Debug for TranscriptSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TranscriptSummary(<redacted>)")
    }
}

#[derive(Clone, Deserialize)]
pub struct DictationEdit {
    pub original: String,
    pub replacement: String,
}

#[derive(Clone, Deserialize)]
pub struct DictationRefinement {
    pub text: String,
    pub edits: Vec<DictationEdit>,
}

impl fmt::Debug for DictationRefinement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DictationRefinement(<redacted>)")
    }
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

struct RequestPolicy<'a> {
    retries: Option<usize>,
    timeout: Duration,
    email: Option<&'a str>,
}

impl<P: TokenPersistence> ApiClient<P> {
    async fn binding_changed(&self, expected: &str) -> ApiError {
        if self.auth.current_email().await.as_deref() == Some(expected) {
            Self::failure(ApiErrorKind::Retryable, None, false)
        } else {
            Self::failure(ApiErrorKind::Unauthorized, Some(401), false)
        }
    }

    pub fn matches_backend_label(&self, label: &str) -> bool {
        url::Url::parse(label.trim_end_matches('/')).is_ok_and(|base| base == self.base)
    }

    pub async fn recovery_context(&self, backend_label: &str) -> Option<String> {
        self.auth
            .current_email()
            .await
            .map(|email| format!("{backend_label}\n{email}"))
    }

    #[cfg(test)]
    pub(crate) fn auth_for_tests(&self) -> Arc<AuthRuntime<P>> {
        self.auth.clone()
    }

    pub async fn transcribe_dictation(
        &self,
        wav: &[u8],
        language: &str,
    ) -> Result<String, ApiError> {
        self.transcribe_with_email(wav, language, None).await
    }

    pub async fn transcribe_bound_dictation(
        &self,
        wav: &[u8],
        language: &str,
        email: &str,
    ) -> Result<String, ApiError> {
        self.transcribe_with_email(wav, language, Some(email)).await
    }

    async fn transcribe_with_email(
        &self,
        wav: &[u8],
        language: &str,
        email: Option<&str>,
    ) -> Result<String, ApiError> {
        if !matches!(language, "auto" | "cs" | "en") || wav.len() > 44 + 32_000 * 30 {
            return Err(Self::failure(ApiErrorKind::BadRequest, None, false));
        }
        voiceprompt_core::wav::validate_capture_wav(wav, voiceprompt_core::wav::ParseMode::Strict)
            .map_err(|_| Self::failure(ApiErrorKind::BadRequest, None, false))?;
        #[derive(Deserialize)]
        struct TextResponse {
            text: String,
        }
        let result: TextResponse = self
            .send_payload(
                Method::POST,
                &format!("/v1/dictation/transcribe?language={language}"),
                Some(wav.to_vec()),
                "audio/wav",
                RequestPolicy {
                    retries: Some(1),
                    timeout: Duration::from_secs(25),
                    email,
                },
            )
            .await?;
        if result.text.len() > crate::recovery::MAX_TEXT_BYTES {
            return Err(Self::failure(ApiErrorKind::Unexpected, Some(200), true));
        }
        Ok(result.text)
    }

    pub async fn refine_dictation(
        &self,
        text: &str,
        previous_text: &str,
    ) -> Result<DictationRefinement, ApiError> {
        if text.trim().is_empty()
            || text.encode_utf16().count() > 4000
            || previous_text.encode_utf16().count() > 8000
        {
            return Err(Self::failure(ApiErrorKind::BadRequest, None, false));
        }
        let body =
            serde_json::to_vec(&serde_json::json!({"text":text,"previous_text":previous_text}))
                .map_err(|_| Self::failure(ApiErrorKind::BadRequest, None, false))?;
        if body.len() > 32 * 1024 {
            return Err(Self::failure(ApiErrorKind::BadRequest, None, false));
        }
        let result: DictationRefinement = self
            .send_payload(
                Method::POST,
                "/v1/dictation/refine",
                Some(body),
                "application/json",
                RequestPolicy {
                    retries: Some(0),
                    timeout: Duration::from_secs(25),
                    email: None,
                },
            )
            .await?;
        if result.text.trim().is_empty()
            || result.text.encode_utf16().count() > 4000
            || result.edits.len() > 64
            || result.edits.iter().any(|edit| {
                edit.original.is_empty()
                    || edit.original.encode_utf16().count() > 512
                    || edit.replacement.encode_utf16().count() > 512
            })
        {
            return Err(Self::failure(ApiErrorKind::Unexpected, Some(200), true));
        }
        Ok(result)
    }

    pub async fn archive_dictation(
        &self,
        state: &crate::recovery::RecoveryManifest,
    ) -> Result<TranscriptSummary, ApiError> {
        state
            .validate()
            .map_err(|_| Self::failure(ApiErrorKind::BadRequest, None, false))?;
        let audit = state
            .audit
            .as_ref()
            .ok_or_else(|| Self::failure(ApiErrorKind::BadRequest, None, false))?;
        let completed_at = OffsetDateTime::from_unix_timestamp_nanos(
            (i128::from(audit.completed_at_ticks)
                - i128::from(crate::recovery::DOTNET_UNIX_EPOCH_TICKS))
                * 100,
        )
        .map_err(|_| Self::failure(ApiErrorKind::BadRequest, None, false))?;
        let model = if state.used_fallback {
            if state.used_streaming {
                "MAI-Transcribe-2-Streaming+MAI-Transcribe-2"
            } else {
                "MAI-Transcribe-2"
            }
        } else {
            "MAI-Transcribe-2-Streaming"
        };
        let body = serde_json::to_vec(&serde_json::json!({
            "text":audit.text, "raw_text":state.confirmed_text, "language":state.language,
            "completed_at":completed_at.format(&time::format_description::well_known::Rfc3339)
                .map_err(|_| Self::failure(ApiErrorKind::BadRequest, None, false))?,
            "polished":audit.polished, "transcribe_model":model,
        }))
        .map_err(|_| Self::failure(ApiErrorKind::BadRequest, None, false))?;
        let receipt: TranscriptSummary = self
            .send_json(
                Method::PUT,
                &format!("/v1/dictation/transcripts/{}", state.id),
                Some(body),
                None,
            )
            .await?;
        let id = format!(
            "{}-{}-{}-{}-{}",
            &state.id[..8],
            &state.id[8..12],
            &state.id[12..16],
            &state.id[16..20],
            &state.id[20..]
        );
        if receipt.transcript_id != id
            || receipt.recording_id != id
            || receipt.source != "windows_dictation"
            || receipt.expires_at <= receipt.completed_at
            || receipt.expires_at - receipt.completed_at != time::Duration::hours(48)
            || receipt.completed_at.unix_timestamp_nanos().div_euclid(1000)
                != completed_at.unix_timestamp_nanos().div_euclid(1000)
        {
            return Err(Self::failure(ApiErrorKind::Unexpected, Some(200), true));
        }
        Ok(receipt)
    }

    pub async fn connect_dictation(
        &self,
        language: &str,
        start_byte: u64,
    ) -> Result<crate::dictation_stream::DictationConnection, ApiError> {
        crate::dictation_stream::connect(
            self.base.clone(),
            self.auth.clone(),
            language,
            start_byte,
            None,
        )
        .await
    }

    pub fn auth_state(&self) -> tokio::sync::watch::Receiver<crate::state::AuthState> {
        self.auth.status()
    }

    pub(crate) fn auth_failure(error: AuthRuntimeError<P::Error>) -> ApiError {
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

    pub(crate) fn failure(
        kind: ApiErrorKind,
        status: Option<u16>,
        response_invalid: bool,
    ) -> ApiError {
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

    pub async fn connect_bound_dictation(
        &self,
        language: &str,
        start_byte: u64,
        email: &str,
    ) -> Result<crate::dictation_stream::DictationConnection, ApiError> {
        crate::dictation_stream::connect(
            self.base.clone(),
            self.auth.clone(),
            language,
            start_byte,
            Some(email),
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
        self.send_payload(
            method,
            path,
            body,
            "application/json",
            RequestPolicy {
                retries: retry_limit,
                timeout: Duration::from_secs(60),
                email: None,
            },
        )
        .await
    }

    async fn send_payload<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        content_type: &str,
        policy: RequestPolicy<'_>,
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
        let limit = policy.retries.unwrap_or(self.max_retries);
        loop {
            if let Some(expected) = policy.email {
                if self.auth.current_email().await.as_deref() != Some(expected) {
                    return Err(Self::failure(ApiErrorKind::Unauthorized, Some(401), false));
                }
            }
            let credential = self
                .auth
                .authorized_session()
                .await
                .map_err(Self::auth_failure)?
                .ok_or_else(|| Self::failure(ApiErrorKind::Unauthorized, Some(401), false))?;
            if policy
                .email
                .is_some_and(|expected| credential.email.as_deref() != Some(expected))
            {
                return Err(Self::failure(ApiErrorKind::Unauthorized, Some(401), false));
            }
            let mut request = self
                .client
                .request(method.clone(), uri.clone())
                .timeout(policy.timeout)
                .bearer_auth(credential.token);
            if let Some(body) = &body {
                request = request
                    .header("Content-Type", content_type)
                    .body(body.clone());
            }
            let mut auth_state = credential.state;
            let response = if let Some(expected) = policy.email {
                if auth_state
                    .has_changed()
                    .map_err(|_| Self::failure(ApiErrorKind::Unauthorized, Some(401), false))?
                {
                    return Err(self.binding_changed(expected).await);
                }
                tokio::select! {
                    result = request.send() => result,
                    _ = auth_state.changed() => return Err(self.binding_changed(expected).await),
                }
            } else {
                request.send().await
            };
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
            let read_body = async {
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
                Ok(data)
            };
            let read = if let Some(expected) = policy.email {
                tokio::select! {
                    result = read_body => result,
                    _ = auth_state.changed() => return Err(self.binding_changed(expected).await),
                }
            } else {
                read_body.await
            };
            let data = match read {
                Ok(data) => data,
                Err(error) if error.kind == ApiErrorKind::Retryable && attempt < limit => {
                    attempt += 1;
                    self.delay(attempt).await;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if let Some(expected) = policy.email {
                if auth_state
                    .has_changed()
                    .map_err(|_| Self::failure(ApiErrorKind::Unauthorized, Some(401), false))?
                {
                    return Err(self.binding_changed(expected).await);
                }
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
