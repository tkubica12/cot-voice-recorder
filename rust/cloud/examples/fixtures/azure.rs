use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use voiceprompt_cloud::{identity::ManagedIdentity, storage::AzureStorage};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    scope: String,
    pub key: String,
    blob: String,
    queue: String,
    table: String,
    identity_endpoint: String,
    identity_header: String,
    #[serde(skip)]
    root: PathBuf,
}

impl Fixture {
    pub fn arguments(args: &[String]) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        if args.is_empty() {
            return Ok(None);
        }
        let [flag, root] = args else {
            return Err("Pass only --azure-fixture and one explicit owned root.".into());
        };
        if flag != "--azure-fixture" || !std::path::Path::new(root).is_absolute() {
            return Err("An explicit absolute Azure fixture root is required.".into());
        }
        let root = std::fs::canonicalize(root)?;
        let path = root.join("fixture.json");
        if std::fs::metadata(&path)?.len() > 8192 {
            return Err("Azure fixture configuration exceeds its bound.".into());
        }
        let mut fixture: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        if fixture.scope != "authenticated-https-azurite-http-no-ai-realtime"
            || fixture.key.len() != 32
            || !fixture
                .key
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("Invalid owned Azure performance fixture marker.".into());
        }
        let endpoint: url::Url = fixture.identity_endpoint.parse()?;
        if endpoint.scheme() != "http"
            || endpoint.host_str() != Some("127.0.0.1")
            || endpoint.port().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/token"
            || fixture.identity_header.is_empty()
            || std::env::var("IDENTITY_ENDPOINT")? != fixture.identity_endpoint
            || std::env::var("IDENTITY_HEADER")? != fixture.identity_header
            || std::env::var("AZURE_CLIENT_ID")? != "synthetic-uami"
        {
            return Err(
                "Azure fixture requires its own loopback identity, not ambient credentials.".into(),
            );
        }
        fixture.root = root;
        Ok(Some(fixture))
    }

    pub fn configure(&self, values: &mut BTreeMap<String, String>) {
        for (name, value) in [
            ("VR_AUDIO_CONTAINER", format!("audio-{}", self.key)),
            (
                "VR_TRANSCRIPT_CONTAINER",
                format!("transcripts-{}", self.key),
            ),
            ("VR_RAW_CONTAINER", format!("raw-{}", self.key)),
            ("VR_WORK_QUEUE", format!("work-{}", self.key)),
            ("VR_RECORDINGS_TABLE", format!("Recordings{}", self.key)),
            ("VR_CHUNKS_TABLE", format!("Chunks{}", self.key)),
            ("VR_TRANSCRIPTS_TABLE", format!("Transcripts{}", self.key)),
        ] {
            values.insert(name.into(), value);
        }
    }

    pub fn storage(&self) -> Result<Arc<AzureStorage>, Box<dyn std::error::Error>> {
        let certificate = self.root.join("server-cert.pem");
        if std::fs::metadata(&certificate)?.len() > 65536 {
            return Err("Azure fixture certificate exceeds its bound.".into());
        }
        Ok(Arc::new(AzureStorage::loopback_fixture(
            Arc::new(ManagedIdentity::from_environment()?),
            [self.blob.parse()?, self.queue.parse()?, self.table.parse()?],
            &std::fs::read(certificate)?,
        )?))
    }
}
