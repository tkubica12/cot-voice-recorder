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

use crate::dictation::{DeliveryResult, DictationCommand, DictationHost, DictationView};
use crate::platform::{EncryptedTokenStore, NativeError, NativeHistoryStore, NativeSettingsStore};

#[derive(Clone)]
pub struct ClientOptions {
    pub state_directory: PathBuf,
    pub oauth_config: Option<PathBuf>,
    pub clipboard_enabled: bool,
    pub installed: bool,
}

#[derive(Clone)]
pub(crate) struct ClientStatus {
    pub auth: AuthState,
    pub realtime: RealtimeState,
    pub signing_in: bool,
    pub signing_out: bool,
    pub issue: Option<&'static str>,
    pub history: Arc<Vec<HistoryEntry>>,
    pub settings: Arc<AppSettings>,
    pub dictation: DictationView,
    pub recovery: Arc<Vec<crate::NativeRecoveryInfo>>,
    pub history_refreshing: bool,
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
            settings: Arc::new(AppSettings::default()),
            dictation: DictationView::default(),
            recovery: Arc::new(Vec::new()),
            history_refreshing: false,
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
    Dictation(DictationCommand),
    DeleteRecovery(String),
    SaveSettings(AppSettings),
    RefreshHistory,
    ClearHistory,
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
    Deliver {
        generation: u64,
        text: String,
        cancelled: Arc<AtomicBool>,
        reply: oneshot::Sender<DeliveryResult>,
    },
    AuditWarning,
    ReloadSettings,
    ValidateShortcuts {
        settings: Arc<AppSettings>,
        reply: oneshot::Sender<bool>,
        retain: bool,
    },
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
    pub options: ClientOptions,
    stop: watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ClientHost {
    pub fn start(options: ClientOptions) -> Result<Self, NativeError> {
        let retained_options = options.clone();
        let thread_options = options.clone();
        if !options.state_directory.is_absolute() || !options.state_directory.is_dir() {
            return Err(NativeError::InvalidArgument(
                "Connected mode requires an explicit existing absolute state directory.",
            ));
        }
        if !options.installed {
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                let installed = PathBuf::from(local).join("VoicePrompt");
                let selected = std::fs::canonicalize(&options.state_directory)
                    .map_err(|error| NativeError::Io("Resolve isolated state directory", error))?;
                if installed
                    .try_exists()
                    .map_err(|error| NativeError::Io("Inspect installed state boundary", error))?
                {
                    let installed = std::fs::canonicalize(&installed).map_err(|error| {
                        NativeError::Io("Resolve installed state boundary", error)
                    })?;
                    let selected = selected.to_string_lossy().to_lowercase();
                    let installed = installed.to_string_lossy().to_lowercase();
                    if selected == installed || selected.starts_with(&format!("{installed}\\")) {
                        return Err(NativeError::InvalidArgument(
                            "Isolated mode cannot use installed VoicePrompt state. Use a separate fixture directory.",
                        ));
                    }
                }
            }
        }
        let clipboard_enabled = options.clipboard_enabled;
        let settings_store =
            NativeSettingsStore::new(options.state_directory.join("settings.json"))?;
        let mut settings = settings_store.load()?;
        if options.installed {
            settings.auto_start = crate::autostart::enabled(true)?;
        }
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
        let recovery = if clipboard_enabled {
            Some(crate::dictation::open_recovery(
                options.state_directory.join("recovery"),
            )?)
        } else {
            None
        };
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
                            api.clone(),
                            history.clone(),
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
                            status.settings = Arc::new(settings.clone());
                        });
                        Ok::<_, NativeError>((auth, api, Arc::new(realtime), events, coordinator))
                    }
                    .await;
                    match initialize {
                        Ok((auth, api, realtime, events, coordinator)) => {
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
                                api,
                                history,
                                recovery,
                                clipboard_enabled,
                                thread_options,
                            )
                            .await;
                        }
                        Err(error) => {
                            let _ = initialized.send(Err(error));
                        }
                    }
                });
                // A timed shutdown can leave cache writers alive across Settings reload/sign-out.
                // Join blocking persistence before any replacement runtime opens the same state.
                drop(runtime);
            })
            .map_err(|error| NativeError::Io("spawn client runtime", error))?;
        match result.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                requests: request_receiver,
                status,
                policy,
                clipboard_enabled,
                options: retained_options,
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
    api: Arc<ApiClient<EncryptedTokenStore>>,
    history: Arc<NativeHistoryStore>,
    recovery: Option<crate::NativeRecoveryStore>,
    devices_enabled: bool,
    options: ClientOptions,
) {
    let mut auth_state = auth.status();
    let mut realtime_state = realtime.state();
    let mut tasks = tokio::task::JoinSet::new();
    let mut audit_task = recovery.clone().map(|root| {
        let outbox = crate::audit_outbox::AuditOutbox::new(root, history.clone(), Arc::new(crate::audit_outbox::BoundAudit {
            api: api.clone(), backend: settings.backend_base_url.clone(),
        }));
        let policy = policy.clone();
        let requests = requests.clone();
        let mut shutdown = stop.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            let mut warning_at = None;
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => break,
                    _ = interval.tick() => {
                        let report = tokio::select! {
                            _ = shutdown.changed() => break,
                            result = outbox.drain(&policy, time::OffsetDateTime::now_utc()) => result,
                        };
                        let failed = match report {
                            Ok(report) => report.failed > 0 || report.expired > 0,
                            Err(error) => { eprintln!("Audit outbox: {error}"); true }
                        };
                        if failed && warning_at.is_none_or(|at: tokio::time::Instant| at.elapsed() >= Duration::from_secs(300)) {
                            warning_at = Some(tokio::time::Instant::now());
                            if requests.try_send(UiRequest::AuditWarning).is_err() { eprintln!("Audit warning could not be queued."); }
                        }
                    }
                }
            }
        })
    });
    let (dictation_sender, dictation_commands) = mpsc::channel(8);
    let (dictation_settings, setting_updates) = watch::channel(settings.clone());
    let recovery_store = recovery.clone();
    let mut dictation_task = recovery.map(|recovery| {
        tokio::spawn(
            DictationHost {
                api: api.clone(),
                recovery,
                history: history.clone(),
                settings: setting_updates,
                status: status.clone(),
                ui: requests.clone(),
                policy: policy.clone(),
                commands: dictation_commands,
                shutdown: stop.clone(),
                devices_enabled,
            }
            .run(),
        )
    });
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
                if let Some(recovery) = recovery_store.clone() {
                    match tokio::task::spawn_blocking(move || {
                        recovery.cleanup(time::OffsetDateTime::now_utc())?;
                        recovery.list()
                    }).await {
                        Ok(Ok(entries)) => status.send_modify(|status| status.recovery = Arc::new(entries)),
                        result => { eprintln!("Recovery refresh failed: {result:?}"); issue(&status, "Recovery could not be read. Saved files are retained."); }
                    }
                }
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
                        if dictation_sender.try_send(DictationCommand::Interrupt).is_err() && devices_enabled {
                            issue(&status, "Dictation interruption could not be queued.");
                        }
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
                                dictation_settings.send_replace(settings.clone());
                                status.send_modify(|status| { status.issue = None; status.settings = Arc::new(settings.clone()); });
                            }
                            result => {
                                eprintln!("Pause setting persistence failed: {result:?}");
                                issue(&status, "Pause setting could not be saved; the previous setting is retained.");
                            }
                        }
                    }
                    ClientCommand::Reconnect => realtime.reconnect(),
                    ClientCommand::Suspend => {
                        if dictation_sender.try_send(DictationCommand::Interrupt).is_err() && devices_enabled {
                            issue(&status, "Dictation suspend could not be queued.");
                        }
                        suspended = true;
                        policy.set_active(false);
                        realtime.suspend();
                    }
                    ClientCommand::Resume => {
                        suspended = false;
                        policy.set_active(!sign_out_requested && *auth_state.borrow() == AuthState::SignedIn);
                        realtime.resume();
                    }
                    ClientCommand::Dictation(command) => {
                        if dictation_sender.try_send(command).is_err() {
                            issue(&status, "Dictation is busy or unavailable.");
                        }
                    }
                    ClientCommand::DeleteRecovery(id) => {
                        if let Some(recovery) = recovery_store.clone() {
                            match tokio::task::spawn_blocking(move || { recovery.delete_idle(&id)?; recovery.list() }).await {
                                Ok(Ok(entries)) => status.send_modify(|status| status.recovery = Arc::new(entries)),
                                result => { eprintln!("Recovery deletion failed: {result:?}"); issue(&status, "Recovery entry could not be deleted. No active journal is removed."); }
                            }
                        }
                    }
                            ClientCommand::SaveSettings(next) => {
                                if policy.dictation_busy() {
                                    issue(&status, "Finish dictation before applying Settings.");
                                    continue;
                                }
                                if let Err(error) = next.validate() {
                                    eprintln!("Settings validation failed: {error}");
                                    issue(&status, "Invalid Settings; previous configuration is retained.");
                                    continue;
                                }
                                let (reply, validation) = oneshot::channel();
                                if requests.send(UiRequest::ValidateShortcuts { settings: Arc::new(next.clone()), reply, retain: true }).await.is_err()
                                    || !matches!(tokio::time::timeout(Duration::from_secs(2), validation).await, Ok(Ok(true)))
                                {
                                    issue(&status, "A shortcut is already in use. Existing Settings and registrations are retained.");
                                    continue;
                                }
                                let store = settings_store.clone();
                                let previous = settings.clone();
                                let apply = next.clone();
                                let paths = options.clone();
                                match tokio::task::spawn_blocking(move || {
                                    let executable = std::env::current_exe().map_err(|error| NativeError::Io("Get autostart executable", error))?;
                                    let command = crate::autostart::command(&executable, &paths.state_directory, paths.oauth_config.as_deref(), paths.installed)?;
                                    if apply.auto_start != previous.auto_start {
                                        crate::autostart::set(apply.auto_start, &command, paths.installed)?;
                                    }
                                    if let Err(error) = store.save(&apply) {
                                        if apply.auto_start != previous.auto_start {
                                            if let Err(rollback) = crate::autostart::set(previous.auto_start, &command, paths.installed) {
                                                eprintln!("Autostart rollback failed: {rollback}");
                                            }
                                        }
                                        return Err(error);
                                    }
                                    Ok::<_, NativeError>(())
                                }).await {
                                    Ok(Ok(())) => {
                                        if requests.send(UiRequest::ReloadSettings).await.is_err() {
                                            issue(&status, "Settings saved; the native client could not reconnect. Reopen it.");
                                        }
                                    }
                                    result => {
                                        eprintln!("Settings persistence failed: {result:?}");
                                        issue(&status, "Settings could not be saved or autostart updated; check diagnostics.");
                                        let (reply, _) = oneshot::channel();
                                        if requests.try_send(UiRequest::ValidateShortcuts { settings: Arc::new(settings.clone()), reply, retain: false }).is_err() {
                                            eprintln!("Previous shortcut configuration could not be queued for restoration.");
                                        }
                                    }
                                }
                            }
                            ClientCommand::RefreshHistory => {
                                if status.borrow().history_refreshing || tasks.len() >= 8 { continue; }
                                status.send_modify(|status| status.history_refreshing = true);
                                let api = api.clone();
                                let history = history.clone();
                                let status = status.clone();
                                tasks.spawn(async move {
                                    let mut auth_state = api.auth_state();
                                    let work = async {
                                        let page = api.list_transcripts(None, 50).await.map_err(|error| { eprintln!("Cloud History list: {error}"); NativeError::Verification("Cloud History could not be listed.") })?;
                                        for summary in page.items {
                                            let cache = history.clone();
                                            let id = summary.transcript_id.clone();
                                            let exists = tokio::task::spawn_blocking(move || cache.all(time::OffsetDateTime::now_utc()).map(|entries| entries.iter().any(|entry| entry.transcript_id == id)))
                                                .await.map_err(|_| NativeError::Verification("History cache worker failed."))??;
                                            if exists { continue; }
                                            let full = api.transcript(&summary.transcript_id).await.map_err(|error| { eprintln!("Cloud History fetch: {error}"); NativeError::Verification("Cloud History transcript could not be fetched.") })?;
                                            let now = time::OffsetDateTime::now_utc();
                                            let entry = HistoryEntry {
                                                transcript_id: full.transcript_id, recording_id: full.recording_id, preview: full.preview,
                                                raw_body: full.raw_body.filter(|raw| raw != &full.body), body: full.body,
                                                character_count: full.character_count, completed_at: full.completed_at, cached_at: now,
                                                refinement_fallback_blocks: 0,
                                            };
                                            let cache = history.clone();
                                            tokio::task::spawn_blocking(move || cache.add(entry, now)).await
                                                .map_err(|_| NativeError::Verification("History save worker failed."))??;
                                        }
                                        Ok::<_, NativeError>(())
                                    };
                                    let refresh = tokio::select! {
                                        result = work => result,
                                        _ = auth_state.changed() => Err(NativeError::Verification("Cloud History refresh cancelled after authentication changed.")),
                                    };
                                    if let Err(error) = refresh { eprintln!("Cloud History refresh: {error}"); issue(&status, "Cloud History refresh failed. Check sign-in, network and disk."); }
                                    let cache = history.clone();
                                    match tokio::task::spawn_blocking(move || cache.all(time::OffsetDateTime::now_utc())).await {
                                        Ok(Ok(entries)) => status.send_modify(|status| status.history = Arc::new(entries)),
                                        result => { eprintln!("History refresh cache read failed: {result:?}"); }
                                    }
                                    status.send_modify(|status| status.history_refreshing = false);
                                });
                            }
                            ClientCommand::ClearHistory => {
                                if status.borrow().history_refreshing { issue(&status, "Wait for cloud History refresh before clearing the cache."); continue; }
                                let history = history.clone();
                                match tokio::task::spawn_blocking(move || history.clear()).await {
                                    Ok(Ok(())) => status.send_modify(|status| status.history = Arc::new(Vec::new())),
                                    result => { eprintln!("History clear failed: {result:?}"); issue(&status, "Local History could not be cleared."); }
                                }
                            }
                }
            }
        }
    }
    realtime.stop();
    policy.set_active(false);
    auth.cancel_sign_in().await;
    if let Some(task) = audit_task.take() {
        if let Err(error) = task.await {
            eprintln!("Audit shutdown task failed: {error}");
        }
    }
    if let Some(task) = dictation_task.take() {
        if let Err(error) = task.await {
            eprintln!("Dictation host shutdown failed: {error}");
        }
    }
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
                installed: false,
            })
            .is_err()
        );
    }
}
