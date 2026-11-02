use super::*;
use crate::recovery_store::tests::TemporaryRoot;
use voiceprompt_client_protocol::recovery::AudioCheckpoint;

fn buffers() -> Option<mpsc::Receiver<CaptureBuffer>> {
    let (sender, receiver) = mpsc::channel(2);
    sender.try_send(CaptureBuffer(vec![1, 0, 2, 0])).unwrap();
    sender.try_send(CaptureBuffer(vec![3, 0, 4, 0])).unwrap();
    drop(sender);
    Some(receiver)
}

#[tokio::test]
async fn interrupted_capture_preserves_all_queued_samples_in_both_recovery_formats() {
    for streaming in [false, true] {
        let directory = TemporaryRoot::new();
        let store = open_recovery(directory.0.clone()).unwrap();
        let journal = store
            .create(
                "auto".into(),
                "synthetic original binding".into(),
                streaming,
                time::OffsetDateTime::now_utc(),
            )
            .unwrap();
        let id = journal.id().to_owned();
        let mut captured = 0;
        // Auth change, interruption and failed microphone stop preserve rather than discard.
        drain_capture(&mut buffers(), false, |pcm| {
            captured += pcm.len() as u64;
            let wav = voiceprompt_core::wav::to_wav(pcm).map_err(|_| SessionError::Worker)?;
            journal
                .checkpoint(
                    &[],
                    captured,
                    &[AudioCheckpoint {
                        wav: &wav,
                        overlaps_previous: false,
                        end_byte: captured,
                    }],
                    false,
                    time::OffsetDateTime::now_utc(),
                )
                .map_err(SessionError::Storage)
        })
        .await
        .unwrap();
        assert_eq!(captured, 8);
        drop(journal);
        let restored = store.open(&id).unwrap();
        let manifest = restored.snapshot().unwrap();
        assert_eq!(manifest.captured_bytes, 8);
        assert_eq!(manifest.confirmed_bytes, 0);
        assert_eq!(manifest.context, "synthetic original binding");
        assert_eq!(&restored.read_audio(0).unwrap()[44..], &[1, 0, 2, 0]);
        assert_eq!(&restored.read_audio(1).unwrap()[44..], &[3, 0, 4, 0]);
    }
}

#[tokio::test]
async fn explicit_discard_drains_without_persisting_or_delivering_audio() {
    drain_capture(&mut buffers(), true, |_| {
        panic!("An explicit discard must never append queued audio.")
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn persistence_failure_is_not_reported_as_success_and_no_more_writes_are_attempted() {
    let mut attempts = 0;
    let result = drain_capture(&mut buffers(), false, |_| {
        attempts += 1;
        Err(SessionError::Storage(NativeError::Verification(
            "Synthetic persistence failure.",
        )))
    })
    .await;
    assert!(matches!(result, Err(SessionError::Storage(_))));
    assert_eq!(attempts, 1);
}
