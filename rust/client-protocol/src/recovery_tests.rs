use super::*;
use voiceprompt_core::wav::to_wav;

fn manifest(streaming: bool) -> (RecoveryManifest, Vec<u8>) {
    let wav = to_wav(&[1; 6400]).unwrap();
    let digest = Sha256::digest(&wav).into();
    (
        RecoveryManifest {
            id: "0123456789abcdef0123456789abcdef".into(),
            created_at_ticks: DOTNET_UNIX_EPOCH_TICKS,
            updated_at_ticks: DOTNET_UNIX_EPOCH_TICKS + 1,
            language: "auto".into(),
            captured_bytes: 6400,
            tail: Vec::new(),
            capture_complete: true,
            chunks: vec![RecoveryChunk {
                index: 0,
                overlaps_previous: false,
                text: None,
                audio_bytes: wav.len() as u32,
                digest,
                end_byte: 6400,
            }],
            context: "synthetic context".into(),
            streaming,
            confirmed_bytes: 0,
            confirmed_text: String::new(),
            used_streaming: false,
            used_fallback: false,
            last_segment_words: 0,
            audit: None,
        },
        wav,
    )
}

fn legacy_bytes(version: u32) -> Vec<u8> {
    let (manifest, _) = manifest(false);
    let mut writer = Writer(Vec::new());
    writer.u32(MAGIC).unwrap();
    writer.u32(version).unwrap();
    writer.bytes(manifest.id.as_bytes()).unwrap();
    writer.i64(manifest.created_at_ticks).unwrap();
    writer.i64(manifest.updated_at_ticks).unwrap();
    writer.bytes(manifest.language.as_bytes()).unwrap();
    writer.u64(manifest.captured_bytes).unwrap();
    writer.bytes(&manifest.tail).unwrap();
    writer.boolean(manifest.capture_complete).unwrap();
    writer.u32(1).unwrap();
    let chunk = &manifest.chunks[0];
    writer.u32(0).unwrap();
    writer.boolean(false).unwrap();
    writer.boolean(false).unwrap();
    writer.u32(chunk.audio_bytes).unwrap();
    writer.bytes(&chunk.digest).unwrap();
    if version >= 3 {
        writer.u64(chunk.end_byte).unwrap();
    }
    if version >= 2 {
        writer.bytes(manifest.context.as_bytes()).unwrap();
    }
    writer.0
}

#[test]
fn reads_legacy_v1_v2_and_roundtrips_current_v3_v4_without_inventing_cursors() {
    for version in 1..=3 {
        let bytes = legacy_bytes(version);
        let parsed = RecoveryManifest::parse(&bytes, "0123456789abcdef0123456789abcdef").unwrap();
        assert!(!parsed.streaming);
        assert_eq!(
            parsed.chunks[0].end_byte,
            if version == 3 { 6400 } else { 0 }
        );
        assert_eq!(
            parsed.context,
            if version == 1 {
                ""
            } else {
                "synthetic context"
            }
        );
        let upgraded = parsed.encode().unwrap();
        assert_eq!(u32::from_le_bytes(upgraded[4..8].try_into().unwrap()), 3);
    }
    let (manifest, _) = manifest(true);
    let bytes = manifest.encode().unwrap();
    let parsed = RecoveryManifest::parse(&bytes, &manifest.id).unwrap();
    assert!(parsed.streaming);
    assert_eq!(parsed.encode().unwrap(), bytes);
    assert!(!format!("{parsed:?}").contains("synthetic"));
}

