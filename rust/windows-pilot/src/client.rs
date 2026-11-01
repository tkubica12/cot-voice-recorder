use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use voiceprompt_client_protocol::ProtocolError;
use voiceprompt_client_protocol::api::ApiClient;
use voiceprompt_client_protocol::auth::DesktopClientConfig;
use voiceprompt_client_protocol::delivery::{
    ClipboardError, ClipboardWriter, CopyResult, DeliveryPolicy, Notice, NotificationError,
    Notifier, TranscriptCoordinator,
};
use voiceprompt_client_protocol::history::HistoryEntry;
use voiceprompt_client_protocol::network::GoogleTokenClient;
use voiceprompt_client_protocol::runtime::AuthRuntime;
use voiceprompt_client_protocol::settings::AppSettings;
use voiceprompt_client_protocol::state::AuthState;
use voiceprompt_client_protocol::websocket::{RealtimeClient, RealtimeState};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

use crate::platform::{EncryptedTokenStore, NativeError, NativeHistoryStore, NativeSettingsStore};

pub struct ClientOptions {
    pub state_directory: PathBuf,
    pub oauth_config: Option<PathBuf>,
    pub clipboard_enabled: bool,
}

#[derive(Clone)]
pub(crate) struct ClientStatus {
    pub auth: AuthState,
    pub realtime: RealtimeState,
    pub signing_in: bool,
    pub signing_out: bool,
    pub issue: Option<&'static str>,
    pub history: Arc<Vec<HistoryEntry>>,
}

impl Default for ClientStatus {
    fn default() -> Self {
        Self {
            auth: AuthState::NotConfigured,
            realtime: RealtimeState::Stopped,
            signing_in: false,
            signing_out: false,
            issue: None,
            history: Arc::new(Vec::new()),
        }
    }
}

pub(crate) enum ClientCommand {
    SignIn,
    SignOut,
    Copy { id: Option<String>, original: bool },
    TogglePause,
    Reconnect,
    Suspend,
    Resume,
}

pub(crate) enum UiRequest {
    Clipboard {
        text: String,
        automatic: Option<Arc<DeliveryPolicy>>,
        cancelled: Arc<AtomicBool>,
        reply: oneshot::Sender<Result<CopyResult, ClipboardError>>,
    },
    Notice(Notice),
    CopyOutcome(Result<CopyResult, ClipboardError>),
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
struct UiClipboard {
    sender: mpsc::Sender<UiRequest>,
}

impl ClipboardWriter for UiClipboard {
    async fn write(
        &self,
        text: &str,
        automatic: Option<Arc<DeliveryPolicy>>,
    ) -> Result<CopyResult, ClipboardError> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel = CancelOnDrop(cancelled.clone());
        let (reply, result) = oneshot::channel();
        let request = UiRequest::Clipboard {
            text: text.into(),
            automatic,
            cancelled,
            reply,
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            self.sender
                .send(request)
                .await
                .map_err(|_| ClipboardError::Unavailable)?;
            result.await.map_err(|_| ClipboardError::Unavailable)?
        })
        .await
        .map_err(|_| ClipboardError::Unavailable)?
    }
}

struct UiNotifier(mpsc::Sender<UiRequest>);

impl Notifier for UiNotifier {
    fn notify(&self, notice: Notice) -> Result<(), NotificationError> {
        self.0
            .try_send(UiRequest::Notice(notice))
            .map_err(|_| NotificationError)
    }
}

type Coordinator = TranscriptCoordinator<
    Arc<ApiClient<EncryptedTokenStore>>,
    NativeHistoryStore,
    UiClipboard,
    UiNotifier,
>;

