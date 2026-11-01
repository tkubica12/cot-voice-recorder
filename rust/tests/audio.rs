use voiceprompt_core::chunker::{ChunkerError, MAX_AUDIO_BYTES, OVERLAP_BYTES, PcmChunker};
use voiceprompt_core::wav::{
    BYTES_PER_SECOND, HEADER_BYTES, ParseMode, WavError, parse_wav, to_wav, validate_capture_wav,
};

fn pcm(milliseconds: usize, sample: i16) -> Vec<u8> {
    sample
        .to_le_bytes()
        .repeat(BYTES_PER_SECOND * milliseconds / 1000 / 2)
}

fn chunker() -> PcmChunker {
    PcmChunker::new(120.0).unwrap()
}

fn update_riff_size(wav: &mut [u8]) {
    let size = (wav.len() - 8) as u32;
    wav[4..8].copy_from_slice(&size.to_le_bytes());
}

#[test]
fn wave_header_matches_pcm_and_capture_format() {
    let audio = pcm(100, -1200);
    let wav = to_wav(&audio).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(&wav[HEADER_BYTES..], audio);
    for mode in [ParseMode::Legacy, ParseMode::Strict] {
        let properties = validate_capture_wav(&wav, mode).unwrap();
        assert_eq!(properties.data_bytes as usize, audio.len());
        assert_eq!(properties.audio_format, 1);
        assert_eq!(properties.channels, 1);
        assert_eq!(properties.sample_rate, 16_000);
        assert_eq!(properties.bits_per_sample, 16);
    }
}

#[test]
fn empty_or_incomplete_pcm_is_rejected() {
    assert_eq!(to_wav(&[]), Err(WavError::IncompletePcm));
    assert_eq!(to_wav(&[1]), Err(WavError::IncompletePcm));
    let mut chunker = chunker();
    chunker.append(&[1]).unwrap();
    assert_eq!(chunker.flush(), Err(ChunkerError::IncompleteSample));
    chunker.append(&[2]).unwrap();
    assert_eq!(&chunker.flush().unwrap()[0].wav[HEADER_BYTES..], &[1, 2]);
}

#[test]
fn strict_mode_rejects_truncated_data_but_preserves_legacy_behavior() {
    let mut wav = to_wav(&pcm(20, 1200)).unwrap();
    wav.truncate(wav.len() - 2);
    update_riff_size(&mut wav);
    assert!(parse_wav(&wav, ParseMode::Legacy).is_ok());
    assert!(parse_wav(&wav, ParseMode::Strict).is_err());
}

#[test]
fn padded_unknown_chunks_are_supported() {
    let original = to_wav(&pcm(20, 1200)).unwrap();
    let mut wav = original[..12].to_vec();
    wav.extend_from_slice(b"JUNK");
    wav.extend_from_slice(&3u32.to_le_bytes());
    wav.extend_from_slice(b"abc\0");
    wav.extend_from_slice(&original[12..]);
    update_riff_size(&mut wav);
    assert_eq!(
        parse_wav(&wav, ParseMode::Strict),
        parse_wav(&original, ParseMode::Strict)
    );
}

#[test]
fn malformed_headers_sizes_and_alignment_are_rejected() {
    let original = to_wav(&pcm(20, 1200)).unwrap();
    for (offset, bytes) in [
        (0, b"FAIL".as_slice()),
        (8, b"FAIL".as_slice()),
        (4, &[0, 0, 0, 0]),
        (16, &[255, 255, 255, 255]),
        (28, &[0, 0, 0, 0]),
        (32, &[0, 0]),
        (40, &[255, 255, 255, 255]),
    ] {
        let mut wav = original.clone();
        wav[offset..offset + bytes.len()].copy_from_slice(bytes);
        assert!(parse_wav(&wav, ParseMode::Strict).is_err());
    }
    for length in 0..original.len() {
        assert!(parse_wav(&original[..length], ParseMode::Strict).is_err());
    }
}

#[test]
fn capture_validation_reports_every_mismatched_field() {
    let mut wav = to_wav(&pcm(20, 1200)).unwrap();
    wav[22..24].copy_from_slice(&2u16.to_le_bytes());
    wav[24..28].copy_from_slice(&8000u32.to_le_bytes());
    wav[34..36].copy_from_slice(&8u16.to_le_bytes());
    wav[28..32].copy_from_slice(&16_000u32.to_le_bytes());
    let Err(WavError::FormatMismatch(fields)) = validate_capture_wav(&wav, ParseMode::Strict)
    else {
        panic!("Expected capture format mismatches.");
    };
    assert_eq!(
        fields.iter().map(|field| field.field).collect::<Vec<_>>(),
        ["channels", "sample_rate", "bits_per_sample"]
    );
}

#[test]
fn five_minutes_of_silence_never_uploads() {
    let mut chunker = chunker();
    assert!(chunker.append(&pcm(300_000, 0)).unwrap().is_empty());
    assert!(chunker.flush().unwrap().is_empty());
}

