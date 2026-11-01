use std::error::Error;
use std::io::{Read, Write};

use serde_json::{Value, json};
use voiceprompt_client_protocol::auth::*;
use voiceprompt_client_protocol::realtime::*;

fn field<'a>(root: &'a Value, key: &str) -> Result<&'a str, Box<dyn Error>> {
    root.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing fixture field: {key}").into())
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 1 {
        return Err("Specify one client-contract mode.".into());
    }
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut input)?;
    let mut output = std::io::stdout().lock();
    match args[0].as_str() {
        "summary-check" => {
            let summary: voiceprompt_client_protocol::api::TranscriptSummary =
                serde_json::from_slice(&input)?;
            output.write_all(summary.transcript_id.as_bytes())?;
        }
        "normalize-batch" => {
            let words: Vec<String> = serde_json::from_slice(&input)?;
            let normalized: Vec<_> = words
                .iter()
                .map(|word| voiceprompt_client_protocol::dictation_text::normalize_word(word))
                .collect();
            output.write_all(&serde_json::to_vec(&normalized)?)?;
        }
        "normalize-dictation" => output.write_all(
            voiceprompt_client_protocol::dictation_text::normalize_word(std::str::from_utf8(
                &input,
            )?)
            .as_bytes(),
        )?,
        "stitch" => {
            let root: Vec<Value> = serde_json::from_slice(&input)?;
            let chunks: Vec<_> = root
                .iter()
                .map(|item| {
                    Ok((
                        field(item, "text")?,
                        item["overlap"].as_bool().ok_or("Missing overlap flag.")?,
                    ))
                })
                .collect::<Result<_, Box<dyn Error>>>()?;
            output.write_all(
                voiceprompt_client_protocol::dictation_text::stitch(chunks).as_bytes(),
            )?;
        }
        "append-prefix" => {
            let root: Value = serde_json::from_slice(&input)?;
            let previous_words = root
                .get("previous_words")
                .and_then(Value::as_u64)
                .map(usize::try_from)
                .transpose()?;
            let appended = voiceprompt_client_protocol::dictation_text::append_preserving_prefix(
                field(&root, "previous")?,
                field(&root, "incoming")?,
                root["overlap"].as_bool().ok_or("Missing overlap flag.")?,
                previous_words,
            );
            output.write_all(appended.as_bytes())?;
        }
        "legacy-state" => {
            let mut chunker = voiceprompt_core::chunker::PcmChunker::new(120.0)?;
            chunker.append(&input)?;
            output.write_all(&voiceprompt_client_protocol::legacy_checkpoint::encode(
                &chunker.snapshot(),
            )?)?;
        }
        "cache" => output.write_all(&CachedTokens::parse(&input)?.encode()?)?,
        "pkce" => {
            let verifier = std::str::from_utf8(&input)?;
            writeln!(output, "{}", PkcePair::challenge(verifier)?)?;
        }
        "callback" => {
            let root: Value = serde_json::from_slice(&input)?;
            writeln!(
                output,
                "{}",
                validate_callback(field(&root, "query")?, field(&root, "state")?)?
            )?;
        }
        "auth-url" => {
            let root: Value = serde_json::from_slice(&input)?;
            let config = DesktopClientConfig::parse(&serde_json::to_vec(&root["config"])?)?;
            let verifier = field(&root, "verifier")?.to_owned();
            let pair = PkcePair {
                challenge: PkcePair::challenge(&verifier)?,
                verifier,
                state: field(&root, "state")?.to_owned(),
            };
            let uri = authorization_url(
                &config,
                field(&root, "redirect")?,
                &pair,
                root.get("login_hint").and_then(Value::as_str),
            )?;
            writeln!(output, "{uri}")?;
        }
        "jwt" => {
            let claims = JwtClaims::parse(std::str::from_utf8(&input)?)?;
            writeln!(
                output,
                "{}",
                json!({
                    "exp": claims.expires_at.map(|value| value.unix_timestamp()),
                    "email": claims.email, "aud": claims.audience, "sub": claims.subject,
                })
            )?;
        }
        "refresh" => writeln!(
            output,
            "{:?}",
            TokenResponse::parse(&input)?.classify_refresh()
        )?,
        "frame" => {
            let frame = Frame::parse(&input)?;
            let completed = frame.completed()?;
            let event = completed.map(|value| {
                json!({
                    "event_id":value.event_id,"transcript_id":value.transcript_id,
                    "recording_id":value.recording_id,"preview":value.preview,
                })
            });
            writeln!(
                output,
                "{}",
                json!({
                    "kind":format!("{:?}", frame.kind), "system_event":frame.system_event,
                    "from":frame.from,"group":frame.group,"data_type":frame.data_type,
                    "user_id":frame.user_id,"connection_id":frame.connection_id,"completed":event,
                })
            )?;
        }
        _ => return Err("Unknown client-contract mode.".into()),
    }
    output.flush()?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Client contract fixture failed: {error}");
        std::process::exit(2);
    }
}