pub(crate) struct ClientHost {
    pub commands: mpsc::Sender<ClientCommand>,
    pub requests: mpsc::Receiver<UiRequest>,
    pub status: watch::Receiver<ClientStatus>,
    pub policy: Arc<DeliveryPolicy>,
    pub clipboard_enabled: bool,
    stop: watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ClientHost {
    pub fn start(options: ClientOptions) -> Result<Self, NativeError> {
        if !options.state_directory.is_absolute() || !options.state_directory.is_dir() {
            return Err(NativeError::InvalidArgument(
                "Connected mode requires an explicit existing absolute state directory.",
            ));
        }
        let clipboard_enabled = options.clipboard_enabled;
        let settings_store =
            NativeSettingsStore::new(options.state_directory.join("settings.json"))?;
        let settings = settings_store.load()?;
        let config = match options.oauth_config {
            Some(path) => {
                let file = std::fs::File::open(path)
                    .map_err(|error| NativeError::Io("open explicit OAuth configuration", error))?;
                let mut data = Vec::new();
                file.take((voiceprompt_client_protocol::MAX_AUTH_JSON_BYTES + 1) as u64)
                    .read_to_end(&mut data)
                    .map_err(|error| NativeError::Io("read explicit OAuth configuration", error))?;
                let parsed = DesktopClientConfig::parse(&data);
                data.fill(0);
                Some(parsed?)
            }
            None => None,
        };
        let store = EncryptedTokenStore::new(options.state_directory.join("tokens.bin"))?;
        let history = Arc::new(NativeHistoryStore::new(
            options.state_directory.join("history.json"),
        )?);
        let (commands, command_receiver) = mpsc::channel(8);
        let (requests, request_receiver) = mpsc::channel(8);
        let (status_sender, status) = watch::channel(ClientStatus::default());
        let (stop, shutdown) = watch::channel(false);
        let policy = Arc::new(DeliveryPolicy::default());
        policy.set_paused(settings.notifications_paused);
        let thread_policy = policy.clone();
        let (initialized, result) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("voiceprompt-client".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ =
                            initialized.send(Err(NativeError::Io("create client runtime", error)));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let initialize = async {
                        let auth = Arc::new(
                            AuthRuntime::initialize(
                                config,
                                GoogleTokenClient::new().map_err(|_| {
                                    NativeError::Verification(
                                        "Create Google token transport failed.",
                                    )
                                })?,
                                store,
                            )
                            .await
                            .map_err(|error| {
                                eprintln!("Client authentication initialization: {error}");
                                NativeError::Verification(
                                    "Authentication initialization failed; cache is preserved.",
                                )
                            })?,
                        );
                        let api = Arc::new(
                            ApiClient::new(settings.backend_url()?, auth.clone()).map_err(
                                |_| {
                                    NativeError::Verification(
                                        "Create authenticated backend transport failed.",
                                    )
                                },
                            )?,
                        );
                        let (realtime, events) =
                            RealtimeClient::new(api.clone(), env!("CARGO_PKG_VERSION").into());
                        let coordinator = Arc::new(TranscriptCoordinator::new(
                            api,
                            history,
                            UiClipboard {
                                sender: requests.clone(),
                            },
                            UiNotifier(requests.clone()),
                            thread_policy.clone(),
                        ));
                        let entries = coordinator.entries().await.map_err(|error| {
                            eprintln!("Client history initialization: {error}");
                            NativeError::Verification(
                                "History initialization failed; cache is preserved.",
                            )
                        })?;
                        status_sender.send_modify(|status| {
                            status.auth = *auth.status().borrow();
                            status.history = Arc::new(entries);
                        });
                        Ok::<_, NativeError>((auth, Arc::new(realtime), events, coordinator))
                    }
                    .await;
                    match initialize {
                        Ok((auth, realtime, events, coordinator)) => {
                            if initialized.send(Ok(())).is_err() {
                                return;
                            }
                            run_client(
                                auth,
                                realtime,
                                events,
                                coordinator,
                                settings_store,
                                settings,
                                command_receiver,
                                requests,
                                status_sender,
                                thread_policy,
                                shutdown,
                            )
                            .await;
                        }
                        Err(error) => {
                            let _ = initialized.send(Err(error));
                        }
                    }
                });
                runtime.shutdown_timeout(Duration::from_secs(2));
            })
            .map_err(|error| NativeError::Io("spawn client runtime", error))?;
        match result.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                requests: request_receiver,
                status,
                policy,
                clipboard_enabled,
                stop,
                thread: Some(thread),
            }),
            result => {
                stop.send_replace(true);
                if thread.join().is_err() {
                    eprintln!("Client initialization thread panicked.");
                }
                match result {
                    Ok(Err(error)) => Err(error),
                    _ => Err(NativeError::Verification(
                        "Client initialization thread stopped unexpectedly.",
                    )),
                }
            }
        }
    }
}

