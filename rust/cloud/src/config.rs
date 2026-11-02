use crate::problem::Problem;
use std::collections::BTreeMap;
use url::Url;

#[derive(Clone)]
pub struct Config {
    pub environment: String,
    pub audiences: Vec<String>,
    pub issuers: Vec<String>,
    pub jwks: Url,
    pub email: String,
    pub storage_account: String,
    pub foundry: Url,
    pub speech: Option<Url>,
    pub webpubsub: Url,
    pub hub: String,
    pub group: String,
    pub refinement: String,
    pub dictation_stream: String,
    pub recording_stream: String,
    pub use_fakes: bool,
    pub audio_container: String,
    pub transcript_container: String,
    pub raw_container: String,
    pub queue: String,
    pub recordings_table: String,
    pub chunks_table: String,
    pub transcripts_table: String,
    pub foundry_api_version: String,
    pub speech_api_version: String,
    pub speech_model: String,
    pub speech_style: String,
    pub speech_timeout: f64,
    pub transcribe_provider: String,
    pub transcribe_deployment: String,
    pub webpubsub_ttl_minutes: u32,
    pub dictation_stream_enabled: bool,
    pub recording_stream_lease_seconds: u32,
    pub recording_stream_replay_speed: f64,
    pub recording_stream_idle_seconds: f64,
    pub recording_stream_poll_seconds: f64,
    pub recording_stream_heartbeat_seconds: f64,
    pub recording_stream_session_seconds: f64,
    pub queue_visibility_seconds: u32,
    pub glossary_terms: String,
    pub overlap_min_chars: usize,
    pub chunk_grace_seconds: u32,
    pub max_transcribe_attempts: u32,
    pub max_refine_attempts: u32,
    pub queue_retry_base_seconds: u32,
    pub queue_receive_error_backoff_seconds: f64,
    pub queue_receive_error_max_backoff_seconds: f64,
}

fn endpoint(value: &str, allow_loopback: bool) -> Result<Url, Problem> {
    let url = Url::parse(value)
        .map_err(|_| Problem::validation("Invalid server endpoint configuration."))?;
    if (url.scheme() != "https"
        && !(allow_loopback && url.scheme() == "http" && url.host_str() == Some("127.0.0.1")))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Problem::validation(
            "Server endpoints require HTTPS without credentials/query/fragment.",
        ));
    }
    Ok(url)
}

