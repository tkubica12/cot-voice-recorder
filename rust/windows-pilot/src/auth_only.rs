use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use voiceprompt_client_protocol::auth::DesktopClientConfig;
use voiceprompt_client_protocol::runtime::{AuthRuntime, AuthRuntimeError};

use crate::client::{ClientCommand, ClientStatus};
use crate::platform::{EncryptedTokenStore, NativeError, WriteMode, atomic_write};

pub(crate) fn validate_google(config: &DesktopClientConfig) -> Result<(), NativeError> {
    if !matches!(
        config.auth_uri.as_str(),
        "https://accounts.google.com/o/oauth2/auth"
            | "https://accounts.google.com/o/oauth2/v2/auth"
    ) || config.token_uri.as_str() != "https://oauth2.googleapis.com/token"
    {
        return Err(NativeError::InvalidArgument(
            "Auth-only acceptance requires the exact official Google endpoints.",
        ));
    }
    Ok(())
}

pub(crate) struct Report {
    scope: &'static str,
    pid: u32,
    sign_in_attempted: bool,
    sign_out_attempted: bool,
    signed_in: bool,
    dpapi_id_token_readback: bool,
    dpapi_refresh_token_readback: bool,
    signed_in_bearer_available: bool,
    signed_out: bool,
    cache_removed: bool,
    stale_bearer_unavailable: bool,
    backend_or_realtime_constructed: bool,
    physical_ports_enabled: bool,
    provider_revocation_attempted: bool,
    cancelled_cleanup: bool,
    closed: bool,
    failed: bool,
    whole_migration_acceptance: bool,
}

impl Report {
    pub(crate) fn start(root: &Path) -> Result<Self, NativeError> {
        let report = Self {
            scope: crate::fixtures::AUTH_SCOPE,
            pid: std::process::id(),
            sign_in_attempted: false,
            sign_out_attempted: false,
            signed_in: false,
            dpapi_id_token_readback: false,
            dpapi_refresh_token_readback: false,
            signed_in_bearer_available: false,
            signed_out: false,
            cache_removed: false,
            stale_bearer_unavailable: false,
            backend_or_realtime_constructed: false,
            physical_ports_enabled: false,
            provider_revocation_attempted: false,
            cancelled_cleanup: false,
            closed: false,
            failed: false,
            whole_migration_acceptance: false,
        };
        report.save(root, WriteMode::CreateNew)?;
        Ok(report)
    }

    fn save(&self, root: &Path, mode: WriteMode) -> Result<(), NativeError> {
        let data = serde_json::to_vec(&serde_json::json!({
            "scope":self.scope, "pid":self.pid,
            "sign_in_attempted":self.sign_in_attempted,
            "sign_out_attempted":self.sign_out_attempted,
            "signed_in":self.signed_in,
            "dpapi_id_token_readback":self.dpapi_id_token_readback,
            "dpapi_refresh_token_readback":self.dpapi_refresh_token_readback,
            "signed_in_bearer_available":self.signed_in_bearer_available,
            "signed_out":self.signed_out, "cache_removed":self.cache_removed,
            "stale_bearer_unavailable":self.stale_bearer_unavailable,
            "backend_or_realtime_constructed":self.backend_or_realtime_constructed,
            "physical_ports_enabled":self.physical_ports_enabled,
            "provider_revocation_attempted":self.provider_revocation_attempted,
            "cancelled_cleanup":self.cancelled_cleanup,
            "closed":self.closed, "failed":self.failed,
            "whole_migration_acceptance":self.whole_migration_acceptance,
        }))
        .map_err(|_| NativeError::Verification("Encode safe auth-only evidence failed."))?;
        atomic_write(&root.join("auth-fixture-result.json"), &data, mode)
    }
}

#[derive(Debug)]
enum Check {
    SignedIn {
        id: bool,
        refresh: bool,
        bearer: bool,
    },
    SignedOut {
        cache_removed: bool,
        no_bearer: bool,
    },
}