impl Drop for ClientHost {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        self.requests.close();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                eprintln!("Client shutdown thread panicked.");
            }
        }
    }
}

fn browser(uri: &url::Url) -> Result<(), ProtocolError> {
    let text: Vec<u16> = uri
        .as_str()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(text.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        return Err(ProtocolError::InvalidField("system browser launch"));
    }
    Ok(())
}

fn issue(status: &watch::Sender<ClientStatus>, message: &'static str) {
    eprintln!("Client: {message}");
    status.send_modify(|status| status.issue = Some(message));
}

async fn refresh_history(coordinator: &Coordinator, status: &watch::Sender<ClientStatus>) {
    match coordinator.entries().await {
        Ok(entries) => status.send_modify(|status| status.history = Arc::new(entries)),
        Err(error) => {
            eprintln!("Client history refresh: {error}");
            issue(status, "History could not be read. The cache is preserved.");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_client(
    auth: Arc<AuthRuntime<EncryptedTokenStore>>,
    realtime: Arc<RealtimeClient<EncryptedTokenStore>>,
    mut events: mpsc::Receiver<voiceprompt_client_protocol::realtime::TranscriptCompleted>,
    coordinator: Arc<Coordinator>,
    settings_store: NativeSettingsStore,
    mut settings: AppSettings,
    mut commands: mpsc::Receiver<ClientCommand>,
    requests: mpsc::Sender<UiRequest>,
    status: watch::Sender<ClientStatus>,
    policy: Arc<DeliveryPolicy>,
    mut stop: watch::Receiver<bool>,
) {
    let mut auth_state = auth.status();
    let mut realtime_state = realtime.state();
    let mut tasks = tokio::task::JoinSet::new();
    policy.set_active(*auth_state.borrow() == AuthState::SignedIn);
    let mut suspended = false;
    let mut sign_out_requested = false;
    let connection = realtime.clone();
    tasks.spawn(async move {
        if let Err(error) = connection.run().await {
            eprintln!("Client realtime loop: {error}");
        }
    });
    let processor = coordinator.clone();
    let processor_status = status.clone();
    let mut processor_auth = auth.status();
    tasks.spawn(async move {
        loop {
            let event = tokio::select! {
                event = events.recv() => match event { Some(event) => event, None => break },
                _ = processor_auth.changed() => {
                    while events.try_recv().is_ok() {}
                    continue;
                }
            };
            if *processor_auth.borrow_and_update() != AuthState::SignedIn { continue; }
            tokio::select! {
                result = processor.handle(&event) => {
                    if let Err(error) = result {
                        eprintln!("Client delivery: {error}");
                        issue(&processor_status, "Transcript delivery needs attention. Use History to retry copying.");
                    }
                }
                _ = processor_auth.changed() => {
                    while events.try_recv().is_ok() {}
                }
            }
            refresh_history(&processor, &processor_status).await;
        }
    });
    let mut cleanup = tokio::time::interval(Duration::from_secs(60));
    loop {
        tokio::select! {
            _ = stop.changed() => if *stop.borrow_and_update() { break; },
            _ = auth_state.changed() => {
                let current = *auth_state.borrow_and_update();
                policy.set_active(!suspended && !sign_out_requested && current == AuthState::SignedIn);
                status.send_modify(|status| status.auth = current);
            }
            _ = realtime_state.changed() => {
                let current = *realtime_state.borrow_and_update();
                status.send_modify(|status| status.realtime = current);
            }
            _ = cleanup.tick() => {
                if let Err(error) = coordinator.cleanup().await {
                    eprintln!("History retention cleanup: {error}");
                    issue(&status, "History retention cleanup failed; the cache is preserved.");
                }
                refresh_history(&coordinator, &status).await;
            }
            Some(result) = tasks.join_next() => {
                if let Err(error) = result {
                    eprintln!("Client task failed: {error}");
                    issue(&status, "A client task failed. Reopen the isolated client.");
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { break };
                match command {
                    ClientCommand::SignIn => {
                        if status.borrow().signing_in || status.borrow().signing_out { continue; }
                        sign_out_requested = false;
                        status.send_modify(|status| { status.signing_in = true; status.issue = None; });
                        let auth = auth.clone();
                        let status = status.clone();
                        let realtime = realtime.clone();
                        tasks.spawn(async move {
                            if let Err(error) = auth.sign_in(browser).await {
                                eprintln!("Client sign-in: {error}");
                                issue(&status, "Sign-in failed or was cancelled. No credentials were replaced.");
                            }
                            status.send_modify(|status| status.signing_in = false);
                            realtime.resume();
                        });
                    }
                    ClientCommand::SignOut => {
                        if status.borrow().signing_out { continue; }
                        sign_out_requested = true;
                        status.send_modify(|status| status.signing_out = true);
                        policy.set_active(false);
                        realtime.suspend();
                        auth.cancel_sign_in().await;
                        let auth = auth.clone();
                        let status = status.clone();
                        tasks.spawn(async move {
                            if let Err(error) = auth.sign_out().await {
                                eprintln!("Client sign-out: {error}");
                                issue(&status, "Sign-out cache removal failed; in-memory tokens are disabled.");
                            }
                            status.send_modify(|status| status.signing_out = false);
                        });
                    }
                    ClientCommand::Copy { id, original } => {
                        if tasks.len() >= 8 {
                            issue(&status, "A copy is already pending. Please wait.");
                            continue;
                        }
                        let coordinator = coordinator.clone();
                        let requests = requests.clone();
                        let status = status.clone();
                        tasks.spawn(async move {
                            let result = match coordinator.copy_cached(id.as_deref(), original).await {
                                Ok(result) => result,
                                Err(error) => {
                                    eprintln!("Manual history copy: {error}");
                                    issue(&status, "The cached transcript could not be copied.");
                                    if requests.send(UiRequest::CopyOutcome(Err(ClipboardError::Unavailable))).await.is_err() {
                                        eprintln!("Manual copy failure notification could not be queued.");
                                    }
                                    return;
                                }
                            };
                            if requests.send(UiRequest::CopyOutcome(Ok(result))).await.is_err() {
                                eprintln!("Manual copy notification could not be queued.");
                            }
                        });
                    }
                    ClientCommand::TogglePause => {
                        let mut next = settings.clone();
                        next.notifications_paused = !next.notifications_paused;
                        let store = settings_store.clone();
                        let to_save = next.clone();
                        match tokio::task::spawn_blocking(move || store.save(&to_save)).await {
                            Ok(Ok(())) => {
                                settings = next;
                                policy.set_paused(settings.notifications_paused);
                                status.send_modify(|status| status.issue = None);
                            }
                            result => {
                                eprintln!("Pause setting persistence failed: {result:?}");
                                issue(&status, "Pause setting could not be saved; the previous setting is retained.");
                            }
                        }
                    }
                    ClientCommand::Reconnect => realtime.reconnect(),
                    ClientCommand::Suspend => {
                        suspended = true;
                        policy.set_active(false);
                        realtime.suspend();
                    }
                    ClientCommand::Resume => {
                        suspended = false;
                        policy.set_active(!sign_out_requested && *auth_state.borrow() == AuthState::SignedIn);
                        realtime.resume();
                    }
                }
            }
        }
    }
    realtime.stop();
    policy.set_active(false);
    auth.cancel_sign_in().await;
    tasks.abort_all();
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            if !error.is_cancelled() {
                eprintln!("Client shutdown task failed: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_clipboard_requests_cannot_later_touch_the_clipboard() {
        let (sender, mut receiver) = mpsc::channel(1);
        let writer = UiClipboard { sender };
        let task = tokio::spawn(async move { writer.write("synthetic fixture", None).await });
        let request = receiver.recv().await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        match request {
            UiRequest::Clipboard {
                cancelled, reply, ..
            } => {
                assert!(cancelled.load(Ordering::Acquire));
                assert!(reply.is_closed());
            }
            _ => panic!("expected clipboard request"),
        }
    }

    #[test]
    fn connected_mode_never_discovers_an_implicit_state_or_oauth_path() {
        assert!(
            ClientHost::start(ClientOptions {
                state_directory: PathBuf::from("relative"),
                oauth_config: None,
                clipboard_enabled: false,
            })
            .is_err()
        );
    }
}