#[test]
fn speech_flushes_without_waiting_for_minimum_chunk() {
    let audio = pcm(300, 1200);
    let mut chunker = chunker();
    assert!(chunker.append(&audio).unwrap().is_empty());
    let chunks = chunker.flush().unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(&chunks[0].wav[HEADER_BYTES..], audio);
    assert!(!chunks[0].overlaps_previous);
    assert_eq!(chunks[0].end_byte, audio.len() as u64);
    assert!(chunker.flush().unwrap().is_empty());
}

#[test]
fn pause_cuts_at_two_seconds_without_overlap() {
    let mut chunker = chunker();
    assert!(chunker.append(&pcm(1600, 1200)).unwrap().is_empty());
    let first = chunker.append(&pcm(400, 0)).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].wav.len(), 64_044);
    assert_eq!(first[0].end_byte, 64_000);
    chunker.append(&pcm(300, 1200)).unwrap();
    assert!(!chunker.flush().unwrap()[0].overlaps_previous);
}

#[test]
fn continuous_speech_reconstructs_exact_samples_with_bounded_overlap() {
    let mut audio = pcm(14_000, 1200);
    for (index, sample) in audio.chunks_exact_mut(2).enumerate() {
        sample.copy_from_slice(&(1000 + (index * 2 % 20_000) as i16).to_le_bytes());
    }
    let mut chunker = chunker();
    let mut chunks = chunker.append(&audio).unwrap();
    chunks.extend(chunker.flush().unwrap());
    assert_eq!(chunks.len(), 3);
    assert!(!chunks[0].overlaps_previous);
    let mut reconstructed = chunks[0].wav[HEADER_BYTES..].to_vec();
    for index in 1..chunks.len() {
        assert!(chunks[index].overlaps_previous);
        let previous = &chunks[index - 1].wav;
        assert_eq!(
            &previous[previous.len() - OVERLAP_BYTES..],
            &chunks[index].wav[HEADER_BYTES..HEADER_BYTES + OVERLAP_BYTES]
        );
        reconstructed.extend_from_slice(&chunks[index].wav[HEADER_BYTES + OVERLAP_BYTES..]);
    }
    assert!(
        chunks
            .iter()
            .all(|c| c.wav.len() <= MAX_AUDIO_BYTES + HEADER_BYTES)
    );
    assert_eq!(reconstructed, audio);
}

#[test]
fn exact_boundary_does_not_emit_overlap_only_tail() {
    let mut chunker = chunker();
    assert_eq!(chunker.append(&pcm(6000, 1200)).unwrap().len(), 1);
    assert!(chunker.flush().unwrap().is_empty());
}

#[test]
fn long_pause_after_boundary_drops_overlap_metadata() {
    let mut chunker = chunker();
    assert_eq!(chunker.append(&pcm(6000, 1200)).unwrap().len(), 1);
    assert!(chunker.append(&pcm(5000, 0)).unwrap().is_empty());
    assert!(chunker.append(&pcm(300, 1200)).unwrap().is_empty());
    assert!(!chunker.flush().unwrap()[0].overlaps_previous);
}

#[test]
fn arbitrary_capture_boundaries_preserve_samples() {
    let audio = pcm(111, -1200);
    let mut chunker = chunker();
    for fragment in audio.chunks(37) {
        assert!(chunker.append(fragment).unwrap().is_empty());
    }
    assert_eq!(&chunker.flush().unwrap()[0].wav[HEADER_BYTES..], audio);
}

#[test]
fn snapshots_preserve_frame_silence_overlap_and_cursor() {
    let audio = [pcm(1000, 0), pcm(14_000, 1200), pcm(500, 0)].concat();
    let mut uninterrupted = chunker();
    let mut expected = uninterrupted.append(&audio).unwrap();
    expected.extend(uninterrupted.flush().unwrap());
    for boundary in [0, 1, 37, 32_001, 191_997, 224_000, 300_007, audio.len()] {
        let mut before_crash = chunker();
        let mut actual = before_crash.append(&audio[..boundary]).unwrap();
        let mut restored = PcmChunker::restore(before_crash.snapshot()).unwrap();
        actual.extend(restored.append(&audio[boundary..]).unwrap());
        actual.extend(restored.flush().unwrap());
        assert_eq!(actual, expected, "restore boundary: {boundary}");
    }
}

#[test]
fn invalid_thresholds_and_checkpoints_are_rejected() {
    for threshold in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            PcmChunker::new(threshold),
            Err(ChunkerError::InvalidThreshold)
        ));
    }
    for kind in 0..7 {
        let mut state = chunker().snapshot();
        match kind {
            0 => state.audio = vec![0; MAX_AUDIO_BYTES],
            1 => state.frame = vec![0; 640],
            2 => state.new_bytes = 1,
            3 => state.version = 2,
            4 => state.audio = vec![0; 2],
            5 => state.processed_bytes = 1,
            _ => state.audio = vec![0; 640],
        }
        assert!(matches!(
            PcmChunker::restore(state),
            Err(ChunkerError::InvalidCheckpoint)
        ));
    }
}
