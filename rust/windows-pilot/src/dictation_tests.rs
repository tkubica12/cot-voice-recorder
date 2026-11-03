use super::*;
use crate::recovery_store::tests::TemporaryRoot;
use std::sync::{Condvar, Mutex};
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

#[tokio::test(flavor = "current_thread")]
async fn blocking_capture_checkpoint_leaves_async_timers_and_control_messages_responsive() {
    let (sender, receiver) = mpsc::channel(2);
    sender.try_send(CaptureBuffer(vec![1, 0])).unwrap();
    sender.try_send(CaptureBuffer(vec![2, 0])).unwrap();
    let discard = Arc::new(AtomicBool::new(false));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = gate.clone();
    let (began, started) = oneshot::channel();
    let (stopped, mut observed) = watch::channel(false);
    let worker = tokio::task::spawn_blocking(move || {
        let mut began = Some(began);
        let mut bytes = Vec::new();
        let result = persist_capture(receiver, &discard, &stopped, |pcm| {
            if let Some(began) = began.take() {
                began.send(()).unwrap();
                let (lock, changed) = &*gate;
                let mut ready = lock.lock().unwrap();
                while !*ready {
                    ready = changed.wait(ready).unwrap();
                }
            }
            bytes.extend_from_slice(pcm);
            Ok(())
        });
        (bytes, result)
    });
    started.await.unwrap();
    let (command, mut commands) = mpsc::channel(1);
    command.send(DictationCommand::Interrupt).await.unwrap();
    let responsiveness = tokio::time::timeout(Duration::from_millis(100), async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(matches!(
            commands.recv().await,
            Some(DictationCommand::Interrupt)
        ));
    })
    .await;
    let (lock, changed) = &*release;
    *lock.lock().unwrap() = true;
    changed.notify_one();
    drop(sender);
    let (bytes, result) = worker.await.unwrap();
    assert!(
        responsiveness.is_ok(),
        "Blocked persistence starved the async control runtime."
    );
    assert_eq!(bytes, [1, 0, 2, 0]);
    assert!(result.is_ok());
    observed.changed().await.unwrap();
    assert!(*observed.borrow());
}

#[tokio::test]
async fn capture_write_failure_notifies_controller_before_source_close_and_drains_without_retry() {
    let (sender, receiver) = mpsc::channel(2);
    sender.try_send(CaptureBuffer(vec![1, 0])).unwrap();
    sender.try_send(CaptureBuffer(vec![2, 0])).unwrap();
    let (stopped, mut observed) = watch::channel(false);
    let worker = tokio::task::spawn_blocking(move || {
        let mut writes = 0;
        let result = persist_capture(receiver, &AtomicBool::new(false), &stopped, |_| {
            writes += 1;
            Err(SessionError::Storage(NativeError::Verification(
                "Synthetic blocked-writer failure.",
            )))
        });
        (writes, result)
    });
    observed.changed().await.unwrap();
    assert!(*observed.borrow());
    assert!(
        !worker.is_finished(),
        "An open source is drained only after capture shutdown."
    );
    drop(sender);
    let (writes, result) = worker.await.unwrap();
    assert_eq!(writes, 1);
    assert!(matches!(result, Err(SessionError::Storage(_))));
}

