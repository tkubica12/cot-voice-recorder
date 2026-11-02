use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::identity::ManagedIdentity;
use crate::models::Refinement;
use crate::problem::Problem;

pub const DICTATION_PROMPT: &str = include_str!("../resources/dictation-system.txt");
pub const RECORDING_PROMPT: &str = include_str!("../resources/recording-system.txt");

#[cfg(test)]
#[path = "ai_tests.rs"]
mod provider_tests;

pub struct TranscriptionHints {
    pub language: String,
    pub prompt: String,
    pub phrases: Vec<String>,
}

pub struct AzureAi {
    pub config: Arc<Config>,
    pub identity: Arc<ManagedIdentity>,
    http: reqwest::Client,
}
impl AzureAi {
    pub fn new(config: Arc<Config>, identity: Arc<ManagedIdentity>) -> Result<Self, Problem> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Problem::internal())?;
        Ok(Self {
            config,
            identity,
            http,
        })
    }
    fn path(base: &url::Url, pieces: &[&str]) -> Result<url::Url, Problem> {
        let mut url = base.clone();
        let mut path = url
            .path_segments_mut()
            .map_err(|_| Problem::validation("Invalid AI resource endpoint."))?;
        path.pop_if_empty();
        for piece in pieces {
            path.push(piece);
        }
        drop(path);
        Ok(url)
    }
    fn status(status: reqwest::StatusCode) -> Result<(), Problem> {
        if !status.is_success() {
            return Err(
                if matches!(status.as_u16(), 408 | 409 | 429 | 500 | 502 | 503 | 504) {
                    Problem::unavailable("AI provider is temporarily unavailable.")
                } else {
                    Problem::new(
                        502,
                        "bad-gateway",
                        "Bad gateway",
                        "AI provider rejected the request.",
                    )
                },
            );
        }
        Ok(())
    }
    async fn bytes(mut response: reqwest::Response) -> Result<Vec<u8>, Problem> {
        Self::status(response.status())?;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Problem::unavailable("AI provider response was interrupted."))?
        {
            if chunk.len() > 4 * 1024 * 1024 - body.len() {
                return Err(Problem::new(
                    502,
                    "bad-gateway",
                    "Bad gateway",
                    "AI response exceeded supported bounds.",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
    async fn json(response: reqwest::Response) -> Result<serde_json::Value, Problem> {
        serde_json::from_slice(&Self::bytes(response).await?).map_err(|_| {
            Problem::new(
                502,
                "bad-gateway",
                "Bad gateway",
                "AI provider returned malformed JSON.",
            )
        })
    }
    pub async fn transcribe(
        &self,
        wav: Vec<u8>,
        hints: TranscriptionHints,
        dictation: bool,
    ) -> Result<String, Problem> {
        if self.config.transcribe_provider != "azure_speech" {
            if dictation {
                return Err(Problem::unavailable(
                    "Dictation requires the azure_speech provider.",
                ));
            }
            return self.openai_transcribe(wav, hints).await;
        }
        if dictation && !self.config.speech_model.starts_with("MAI-Transcribe-") {
            return Err(Problem::unavailable(
                "Dictation requires a configured MAI transcription model.",
            ));
        }
        let endpoint = self
            .config
            .speech
            .as_ref()
            .ok_or_else(|| Problem::unavailable("Azure Speech is not configured."))?;
        let mut url = Self::path(endpoint, &["speechtotext", "transcriptions:transcribe"])?;
        url.query_pairs_mut()
            .append_pair("api-version", &self.config.speech_api_version);
        let mut definition = serde_json::json!({
            "enhancedMode":{"enabled":true,"model":self.config.speech_model,
                "modelOptions":{"transcribeStyle":if dictation {"clean"} else {&self.config.speech_style}}},
        });
        if hints.language != "auto" {
            definition["locales"] = serde_json::json!([hints.language]);
        }
        if !hints.phrases.is_empty() {
            definition["phraseList"] = serde_json::json!({"phrases": hints.phrases});
        }
        let form = reqwest::multipart::Form::new()
            .part(
                "audio",
                reqwest::multipart::Part::bytes(wav)
                    .file_name("chunk.wav")
                    .mime_str("audio/wav")
                    .map_err(|_| Problem::internal())?,
            )
            .part(
                "definition",
                reqwest::multipart::Part::text(definition.to_string())
                    .mime_str("application/json")
                    .map_err(|_| Problem::internal())?,
            );
        let token = self
            .identity
            .token("https://cognitiveservices.azure.com/")
            .await?;
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .multipart(form)
            .timeout(Duration::from_secs_f64(if dictation {
                20.0
            } else {
                self.config.speech_timeout
            }))
            .send()
            .await
            .map_err(|_| Problem::unavailable("Speech request failed or timed out."))?;
        let payload = Self::json(response).await?;
        let phrases = payload["combinedPhrases"].as_array().ok_or_else(|| {
            Problem::new(
                502,
                "bad-gateway",
                "Bad gateway",
                "Speech returned malformed phrases.",
            )
        })?;
        let mut text = Vec::new();
        for phrase in phrases {
            let value = phrase.get("text").and_then(serde_json::Value::as_str);
            if dictation && value.is_none() {
                return Err(Problem::new(
                    502,
                    "bad-gateway",
                    "Bad gateway",
                    "Speech returned malformed phrases.",
                ));
            }
            if let Some(value) = value
                .map(|value| value.trim_matches(crate::edits::python_whitespace))
                .filter(|value| !value.is_empty())
            {
                text.push(value);
            }
        }
        Ok(text.join(" "))
    }

    async fn openai_transcribe(
        &self,
        wav: Vec<u8>,
        hints: TranscriptionHints,
    ) -> Result<String, Problem> {
        let mut url = Self::path(
            &self.config.foundry,
            &[
                "openai",
                "deployments",
                &self.config.transcribe_deployment,
                "audio",
                "transcriptions",
            ],
        )?;
        url.query_pairs_mut()
            .append_pair("api-version", &self.config.foundry_api_version);
        let form = reqwest::multipart::Form::new()
            .text("model", self.config.transcribe_deployment.clone())
            .text("language", hints.language)
            .text("response_format", "text")
            .text("prompt", hints.prompt)
            .part(
                "file",
                reqwest::multipart::Part::bytes(wav)
                    .file_name("chunk.wav")
                    .mime_str("audio/wav")
                    .map_err(|_| Problem::internal())?,
            );
        let token = self
            .identity
            .token("https://cognitiveservices.azure.com/")
            .await?;
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .map_err(|_| Problem::unavailable("OpenAI transcription request failed."))?;
        let data = Self::bytes(response).await?;
        String::from_utf8(data).map_err(|_| {
            Problem::new(
                502,
                "bad-gateway",
                "Bad gateway",
                "OpenAI transcription returned invalid text.",
            )
        })
    }
    pub async fn refine_recording(
        &self,
        raw_text: &str,
        deployment: &str,
    ) -> Result<String, Problem> {
        let mut url = Self::path(
            &self.config.foundry,
            &["openai", "deployments", deployment, "chat", "completions"],
        )?;
        url.query_pairs_mut()
            .append_pair("api-version", &self.config.foundry_api_version);
        let token = self
            .identity
            .token("https://cognitiveservices.azure.com/")
            .await?;
        let body = serde_json::json!({"model":deployment, "messages":[
            {"role":"system", "content":RECORDING_PROMPT.replace("\r\n", "\n").trim_end_matches('\n')},
            {"role":"user", "content":format!("Clean up the following raw transcript according to the rules. Return only the cleaned transcript.\n\n<transcript>\n{raw_text}\n</transcript>")}
        ]});
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(&body)
            .timeout(Duration::from_secs(600))
            .send()
            .await
            .map_err(|_| Problem::unavailable("Recording refinement request failed."))?;
        let payload = Self::json(response).await?;
        payload["choices"]
            .as_array()
            .and_then(|choices| choices.first())
            .and_then(|choice| choice["message"]["content"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                Problem::new(
                    502,
                    "bad-gateway",
                    "Bad gateway",
                    "Recording refinement returned no text.",
                )
            })
    }
    pub async fn refine_dictation(
        &self,
        text: String,
        previous: String,
    ) -> Result<Refinement, Problem> {
        let mut url = Self::path(
            &self.config.foundry,
            &[
                "openai",
                "deployments",
                &self.config.refinement,
                "chat",
                "completions",
            ],
        )?;
        url.query_pairs_mut()
            .append_pair("api-version", &self.config.foundry_api_version);
        let request = serde_json::json!({
            "model":self.config.refinement,
            "messages":[{"role":"system","content":DICTATION_PROMPT.replace("\r\n", "\n").trim_end_matches('\n')},
                {"role":"user","content":serde_json::json!({"current":text,"previous":previous}).to_string()}],
            "reasoning_effort":"none","response_format":{"type":"json_object"},"max_completion_tokens":2048,
        });
        let token = self
            .identity
            .token("https://cognitiveservices.azure.com/")
            .await?;
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(&request)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| Problem::unavailable("Refinement request failed or timed out."))?;
        let payload = Self::json(response).await?;
        let choices = payload["choices"]
            .as_array()
            .filter(|choices| choices.len() == 1)
            .ok_or_else(|| {
                Problem::new(
                    502,
                    "bad-gateway",
                    "Bad gateway",
                    "Refinement choices are invalid.",
                )
            })?;
        let choice = &choices[0];
        if choice["finish_reason"].as_str() != Some("stop")
            || choice["message"]["refusal"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        {
            return Err(Problem::new(
                502,
                "bad-gateway",
                "Bad gateway",
                "Refinement did not complete.",
            ));
        }
        let edits = choice["message"]["content"].as_str().ok_or_else(|| {
            Problem::new(
                502,
                "bad-gateway",
                "Bad gateway",
                "Refinement returned no text.",
            )
        })?;
        crate::edits::validate(&text, edits, &previous).map_err(|error| {
            eprintln!("Refinement rejected: {}", error.0);
            Problem::new(
                502,
                "bad-gateway",
                "Bad gateway",
                "Refinement returned unusable edits.",
            )
        })
    }
}

pub struct ExecutionPool {
    slots: Arc<Semaphore>,
    timeout: Duration,
}
impl ExecutionPool {
    pub fn new(slots: usize, timeout: Duration) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(slots)),
            timeout,
        }
    }
    pub async fn execute<T: Send + 'static>(
        &self,
        work: impl Future<Output = Result<T, Problem>> + Send + 'static,
    ) -> Result<T, Problem> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            Problem::new(
                429,
                "too-many-requests",
                "Too many requests",
                "All model-call slots are occupied.",
            )
        })?;
        let task = tokio::spawn(async move {
            let _permit = permit;
            let result = work.await;
            if let Err(error) = &result {
                eprintln!("Model-call failed: {error}");
            }
            result
        });
        match tokio::time::timeout(self.timeout, task).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(Problem::new(
                502,
                "bad-gateway",
                "Bad gateway",
                "Model-call task failed.",
            )),
            Err(_) => Err(Problem::new(
                504,
                "gateway-timeout",
                "Gateway timeout",
                "Model-call deadline exceeded; physical work retains capacity.",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn timeout_does_not_release_capacity_before_the_physical_call_exits() {
        let pool = ExecutionPool::new(1, Duration::from_millis(20));
        let release = Arc::new(tokio::sync::Notify::new());
        let waiting = release.clone();
        let first = pool
            .execute(async move {
                waiting.notified().await;
                Ok(())
            })
            .await;
        assert_eq!(first.err().unwrap().status, 504);
        assert_eq!(
            pool.execute(async { Ok(()) }).await.err().unwrap().status,
            429
        );
        release.notify_one();
        for _ in 0..100 {
            if pool.slots.available_permits() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(pool.execute(async { Ok(()) }).await.is_ok());
    }
}