#[test]
fn malformed_manifests_are_bounded_and_never_return_partial_success() {
    let (manifest, _) = manifest(true);
    let data = manifest.encode().unwrap();
    for end in 0..data.len() {
        assert!(RecoveryManifest::parse(&data[..end], &manifest.id).is_err());
    }
    let mut trailing = data.clone();
    trailing.push(0);
    assert!(RecoveryManifest::parse(&trailing, &manifest.id).is_err());
    assert!(RecoveryManifest::parse(&data, "ABCDEF0123456789ABCDEF0123456789").is_err());
    let mut version = data.clone();
    version[4..8].copy_from_slice(&5u32.to_le_bytes());
    assert!(RecoveryManifest::parse(&version, &manifest.id).is_err());
    let mut negative_length = data;
    negative_length[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(RecoveryManifest::parse(&negative_length, &manifest.id).is_err());
}

#[test]
fn wav_digest_and_canonical_layout_are_verified() {
    let (manifest, wav) = manifest(true);
    manifest.chunks[0].verify_audio(&wav).unwrap();
    let mut corrupt = wav.clone();
    corrupt[44] ^= 1;
    assert!(manifest.chunks[0].verify_audio(&corrupt).is_err());
    corrupt = wav;
    corrupt[20] = 3;
    assert!(manifest.chunks[0].verify_audio(&corrupt).is_err());
}

#[test]
fn confirmation_never_releases_audio_for_an_ack_or_a_provisional_hypothesis() {
    let (mut manifest, _) = manifest(true);
    let unchanged = manifest.encode().unwrap();
    assert!(
        manifest
            .confirm(6401, "invalid".into(), Vec::new(), false, None, 1)
            .is_err()
    );
    assert_eq!(manifest.encode().unwrap(), unchanged);
    assert!(
        manifest
            .freeze_audit("provisional".into(), 2, false, 3)
            .is_err()
    );
    assert_eq!(manifest.chunks[0].text, None);
    let ready = manifest
        .confirm(3200, "first".into(), vec![0; 100], false, None, 2)
        .unwrap();
    assert!(ready.is_empty());
    assert!(
        manifest
            .confirm(3200, "conflicting".into(), Vec::new(), false, None, 3)
            .is_err()
    );
    let ready = manifest
        .confirm(6400, "first final".into(), vec![0; 100], true, Some(1), 3)
        .unwrap();
    assert_eq!(ready, [0]);
    assert_eq!(manifest.chunks[0].text.as_deref(), Some(""));
    assert!(manifest.used_streaming && manifest.used_fallback);
    assert_eq!(manifest.last_segment_words, 1);
}

#[test]
fn repeated_streaming_confirmations_release_each_chunk_once_not_quadratic_old_deletions() {
    let wav = to_wav(&[1, 0]).unwrap();
    let mut state = RecoveryManifest::create(
        "auto".into(),
        "synthetic".into(),
        true,
        DOTNET_UNIX_EPOCH_TICKS,
    )
    .unwrap();
    let checkpoints: Vec<_> = (0..1000)
        .map(|index| AudioCheckpoint {
            wav: &wav,
            overlaps_previous: false,
            end_byte: (index + 1) * 2,
        })
        .collect();
    state = state
        .checkpoint(&[], 2000, &checkpoints, true, DOTNET_UNIX_EPOCH_TICKS)
        .unwrap();
    let mut text = String::new();
    let mut deleted = 0;
    for index in 0..1000 {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str("synthetic");
        let through = (index + 1) * 2;
        let released = state
            .confirm(
                through,
                text.clone(),
                vec![0; through as usize],
                false,
                Some(1),
                DOTNET_UNIX_EPOCH_TICKS,
            )
            .unwrap();
        assert_eq!(released, [index as u32]);
        deleted += released.len();
    }
    assert_eq!(deleted, 1000);
    assert!(
        state
            .confirm(
                2000,
                text,
                vec![0; 2000],
                false,
                Some(0),
                DOTNET_UNIX_EPOCH_TICKS
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn frozen_audit_completion_is_fixed_and_conflicts_cannot_extend_retention() {
    let (mut manifest, _) = manifest(true);
    manifest
        .confirm(6400, "original".into(), vec![0; 100], false, None, 1)
        .unwrap();
    manifest.freeze_audit("final".into(), 2, true, 3).unwrap();
    manifest.freeze_audit("final".into(), 2, true, 4).unwrap();
    assert!(manifest.tail.is_empty());
    assert!(manifest.freeze_audit("final".into(), 3, true, 5).is_err());
    assert!(manifest.freeze_audit("other".into(), 2, true, 5).is_err());
    assert_eq!(manifest.audit.as_ref().unwrap().completed_at_ticks, 2);
    let bytes = manifest.encode().unwrap();
    let parsed = RecoveryManifest::parse(&bytes, &manifest.id).unwrap();
    assert_eq!(parsed.audit.as_ref().unwrap().text, "final");
}

#[test]
fn streaming_gaps_overlap_odd_cursors_and_text_bounds_are_explicit_errors() {
    let (mut manifest, _) = manifest(true);
    manifest.chunks[0].end_byte = 6398;
    assert!(manifest.validate().is_err());
    manifest.chunks[0].end_byte = 6400;
    manifest.chunks[0].overlaps_previous = true;
    assert!(manifest.validate().is_err());
    manifest.chunks[0].overlaps_previous = false;
    manifest.confirmed_bytes = 1;
    assert!(manifest.validate().is_err());
    manifest.confirmed_bytes = 0;
    manifest.context = "x".repeat(MAX_CONTEXT_BYTES + 1);
    assert!(manifest.validate().is_err());
    assert_eq!(
        utc_ticks(time::OffsetDateTime::UNIX_EPOCH).unwrap(),
        DOTNET_UNIX_EPOCH_TICKS
    );
    assert_eq!(
        utc_ticks(time::OffsetDateTime::UNIX_EPOCH - time::Duration::nanoseconds(1)).unwrap(),
        DOTNET_UNIX_EPOCH_TICKS - 1
    );
}
