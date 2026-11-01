use std::future::Future;
use std::sync::Arc;

use voiceprompt_client_protocol::api::{ApiClient, ApiError};
use voiceprompt_client_protocol::delivery::DeliveryPolicy;
use voiceprompt_client_protocol::history::HistoryEntry;
use voiceprompt_client_protocol::recovery::{RecoveryManifest, utc_ticks};

use crate::dictation::dashed_id;
use crate::{EncryptedTokenStore, NativeError, NativeHistoryStore, NativeRecoveryStore};

pub trait AuditUploader: Send + Sync + 'static {
    fn upload(&self, state: &RecoveryManifest)
    -> impl Future<Output = Result<(), ApiError>> + Send;
    fn context(&self) -> impl Future<Output = Option<String>> + Send;
}

pub(crate) struct BoundAudit {
    pub api: Arc<ApiClient<EncryptedTokenStore>>,
    pub backend: String,
}
impl AuditUploader for BoundAudit {
    async fn context(&self) -> Option<String> {
        self.api.recovery_context(&self.backend).await
    }
    async fn upload(&self, state: &RecoveryManifest) -> Result<(), ApiError> {
        let (_, email) = state
            .context
            .split_once('\n')
            .filter(|(backend, email)| *backend == self.backend && !email.is_empty())
            .ok_or(ApiError {
                kind: voiceprompt_client_protocol::api::ApiErrorKind::Unauthorized,
                status: Some(401),
                response_invalid: false,
                problem: None,
            })?;
        self.api
            .archive_bound_dictation(state, email)
            .await
            .map(|_| ())
    }
}

pub struct AuditOutbox<S: AuditUploader> {
    recovery: NativeRecoveryStore,
    history: Arc<NativeHistoryStore>,
    uploader: Arc<S>,
    gate: tokio::sync::Mutex<()>,
}

#[derive(Default, Debug)]
pub struct DrainResult {
    pub acknowledged: usize,
    pub expired: usize,
    pub failed: usize,
}

impl<S: AuditUploader> AuditOutbox<S> {
    pub fn new(
        recovery: NativeRecoveryStore,
        history: Arc<NativeHistoryStore>,
        uploader: Arc<S>,
    ) -> Self {
        Self {
            recovery,
            history,
            uploader,
            gate: tokio::sync::Mutex::new(()),
        }
    }

