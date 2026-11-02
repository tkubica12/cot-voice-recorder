use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use voiceprompt_client_protocol::api::ApiClient;
use voiceprompt_client_protocol::delivery::DeliveryPolicy;
use voiceprompt_client_protocol::dictation_text::preview_tail;
use voiceprompt_client_protocol::history::HistoryEntry;
use voiceprompt_client_protocol::polishing::{Polisher, RefinementService};
use voiceprompt_client_protocol::settings::AppSettings;

use crate::client::{ClientStatus, UiRequest};
use crate::microphone::{CaptureBuffer, Microphone};
use crate::{
    BoundBatchBackend, DEFAULT_RECOVERY_QUOTA, EncryptedTokenStore, LegacySession, NativeError,
    NativeHistoryStore, NativeRecoveryStore, SessionError, StreamingSession,
};

#[cfg(test)]
#[path = "dictation_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictationMode {
    Hold,
    Toggle,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictationStage {
    Idle,
    Starting,
    Recording,
    Finishing,
    Saved,
    Pasted,
    Failed,
}

#[derive(Clone)]
pub struct DictationView {
    pub generation: u64,
    pub mode: DictationMode,
    pub stage: DictationStage,
    pub preview: String,
}

impl Default for DictationView {
    fn default() -> Self {
        Self {
            generation: 0,
            mode: DictationMode::Hold,
            stage: DictationStage::Idle,
            preview: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryResult {
    Pasted,
    CopiedOnly,
    ClipboardFailed,
}

pub(crate) enum DictationCommand {
    Begin {
        generation: u64,
        mode: DictationMode,
    },
    Stop {
        generation: u64,
    },
    Discard,
    Interrupt,
    Recover {
        id: String,
        generation: u64,
    },
}

type Service = BoundBatchBackend<EncryptedTokenStore>;
struct Refiner {
    api: Arc<ApiClient<EncryptedTokenStore>>,
    email: String,
}
impl RefinementService for Refiner {
    async fn refine(
        &self,
        text: &str,
        context: &str,
    ) -> Result<
        voiceprompt_client_protocol::api::DictationRefinement,
        voiceprompt_client_protocol::api::ApiError,
    > {
        self.api
            .refine_bound_dictation(text, context, &self.email)
            .await
    }
}
enum Session {
    Legacy(LegacySession<Service>),
    Streaming(StreamingSession<Service>),
}

impl Session {
    fn append(&self, bytes: &[u8]) -> Result<(), SessionError> {
        match self {
            Self::Legacy(session) => session.append(bytes),
            Self::Streaming(session) => session.append(bytes),
        }
    }
    fn finish(&self) -> Result<(), SessionError> {
        match self {
            Self::Legacy(session) => session.finish_capture(),
            Self::Streaming(session) => session.finish_capture(),
        }
    }
    fn snapshot(
        &self,
    ) -> Result<voiceprompt_client_protocol::recovery::RecoveryManifest, SessionError> {
        match self {
            Self::Legacy(session) => session.snapshot(),
            Self::Streaming(session) => session.snapshot(),
        }
    }
    fn progress(&self) -> watch::Receiver<crate::DictationProgress> {
        match self {
            Self::Legacy(session) => session.progress(),
            Self::Streaming(session) => session.progress(),
        }
    }
    async fn completion(
        &self,
        wait: Duration,
        reset: bool,
    ) -> Result<Option<String>, SessionError> {
        match self {
            Self::Legacy(session) => session.wait_for_completion(wait, reset).await,
            Self::Streaming(session) => session.wait_for_completion(wait, reset).await,
        }
    }
    async fn shutdown(self) -> Result<(), SessionError> {
        match self {
            Self::Legacy(session) => session.shutdown().await,
            Self::Streaming(session) => session.shutdown().await,
        }
    }
}

pub(crate) struct DictationHost {
    pub api: Arc<ApiClient<EncryptedTokenStore>>,
    pub recovery: NativeRecoveryStore,
    pub history: Arc<NativeHistoryStore>,
    pub settings: watch::Receiver<AppSettings>,
    pub status: watch::Sender<ClientStatus>,
    pub ui: mpsc::Sender<UiRequest>,
    pub policy: Arc<DeliveryPolicy>,
    pub commands: mpsc::Receiver<DictationCommand>,
    pub shutdown: watch::Receiver<bool>,
    pub devices_enabled: bool,
}

fn update(
    status: &watch::Sender<ClientStatus>,
    generation: u64,
    mode: DictationMode,
    stage: DictationStage,
    preview: String,
) {
    status.send_modify(|status| {
        status.dictation = DictationView {
            generation,
            mode,
            stage,
            preview,
        }
    });
}

fn report(status: &watch::Sender<ClientStatus>, issue: &'static str) {
    eprintln!("Dictation: {issue}");
    status.send_modify(|status| status.issue = Some(issue));
}

async fn load_session(
    host: &DictationHost,
    recover: Option<&str>,
) -> Result<(Session, String, String), NativeError> {
    let settings = host.settings.borrow().clone();
    let context = host
        .api
        .recovery_context(&settings.backend_base_url)
        .await
        .ok_or(NativeError::Verification(
            "Sign in before dictating or recovering saved audio.",
        ))?;
    let root = host.recovery.clone();
    let language = settings.dictation_language;
    let binding = context.clone();
    let streaming = settings.dictation_streaming_enabled;
    let recovery_id = recover.map(str::to_owned);
    let journal = tokio::task::spawn_blocking(move || match recovery_id {
        Some(id) => root.open(&id),
        None => root.create(
            language,
            binding,
            streaming,
            time::OffsetDateTime::now_utc(),
        ),
    })
    .await
    .map_err(|_| NativeError::Verification("Recovery initialization worker failed."))??;
    let state = journal.snapshot()?;
    if state.context != context {
        return Err(NativeError::Verification(
            "Use the original signed-in account and backend for Recovery.",
        ));
    }
    let service = Arc::new(BoundBatchBackend::new(
        host.api.clone(),
        settings.backend_base_url,
    )?);
    let id = state.id.clone();
    let session = if state.streaming {
        StreamingSession::start(journal, service, recover.is_some()).map(Session::Streaming)
    } else {
        LegacySession::start(journal, service).map(Session::Legacy)
    }
    .map_err(|error| {
        eprintln!("Dictation session initialization: {error}");
        NativeError::Verification("Dictation session could not start.")
    })?;
    Ok((session, id, context))
}

async fn saved_history(
    host: &DictationHost,
    session: &Session,
    text: &str,
    raw_text: Option<&str>,
    fallback_blocks: usize,
    completed: Option<time::OffsetDateTime>,
) -> Result<time::OffsetDateTime, NativeError> {
    let state = session
        .snapshot()
        .map_err(|_| NativeError::Verification("Could not read finished dictation state."))?;
    let id = if state.streaming {
        dashed_id(&state.id)
    } else {
        format!("dictation-{}", state.id)
    };
    let now = time::OffsetDateTime::now_utc();
    let entry = HistoryEntry {
        transcript_id: id.clone(),
        recording_id: id,
        body: text.into(),
        raw_body: raw_text.map(str::to_owned),
        preview: text.chars().take(160).collect(),
        character_count: text.encode_utf16().count(),
        completed_at: completed.unwrap_or(now),
        cached_at: now,
        refinement_fallback_blocks: fallback_blocks,
    };
    let history = host.history.clone();
    tokio::task::spawn_blocking(move || history.add(entry, now))
        .await
        .map_err(|_| NativeError::Verification("Dictation history worker failed."))??;
    Ok(completed.unwrap_or(now))
}

pub(crate) fn dashed_id(id: &str) -> String {
    format!(
        "{}-{}-{}-{}-{}",
        &id[..8],
        &id[8..12],
        &id[12..16],
        &id[16..20],
        &id[20..]
    )
}

async fn stop_microphone(microphone: Option<Microphone>) -> bool {
    if let Some(microphone) = microphone {
        match tokio::task::spawn_blocking(move || microphone.finish()).await {
            Ok(Ok(())) => true,
            result => {
                eprintln!("Microphone shutdown failed: {result:?}");
                false
            }
        }
    } else {
        true
    }
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl DictationHost {
    pub async fn run(mut self) {
        loop {
            let command = tokio::select! {
                biased;
                _ = self.shutdown.changed() => return,
                command = self.commands.recv() => match command { Some(command) => command, None => return },
            };
            let (generation, mode, recovering) = match command {
                DictationCommand::Begin { generation, mode } => {
                    if !self.devices_enabled || !self.settings.borrow().dictation_enabled {
                        continue;
                    }
                    (generation, mode, None)
                }
                DictationCommand::Recover { id, generation } => {
                    (generation, DictationMode::Toggle, Some(id))
                }
                _ => continue,
            };
            self.policy.set_dictation_busy(true);
            update(
                &self.status,
                generation,
                mode,
                DictationStage::Starting,
                String::new(),
            );
            let mut auth = self.api.auth_state();
            let initialized = load_session(&self, recovering.as_deref()).await;
            let (session, id, context) = match initialized {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("Dictation startup: {error}");
                    report(
                        &self.status,
                        "Dictation could not start. Check sign-in, Settings and Recovery.",
                    );
                    update(
                        &self.status,
                        generation,
                        mode,
                        DictationStage::Failed,
                        String::new(),
                    );
                    self.policy.set_dictation_busy(false);
                    continue;
                }
            };
            let is_streaming = matches!(&session, Session::Streaming(_));
            let mut microphone = None;
            let mut audio = None;
            if recovering.is_none() {
                match tokio::task::spawn_blocking(Microphone::start).await {
                    Ok(Ok((capture, receiver))) => {
                        microphone = Some(capture);
                        audio = Some(receiver);
                    }
                    error => {
                        match error {
                            Ok(Err(error)) => eprintln!("Dictation microphone startup: {error}"),
                            Err(error) => eprintln!("Dictation microphone worker failed: {error}"),
                            _ => unreachable!("success handled above"),
                        }
                        report(
                            &self.status,
                            "The microphone could not start. No text was pasted.",
                        );
                        let _ = session.shutdown().await;
                        update(
                            &self.status,
                            generation,
                            mode,
                            DictationStage::Failed,
                            String::new(),
                        );
                        self.policy.set_dictation_busy(false);
                        continue;
                    }
                }
            }
            let mut progress = session.progress();
            let polishing_settings = self.settings.borrow().clone();
            let polisher =
                if recovering.is_none() && polishing_settings.dictation_refinement_enabled {
                    context.split_once('\n').map(|(_, email)| {
                        Polisher::new(Arc::new(Refiner {
                            api: self.api.clone(),
                            email: email.to_owned(),
                        }))
                    })
                } else {
                    None
                };
            let mut discard = false;
            let mut explicit_discard = false;
            let mut preserve = false;
            update(
                &self.status,
                generation,
                mode,
                if recovering.is_some() {
                    DictationStage::Finishing
                } else {
                    DictationStage::Recording
                },
                String::new(),
            );
            if recovering.is_none() {
                loop {
                    tokio::select! {
                        biased;
                        _ = self.shutdown.changed() => { preserve = true; break; }
                        _ = auth.changed() => { preserve = true; break; }
                        command = self.commands.recv() => match command {
                            Some(DictationCommand::Stop { generation: expected }) if expected == generation => break,
                            Some(DictationCommand::Discard) => { discard = true; explicit_discard = true; break; }
                            Some(DictationCommand::Interrupt) | None => { preserve = true; break; }
                            _ => {},
                        },
                        buffer = next_audio(&mut audio) => match buffer {
                            Some(buffer) => if let Err(error) = session.append(&buffer.0) {
                                eprintln!("Audio persistence: {error}"); preserve = true; break;
                            },
                            None => { preserve = true; break; },
                        },
                        result = progress.changed() => {
                            if result.is_err() || progress.borrow().storage_failed { preserve = true; break; }
                            let preview = progress.borrow().preview.clone();
                            if let Some(polisher) = &polisher { polisher.update(&progress.borrow().transcript); }
                            update(&self.status, generation, mode, DictationStage::Recording, preview);
                        }
                    }
                }
            }
            let stopped = tokio::time::Instant::now();
            if let Some(polisher) = &polisher {
                polisher.stop(Duration::from_millis(u64::from(
                    polishing_settings.dictation_final_wait_milliseconds,
                )));
            }
            let capture_ok = stop_microphone(microphone.take()).await;
            preserve |= !capture_ok;
            if let Err(error) = drain_capture(&mut audio, discard, |pcm| session.append(pcm)).await
            {
                eprintln!("Final capture checkpoint: {error}");
                preserve = true;
            }
            if !discard {
                if let Err(error) = session.finish() {
                    eprintln!("Final recovery checkpoint: {error}");
                    preserve = true;
                }
            }
            if preserve {
                report(
                    &self.status,
                    "Dictation was interrupted. Saved audio is in Recovery; nothing was pasted.",
                );
            }
            update(
                &self.status,
                generation,
                mode,
                DictationStage::Finishing,
                progress.borrow().preview.clone(),
            );
            if !discard && !preserve {
                let streaming = matches!(session, Session::Streaming(_));
                let wait = if recovering.is_some() {
                    Duration::from_secs(30)
                } else if streaming {
                    Duration::from_secs(2)
                        .saturating_sub(stopped.elapsed())
                        .saturating_sub(Duration::from_millis(250))
                } else {
                    Duration::from_secs(30)
                };
                let mut completing = Box::pin(session.completion(wait, recovering.is_some()));
                let outcome = tokio::select! {
                    biased;
                    _ = self.shutdown.changed() => None,
                    _ = auth.changed() => None,
                    command = self.commands.recv() => {
                        discard = matches!(command, Some(DictationCommand::Discard)) && recovering.is_none();
                        explicit_discard |= discard;
                        None
                    },
                    result = &mut completing => Some(result),
                };
                drop(completing);
                match outcome {
                    Some(Ok(Some(mut text))) if !text.trim().is_empty() => {
                        let existing = match session.snapshot() {
                            Ok(state) => state
                                .audit
                                .clone()
                                .map(|audit| (audit, state.confirmed_text.clone())),
                            Err(error) => {
                                eprintln!("Completed dictation state could not be read: {error}");
                                report(
                                    &self.status,
                                    "Completed state could not be read. Recovery is retained and no text is delivered.",
                                );
                                let _ = session.shutdown().await;
                                self.policy.set_dictation_busy(false);
                                update(
                                    &self.status,
                                    generation,
                                    mode,
                                    DictationStage::Saved,
                                    String::new(),
                                );
                                continue;
                            }
                        };
                        let original = existing
                            .as_ref()
                            .and_then(|(audit, raw)| audit.polished.then_some(raw.as_str()));
                        let prior_completion = existing.as_ref().and_then(|(audit, _)| time::OffsetDateTime::from_unix_timestamp_nanos(
                            (i128::from(audit.completed_at_ticks) - i128::from(voiceprompt_client_protocol::recovery::DOTNET_UNIX_EPOCH_TICKS)) * 100).ok());
                        if let Some((audit, _)) = existing.as_ref() {
                            text = audit.text.clone();
                        }
                        match saved_history(&self, &session, &text, original, 0, prior_completion)
                            .await
                        {
                            Ok(completed) => {
                                let mut polished = false;
                                if let Some(polisher) = &polisher {
                                    let raw = text.clone();
                                    let remaining = Duration::from_millis(u64::from(
                                        polishing_settings.dictation_final_wait_milliseconds,
                                    ))
                                    .saturating_sub(stopped.elapsed())
                                    .saturating_sub(Duration::from_millis(250));
                                    let result = tokio::select! {
                                        biased;
                                        _ = self.shutdown.changed() => { preserve = true; polisher.freeze() },
                                        _ = auth.changed() => { preserve = true; polisher.freeze() },
                                        command = self.commands.recv() => {
                                            discard = matches!(command, Some(DictationCommand::Discard)) && recovering.is_none();
                                            explicit_discard |= discard;
                                            preserve = true;
                                            polisher.freeze()
                                        },
                                        result = polisher.complete(&raw, remaining) => result,
                                    };
                                    polished = result.successful_blocks > 0;
                                    match saved_history(
                                        &self,
                                        &session,
                                        &result.text,
                                        Some(&raw),
                                        result.fallback_blocks,
                                        Some(completed),
                                    )
                                    .await
                                    {
                                        Ok(_) => text = result.text,
                                        Err(error) => {
                                            eprintln!("Polished History update failed: {error}");
                                            report(
                                                &self.status,
                                                "Polished text could not be saved; original History is retained.",
                                            );
                                            preserve = true;
                                        }
                                    }
                                }
                                if preserve {
                                    let _ = session.shutdown().await;
                                    if discard {
                                        if explicit_discard {
                                            let history_id = if is_streaming {
                                                dashed_id(&id)
                                            } else {
                                                format!("dictation-{id}")
                                            };
                                            if let Err(error) = self.history.remove(&history_id) {
                                                eprintln!(
                                                    "Cancelled History removal failed: {error}"
                                                );
                                            }
                                        }
                                        if let Err(error) = self.recovery.delete_idle(&id) {
                                            eprintln!(
                                                "Cancelled dictation deletion failed: {error}"
                                            );
                                        }
                                    }
                                    self.policy.set_dictation_busy(false);
                                    update(
                                        &self.status,
                                        generation,
                                        mode,
                                        DictationStage::Saved,
                                        String::new(),
                                    );
                                    continue;
                                }
                                if let Session::Streaming(streaming) = &session {
                                    if existing.is_none() {
                                        match streaming.prepare_audit(
                                            text.clone(),
                                            completed,
                                            polished,
                                        ) {
                                            Ok(()) => {}
                                            Err(error) => {
                                                eprintln!("Audit freeze: {error}");
                                                report(
                                                    &self.status,
                                                    "Audit could not be frozen. History and recovery are retained.",
                                                );
                                            }
                                        }
                                    }
                                }
                                if recovering.is_none() {
                                    let cancel = Arc::new(AtomicBool::new(false));
                                    let _cancel = CancelOnDrop(cancel.clone());
                                    let (reply, result) = oneshot::channel();
                                    let request = UiRequest::Deliver {
                                        generation,
                                        text,
                                        cancelled: cancel,
                                        reply,
                                    };
                                    if self.ui.send(request).await.is_ok() {
                                        let delivered = tokio::select! {
                                            biased;
                                            _ = self.shutdown.changed() => None,
                                            _ = auth.changed() => None,
                                            command = self.commands.recv() => {
                                                discard = matches!(command, Some(DictationCommand::Discard));
                                                explicit_discard |= discard;
                                                None
                                            },
                                            result = tokio::time::timeout(Duration::from_secs(3), result) => result.ok().and_then(Result::ok),
                                        };
                                        update(
                                            &self.status,
                                            generation,
                                            mode,
                                            if delivered == Some(DeliveryResult::Pasted) {
                                                DictationStage::Pasted
                                            } else {
                                                DictationStage::Saved
                                            },
                                            progress.borrow().preview.clone(),
                                        );
                                    } else {
                                        report(
                                            &self.status,
                                            "Dictation is in History; the UI could not receive delivery.",
                                        );
                                    }
                                } else {
                                    update(
                                        &self.status,
                                        generation,
                                        mode,
                                        DictationStage::Saved,
                                        preview_tail(&text),
                                    );
                                }
                                if !streaming {
                                    discard = true;
                                }
                            }
                            Err(error) => {
                                eprintln!("Dictation History: {error}");
                                report(
                                    &self.status,
                                    "History could not be saved. Recovery is retained; nothing was pasted.",
                                );
                            }
                        }
                    }
                    Some(Ok(Some(_))) => {
                        discard = true;
                        update(
                            &self.status,
                            generation,
                            mode,
                            DictationStage::Idle,
                            String::new(),
                        );
                    }
                    Some(Ok(None)) => {
                        report(
                            &self.status,
                            "Transcription is pending. Open Recovery; no incomplete text was pasted.",
                        );
                        update(
                            &self.status,
                            generation,
                            mode,
                            DictationStage::Saved,
                            String::new(),
                        );
                    }
                    Some(Err(error)) => {
                        eprintln!("Dictation completion: {error}");
                        report(
                            &self.status,
                            "Transcription failed. Saved audio remains in Recovery.",
                        );
                    }
                    None => {}
                }
            }
            if let Err(error) = session.shutdown().await {
                eprintln!("Dictation engine shutdown: {error}");
            }
            if discard {
                if explicit_discard {
                    let history_id = if is_streaming {
                        dashed_id(&id)
                    } else {
                        format!("dictation-{id}")
                    };
                    if let Err(error) = self.history.remove(&history_id) {
                        eprintln!("Cancelled History removal failed: {error}");
                        report(
                            &self.status,
                            "Cancelled text could not be removed from History. Clear it manually.",
                        );
                    }
                }
                if let Err(error) = self.recovery.delete_idle(&id) {
                    eprintln!("Discard recovery deletion: {error}");
                    report(
                        &self.status,
                        "Discarded audio could not be deleted. Use Recovery to delete explicitly.",
                    );
                }
            }
            self.policy.set_dictation_busy(false);
            if matches!(
                self.status.borrow().dictation.stage,
                DictationStage::Starting | DictationStage::Recording | DictationStage::Finishing
            ) {
                update(
                    &self.status,
                    generation,
                    mode,
                    DictationStage::Idle,
                    String::new(),
                );
            }
            if *self.shutdown.borrow() {
                return;
            }
        }
    }
}

async fn next_audio(receiver: &mut Option<mpsc::Receiver<CaptureBuffer>>) -> Option<CaptureBuffer> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

async fn drain_capture(
    receiver: &mut Option<mpsc::Receiver<CaptureBuffer>>,
    discard: bool,
    mut persist: impl FnMut(&[u8]) -> Result<(), SessionError>,
) -> Result<(), SessionError> {
    let mut failure = None;
    if let Some(receiver) = receiver {
        while let Some(buffer) = receiver.recv().await {
            if !discard && failure.is_none() {
                if let Err(error) = persist(&buffer.0) {
                    failure = Some(error);
                }
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(crate) fn open_recovery(path: std::path::PathBuf) -> Result<NativeRecoveryStore, NativeError> {
    NativeRecoveryStore::new(path, DEFAULT_RECOVERY_QUOTA)
}