#[tokio::test]
async fn explicit_discard_invalidates_queued_worker_audio_but_interruption_preserves_it() {
    for discarded in [false, true] {
        let receiver = buffers().unwrap();
        let (stopped, _) = watch::channel(false);
        let worker = tokio::task::spawn_blocking(move || {
            let mut bytes = Vec::new();
            let result = persist_capture(receiver, &AtomicBool::new(discarded), &stopped, |pcm| {
                bytes.extend_from_slice(pcm);
                Ok(())
            });
            (bytes, result)
        });
        let (bytes, result) = worker.await.unwrap();
        assert!(result.is_ok());
        if discarded {
            assert!(bytes.is_empty());
        } else {
            assert_eq!(bytes, [1, 0, 2, 0, 3, 0, 4, 0]);
        }
    }
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

struct StartupChannels {
    command_sender: mpsc::Sender<DictationCommand>,
    commands: mpsc::Receiver<DictationCommand>,
    auth_sender: watch::Sender<AuthState>,
    auth: watch::Receiver<AuthState>,
    shutdown_sender: watch::Sender<bool>,
    shutdown: watch::Receiver<bool>,
}

impl StartupChannels {
    fn new() -> Self {
        let (command_sender, commands) = mpsc::channel(8);
        let (auth_sender, auth) = watch::channel(AuthState::SignedIn);
        let (shutdown_sender, shutdown) = watch::channel(false);
        Self {
            command_sender,
            commands,
            auth_sender,
            auth,
            shutdown_sender,
            shutdown,
        }
    }

    fn interrupt(&self, reason: StartupInterruption) {
        match reason {
            StartupInterruption::Stopped => self
                .command_sender
                .try_send(DictationCommand::Stop { generation: 13 })
                .unwrap(),
            StartupInterruption::Discarded => self
                .command_sender
                .try_send(DictationCommand::Discard)
                .unwrap(),
            StartupInterruption::Interrupted => self
                .command_sender
                .try_send(DictationCommand::Interrupt)
                .unwrap(),
            StartupInterruption::AuthChanged => {
                self.auth_sender.send_replace(AuthState::SignedOut);
            }
            StartupInterruption::Shutdown => {
                self.shutdown_sender.send_replace(true);
            }
        }
    }
}

const STARTUP_INTERRUPTS: [StartupInterruption; 5] = [
    StartupInterruption::Stopped,
    StartupInterruption::Discarded,
    StartupInterruption::Interrupted,
    StartupInterruption::AuthChanged,
    StartupInterruption::Shutdown,
];

#[tokio::test]
async fn pending_stop_discard_auth_and_shutdown_never_poll_initialization() {
    for reason in STARTUP_INTERRUPTS {
        let mut channels = StartupChannels::new();
        channels.interrupt(reason);
        let result = initialize_startup(
            async { panic!("Already interrupted initialization must not acquire any resources.") },
            13,
            &mut channels.commands,
            &channels.auth,
            &channels.shutdown,
        )
        .await;
        assert!(matches!(result, StartupOutcome::Interrupted(actual, None) if actual == reason));
    }
}

#[tokio::test]
async fn in_flight_initialization_is_joined_but_cannot_start_an_engine_or_capture_after_interrupt()
{
    for reason in STARTUP_INTERRUPTS {
        let mut channels = StartupChannels::new();
        let command_sender = channels.command_sender.clone();
        let auth_sender = channels.auth_sender.clone();
        let shutdown_sender = channels.shutdown_sender.clone();
        let (started, starting) = oneshot::channel();
        let (finish, finishing) = oneshot::channel();
        let initialized = Arc::new(AtomicBool::new(false));
        let completed = initialized.clone();
        let worker = tokio::spawn(async move {
            initialize_startup(
                async {
                    started.send(()).unwrap();
                    finishing.await.unwrap();
                    completed.store(true, Ordering::Release);
                    123
                },
                13,
                &mut channels.commands,
                &channels.auth,
                &channels.shutdown,
            )
            .await
        });
        starting.await.unwrap();
        match reason {
            StartupInterruption::Stopped => command_sender
                .try_send(DictationCommand::Stop { generation: 13 })
                .unwrap(),
            StartupInterruption::Discarded => {
                command_sender.try_send(DictationCommand::Discard).unwrap()
            }
            StartupInterruption::Interrupted => command_sender
                .try_send(DictationCommand::Interrupt)
                .unwrap(),
            StartupInterruption::AuthChanged => {
                auth_sender.send_replace(AuthState::SignedOut);
            }
            StartupInterruption::Shutdown => {
                shutdown_sender.send_replace(true);
            }
        }
        assert!(!initialized.load(Ordering::Acquire));
        assert!(!worker.is_finished());
        finish.send(()).unwrap();
        match worker.await.unwrap() {
            StartupOutcome::Interrupted(actual, Some(123)) => assert_eq!(actual, reason),
            _ => panic!("Interrupted initialization must never reach session/capture startup."),
        }
        assert!(initialized.load(Ordering::Acquire));
    }
}

#[tokio::test]
async fn healthy_startup_ignores_stale_stop_and_duplicate_begin_for_both_modes() {
    for mode in [DictationMode::Hold, DictationMode::Toggle] {
        let mut channels = StartupChannels::new();
        channels
            .command_sender
            .try_send(DictationCommand::Stop { generation: 12 })
            .unwrap();
        channels
            .command_sender
            .try_send(DictationCommand::Begin {
                generation: 14,
                mode,
            })
            .unwrap();
        let mut preparations = 0;
        let result = initialize_startup(
            async {
                preparations += 1;
                Ok::<_, NativeError>(456)
            },
            13,
            &mut channels.commands,
            &channels.auth,
            &channels.shutdown,
        )
        .await;
        assert!(matches!(result, StartupOutcome::Ready(Ok(456))));
        assert_eq!(preparations, 1);
        assert!(channels.commands.try_recv().is_err());
    }
}

#[test]
fn closed_control_channels_and_already_seen_shutdown_or_auth_changes_block_startup() {
    let mut channels = StartupChannels::new();
    drop(channels.command_sender);
    assert_eq!(
        startup_interruption(
            13,
            &mut channels.commands,
            &channels.auth,
            &channels.shutdown
        ),
        Some(StartupInterruption::Interrupted)
    );
    let mut channels = StartupChannels::new();
    drop(channels.auth_sender);
    assert_eq!(
        startup_interruption(
            13,
            &mut channels.commands,
            &channels.auth,
            &channels.shutdown
        ),
        Some(StartupInterruption::AuthChanged)
    );
    let mut channels = StartupChannels::new();
    drop(channels.shutdown_sender);
    assert_eq!(
        startup_interruption(
            13,
            &mut channels.commands,
            &channels.auth,
            &channels.shutdown
        ),
        Some(StartupInterruption::Shutdown)
    );
    let mut channels = StartupChannels::new();
    channels.shutdown_sender.send_replace(true);
    channels.shutdown.borrow_and_update();
    assert_eq!(
        startup_interruption(
            13,
            &mut channels.commands,
            &channels.auth,
            &channels.shutdown
        ),
        Some(StartupInterruption::Shutdown)
    );
    let mut channels = StartupChannels::new();
    channels.auth_sender.send_replace(AuthState::SignedIn);
    assert_eq!(
        startup_interruption(
            13,
            &mut channels.commands,
            &channels.auth,
            &channels.shutdown
        ),
        Some(StartupInterruption::AuthChanged),
        "An unseen auth epoch still invalidates the old initialization, even with the same state."
    );
}

#[tokio::test]
async fn initialization_errors_remain_errors_in_ready_and_interrupted_outcomes() {
    for interrupted in [false, true] {
        let mut channels = StartupChannels::new();
        let sender = channels.command_sender.clone();
        let result = initialize_startup(
            async {
                if interrupted {
                    sender.try_send(DictationCommand::Discard).unwrap();
                }
                Err::<(), _>(NativeError::Verification(
                    "Synthetic initialization failure.",
                ))
            },
            13,
            &mut channels.commands,
            &channels.auth,
            &channels.shutdown,
        )
        .await;
        match result {
            StartupOutcome::Ready(Err(_)) => assert!(!interrupted),
            StartupOutcome::Interrupted(StartupInterruption::Discarded, Some(Err(_))) => {
                assert!(interrupted);
            }
            _ => panic!("Initialization failure must never become a ready resource."),
        }
    }
}

#[test]
fn startup_cancellation_only_deletes_new_stopped_or_discarded_journals_and_retains_recovery() {
    for streaming in [false, true] {
        for recovering in [false, true] {
            for reason in STARTUP_INTERRUPTS {
                let directory = TemporaryRoot::new();
                let root = open_recovery(directory.0.clone()).unwrap();
                let journal = root
                    .create(
                        "auto".into(),
                        "synthetic startup cancellation".into(),
                        streaming,
                        time::OffsetDateTime::now_utc(),
                    )
                    .unwrap();
                let id = journal.id().to_owned();
                let wav = voiceprompt_core::wav::to_wav(&[1, 0, 2, 0]).unwrap();
                if recovering {
                    journal
                        .checkpoint(
                            &[],
                            4,
                            &[AudioCheckpoint {
                                wav: &wav,
                                overlaps_previous: false,
                                end_byte: 4,
                            }],
                            false,
                            time::OffsetDateTime::now_utc(),
                        )
                        .unwrap();
                }
                let before = journal.snapshot().unwrap();
                release_interrupted_journal(journal, &root, reason, recovering).unwrap();
                if !recovering
                    && matches!(
                        reason,
                        StartupInterruption::Stopped | StartupInterruption::Discarded
                    )
                {
                    assert!(root.list().unwrap().is_empty());
                } else {
                    let retained = root.open(&id).unwrap();
                    if recovering {
                        assert_eq!(retained.read_audio(0).unwrap(), wav);
                    }
                    let retained = retained.snapshot().unwrap();
                    assert_eq!(retained.encode().unwrap(), before.encode().unwrap());
                }
            }
        }
    }
}
