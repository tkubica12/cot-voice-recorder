use std::error::Error;
use std::fmt;

pub const SAMPLE_RATE: u32 = 16_000;
pub const BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * 2;
pub const HEADER_BYTES: usize = 44;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseMode {
    Legacy,
    Strict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavProperties {
    pub audio_format: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub data_bytes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldMismatch {
    pub field: &'static str,
    pub expected: u32,
    pub actual: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WavError {
    InvalidContainer(&'static str),
    FormatMismatch(Vec<FieldMismatch>),
    IncompletePcm,
    TooLarge,
}

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidContainer(message) => f.write_str(message),
            Self::FormatMismatch(fields) => {
                f.write_str("WAV PCM properties do not match the required capture format.")?;
                for field in fields {
                    write!(
                        f,
                        " {}: expected {}, got {}.",
                        field.field, field.expected, field.actual
                    )?;
                }
                Ok(())
            }
            Self::IncompletePcm => {
                f.write_str("PCM must contain complete, nonempty 16-bit samples.")
            }
            Self::TooLarge => f.write_str("PCM exceeds the RIFF/WAVE size limit."),
        }
    }
}

impl Error for WavError {}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Legacy mode preserves the Python API's relaxed header-only validation.
/// Use Strict before consuming uploaded audio or relying on declared data lengths.
pub fn parse_wav(data: &[u8], mode: ParseMode) -> Result<WavProperties, WavError> {
    use WavError::InvalidContainer as Invalid;

    if data.len() < HEADER_BYTES {
        return Err(Invalid(
            "WAV payload is too small to contain a valid header.",
        ));
    }
    if &data[..4] != b"RIFF" {
        return Err(Invalid("Missing RIFF header."));
    }
    if &data[8..12] != b"WAVE" {
        return Err(Invalid("Missing WAVE format marker."));
    }
    let strict = mode == ParseMode::Strict;
    if strict && u32_at(data, 4) as usize != data.len() - 8 {
        return Err(Invalid("RIFF size does not match the WAV payload."));
    }

    let mut format: Option<WavProperties> = None;
    let mut data_bytes = None;
    let mut offset = 12;
    while offset <= data.len() - 8 {
        let id = &data[offset..offset + 4];
        let size = u32_at(data, offset + 4) as usize;
        let body = offset + 8;
        let next = body
            .checked_add(size)
            .and_then(|end| end.checked_add(size & 1))
            .ok_or(Invalid("WAV contains a truncated chunk."))?;
        if strict && next > data.len() {
            return Err(Invalid("WAV contains a truncated chunk."));
        }
        if id == b"fmt " {
            if strict && format.is_some() {
                return Err(Invalid("WAV contains duplicate fmt chunks."));
            }
            if size < 16 || data.len() - body < 16 {
                return Err(Invalid("Malformed fmt chunk."));
            }
            let properties = WavProperties {
                audio_format: u16_at(data, body),
                channels: u16_at(data, body + 2),
                sample_rate: u32_at(data, body + 4),
                bits_per_sample: u16_at(data, body + 14),
                data_bytes: 0,
            };
            if strict {
                let align =
                    u64::from(properties.channels) * u64::from(properties.bits_per_sample) / 8;
                if align == 0
                    || properties.bits_per_sample % 8 != 0
                    || u64::from(u16_at(data, body + 12)) != align
                    || u64::from(u32_at(data, body + 8))
                        != u64::from(properties.sample_rate) * align
                {
                    return Err(Invalid("WAV byte rate or block alignment is invalid."));
                }
                if size != 16 && (size < 18 || usize::from(u16_at(data, body + 16)) != size - 18) {
                    return Err(Invalid("WAV fmt extension size is invalid."));
                }
            }
            format = Some(properties);
        } else if id == b"data" {
            if strict {
                let properties =
                    format.ok_or(Invalid("WAV requires one data chunk after its fmt chunk."))?;
                if data_bytes.is_some() {
                    return Err(Invalid("WAV requires one data chunk after its fmt chunk."));
                }
                let align =
                    usize::from(properties.channels) * usize::from(properties.bits_per_sample) / 8;
                if size == 0 || size % align != 0 {
                    return Err(Invalid("WAV must contain nonempty, complete PCM frames."));
                }
            }
            data_bytes = Some(size as u32);
        }
        offset = next;
    }
    if strict && offset != data.len() {
        return Err(Invalid("WAV contains an incomplete trailing chunk header."));
    }
    let mut properties = format.ok_or(Invalid("WAV is missing its fmt chunk."))?;
    properties.data_bytes = data_bytes.ok_or(Invalid("WAV is missing its data chunk."))?;
    if properties.audio_format != 1 {
        return Err(Invalid(
            "WAV must be uncompressed 16-bit PCM (format code 1).",
        ));
    }
    Ok(properties)
}

pub fn validate_capture_wav(data: &[u8], mode: ParseMode) -> Result<WavProperties, WavError> {
    let properties = parse_wav(data, mode)?;
    let mut mismatches = Vec::new();
    for (field, expected, actual) in [
        ("channels", 1, u32::from(properties.channels)),
        ("sample_rate", SAMPLE_RATE, properties.sample_rate),
        ("bits_per_sample", 16, u32::from(properties.bits_per_sample)),
    ] {
        if actual != expected {
            mismatches.push(FieldMismatch {
                field,
                expected,
                actual,
            });
        }
    }
    if mismatches.is_empty() {
        Ok(properties)
    } else {
        Err(WavError::FormatMismatch(mismatches))
    }
}

pub fn to_wav(pcm: &[u8]) -> Result<Vec<u8>, WavError> {
    if pcm.is_empty() || pcm.len() % 2 != 0 {
        return Err(WavError::IncompletePcm);
    }
    let data_size = u32::try_from(pcm.len()).map_err(|_| WavError::TooLarge)?;
    let riff_size = data_size.checked_add(36).ok_or(WavError::TooLarge)?;
    let capacity = pcm
        .len()
        .checked_add(HEADER_BYTES)
        .ok_or(WavError::TooLarge)?;
    let mut wav = Vec::with_capacity(capacity);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(BYTES_PER_SECOND as u32).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(pcm);
    Ok(wav)
}
