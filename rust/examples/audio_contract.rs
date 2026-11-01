use std::error::Error;
use std::io::{self, Read, Write};

use voiceprompt_core::chunker::{AudioChunk, PcmChunker};
use voiceprompt_core::wav::{ParseMode, parse_wav, to_wav, validate_capture_wav};

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input = Vec::new();
    io::stdin().lock().read_to_end(&mut input)?;
    let mut output = io::stdout().lock();
    match args.first().map(String::as_str) {
        Some("wrap") if args.len() == 1 => output.write_all(&to_wav(&input)?)?,
        Some("parse" | "validate") if args.len() == 2 => {
            let mode = match args[1].as_str() {
                "legacy" => ParseMode::Legacy,
                "strict" => ParseMode::Strict,
                _ => return Err("Expected legacy or strict mode.".into()),
            };
            let properties = if args[0] == "validate" {
                validate_capture_wav(&input, mode)?
            } else {
                parse_wav(&input, mode)?
            };
            writeln!(
                output,
                "{}\t{}\t{}\t{}\t{}",
                properties.audio_format,
                properties.channels,
                properties.sample_rate,
                properties.bits_per_sample,
                properties.data_bytes
            )?;
        }
        Some("chunk") if (2..=3).contains(&args.len()) => {
            let fragment = args[1].parse::<usize>()?;
            if fragment == 0 {
                return Err("Fragment size must be positive.".into());
            }
            let boundary = args.get(2).map(|arg| arg.parse::<usize>()).transpose()?;
            if boundary.is_some_and(|value| value > input.len()) {
                return Err("Restore boundary exceeds input length.".into());
            }
            let mut chunker = PcmChunker::new(120.0)?;
            let mut chunks: Vec<AudioChunk> = Vec::new();
            let split = boundary.unwrap_or(input.len());
            for block in input[..split].chunks(fragment) {
                chunks.extend(chunker.append(block)?);
            }
            if boundary.is_some() {
                chunker = PcmChunker::restore(chunker.snapshot())?;
                for block in input[split..].chunks(fragment) {
                    chunks.extend(chunker.append(block)?);
                }
            }
            chunks.extend(chunker.flush()?);
            output.write_all(&u32::try_from(chunks.len())?.to_le_bytes())?;
            for chunk in chunks {
                output.write_all(&chunk.end_byte.to_le_bytes())?;
                output.write_all(&[u8::from(chunk.overlaps_previous)])?;
                output.write_all(&u32::try_from(chunk.wav.len())?.to_le_bytes())?;
                output.write_all(&chunk.wav)?;
            }
        }
        _ => {
            return Err(
                "Usage: audio_contract wrap | parse|validate legacy|strict | chunk fragment [restore-byte]".into()
            );
        }
    }
    output.flush()?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Audio contract probe: {error}");
        std::process::exit(2);
    }
}