    pub async fn drain(
        &self,
        policy: &DeliveryPolicy,
        now: time::OffsetDateTime,
    ) -> Result<DrainResult, NativeError> {
        let mut result = DrainResult::default();
        if !policy.can_upload_audit() {
            return Ok(result);
        }
        let Ok(_guard) = self.gate.try_lock() else {
            return Ok(result);
        };
        let root = self.recovery.clone();
        let entries = tokio::task::spawn_blocking(move || root.list())
            .await
            .map_err(|_| NativeError::Verification("Audit discovery worker failed."))??;
        for info in entries
            .into_iter()
            .filter(|entry| entry.error.is_none() && entry.audit_pending)
        {
            if !policy.can_upload_audit() {
                break;
            }
            let root = self.recovery.clone();
            let id = info.id.clone();
            let mut journal = match tokio::task::spawn_blocking(move || root.open(&id)).await {
                Ok(Ok(journal)) => journal,
                Ok(Err(error)) => {
                    eprintln!("Audit journal could not open: {error}");
                    result.failed += 1;
                    continue;
                }
                Err(error) => {
                    eprintln!("Audit journal worker failed: {error}");
                    result.failed += 1;
                    continue;
                }
            };
            let state = journal.snapshot()?;
            let Some(audit) = &state.audit else { continue };
            if self.uploader.context().await.as_deref() != Some(state.context.as_str()) {
                continue;
            }
            if utc_ticks(now)? - audit.completed_at_ticks >= 48 * 60 * 60 * 10_000_000 {
                journal.delete()?;
                result.expired += 1;
                eprintln!(
                    "Audit expired before cloud acknowledgment; fixed original retention applies."
                );
                continue;
            }
            let history = self.history.clone();
            let snapshot = state.clone();
            tokio::task::spawn_blocking(move || {
                let id = dashed_id(&snapshot.id);
                if !history
                    .all(now)?
                    .iter()
                    .any(|entry| entry.transcript_id == id)
                {
                    let audit = snapshot
                        .audit
                        .as_ref()
                        .ok_or(NativeError::Verification("Missing audit checkpoint."))?;
                    let completed_at = time::OffsetDateTime::from_unix_timestamp_nanos(
                        (i128::from(audit.completed_at_ticks)
                            - i128::from(
                                voiceprompt_client_protocol::recovery::DOTNET_UNIX_EPOCH_TICKS,
                            ))
                            * 100,
                    )
                    .map_err(|_| NativeError::Verification("Invalid audit completion ticks."))?;
                    history.add(
                        HistoryEntry {
                            transcript_id: id.clone(),
                            recording_id: id,
                            body: audit.text.clone(),
                            raw_body: audit.polished.then(|| snapshot.confirmed_text.clone()),
                            preview: audit.text.chars().take(160).collect(),
                            completed_at,
                            cached_at: now,
                            character_count: audit.text.encode_utf16().count(),
                            refinement_fallback_blocks: 0,
                        },
                        now,
                    )?;
                }
                Ok::<_, NativeError>(())
            })
            .await
            .map_err(|_| NativeError::Verification("Audit History worker failed."))??;
            if !policy.can_upload_audit() {
                break;
            }
            match self.uploader.upload(&state).await {
                Ok(()) => {
                    if policy.can_upload_audit()
                        && self.uploader.context().await.as_deref() == Some(state.context.as_str())
                    {
                        journal.delete()?;
                        result.acknowledged += 1;
                    }
                }
                Err(error) => {
                    eprintln!("Audit upload remains pending: {error}");
                    result.failed += 1;
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_RECOVERY_QUOTA;
    use crate::recovery_store::tests::TemporaryRoot;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Upload {
        fail: AtomicBool,
        calls: AtomicUsize,
        context: String,
    }
    impl AuditUploader for Upload {
        async fn context(&self) -> Option<String> {
            Some(self.context.clone())
        }
        async fn upload(&self, _: &RecoveryManifest) -> Result<(), ApiError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            if self.fail.load(Ordering::Acquire) {
                Err(ApiError {
                    kind: voiceprompt_client_protocol::api::ApiErrorKind::Retryable,
                    status: Some(503),
                    problem: None,
                    response_invalid: false,
                })
            } else {
                Ok(())
            }
        }
    }
    #[tokio::test]
    async fn failed_audit_survives_restart_and_only_ack_deletes_it_with_fixed_history_expiry() {
        let root = TemporaryRoot::new();
        let recovery = NativeRecoveryStore::new(root.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
        let history_root = TemporaryRoot::new();
        let history =
            Arc::new(NativeHistoryStore::new(history_root.0.join("history.json")).unwrap());
        let current = time::OffsetDateTime::now_utc();
        let now = time::OffsetDateTime::from_unix_timestamp_nanos(
            current.unix_timestamp_nanos().div_euclid(100) * 100,
        )
        .unwrap();
        let journal = recovery
            .create(
                "auto".into(),
                "https://fixture\nfixture@example.invalid".into(),
                true,
                now,
            )
            .unwrap();
        journal.checkpoint(&[], 0, &[], true, now).unwrap();
        journal
            .prepare_audit("synthetic text".into(), now, false, now)
            .unwrap();
        let id = journal.id().to_owned();
        drop(journal);
        let uploader = Arc::new(Upload {
            fail: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
            context: "https://fixture\nfixture@example.invalid".into(),
        });
        let outbox = AuditOutbox::new(recovery.clone(), history.clone(), uploader.clone());
        let policy = DeliveryPolicy::default();
        policy.set_paused(true);
        assert_eq!(outbox.drain(&policy, now).await.unwrap().failed, 1);
        assert!(
            recovery
                .open(&id)
                .unwrap()
                .snapshot()
                .unwrap()
                .audit
                .is_some()
        );
        assert_eq!(history.all(now).unwrap()[0].completed_at, now);
        uploader.fail.store(false, Ordering::Release);
        assert_eq!(
            outbox
                .drain(&policy, now + time::Duration::hours(1))
                .await
                .unwrap()
                .acknowledged,
            1
        );
        assert!(recovery.open(&id).is_err());
        assert_eq!(history.all(now).unwrap()[0].completed_at, now);
    }
    #[tokio::test]
    async fn wrong_binding_and_busy_dictation_never_upload_or_delete_saved_outbox() {
        let root = TemporaryRoot::new();
        let recovery = NativeRecoveryStore::new(root.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
        let history_root = TemporaryRoot::new();
        let history =
            Arc::new(NativeHistoryStore::new(history_root.0.join("history.json")).unwrap());
        let now = time::OffsetDateTime::now_utc();
        let journal = recovery
            .create("auto".into(), "original".into(), true, now)
            .unwrap();
        journal.checkpoint(&[], 0, &[], true, now).unwrap();
        journal
            .prepare_audit("synthetic".into(), now, false, now)
            .unwrap();
        let id = journal.id().to_owned();
        drop(journal);
        let uploader = Arc::new(Upload {
            fail: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            context: "another".into(),
        });
        let outbox = AuditOutbox::new(recovery.clone(), history, uploader.clone());
        let policy = DeliveryPolicy::default();
        policy.set_dictation_busy(true);
        outbox.drain(&policy, now).await.unwrap();
        policy.set_dictation_busy(false);
        outbox.drain(&policy, now).await.unwrap();
        assert_eq!(uploader.calls.load(Ordering::Acquire), 0);
        assert!(recovery.open(&id).is_ok());
    }
}
