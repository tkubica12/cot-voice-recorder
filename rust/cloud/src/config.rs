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
    pub speech: Url,
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
            speech: endpoint(&required("VR_SPEECH_ENDPOINT")?, allow_test)?,
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
        ] {
            if name.trim().is_empty() || name.len() > 1024 || name.chars().any(char::is_control) {
                return Err(Problem::validation(
                    "Server resource/deployment names must be nonempty and bounded.",
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
}
