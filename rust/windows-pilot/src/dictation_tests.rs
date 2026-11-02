use super::*;
use crate::recovery_store::tests::TemporaryRoot;
use voiceprompt_client_protocol::recovery::AudioCheckpoint;

#[test]
fn recovery_view_origin_survives_progress_and_resets_for_a_new_live_generation() {
    let (sender, receiver) = watch::channel(ClientStatus::default());
    sender.send_modify(|status| {
        status.dictation.generation = 10;
        status.dictation.recovering = true;
    });
    update(
        &sender,
        10,
        DictationMode::Toggle,
        DictationStage::Finishing,
        String::new(),
    );
    assert!(receiver.borrow().dictation.recovering);
    update(
        &sender,
        10,
        DictationMode::Toggle,
        DictationStage::Saved,
        "Synthetic recovered text".into(),
    );
    assert!(receiver.borrow().dictation.recovering);
    update(
        &sender,
        11,
        DictationMode::Hold,
        DictationStage::Recording,
        String::new(),
    );
    assert!(!receiver.borrow().dictation.recovering);
}

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

fn delivery() -> (
    UiRequest,
    oneshot::Receiver<DeliveryResult>,
    Arc<AtomicBool>,
) {
    let cancelled = Arc::new(AtomicBool::new(false));
    let (reply, result) = oneshot::channel();
    (
        UiRequest::Deliver {
            generation: 1,
            text: "synthetic never access system clipboard".into(),
            cancelled: cancelled.clone(),
            reply,
        },
        result,
        cancelled,
    )
}

#[tokio::test(start_paused = true)]
async fn ui_queue_backpressure_is_part_of_the_delivery_budget() {
    let (sender, mut receiver) = mpsc::channel(1);
    sender.send(UiRequest::AuditWarning).await.unwrap();
    let (request, reply, cancelled) = delivery();
    let result = deliver_to_ui(sender, request, reply, cancelled.clone()).await;
    assert_eq!(result, Err(UiDeliveryError::TimedOut));
    assert!(cancelled.load(Ordering::Acquire));
    assert!(matches!(
        receiver.try_recv().unwrap(),
        UiRequest::AuditWarning
    ));
    assert!(receiver.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn timeout_invalidates_a_queued_unacknowledged_delivery_before_late_ui_processing() {
    let (sender, mut receiver) = mpsc::channel(1);
    let (request, reply, cancelled) = delivery();
    let pending = tokio::spawn(deliver_to_ui(sender, request, reply, cancelled.clone()));
    let UiRequest::Deliver {
        cancelled: token,
        reply,
        ..
    } = receiver.recv().await.unwrap()
    else {
        panic!("Expected an explicit synthetic delivery.");
    };
    assert!(!token.load(Ordering::Acquire));
    tokio::time::advance(Duration::from_secs(3)).await;
    assert_eq!(pending.await.unwrap(), Err(UiDeliveryError::TimedOut));
    assert!(token.load(Ordering::Acquire));
    assert!(reply.send(DeliveryResult::Pasted).is_err());
}

#[tokio::test]
async fn unavailable_ui_is_a_visible_failure_and_not_a_successful_delivery() {
    let (sender, receiver) = mpsc::channel(1);
    drop(receiver);
    let (request, reply, cancelled) = delivery();
    assert_eq!(
        deliver_to_ui(sender, request, reply, cancelled.clone()).await,
        Err(UiDeliveryError::Unavailable)
    );
    assert!(cancelled.load(Ordering::Acquire));
}
