use std::io::{Read, Write};
use voiceprompt_cloud::domain::{Chunk, Recording, Transcript};
use voiceprompt_cloud::storage::Entity;

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args()
        .nth(1)
        .ok_or("Specify one entity-contract mode.")?;
    let mut data = Vec::new();
    std::io::stdin()
        .lock()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    if data.len() > 16 * 1024 * 1024 {
        return Err("Entity fixture exceeds supported bounds.".into());
    }
    let values: serde_json::Value = serde_json::from_slice(&data)?;
    if mode == "pipeline-text" {
        let texts: Vec<String> = serde_json::from_value(values["texts"].clone())?;
        let tokens: Vec<String> = serde_json::from_value(values["tokens"].clone())?;
        let id = values["recording_id"]
            .as_str()
            .ok_or("Missing synthetic recording id.")?;
        let result = serde_json::json!({
            "text":voiceprompt_cloud::pipeline_text::stitch_chunks(&texts, values["min_chars"].as_u64().ok_or("Missing minimum overlap.")? as usize, 60),
            "tokens":tokens.iter().map(|token| voiceprompt_cloud::pipeline_text::normalize_token(token)).collect::<Vec<_>>(),
            "transcript_id":voiceprompt_cloud::pipeline_text::transcript_id(id),
            "event_id":voiceprompt_cloud::pipeline_text::event_id(id),
        });
        std::io::stdout()
            .lock()
            .write_all(&serde_json::to_vec(&result)?)?;
        return Ok(());
    }
    if mode == "relay" {
        let mut state = voiceprompt_cloud::relay::RelayState::new(
            values["start"].as_u64().ok_or("Missing start cursor.")?,
        );
        let mut messages = Vec::new();
        let mut accepted = true;
        for action in values["actions"]
            .as_array()
            .ok_or("Missing relay actions.")?
        {
            let result = match action["kind"].as_str() {
                Some("audio") => {
                    let data = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        action["data"].as_str().ok_or("Missing synthetic PCM.")?,
                    )?;
                    state.audio(&data).map(|_| Vec::new())
                }
                Some("control") => state
                    .control(action["data"].as_str().ok_or("Missing control.")?)
                    .map(|_| Vec::new()),
                Some("event") => state.event(action["data"].clone()),
                _ => return Err("Unknown relay fixture action.".into()),
            };
            match result {
                Ok(events) => messages.extend(events),
                Err(_) => {
                    accepted = false;
                    break;
                }
            }
        }
        if accepted {
            messages.extend(state.done());
        }
        std::io::stdout().lock().write_all(&serde_json::to_vec(
            &serde_json::json!({"accepted":accepted, "messages":messages}),
        )?)?;
        return Ok(());
    }
    if mode == "archive" {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(async {
            let config = voiceprompt_cloud::config::Config::parse(&[
                ("VR_ENVIRONMENT", "test"),
                ("VR_GOOGLE_ALLOWED_AUDIENCES", "fixture"),
                ("VR_ALLOWLISTED_EMAIL", "fixture@example.invalid"),
                ("VR_STORAGE_ACCOUNT_NAME", "fixture123"),
                ("VR_FOUNDRY_ENDPOINT", "https://fixture.invalid/"),
                ("VR_SPEECH_ENDPOINT", "https://fixture.invalid/"),
                ("VR_WEBPUBSUB_ENDPOINT", "https://fixture.invalid/"),
            ].into_iter().map(|(key, value)| (key.into(), value.into())).collect())?;
            let config = std::sync::Arc::new(config);
            let services = voiceprompt_cloud::services::Services::new(config.clone(),
                voiceprompt_cloud::repository::Repository::memory(),
                voiceprompt_cloud::ports::Blobs::memory(), voiceprompt_cloud::ports::Queue::memory())?;
            let payload = serde_json::from_value(values["payload"].clone()).map_err(|_| voiceprompt_cloud::problem::Problem::validation("Invalid synthetic archive payload."))?;
            let transcript = services.archive(values["id"].as_str().unwrap_or(""), payload).await?;
            let full = services.transcript(&transcript.transcript_id).await?;
            let envelope = services.blobs.get(&config.transcript_container, &transcript.body_path, 16*1024*1024).await
                .map_err(|_| voiceprompt_cloud::problem::Problem::internal())?;
            Ok::<_, voiceprompt_cloud::problem::Problem>(serde_json::json!({
                "status":200, "summary":voiceprompt_cloud::models::summary(&transcript), "full":full,
                "envelope":base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &envelope),
                "digest":transcript.content_digest, "path":transcript.body_path,
            }))
        });
        let result = match result {
            Ok(result) => result,
            Err(error) => serde_json::json!({"status":error.status, "code":error.code}),
        };
        std::io::stdout()
            .lock()
            .write_all(&serde_json::to_vec(&result)?)?;
        return Ok(());
    }
    if mode == "edits" {
        let text = values["text"].as_str().ok_or("Missing text fixture.")?;
        let payload = values["payload"]
            .as_str()
            .ok_or("Missing edit payload fixture.")?;
        let previous = values["previous"].as_str().unwrap_or("");
        let result = match voiceprompt_cloud::edits::validate(text, payload, previous) {
            Ok(result) => {
                serde_json::json!({"accepted":true,"text":result.text,"edits":result.edits})
            }
            Err(error) => serde_json::json!({"accepted":false,"category":error.0}),
        };
        std::io::stdout()
            .lock()
            .write_all(&serde_json::to_vec(&result)?)?;
        return Ok(());
    }
    if let Some(kind) = mode.strip_prefix("encode-") {
        let entity = match kind {
            "recording" => serde_json::from_value::<Recording>(values)?.entity()?,
            "chunk" => serde_json::from_value::<Chunk>(values)?.entity()?,
            "transcript" => serde_json::from_value::<Transcript>(values)?.entity()?,
            _ => return Err("Unknown entity encode mode.".into()),
        };
        std::io::stdout()
            .lock()
            .write_all(&serde_json::to_vec(&entity.values)?)?;
        return Ok(());
    }
    let entity = Entity {
        values: serde_json::from_value(values)?,
        etag: "synthetic-etag".into(),
    };
    let output = match mode.as_str() {
        "recording" => serde_json::to_vec(&Recording::from_entity(&entity)?)?,
        "chunk" => serde_json::to_vec(&Chunk::from_entity(&entity)?)?,
        "transcript" => serde_json::to_vec(&Transcript::from_entity(&entity)?)?,
        _ => return Err("Unknown entity-contract mode.".into()),
    };
    std::io::stdout().lock().write_all(&output)?;
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("Entity contract fixture failed: {error}");
        std::process::exit(2);
    }
}