impl Config {
    pub fn from_environment() -> Result<Self, Problem> {
        let values = std::env::vars()
            .filter(|(key, _)| key.starts_with("VR_"))
            .collect();
        Self::parse(&values)
    }
    pub fn parse(values: &BTreeMap<String, String>) -> Result<Self, Problem> {
        let value =
            |key: &str, default: &str| values.get(key).cloned().unwrap_or_else(|| default.into());
        let required = |key: &str| {
            values
                .get(key)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .ok_or_else(|| Problem::validation(format!("{key} is required.")))
        };
        let flag = |key: &str| match values
            .get(key)
            .map(|value| value.to_ascii_lowercase())
            .as_deref()
        {
            None | Some("false" | "0") => Ok(false),
            Some("true" | "1") => Ok(true),
            _ => Err(Problem::validation(format!("{key} must be a boolean."))),
        };
        let environment = value("VR_ENVIRONMENT", "production");
        if !matches!(environment.as_str(), "production" | "local" | "test") {
            return Err(Problem::validation("Invalid VR_ENVIRONMENT."));
        }
        let fake_ai = flag("VR_USE_FAKE_AI")?;
        let fake_storage = flag("VR_USE_FAKE_STORAGE")?;
        let fake_realtime = flag("VR_USE_FAKE_REALTIME")?;
        let auth_disabled = flag("VR_AUTH_DISABLED")?;
        let azurite = flag("VR_STORAGE_USE_AZURITE")?;
        if environment == "production"
            && (fake_ai || fake_storage || fake_realtime || auth_disabled || azurite)
        {
            return Err(Problem::validation(
                "Production forbids fake/auth-disabled/Azurite adapters.",
            ));
        }
        if fake_ai != fake_storage || fake_ai != fake_realtime {
            return Err(Problem::validation(
                "Staged local mode requires all fake ports together.",
            ));
        }
        if auth_disabled {
            return Err(Problem::validation(
                "Rust cloud never disables bearer authentication; inject a verifier explicitly in tests.",
            ));
        }
        if azurite {
            return Err(Problem::validation(
                "Azurite adapter is not wired in this Rust increment.",
            ));
        }
        let allow_test = environment != "production";
        let audiences: Vec<_> = required("VR_GOOGLE_ALLOWED_AUDIENCES")?
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect();
        let email = required("VR_ALLOWLISTED_EMAIL")?.trim().to_lowercase();
        if audiences.is_empty() || email.is_empty() {
            return Err(Problem::validation(
                "Auth audience/email configuration is empty.",
            ));
        }
        let issuers: Vec<_> = value(
            "VR_GOOGLE_ALLOWED_ISSUERS",
            "https://accounts.google.com,accounts.google.com",
        )
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();
        if issuers.is_empty() {
            return Err(Problem::validation("Accepted Google issuers are empty."));
        }
        let storage_account = required("VR_STORAGE_ACCOUNT_NAME")?;
        if !(3..=24).contains(&storage_account.len())
            || !storage_account
                .bytes()
                .all(|value| value.is_ascii_lowercase() || value.is_ascii_digit())
        {
            return Err(Problem::validation("Invalid Azure storage account name."));
        }
        let config = Self {
            environment,
            audiences,
            issuers,
            email,
            storage_account,
            jwks: endpoint(
                &value(
                    "VR_GOOGLE_JWKS_URI",
                    "https://www.googleapis.com/oauth2/v3/certs",
                ),
                allow_test,
            )?,
            foundry: endpoint(&required("VR_FOUNDRY_ENDPOINT")?, allow_test)?,
            speech: values
                .get("VR_SPEECH_ENDPOINT")
                .filter(|value| !value.trim().is_empty())
                .map(|value| endpoint(value, allow_test))
                .transpose()?,
            webpubsub: endpoint(&required("VR_WEBPUBSUB_ENDPOINT")?, allow_test)?,
            hub: value("VR_WEBPUBSUB_HUB", "transcripts"),
            group: value("VR_WEBPUBSUB_GROUP", "user"),
            refinement: value("VR_REFINE_DEPLOYMENT_DEFAULT", "gpt-6-luna"),
            dictation_stream: value(
                "VR_DICTATION_STREAM_DEPLOYMENT",
                "MAI-Transcribe-2-Streaming",
            ),
            recording_stream: value(
                "VR_RECORDING_STREAM_DEPLOYMENT",
                "MAI-Transcribe-2-Streaming",
            ),
            use_fakes: fake_ai,
            audio_container: value("VR_AUDIO_CONTAINER", "audio"),
            transcript_container: value("VR_TRANSCRIPT_CONTAINER", "transcripts"),
            raw_container: value("VR_RAW_CONTAINER", "raw"),
            queue: value("VR_WORK_QUEUE", "work"),
            recordings_table: value("VR_RECORDINGS_TABLE", "recordings"),
            chunks_table: value("VR_CHUNKS_TABLE", "chunks"),
            transcripts_table: value("VR_TRANSCRIPTS_TABLE", "transcripts"),
            foundry_api_version: value("VR_FOUNDRY_API_VERSION", "2024-10-21"),
            speech_api_version: value("VR_SPEECH_API_VERSION", "2025-10-15"),
            speech_model: value("VR_SPEECH_MODEL", "MAI-Transcribe-2"),
            speech_style: value("VR_SPEECH_TRANSCRIBE_STYLE", "verbatim"),
            speech_timeout: value("VR_SPEECH_TIMEOUT_SECONDS", "180")
                .parse()
                .map_err(|_| Problem::validation("Invalid speech timeout."))?,
            transcribe_provider: value("VR_TRANSCRIBE_PROVIDER", "azure_speech"),
            transcribe_deployment: value("VR_TRANSCRIBE_DEPLOYMENT", "gpt-4o-transcribe"),
            webpubsub_ttl_minutes: value("VR_WEBPUBSUB_TOKEN_TTL_MINUTES", "60")
                .parse()
                .map_err(|_| Problem::validation("Invalid Web PubSub token lifetime."))?,
            dictation_stream_enabled: if values.contains_key("VR_DICTATION_STREAM_ENABLED") {
                flag("VR_DICTATION_STREAM_ENABLED")?
            } else {
                true
            },
            recording_stream_lease_seconds: value("VR_RECORDING_STREAM_LEASE_SECONDS", "120")
                .parse()
                .map_err(|_| Problem::validation("Invalid recording stream lease duration."))?,
            recording_stream_replay_speed: value("VR_RECORDING_STREAM_REPLAY_SPEED", "4")
                .parse()
                .map_err(|_| {
                Problem::validation("Invalid recording replay speed.")
            })?,
            recording_stream_idle_seconds: value("VR_RECORDING_STREAM_IDLE_SECONDS", "120")
                .parse()
                .map_err(|_| Problem::validation("Invalid recording idle duration."))?,
            recording_stream_poll_seconds: value("VR_RECORDING_STREAM_POLL_SECONDS", "1")
                .parse()
                .map_err(|_| {
                Problem::validation("Invalid recording poll duration.")
            })?,
            recording_stream_heartbeat_seconds: value(
                "VR_RECORDING_STREAM_HEARTBEAT_SECONDS",
                "20",
            )
            .parse()
            .map_err(|_| Problem::validation("Invalid recording heartbeat duration."))?,
            recording_stream_session_seconds: value("VR_RECORDING_STREAM_SESSION_SECONDS", "3300")
                .parse()
                .map_err(|_| Problem::validation("Invalid recording session duration."))?,
            queue_visibility_seconds: value("VR_QUEUE_VISIBILITY_SECONDS", "300")
                .parse()
                .map_err(|_| Problem::validation("Invalid queue visibility duration."))?,
            glossary_terms: value(
                "VR_GLOSSARY_TERMS",
                "Microsoft, Azure, GitHub, SDK, API, AI, Container Apps, Managed Identity, Foundry, Copilot, Backend, Speech-to-text, MAI-Transcribe-2, GPT-5.6 Luna, GPT-6 Luna, REST API, Blob Storage, Kubernetes, Entra, OpenAI, FastAPI, Python, DevOps, Terraform, Azure OpenAI, Web PubSub, DNS, OIDC, JWT",
            ),
            overlap_min_chars: value("VR_OVERLAP_MIN_CHARS", "8")
                .parse()
                .map_err(|_| Problem::validation("Invalid minimum chunk overlap."))?,
            chunk_grace_seconds: value("VR_CHUNK_GRACE_SECONDS", "900")
                .parse()
                .map_err(|_| Problem::validation("Invalid chunk grace duration."))?,
            max_transcribe_attempts: value("VR_MAX_TRANSCRIBE_ATTEMPTS", "5")
                .parse()
                .map_err(|_| Problem::validation("Invalid transcription attempts."))?,
            max_refine_attempts: value("VR_MAX_REFINE_ATTEMPTS", "5")
                .parse()
                .map_err(|_| Problem::validation("Invalid refinement attempts."))?,
            queue_retry_base_seconds: value("VR_QUEUE_RETRY_BASE_SECONDS", "30")
                .parse()
                .map_err(|_| Problem::validation("Invalid queue retry base."))?,
            queue_receive_error_backoff_seconds: value(
                "VR_QUEUE_RECEIVE_ERROR_BACKOFF_SECONDS",
                "2",
            )
            .parse()
            .map_err(|_| Problem::validation("Invalid queue receive backoff."))?,
            queue_receive_error_max_backoff_seconds: value(
                "VR_QUEUE_RECEIVE_ERROR_MAX_BACKOFF_SECONDS",
                "60",
            )
            .parse()
            .map_err(|_| Problem::validation("Invalid queue receive backoff cap."))?,
        };
        for name in [
            &config.hub,
            &config.group,
            &config.refinement,
            &config.dictation_stream,
            &config.recording_stream,
            &config.audio_container,
            &config.transcript_container,
            &config.raw_container,
            &config.queue,
            &config.recordings_table,
            &config.chunks_table,
            &config.transcripts_table,
            &config.transcribe_deployment,
            &config.foundry_api_version,
            &config.speech_api_version,
            &config.speech_model,
        ] {
            if name.trim().is_empty() || name.len() > 1024 || name.chars().any(char::is_control) {
                return Err(Problem::validation(
                    "Server resource/deployment names must be nonempty and bounded.",
                ));
            }
        }
        if !matches!(config.speech_style.as_str(), "verbatim" | "clean")
            || !config.speech_timeout.is_finite()
            || config.speech_timeout <= 0.0
            || config.speech_timeout > 3600.0
            || !matches!(
                config.transcribe_provider.as_str(),
                "azure_speech" | "azure_openai"
            )
            || config.transcribe_provider == "azure_speech" && config.speech.is_none()
            || !(1..=1440).contains(&config.webpubsub_ttl_minutes)
            || !(90..=300).contains(&config.recording_stream_lease_seconds)
            || !(1..=604800).contains(&config.queue_visibility_seconds)
            || f64::from(config.queue_visibility_seconds)
                <= config.recording_stream_heartbeat_seconds
            || config.max_transcribe_attempts == 0
            || config.max_refine_attempts == 0
            || !config.queue_receive_error_backoff_seconds.is_finite()
            || !config.queue_receive_error_max_backoff_seconds.is_finite()
            || config.queue_receive_error_backoff_seconds <= 0.0
            || config.queue_receive_error_max_backoff_seconds
                < config.queue_receive_error_backoff_seconds
        {
            return Err(Problem::validation(
                "Invalid transcription provider/style/timeout.",
            ));
        }
        for (seconds, minimum, maximum) in [
            (config.recording_stream_replay_speed, 1.0, 8.0),
            (config.recording_stream_idle_seconds, 1.0, 900.0),
            (
                config.recording_stream_poll_seconds,
                f64::MIN_POSITIVE,
                10.0,
            ),
            (
                config.recording_stream_heartbeat_seconds,
                f64::MIN_POSITIVE,
                30.0,
            ),
            (
                config.recording_stream_session_seconds,
                f64::MIN_POSITIVE,
                3300.0,
            ),
        ] {
            if !seconds.is_finite() || seconds < minimum || seconds > maximum {
                return Err(Problem::validation(
                    "Recording stream limits must be finite and within supported ranges.",
                ));
            }
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> BTreeMap<String, String> {
        [
            ("VR_GOOGLE_ALLOWED_AUDIENCES", "fixture"),
            ("VR_ALLOWLISTED_EMAIL", "fixture@example.invalid"),
            ("VR_STORAGE_ACCOUNT_NAME", "fixture123"),
            (
                "VR_FOUNDRY_ENDPOINT",
                "https://fixture.cognitiveservices.azure.com/",
            ),
            (
                "VR_SPEECH_ENDPOINT",
                "https://fixture.cognitiveservices.azure.com/",
            ),
            (
                "VR_WEBPUBSUB_ENDPOINT",
                "https://fixture.webpubsub.azure.com/",
            ),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect()
    }
    #[test]
    fn production_is_explicit_and_cannot_boot_with_insecure_success_defaults() {
        assert!(Config::parse(&BTreeMap::new()).is_err());
        let values = valid();
        assert_eq!(Config::parse(&values).unwrap().environment, "production");
        for flag in [
            "VR_AUTH_DISABLED",
            "VR_USE_FAKE_AI",
            "VR_USE_FAKE_STORAGE",
            "VR_USE_FAKE_REALTIME",
            "VR_STORAGE_USE_AZURITE",
        ] {
            let mut values = valid();
            values.insert(flag.into(), "true".into());
            assert!(Config::parse(&values).is_err());
        }
        let mut values = valid();
        values.insert("VR_GOOGLE_JWKS_URI".into(), "http://127.0.0.1:1234/".into());
        assert!(Config::parse(&values).is_err());
    }
    #[test]
    fn legacy_openai_can_omit_speech_and_model_timeouts_remain_finite() {
        let mut values = valid();
        values.remove("VR_SPEECH_ENDPOINT");
        assert!(Config::parse(&values).is_err());
        values.insert("VR_TRANSCRIBE_PROVIDER".into(), "azure_openai".into());
        assert!(Config::parse(&values).unwrap().speech.is_none());
        values.insert("VR_SPEECH_TIMEOUT_SECONDS".into(), "180.5".into());
        assert_eq!(Config::parse(&values).unwrap().speech_timeout, 180.5);
        for value in ["NaN", "inf", "-1", "0", "bad"] {
            values.insert("VR_SPEECH_TIMEOUT_SECONDS".into(), value.into());
            assert!(Config::parse(&values).is_err(), "{value}");
        }
    }
    #[test]
    fn continuous_limits_and_queue_visibility_preserve_defaults_and_reject_unsafe_values() {
        let mut values = valid();
        let defaults = Config::parse(&values).unwrap();
        assert_eq!(defaults.recording_stream_replay_speed, 4.0);
        assert_eq!(defaults.recording_stream_heartbeat_seconds, 20.0);
        assert_eq!(defaults.queue_visibility_seconds, 300);
        for (key, value) in [
            ("VR_RECORDING_STREAM_REPLAY_SPEED", "9"),
            ("VR_RECORDING_STREAM_POLL_SECONDS", "0"),
            ("VR_RECORDING_STREAM_HEARTBEAT_SECONDS", "31"),
            ("VR_RECORDING_STREAM_SESSION_SECONDS", "3301"),
            ("VR_RECORDING_STREAM_LEASE_SECONDS", "89"),
            ("VR_QUEUE_VISIBILITY_SECONDS", "10"),
        ] {
            values.insert(key.into(), value.into());
            assert!(Config::parse(&values).is_err(), "{key}");
            values.remove(key);
        }
    }
}
