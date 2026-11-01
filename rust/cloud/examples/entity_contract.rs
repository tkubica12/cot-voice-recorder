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