// Deliberately separate from run_client: no backend, realtime, outbox, recovery or devices exist.
pub(crate) async fn run(
    auth: Arc<AuthRuntime<EncryptedTokenStore>>,
    mut commands: mpsc::Receiver<ClientCommand>,
    status: watch::Sender<ClientStatus>,
    mut stop: watch::Receiver<bool>,
    root: PathBuf,
    mut report: Report,
) {
    let mut state = auth.status();
    let mut tasks = tokio::task::JoinSet::<Result<Check, AuthRuntimeError<NativeError>>>::new();
    loop {
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            _ = state.changed() => {
                state.borrow_and_update();
                status.send_modify(|value| value.set_identity(auth.display_identity()));
            }
            Some(result) = tasks.join_next() => {
                match result {
                    Ok(Ok(Check::SignedIn { id, refresh, bearer })) => {
                        report.signed_in = true;
                        report.dpapi_id_token_readback = id;
                        report.dpapi_refresh_token_readback = refresh;
                        report.signed_in_bearer_available = bearer;
                        report.failed |= !id || !refresh || !bearer;
                    }
                    Ok(Ok(Check::SignedOut { cache_removed, no_bearer })) => {
                        report.signed_out = true;
                        report.cache_removed = cache_removed;
                        report.stale_bearer_unavailable = no_bearer;
                        report.failed |= !cache_removed || !no_bearer;
                    }
                    error => {
                        report.failed = true;
                        eprintln!("Auth-only operation failed: {error:?}");
                    }
                }
                status.send_modify(|value| {
                    value.signing_in = false;
                    value.signing_out = false;
                    if report.failed { value.issue = Some("Auth-only acceptance failed; do not repeat without permission."); }
                });
            }
            command = commands.recv() => {
                match command {
                    Some(ClientCommand::SignIn) if !report.sign_in_attempted && !report.sign_out_attempted => {
                        report.sign_in_attempted = true;
                        if let Err(error) = report.save(&root, WriteMode::Replace) {
                            super::client::issue(&status, "Auth-only evidence could not be saved; browser login was not started.");
                            eprintln!("Auth-only evidence: {error}");
                            break;
                        }
                        status.send_modify(|value| { value.signing_in = true; value.issue = None; });
                        let auth = auth.clone();
                        let root = root.clone();
                        tasks.spawn(async move {
                            auth.sign_in(super::client::browser).await?;
                            let store = EncryptedTokenStore::new(root.join("tokens.bin")).map_err(AuthRuntimeError::Storage)?;
                            let (id, refresh) = tokio::task::spawn_blocking(move || {
                                store.load().map(|cached| cached.map_or((false, false), |value| (
                                    value.id_token.is_some_and(|token| !token.is_empty()),
                                    value.refresh_token.is_some_and(|token| !token.is_empty()),
                                )))
                            }).await.map_err(AuthRuntimeError::Worker)?.map_err(AuthRuntimeError::Storage)?;
                            let bearer = auth.backend_id_token().await?.is_some();
                            Ok(Check::SignedIn { id, refresh, bearer })
                        });
                    }
                    Some(ClientCommand::SignOut) if report.signed_in && !report.sign_out_attempted && tasks.is_empty() => {
                        report.sign_out_attempted = true;
                        status.send_modify(|value| value.signing_out = true);
                        let auth = auth.clone();
                        let root = root.clone();
                        tasks.spawn(async move {
                            auth.sign_out().await?;
                            let cache_removed = !root.join("tokens.bin").try_exists().map_err(|error|
                                AuthRuntimeError::Storage(NativeError::Io("Inspect own signed-out cache", error)))?;
                            let no_bearer = auth.backend_id_token().await?.is_none();
                            Ok(Check::SignedOut { cache_removed, no_bearer })
                        });
                    }
                    None => break,
                    Some(_) => super::client::issue(&status, "Auth-only fixture allows exactly one sign-in and one completed sign-out, no other client actions."),
                }
            }
        }
        if let Err(error) = report.save(&root, WriteMode::Replace) {
            report.failed = true;
            super::client::issue(&status, "Auth-only evidence write failed.");
            eprintln!("Auth-only evidence: {error}");
        }
    }
    auth.cancel_sign_in().await;
    if !report.sign_out_attempted {
        tasks.abort_all();
    }
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(Check::SignedOut {
                cache_removed,
                no_bearer,
            })) => {
                report.signed_out = true;
                report.cache_removed = cache_removed;
                report.stale_bearer_unavailable = no_bearer;
                report.failed |= !cache_removed || !no_bearer;
            }
            Err(error) if error.is_cancelled() => {}
            error => {
                report.failed = true;
                eprintln!("Auth-only shutdown operation did not complete: {error:?}");
            }
        }
    }
    if report.sign_in_attempted && !report.signed_out {
        report.failed = true;
        report.cancelled_cleanup = true;
        if !report.sign_out_attempted {
            report.sign_out_attempted = true;
            if let Err(error) = auth.sign_out().await {
                eprintln!("Auth-only cancelled local-cache cleanup failed: {error}");
            }
        }
        // The session lock also joins any already-started blocking cache clear.
        match auth.backend_id_token().await {
            Ok(token) => report.stale_bearer_unavailable = token.is_none(),
            Err(error) => eprintln!("Auth-only cancelled bearer inspection failed: {error}"),
        }
        match root.join("tokens.bin").try_exists() {
            Ok(exists) => report.cache_removed = !exists,
            Err(error) => eprintln!("Auth-only cancelled cache inspection failed: {error}"),
        }
    }
    report.closed = true;
    if let Err(error) = report.save(&root, WriteMode::Replace) {
        eprintln!("Auth-only final evidence write failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_only_config_cannot_redirect_the_authorized_google_operation() {
        let mut config = DesktopClientConfig::parse(
            br#"{"installed":{"client_id":"synthetic","client_secret":"synthetic"}}"#,
        )
        .unwrap();
        validate_google(&config).unwrap();
        config.token_uri = "https://fixture.example.invalid/token".parse().unwrap();
        assert!(validate_google(&config).is_err());
        config.token_uri = "https://oauth2.googleapis.com/token".parse().unwrap();
        config.auth_uri = "https://accounts.google.com/o/oauth2/auth?unexpected=1"
            .parse()
            .unwrap();
        assert!(validate_google(&config).is_err());
    }

    #[test]
    fn auth_evidence_is_exclusive_and_contains_only_safe_state_not_credentials() {
        let directory = crate::recovery_store::tests::TemporaryRoot::new();
        let report = Report::start(&directory.0).unwrap();
        assert!(Report::start(&directory.0).is_err());
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.0.join("auth-fixture-result.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["scope"], crate::fixtures::AUTH_SCOPE);
        assert_eq!(value["backend_or_realtime_constructed"], false);
        assert_eq!(value["provider_revocation_attempted"], false);
        assert!(!report.signed_in && !report.sign_in_attempted);
        assert!(
            value
                .as_object()
                .unwrap()
                .values()
                .all(|value| value.is_boolean()
                    || value.is_u64()
                    || value.as_str() == Some(crate::fixtures::AUTH_SCOPE))
        );
    }

    #[tokio::test]
    async fn auth_only_shutdown_joins_the_one_local_sign_out_and_never_leaves_a_stale_cache() {
        let directory = crate::recovery_store::tests::TemporaryRoot::new();
        let store = EncryptedTokenStore::new(directory.0.join("tokens.bin")).unwrap();
        store
            .save(&voiceprompt_client_protocol::auth::CachedTokens {
                refresh_token: Some("synthetic-refresh".into()),
                id_token: Some("synthetic-id".into()),
                id_token_expires_at: Some(
                    time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                ),
                ..Default::default()
            })
            .unwrap();
        let config = DesktopClientConfig::parse(
            br#"{"installed":{"client_id":"synthetic","client_secret":"synthetic"}}"#,
        )
        .unwrap();
        let auth = Arc::new(
            AuthRuntime::initialize(
                Some(config),
                voiceprompt_client_protocol::network::GoogleTokenClient::new().unwrap(),
                store,
            )
            .await
            .unwrap(),
        );
        let mut report = Report::start(&directory.0).unwrap();
        report.sign_in_attempted = true;
        report.signed_in = true;
        let (sender, commands) = mpsc::channel(1);
        let (status, _) = watch::channel(ClientStatus::default());
        let (shutdown, stop) = watch::channel(false);
        let task = tokio::spawn(run(
            auth,
            commands,
            status,
            stop,
            directory.0.clone(),
            report,
        ));
        sender.send(ClientCommand::SignOut).await.unwrap();
        let until = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let value: serde_json::Value = serde_json::from_slice(
                &std::fs::read(directory.0.join("auth-fixture-result.json")).unwrap(),
            )
            .unwrap();
            if value["sign_out_attempted"] == true {
                break;
            }
            assert!(tokio::time::Instant::now() < until);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        shutdown.send_replace(true);
        task.await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.0.join("auth-fixture-result.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["closed"], true);
        assert_eq!(value["signed_out"], true);
        assert_eq!(value["stale_bearer_unavailable"], true);
        assert_eq!(value["cache_removed"], true);
        assert_eq!(value["failed"], false);
        assert!(!directory.0.join("tokens.bin").exists());
    }
}
