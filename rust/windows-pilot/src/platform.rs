use std::cell::{Cell, RefCell};
use std::error::Error;
use std::ffi::{OsStr, c_void};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::client::{ClientCommand, ClientHost, ClientOptions, UiRequest};
use voiceprompt_client_protocol::delivery::{ClipboardError, CopyResult, Notice};
use voiceprompt_client_protocol::{MAX_AUTH_JSON_BYTES, ProtocolError, auth::CachedTokens};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HLOCAL, HWND, LPARAM,
    LRESULT, LocalFree, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    COLOR_WINDOW, DEFAULT_GUI_FONT, GetStockObject, GetSysColorBrush,
};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, MoveFileW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::{
    SS_CENTER, SS_ENDELLIPSIS, SS_ICON, SS_REALSIZECONTROL,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Controls as controls;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_C,
    VK_CONTROL, VK_F5, VK_LWIN, VK_MENU, VK_RETURN, VK_RWIN, VK_SHIFT, VK_SPACE,
};
use windows::Win32::UI::Shell as shell;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETFOCUS, NIM_SETVERSION, NIN_BALLOONUSERCLICK, NIN_SELECT,
    NINF_KEY, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const ENTROPY: &[u8] = b"VoicePrompt.DPAPI.v1";
const MAX_PLAINTEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CIPHERTEXT_BYTES: usize = MAX_PLAINTEXT_BYTES + 64 * 1024;
const TRAY_MESSAGE: u32 = WM_APP + 1;
const TRAY_KEY_SELECT: u32 = NIN_SELECT | NINF_KEY;
const ACTIVATE_MESSAGE: u32 = WM_APP + 2;
const QUIT_MESSAGE: u32 = WM_APP + 3;
const FIXTURE_READY_MESSAGE: u32 = WM_APP + 4;
#[cfg(test)]
const UI_PREVIEW_MESSAGE: u32 = WM_APP + 5;
const EXIT_TIMER: usize = 1;
const MENU_OPEN: usize = 1001;
const MENU_EXIT: usize = 1002;
const STATUS_CONTROL: usize = 1003;
const CLIENT_TIMER: usize = 2;
const SIGN_IN: usize = 1010;
const SIGN_OUT: usize = 1011;
const COPY_LATEST: usize = 1012;
const PAUSE: usize = 1013;
const RECONNECT: usize = 1014;
const HISTORY_LIST: usize = 1015;
const HISTORY_BODY: usize = 1016;
const COPY_SELECTED: usize = 1017;
const COPY_ORIGINAL: usize = 1018;
const RECOVERY_LIST: usize = 1040;
const RECOVER_SELECTED: usize = 1041;
const DELETE_RECOVERY: usize = 1042;
const OPEN_SETTINGS: usize = 1044;
const REFRESH_HISTORY: usize = 1045;
const CLEAR_HISTORY: usize = 1046;
const CLIENT_ACTIONS: [usize; 13] = [
    SIGN_IN,
    SIGN_OUT,
    COPY_LATEST,
    PAUSE,
    RECONNECT,
    COPY_SELECTED,
    COPY_ORIGINAL,
    RECOVER_SELECTED,
    REFRESH_RECOVERY,
    DELETE_RECOVERY,
    OPEN_SETTINGS,
    REFRESH_HISTORY,
    CLEAR_HISTORY,
];
const MAIN_TABS: usize = 1050;
const SHOW_HISTORY_TEXT: usize = 1051;
const RECOVERY_HELP: usize = 1052;
const REFRESH_RECOVERY: usize = 1053;
const HEADER_LOGO: usize = 1060;
const HEADER_TITLE: usize = 1061;
const CONNECTION_LABEL: usize = 1062;
const ACCOUNT_LABEL: usize = 1063;
const CACHE_LABEL: usize = 1064;
const CONNECTION_VALUE: usize = 1065;
const ACCOUNT_VALUE: usize = 1066;
const CACHE_VALUE: usize = 1067;
const HISTORY_EMPTY: usize = 1068;
const RECOVERY_STATUS: usize = 1069;
const SECTION_PAINT_SUBCLASS: usize = 0x5650;
const INSTANCE_TIMER: usize = 3;

thread_local! {
    static CALLBACK_ERROR: RefCell<Option<NativeError>> = const { RefCell::new(None) };
    static FIXTURE_READY:RefCell<Option<(PathBuf,Vec<u8>)>>=const{RefCell::new(None)};
    static CLIENT: RefCell<Option<ClientHost>> = const { RefCell::new(None) };
    static DICTATION_UI: RefCell<Option<crate::dictation_ui::DictationUi>> = const { RefCell::new(None) };
    static RELOAD_CLIENT: RefCell<Option<ClientOptions>> = const { RefCell::new(None) };
    static CLIENT_TICK: RefCell<u32> = const { RefCell::new(100) };
    static COMPATIBLE_INSTANCE: RefCell<Option<crate::CompatibleInstance>> = const { RefCell::new(None) };
    static MAIN_FONT: RefCell<Option<crate::main_theme::MainTheme>> = const { RefCell::new(None) };
    static MAIN_LAYOUT_READY: Cell<bool> = const { Cell::new(false) };
    static MAIN_SECTION_STATE: Cell<Option<(isize, bool)>> = const { Cell::new(None) };
    static MAIN_HOVER: Cell<Option<(HWND, i32, i32)>> = const { Cell::new(None) };
    static MAIN_VIEW_CHANGED: Cell<bool> = const { Cell::new(false) };
    static RECOVERY_COMPACT_FOOTER: Cell<bool> = const { Cell::new(false) };
    #[cfg(test)]
    static UI_PREVIEW_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    #[cfg(test)]
    static UI_PREVIEW_MINIMUM: Cell<bool> = const { Cell::new(false) };
    #[cfg(test)]
    static UI_PREVIEW_KEYBOARD: Cell<bool> = const { Cell::new(false) };
    #[cfg(test)]
    static UI_PREVIEW_AUTOMATED: Cell<bool> = const { Cell::new(false) };
    static UI_HISTORY: RefCell<Option<std::sync::Arc<Vec<voiceprompt_client_protocol::history::HistoryEntry>>>> =
        const { RefCell::new(None) };
    static UI_RECOVERY: RefCell<Option<std::sync::Arc<Vec<crate::NativeRecoveryInfo>>>> =
        const { RefCell::new(None) };
}

pub enum NativeError {
    InvalidArgument(&'static str),
    AlreadyRunning,
    Api(&'static str, windows::core::Error),
    Io(&'static str, io::Error),
    Verification(&'static str),
    Protocol(ProtocolError),
    RecoveryPath {
        relative_path: String,
        source: Box<NativeError>,
    },
}

impl fmt::Debug for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument(message) | Self::Verification(message) => f.write_str(message),
            Self::AlreadyRunning => {
                f.write_str("This isolated Rust pilot instance is already running.")
            }
            Self::Api(operation, source) => write!(
                f,
                "{operation} returned failure: HRESULT=0x{:08X}",
                source.code().0 as u32
            ),
            Self::Io(operation, source) => write!(
                f,
                "{operation} failed: kind={:?}, os_code={:?}",
                source.kind(),
                source.raw_os_error()
            ),
            Self::Protocol(error) => error.fmt(f),
            Self::RecoveryPath {
                relative_path,
                source,
            } => write!(f, "Recovery path={relative_path}: {source}"),
        }
    }
}

impl Error for NativeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Api(_, source) => Some(source),
            Self::Io(_, source) => Some(source),
            Self::Protocol(source) => Some(source),
            Self::RecoveryPath { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<ProtocolError> for NativeError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

fn api<T>(operation: &'static str, result: windows::core::Result<T>) -> Result<T, NativeError> {
    result.map_err(|source| NativeError::Api(operation, source))
}

fn wide(value: &OsStr) -> Result<Vec<u16>, NativeError> {
    let mut result: Vec<u16> = value.encode_wide().collect();
    if result.contains(&0) {
        return Err(NativeError::InvalidArgument(
            "A Windows string contains a null character.",
        ));
    }
    result.push(0);
    Ok(result)
}

pub fn validate_instance_key(key: &str) -> Result<(), NativeError> {
    if key.is_empty()
        || key.len() > 64
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(NativeError::InvalidArgument(
            "Instance key must be 1-64 ASCII letters, digits, hyphens or underscores.",
        ));
    }
    Ok(())
}

struct LocalBuffer {
    blob: CRYPT_INTEGER_BLOB,
    plaintext: bool,
}

impl Drop for LocalBuffer {
    fn drop(&mut self) {
        if !self.blob.pbData.is_null() {
            // DPAPI returns LocalAlloc memory. Clear its plaintext before releasing our one owner.
            unsafe {
                if self.plaintext {
                    self.blob.pbData.write_bytes(0, self.blob.cbData as usize);
                }
                let remaining = LocalFree(Some(HLOCAL(self.blob.pbData.cast())));
                if !remaining.0.is_null() {
                    eprintln!("Native pilot cleanup: LocalFree returned failure.");
                }
            }
        }
    }
}

fn transform(data: &[u8], protect: bool) -> Result<Vec<u8>, NativeError> {
    let limit = if protect {
        MAX_PLAINTEXT_BYTES
    } else {
        MAX_CIPHERTEXT_BYTES
    };
    if data.len() > limit {
        return Err(NativeError::InvalidArgument(
            "DPAPI input exceeds supported bounds.",
        ));
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: ENTROPY.len() as u32,
        pbData: ENTROPY.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // The input/entropy pointers are read-only borrows for this call. Never select LOCAL_MACHINE.
    unsafe {
        if protect {
            api(
                "CryptProtectData",
                CryptProtectData(
                    &input,
                    PCWSTR::null(),
                    Some(&entropy),
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                ),
            )?;
        } else {
            api(
                "CryptUnprotectData",
                CryptUnprotectData(
                    &input,
                    None,
                    Some(&entropy),
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                ),
            )?;
        }
    }
    let owned = LocalBuffer {
        blob: output,
        plaintext: !protect,
    };
    let output_limit = if protect {
        MAX_CIPHERTEXT_BYTES
    } else {
        MAX_PLAINTEXT_BYTES
    };
    if owned.blob.cbData as usize > output_limit
        || (owned.blob.cbData > 0 && owned.blob.pbData.is_null())
    {
        return Err(NativeError::Verification(
            "DPAPI returned an invalid output buffer.",
        ));
    }
    if owned.blob.cbData == 0 {
        return Ok(Vec::new());
    }
    // A successful DPAPI call guarantees this buffer's length and lifetime until LocalFree.
    Ok(unsafe {
        std::slice::from_raw_parts(owned.blob.pbData, owned.blob.cbData as usize).to_vec()
    })
}

pub fn protect(data: &[u8]) -> Result<Vec<u8>, NativeError> {
    #[cfg(test)]
    let mut phase = crate::mutation_trace::span("dpapi-protect");
    #[cfg(test)]
    if let Some(phase) = &mut phase {
        phase.counts(0, 0, data.len() as u64);
    }
    transform(data, true)
}

pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, NativeError> {
    #[cfg(test)]
    let mut phase = crate::mutation_trace::span("dpapi-unprotect");
    #[cfg(test)]
    if let Some(phase) = &mut phase {
        phase.counts(0, 0, data.len() as u64);
    }
    transform(data, false)
}

pub struct EncryptedTokenStore {
    path: PathBuf,
}

pub struct NativeHistoryStore {
    path: PathBuf,
    gate: std::sync::Mutex<()>,
}

pub struct NativeRecoveryManifestStore {
    path: PathBuf,
    id: String,
}

pub(crate) fn guard_recovery_path(path: &Path) -> Result<(), NativeError> {
    use std::os::windows::fs::MetadataExt;
    #[cfg(test)]
    let mut phase = crate::mutation_trace::span("path-reparse-preflight");
    #[cfg(test)]
    let mut probes = 0;
    for path in path.ancestors() {
        #[cfg(test)]
        {
            probes += 1;
            if let Some(phase) = &mut phase {
                phase.counts(0, probes, 0);
            }
        }
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_attributes() & 0x400 != 0 => {
                return Err(NativeError::Verification(
                    "Recovery reparse points are not supported.",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(NativeError::Io("inspect recovery path", error)),
        }
    }
    Ok(())
}

impl NativeRecoveryManifestStore {
    pub fn new(path: PathBuf) -> Result<Self, NativeError> {
        let id = path
            .parent()
            .and_then(Path::file_name)
            .and_then(OsStr::to_str)
            .filter(|id| voiceprompt_client_protocol::recovery::valid_id(id))
            .ok_or(NativeError::InvalidArgument(
                "Recovery requires an explicit lowercase session ID directory.",
            ))?;
        if !path.is_absolute() || path.file_name() != Some(OsStr::new("manifest.enc")) {
            return Err(NativeError::InvalidArgument(
                "Recovery requires an explicit absolute manifest.enc path.",
            ));
        }
        let id = id.to_owned();
        Ok(Self { path, id })
    }

    fn guard(&self) -> Result<(), NativeError> {
        guard_recovery_path(&self.path)
    }

    pub fn load(
        &self,
    ) -> Result<voiceprompt_client_protocol::recovery::RecoveryManifest, NativeError> {
        self.guard()?;
        let file = std::fs::File::open(&self.path)
            .map_err(|error| NativeError::Io("open recovery manifest", error))?;
        let limit = voiceprompt_client_protocol::recovery::MAX_MANIFEST_BYTES;
        let mut cipher = Vec::new();
        file.take((limit + 64 * 1024 + 1) as u64)
            .read_to_end(&mut cipher)
            .map_err(|error| NativeError::Io("read recovery manifest", error))?;
        if cipher.is_empty() || cipher.len() > limit + 64 * 1024 {
            return Err(ProtocolError::TooLarge.into());
        }
        let mut plain = unprotect(&cipher)?;
        let parsed =
            voiceprompt_client_protocol::recovery::RecoveryManifest::parse(&plain, &self.id);
        plain.fill(0);
        parsed.map_err(NativeError::Protocol)
    }

    /// Codec/parity fixture hook, not a quota-aware concurrent recovery journal.
    pub fn save_fixture(&self, plain: &[u8]) -> Result<(), NativeError> {
        self.guard()?;
        let manifest =
            voiceprompt_client_protocol::recovery::RecoveryManifest::parse(plain, &self.id)?;
        let mut latest = manifest.encode()?;
        let cipher = protect(&latest);
        latest.fill(0);
        let cipher = cipher?;
        self.guard()?;
        atomic_write(&self.path, &cipher, WriteMode::Replace)
    }
}

#[derive(Clone)]
pub struct NativeSettingsStore {
    path: PathBuf,
}

impl NativeSettingsStore {
    pub fn new(path: PathBuf) -> Result<Self, NativeError> {
        if path.file_name() != Some(OsStr::new("settings.json")) {
            return Err(NativeError::InvalidArgument(
                "Settings require an explicit settings.json path.",
            ));
        }
        Ok(Self { path })
    }

    pub fn load(&self) -> Result<voiceprompt_client_protocol::settings::AppSettings, NativeError> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(voiceprompt_client_protocol::settings::AppSettings::default());
            }
            Err(error) => return Err(NativeError::Io("open settings", error)),
        };
        let mut data = Vec::new();
        file.take((MAX_AUTH_JSON_BYTES + 1) as u64)
            .read_to_end(&mut data)
            .map_err(|error| NativeError::Io("read settings", error))?;
        voiceprompt_client_protocol::settings::AppSettings::parse(&data)
            .map_err(NativeError::Protocol)
    }

    pub fn save(
        &self,
        settings: &voiceprompt_client_protocol::settings::AppSettings,
    ) -> Result<(), NativeError> {
        atomic_write(&self.path, &settings.encode()?, WriteMode::Replace)
    }
}

impl NativeHistoryStore {
    pub fn remove(&self, id: &str) -> Result<(), NativeError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| NativeError::Verification("History cache lock is poisoned."))?;
        let mut entries = self.read()?;
        let before = entries.len();
        entries.retain(|entry| entry.transcript_id != id);
        if before != entries.len() {
            atomic_write(
                &self.path,
                &voiceprompt_client_protocol::history::encode_history(&entries)?,
                WriteMode::Replace,
            )?;
        }
        Ok(())
    }

    pub fn clear(&self) -> Result<(), NativeError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| NativeError::Verification("History cache lock is poisoned."))?;
        atomic_write(&self.path, b"[]", WriteMode::Replace)
    }

    pub fn new(path: PathBuf) -> Result<Self, NativeError> {
        if path.file_name() != Some(OsStr::new("history.json")) {
            return Err(NativeError::InvalidArgument(
                "History must use an explicit history.json path.",
            ));
        }
        Ok(Self {
            path,
            gate: std::sync::Mutex::new(()),
        })
    }

    fn read(&self) -> Result<Vec<voiceprompt_client_protocol::history::HistoryEntry>, NativeError> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(NativeError::Io("open history cache", error)),
        };
        let mut data = Vec::new();
        file.take((voiceprompt_client_protocol::history::MAX_HISTORY_BYTES + 1) as u64)
            .read_to_end(&mut data)
            .map_err(|error| NativeError::Io("read history cache", error))?;
        voiceprompt_client_protocol::history::parse_history(&data).map_err(NativeError::Protocol)
    }

    pub fn all(
        &self,
        now: time::OffsetDateTime,
    ) -> Result<Vec<voiceprompt_client_protocol::history::HistoryEntry>, NativeError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| NativeError::Verification("History cache lock is poisoned."))?;
        Ok(voiceprompt_client_protocol::history::live_entries(
            self.read()?,
            now,
        ))
    }

    pub fn add(
        &self,
        entry: voiceprompt_client_protocol::history::HistoryEntry,
        now: time::OffsetDateTime,
    ) -> Result<(), NativeError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| NativeError::Verification("History cache lock is poisoned."))?;
        let entries = voiceprompt_client_protocol::history::add_entry(self.read()?, entry, now);
        atomic_write(
            &self.path,
            &voiceprompt_client_protocol::history::encode_history(&entries)?,
            WriteMode::Replace,
        )
    }

    pub fn cleanup(&self, now: time::OffsetDateTime) -> Result<usize, NativeError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| NativeError::Verification("History cache lock is poisoned."))?;
        let before = self.read()?;
        let count = before.len();
        let live = voiceprompt_client_protocol::history::live_entries(before, now);
        let removed = count - live.len();
        if removed > 0 {
            atomic_write(
                &self.path,
                &voiceprompt_client_protocol::history::encode_history(&live)?,
                WriteMode::Replace,
            )?;
        }
        Ok(removed)
    }
}

impl voiceprompt_client_protocol::runtime::TokenPersistence for EncryptedTokenStore {
    type Error = NativeError;
    fn load(&self) -> Result<Option<CachedTokens>, Self::Error> {
        self.load()
    }
    fn save(&self, tokens: &CachedTokens) -> Result<(), Self::Error> {
        self.save(tokens)
    }
    fn clear(&self) -> Result<(), Self::Error> {
        self.clear()
    }
}

impl voiceprompt_client_protocol::delivery::HistoryPersistence for NativeHistoryStore {
    type Error = NativeError;

    fn all(
        &self,
        now: time::OffsetDateTime,
    ) -> Result<Vec<voiceprompt_client_protocol::history::HistoryEntry>, Self::Error> {
        self.all(now)
    }

    fn add(
        &self,
        entry: voiceprompt_client_protocol::history::HistoryEntry,
        now: time::OffsetDateTime,
    ) -> Result<(), Self::Error> {
        self.add(entry, now)
    }

    fn cleanup(&self, now: time::OffsetDateTime) -> Result<usize, Self::Error> {
        self.cleanup(now)
    }
}

impl EncryptedTokenStore {
    pub fn new(path: PathBuf) -> Result<Self, NativeError> {
        if path.file_name() != Some(OsStr::new("tokens.bin")) {
            return Err(NativeError::InvalidArgument(
                "Token store must use an explicit tokens.bin path.",
            ));
        }
        Ok(Self { path })
    }

    pub fn load(&self) -> Result<Option<CachedTokens>, NativeError> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(NativeError::Io("open encrypted token cache", error)),
        };
        let mut cipher = Vec::new();
        file.take((MAX_AUTH_JSON_BYTES + 64 * 1024 + 1) as u64)
            .read_to_end(&mut cipher)
            .map_err(|source| NativeError::Io("read encrypted token cache", source))?;
        if cipher.len() > MAX_AUTH_JSON_BYTES + 64 * 1024 {
            return Err(NativeError::Protocol(ProtocolError::TooLarge));
        }
        let mut plain = unprotect(&cipher)?;
        let parsed = CachedTokens::parse(&plain)
            .map(Some)
            .map_err(NativeError::Protocol);
        plain.fill(0);
        parsed
    }

    pub fn save(&self, tokens: &CachedTokens) -> Result<(), NativeError> {
        let mut plain = tokens.encode()?;
        let encrypted = protect(&plain);
        plain.fill(0);
        let cipher = encrypted?;
        atomic_write(&self.path, &cipher, WriteMode::Replace)
    }

    pub fn clear(&self) -> Result<(), NativeError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(NativeError::Io("delete encrypted token cache", error)),
        }
    }
}

pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    pub fn acquire(key: &str) -> Result<Self, NativeError> {
        validate_instance_key(key)?;
        let name = wide(OsStr::new(&format!(
            "Local\\VoicePrompt.RustPilot.{key}.mutex"
        )))?;
        // Existence, not thread-owned WaitOne, decides primacy; this namespace never touches C#.
        let handle = api("CreateMutexW", unsafe {
            CreateMutexW(None, false, PCWSTR(name.as_ptr()))
        })?;
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let guard = Self { handle };
        if existed {
            Err(NativeError::AlreadyRunning)
        } else {
            Ok(guard)
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        if let Err(error) = unsafe { CloseHandle(self.handle) } {
            eprintln!("Native pilot cleanup: CloseHandle returned failure: {error}");
        }
    }
}

fn class_name(key: &str) -> Result<Vec<u16>, NativeError> {
    validate_instance_key(key)?;
    wide(OsStr::new(&format!("VoicePrompt.RustPilot.Window.{key}")))
}

pub fn request_quit(key: &str) -> Result<(), NativeError> {
    let name = class_name(key)?;
    // Find only our isolated class, not any HWND or mutex belonging to the installed app.
    let hwnd = api("FindWindowW (Rust pilot)", unsafe {
        FindWindowW(PCWSTR(name.as_ptr()), PCWSTR::null())
    })?;
    api("PostMessageW (Rust pilot quit)", unsafe {
        PostMessageW(Some(hwnd), QUIT_MESSAGE, WPARAM(0), LPARAM(0))
    })
}

fn request_activation(key: &str) -> Result<(), NativeError> {
    let name = class_name(key)?;
    let hwnd = api("FindWindowW (Rust pilot)", unsafe {
        FindWindowW(PCWSTR(name.as_ptr()), PCWSTR::null())
    })?;
    api("PostMessageW (Rust pilot activation)", unsafe {
        PostMessageW(Some(hwnd), ACTIVATE_MESSAGE, WPARAM(0), LPARAM(0))
    })
}

struct RegisteredClass {
    name: Vec<u16>,
    instance: HINSTANCE,
}

impl Drop for RegisteredClass {
    fn drop(&mut self) {
        if let Err(error) =
            unsafe { UnregisterClassW(PCWSTR(self.name.as_ptr()), Some(self.instance)) }
        {
            eprintln!("Native pilot cleanup: UnregisterClassW returned failure: {error}");
        }
    }
}

struct OwnedWindow(HWND);

struct MainFontLifetime;
impl Drop for MainFontLifetime {
    fn drop(&mut self) {
        MAIN_LAYOUT_READY.with(|ready| ready.set(false));
        MAIN_SECTION_STATE.with(|state| state.set(None));
        MAIN_HOVER.with(|state| state.set(None));
        MAIN_VIEW_CHANGED.with(|state| state.set(false));
        RECOVERY_COMPACT_FOOTER.with(|state| state.set(false));
        let font = MAIN_FONT.with(|slot| slot.borrow_mut().take());
        drop(font);
    }
}

impl Drop for OwnedWindow {
    fn drop(&mut self) {
        if unsafe { IsWindow(Some(self.0)) }.as_bool() {
            if let Err(error) = unsafe { DestroyWindow(self.0) } {
                eprintln!("Native pilot cleanup: DestroyWindow returned failure: {error}");
            }
        }
    }
}

struct OwnedMenu(HMENU);

impl Drop for OwnedMenu {
    fn drop(&mut self) {
        if let Err(error) = unsafe { DestroyMenu(self.0) } {
            eprintln!("Native pilot cleanup: DestroyMenu returned failure: {error}");
        }
    }
}

struct Tray {
    data: NOTIFYICONDATAW,
    added: bool,
}

impl Tray {
    fn add(hwnd: HWND, icon: HICON) -> Result<Self, NativeError> {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY_MESSAGE,
            hIcon: icon,
            ..Default::default()
        };
        let tip: Vec<u16> = "VoicePrompt".encode_utf16().collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        api(
            "Shell_NotifyIconW (add)",
            unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.ok(),
        )?;
        let mut tray = Self { data, added: true };
        tray.data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        api(
            "Shell_NotifyIconW (version)",
            unsafe { Shell_NotifyIconW(NIM_SETVERSION, &tray.data) }.ok(),
        )?;
        Ok(tray)
    }

    fn remove(&mut self) -> Result<(), NativeError> {
        if self.added {
            api(
                "Shell_NotifyIconW (delete)",
                unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data) }.ok(),
            )?;
            self.added = false;
        }
        Ok(())
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Err(error) = self.remove() {
            eprintln!("Native pilot cleanup: {error}");
        }
    }
}

fn show_status(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        if !SetForegroundWindow(hwnd).as_bool() {
            eprintln!(
                "Native pilot: status window is visible, but Windows refused foreground focus."
            );
        }
    }
}

thread_local! {
    static TRAY_STATUS_TEXT: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn tray_status_text(status: &crate::client::ClientStatus) -> String {
    let state = if status.auth == voiceprompt_client_protocol::state::AuthState::NotConfigured {
        "Not configured".into()
    } else {
        format!("{:?}", status.realtime)
    };
    format!("VoicePrompt \u{2014} {state}")
}

fn update_tray_status(hwnd: HWND, status: &crate::client::ClientStatus) -> Result<(), NativeError> {
    let text = tray_status_text(status);
    if TRAY_STATUS_TEXT.with(|slot| slot.borrow().as_ref() == Some(&text)) {
        return Ok(());
    }
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_TIP | NIF_SHOWTIP,
        ..Default::default()
    };
    let tip: Vec<_> = text.encode_utf16().take(63).collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    api(
        "Update live VoicePrompt tray status",
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.ok(),
    )?;
    TRAY_STATUS_TEXT.with(|slot| *slot.borrow_mut() = Some(text));
    Ok(())
}

fn context_menu_items(
    connected: bool,
    paused: bool,
    can_copy: bool,
) -> Result<OwnedMenu, NativeError> {
    let menu = OwnedMenu(api("CreatePopupMenu", unsafe { CreatePopupMenu() })?);
    unsafe {
        api(
            "AppendMenuW",
            AppendMenuW(menu.0, MF_STRING, MENU_OPEN, w!("&Open")),
        )?;
        api(
            "Set default Open tray action",
            SetMenuDefaultItem(menu.0, MENU_OPEN as u32, 0),
        )?;
        if connected {
            api(
                "AppendMenuW",
                AppendMenuW(
                    menu.0,
                    MF_STRING | if can_copy { MF_ENABLED } else { MF_GRAYED },
                    COPY_LATEST,
                    w!("&Copy latest"),
                ),
            )?;
            api(
                "AppendMenuW",
                AppendMenuW(
                    menu.0,
                    MF_STRING | if paused { MF_CHECKED } else { MF_UNCHECKED },
                    PAUSE,
                    w!("&Pause notifications"),
                ),
            )?;
        }
        api(
            "AppendMenuW",
            AppendMenuW(menu.0, MF_SEPARATOR, 0, PCWSTR::null()),
        )?;
        api(
            "AppendMenuW",
            AppendMenuW(menu.0, MF_STRING, MENU_EXIT, w!("E&xit")),
        )?;
    }
    Ok(menu)
}

fn tray_activation(event: u32) -> bool {
    matches!(
        event,
        WM_LBUTTONDBLCLK | TRAY_KEY_SELECT | NIN_BALLOONUSERCLICK
    )
}

fn history_key_action(
    history_focused: bool,
    key: usize,
    modifiers: HOT_KEY_MODIFIERS,
) -> Option<usize> {
    let unmodified = modifiers == HOT_KEY_MODIFIERS::default();
    if key == VK_F5.0 as usize && unmodified {
        return Some(REFRESH_HISTORY);
    }
    if history_focused
        && ((key == VK_RETURN.0 as usize || key == VK_SPACE.0 as usize)
            || (key == VK_C.0 as usize && modifiers == MOD_CONTROL))
    {
        return Some(COPY_SELECTED);
    }
    None
}

fn route_history_keys(hwnd: HWND, message: &MSG) -> Result<bool, NativeError> {
    if message.message != WM_KEYDOWN
        || (message.hwnd != hwnd && !unsafe { IsChild(hwnd, message.hwnd) }.as_bool())
    {
        return Ok(false);
    }
    let mut modifiers = HOT_KEY_MODIFIERS::default();
    for (key, modifier) in [
        (VK_CONTROL, MOD_CONTROL),
        (VK_SHIFT, MOD_SHIFT),
        (VK_MENU, MOD_ALT),
        (VK_LWIN, MOD_WIN),
        (VK_RWIN, MOD_WIN),
    ] {
        if unsafe { GetKeyState(key.0 as i32) } < 0 {
            modifiers |= modifier;
        }
    }
    let history_focused = unsafe { GetDlgCtrlID(message.hwnd) } == HISTORY_LIST as i32;
    let Some(command) = history_key_action(history_focused, message.wParam.0, modifiers) else {
        return Ok(false);
    };
    let button = api("Read keyboard action availability", unsafe {
        GetDlgItem(Some(hwnd), command as i32)
    })?;
    if !unsafe { windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(button) }.as_bool() {
        return Ok(true);
    }
    client_command(hwnd, command)?;
    Ok(true)
}

fn context_menu(hwnd: HWND) -> Result<(), NativeError> {
    let (connected, paused, can_copy) = CLIENT.with(|client| {
        client
            .borrow()
            .as_ref()
            .map_or((false, false, false), |host| {
                (
                    true,
                    host.policy.paused(),
                    host.clipboard_enabled && !host.status.borrow().history.is_empty(),
                )
            })
    });
    let menu = context_menu_items(connected, paused, can_copy)?;
    let mut position = POINT::default();
    unsafe {
        api("GetCursorPos", GetCursorPos(&mut position))?;
        if !SetForegroundWindow(hwnd).as_bool() {
            return Err(NativeError::Verification(
                "Windows refused the pilot context menu's foreground owner.",
            ));
        }
        let command = TrackPopupMenu(
            menu.0,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            position.x,
            position.y,
            None,
            hwnd,
            None,
        )
        .0 as usize;
        match command {
            MENU_OPEN => show_status(hwnd),
            MENU_EXIT => PostQuitMessage(0),
            COPY_LATEST | PAUSE | RECONNECT => client_command(hwnd, command)?,
            0 => {
                let data = NOTIFYICONDATAW {
                    cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: hwnd,
                    uID: 1,
                    ..Default::default()
                };
                api(
                    "Shell_NotifyIconW (focus)",
                    Shell_NotifyIconW(NIM_SETFOCUS, &data).ok(),
                )?;
            }
            _ => {
                return Err(NativeError::Verification(
                    "The pilot menu returned an unknown command.",
                ));
            }
        }
        api(
            "PostMessageW",
            PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0)),
        )?;
    }
    Ok(())
}

fn selected_id(hwnd: HWND) -> Result<Option<String>, NativeError> {
    let list = api("GetDlgItem (history)", unsafe {
        GetDlgItem(Some(hwnd), HISTORY_LIST as i32)
    })?;
    let index = unsafe { SendMessageW(list, LB_GETCURSEL, None, None) }.0;
    Ok(UI_HISTORY.with(|history| {
        history.borrow().as_ref().and_then(|entries| {
            usize::try_from(index)
                .ok()
                .and_then(|index| entries.get(index))
                .map(|entry| entry.transcript_id.clone())
        })
    }))
}

fn fixture_ui_allows(options: &ClientOptions, command: usize) -> bool {
    !options.fixture_runtime || command == OPEN_SETTINGS
}

fn client_command(hwnd: HWND, command: usize) -> Result<(), NativeError> {
    if CLIENT.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|host| !fixture_ui_allows(&host.options, command))
    }) {
        return Err(NativeError::Verification(
            "Read-only fixture rejected an action before dialogs, shortcuts or client commands.",
        ));
    }
    if CLIENT.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|host| host.options.auth_only && !matches!(command, SIGN_IN | SIGN_OUT))
    }) {
        eprintln!("Auth-only fixture rejected a non-authentication UI command.");
        return Ok(());
    }
    if command == OPEN_SETTINGS {
        return CLIENT.with(|slot| {
            if let Some(host) = slot.borrow().as_ref() {
                crate::settings_ui::open(
                    hwnd,
                    host.commands.clone(),
                    host.status.borrow().settings.clone(),
                    !host.options.fixture_runtime && !host.options.auth_only,
                )?;
            }
            Ok(())
        });
    }
    let command = match command {
        SIGN_IN => ClientCommand::SignIn,
        SIGN_OUT => ClientCommand::SignOut,
        COPY_LATEST => ClientCommand::Copy {
            id: None,
            original: false,
        },
        COPY_SELECTED | COPY_ORIGINAL => {
            let Some(id) = selected_id(hwnd)? else {
                return Ok(());
            };
            ClientCommand::Copy {
                id: Some(id),
                original: command == COPY_ORIGINAL,
            }
        }
        PAUSE => ClientCommand::TogglePause,
        RECONNECT => ClientCommand::Reconnect,
        REFRESH_HISTORY => ClientCommand::RefreshHistory,
        REFRESH_RECOVERY => ClientCommand::RefreshRecovery,
        CLEAR_HISTORY => {
            if unsafe {
                MessageBoxW(
                    Some(hwnd),
                    w!(
                        "Delete all locally cached transcripts? Cloud transcripts still within retention can be fetched again with Refresh."
                    ),
                    w!("Clear History"),
                    MB_OKCANCEL | MB_ICONWARNING,
                )
            } != IDOK
            {
                return Ok(());
            }
            ClientCommand::ClearHistory
        }
        RECOVER_SELECTED | DELETE_RECOVERY => {
            let Some(id) = selected_recovery(hwnd)? else {
                return Ok(());
            };
            if command == RECOVER_SELECTED {
                DICTATION_UI.with(|slot| {
                    if let Some(ui) = slot.borrow_mut().as_mut() {
                        if let Err(error) = ui.recover(id.clone()) {
                            eprintln!("Recovery command refused: {error}");
                            notify(hwnd, "Recovery unavailable", "Finish dictation and select a readable saved entry before recovery.", true)?;
                        }
                    }
                    Ok::<_, NativeError>(())
                })?;
                let tick = DICTATION_UI.with(|slot| {
                    if slot.borrow().as_ref().is_some_and(|ui| ui.fast_poll()) {
                        20
                    } else {
                        100
                    }
                });
                CLIENT_TICK.with(|previous| {
                    if *previous.borrow() != tick {
                        if unsafe { SetTimer(Some(hwnd), CLIENT_TIMER, tick, None) } == 0 {
                            return Err(NativeError::Api(
                                "Adjust native client timer",
                                windows::core::Error::from_thread(),
                            ));
                        }
                        *previous.borrow_mut() = tick;
                    }
                    Ok::<_, NativeError>(())
                })?;
                return Ok(());
            }
            if unsafe {
                MessageBoxW(
                    Some(hwnd),
                    w!("Delete this saved dictation and its audio? This cannot be undone."),
                    w!("Delete Recovery entry"),
                    MB_YESNO | MB_ICONWARNING,
                )
            } != IDYES
            {
                return Ok(());
            }
            ClientCommand::DeleteRecovery(id)
        }
        _ => return Ok(()),
    };
    CLIENT.with(|client| {
        if let Some(host) = client.borrow().as_ref() {
            if host.commands.try_send(command).is_err() {
                eprintln!("Client command could not be queued.");
                notify(
                    hwnd,
                    "VoicePrompt",
                    "The client is busy or stopped. Try again.",
                    true,
                )?;
            }
        }
        Ok(())
    })
}

fn selected_recovery(hwnd: HWND) -> Result<Option<String>, NativeError> {
    let list = api("GetDlgItem (recovery)", unsafe {
        GetDlgItem(Some(hwnd), RECOVERY_LIST as i32)
    })?;
    let selected = unsafe { SendMessageW(list, LB_GETCURSEL, None, None) }.0;
    Ok(UI_RECOVERY.with(|slot| {
        slot.borrow().as_ref().and_then(|entries| {
            usize::try_from(selected)
                .ok()
                .and_then(|index| entries.get(index))
                .map(|entry| entry.id.clone())
        })
    }))
}

fn set_text(hwnd: HWND, id: usize, text: &str) -> Result<(), NativeError> {
    let child = api("GetDlgItem", unsafe { GetDlgItem(Some(hwnd), id as i32) })?;
    let text = wide(OsStr::new(text))?;
    api("SetWindowTextW", unsafe {
        SetWindowTextW(child, PCWSTR(text.as_ptr()))
    })
}

pub(crate) fn notify(
    hwnd: HWND,
    title: &str,
    body: &str,
    warning: bool,
) -> Result<(), NativeError> {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_INFO,
        dwInfoFlags: if warning { NIIF_WARNING } else { NIIF_INFO },
        ..Default::default()
    };
    data.Anonymous.uTimeout = 4000;
    let title = wide(OsStr::new(title))?;
    let body = wide(OsStr::new(body))?;
    if title.len() > data.szInfoTitle.len() || body.len() > data.szInfo.len() {
        return Err(NativeError::InvalidArgument(
            "Native notification text exceeds its bounds.",
        ));
    }
    data.szInfoTitle[..title.len()].copy_from_slice(&title);
    data.szInfo[..body.len()].copy_from_slice(&body);
    api(
        "Shell_NotifyIconW (notification)",
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.ok(),
    )
}

fn selected_history_entry<'a>(
    entries: &'a [voiceprompt_client_protocol::history::HistoryEntry],
    selected: Option<&str>,
) -> Option<&'a voiceprompt_client_protocol::history::HistoryEntry> {
    let selected = selected?;
    entries.iter().find(|entry| entry.transcript_id == selected)
}

fn show_history_body(hwnd: HWND) -> Result<(), NativeError> {
    let selected = selected_id(hwnd)?;
    UI_HISTORY.with(|history| {
        let history = history.borrow();
        let entry = history
            .as_ref()
            .and_then(|entries| selected_history_entry(entries, selected.as_deref()));
        set_text(
            hwnd,
            HISTORY_BODY,
            entry.map_or("", |entry| entry.body.as_str()),
        )?;
        let original = api("Read original-copy control", unsafe {
            GetDlgItem(Some(hwnd), COPY_ORIGINAL as i32)
        })?;
        let latest = api("Read manual-copy availability", unsafe {
            GetDlgItem(Some(hwnd), COPY_LATEST as i32)
        })?;
        unsafe {
            let _ = EnableWindow(
                original,
                windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(latest).as_bool()
                    && entry.is_some_and(|entry| entry.raw_body.is_some()),
            );
        }
        Ok(())
    })
}

fn refresh_client_ui(hwnd: HWND, force_presentation: bool) -> Result<(), NativeError> {
    CLIENT.with(|client| {
        let mut client = client.borrow_mut();
        let Some(host) = client.as_mut() else { return Ok(()) };
        for _ in 0..8 {
            let request = match host.requests.try_recv() {
                Ok(request) => request,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    return Err(NativeError::Verification("The client UI request channel stopped."));
                }
            };
            match request {
                UiRequest::Clipboard { text, automatic, cancelled, reply } => {
                    let result = if cancelled.load(Ordering::Acquire) || reply.is_closed() {
                        Err(ClipboardError::Cancelled)
                    } else if !host.clipboard_enabled {
                        Err(ClipboardError::Unavailable)
                    } else if automatic.as_ref().is_some_and(|policy| !policy.allows_auto_copy()) {
                        Ok(CopyResult::Suppressed)
                    } else {
                        crate::clipboard::write(hwnd, &text).map(|()| CopyResult::Copied)
                    };
                    let _ = reply.send(result);
                }
                UiRequest::Notice(notice) => {
                    let (title, body, warning) = match notice {
                        Notice::TranscriptCopied if host.policy.paused() => continue,
                        Notice::TranscriptCopied => ("Transcript copied", "Ready on the clipboard.", false),
                        Notice::FetchFailed => ("VoicePrompt", "Could not fetch the transcript.", true),
                        Notice::HistoryFailed => ("VoicePrompt", "The transcript could not be saved. Check the history directory and disk.", true),
                        Notice::ClipboardFailed => ("VoicePrompt", "Transcript ready, but the clipboard could not be written. Use Copy latest.", true),
                    };
                    if let Err(error) = notify(hwnd, title, body, warning) {
                        eprintln!("Client notification: {error}");
                        set_text(hwnd, STATUS_CONTROL, "A notification failed. Open History for the cached transcript.")?;
                    }
                }
                UiRequest::CopyOutcome(result) => {
                    let (body, warning) = match result {
                        Ok(CopyResult::Copied) => ("Cached transcript copied.", false),
                        Ok(CopyResult::Empty) => ("No matching cached transcript or original is available.", false),
                        Ok(CopyResult::Suppressed) => ("Copy was suppressed.", false),
                        Err(_) => ("The clipboard could not be written. Try again.", true),
                    };
                    if let Err(error) = notify(hwnd, "VoicePrompt", body, warning) {
                        eprintln!("Manual copy notification: {error}");
                    }
                }
                UiRequest::Deliver { generation, text, cancelled, reply } => {
                    DICTATION_UI.with(|slot| {
                        if let Some(ui) = slot.borrow_mut().as_mut() { ui.deliver(generation, text, cancelled, reply); }
                        else { let _ = reply.send(crate::dictation::DeliveryResult::CopiedOnly); }
                    });
                }
                UiRequest::ReloadSettings => {
                    RELOAD_CLIENT.with(|slot| *slot.borrow_mut() = Some(host.options.clone()));
                }
                UiRequest::ValidateShortcuts { settings, reply, retain } => {
                    let eligible = !retain || !reply.is_closed();
                    let outcome = eligible && DICTATION_UI.with(|slot| {
                        slot.borrow_mut().as_mut().is_some_and(|ui| match ui.validate_settings(&settings, host.status.borrow().auth, retain) {
                            Ok(()) => true,
                            Err(error) => { eprintln!("Settings shortcut preflight failed: {error}"); false }
                        })
                    });
                    if reply.send(outcome).is_err() && retain && outcome {
                        DICTATION_UI.with(|slot| {
                            if let Some(ui) = slot.borrow_mut().as_mut() {
                                let previous = host.status.borrow();
                                if let Err(error) = ui.validate_settings(&previous.settings, previous.auth, false) {
                                    eprintln!("Cancelled shortcut preflight rollback failed: {error}");
                                }
                            }
                        });
                    }
                }
            }
        }
        let current = host.status.borrow().clone();
        DICTATION_UI.with(|slot| {
            if let Some(ui) = slot.borrow_mut().as_mut() {
                if let Err(error) = ui.configure(&current.settings, current.auth) {
                    eprintln!("Dictation shortcut configuration: {error}");
                    set_text(hwnd, STATUS_CONTROL, "Dictation shortcut is unavailable. Choose another shortcut in Settings.")?;
                }
                ui.poll(&current)?;
            }
            Ok::<_, NativeError>(())
        })?;
        if MAIN_LAYOUT_READY.with(Cell::get)
            && MAIN_SECTION_STATE.with(Cell::get) != Some(section_state(hwnd)?)
        {
            relayout(hwnd)?;
        }
        refresh_hover(hwnd);
        let view_changed = MAIN_VIEW_CHANGED.with(|changed| changed.replace(false));
        let visible = force_presentation || unsafe { IsWindowVisible(hwnd) }.as_bool();
        if !host.status.has_changed().map_err(|_| {
            NativeError::Verification("The client status channel stopped.")
        })? && !view_changed && (!visible || UI_HISTORY.with(|history| history.borrow().is_some())) {
            return Ok(());
        }
        let status = host.status.borrow_and_update().clone();
        update_tray_status(hwnd, &status)?;
        if !visible {
            return Ok(());
        }
        let shortcut_error = DICTATION_UI.with(|slot| slot.borrow().as_ref().is_some_and(|ui| ui.shortcut_error()));
        let recovery_changed = UI_RECOVERY.with(|slot| slot.borrow().as_ref().is_none_or(|old| !std::sync::Arc::ptr_eq(old, &status.recovery)));
        if recovery_changed {
            let selected = selected_recovery(hwnd)?;
            let list = api("GetDlgItem (recovery)", unsafe { GetDlgItem(Some(hwnd), RECOVERY_LIST as i32) })?;
            unsafe { let _ = SendMessageW(list, LB_RESETCONTENT, None, None); }
            for entry in status.recovery.iter() {
                let label = crate::main_presentation::recovery_lines(entry, false)?.join(". ");
                let text = wide(OsStr::new(&label))?;
                if unsafe { SendMessageW(list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize))) }.0 < 0 {
                    return Err(NativeError::Verification("Recovery list could not accept an entry."));
                }
            }
            let index = selected.and_then(|id| status.recovery.iter().position(|entry| entry.id == id)).unwrap_or(0);
            UI_RECOVERY.with(|slot| *slot.borrow_mut() = Some(status.recovery.clone()));
            unsafe { let _ = SendMessageW(list, LB_SETCURSEL, Some(WPARAM(index)), None); }
        }
        set_text(hwnd, STATUS_CONTROL, if shortcut_error { "Dictation shortcut unavailable. Choose an unused shortcut in Settings." }
            else { status.issue.unwrap_or("Transcripts arrive from the cloud and land on your clipboard.") })?;
        set_text(hwnd, CONNECTION_VALUE, match status.realtime {
            voiceprompt_client_protocol::websocket::RealtimeState::Stopped => "Stopped",
            voiceprompt_client_protocol::websocket::RealtimeState::Suspended => "Suspended",
            voiceprompt_client_protocol::websocket::RealtimeState::Connecting => "Connecting\u{2026}",
            voiceprompt_client_protocol::websocket::RealtimeState::Connected => "Connected",
            voiceprompt_client_protocol::websocket::RealtimeState::Reconnecting => "Reconnecting\u{2026}",
            voiceprompt_client_protocol::websocket::RealtimeState::SignInRequired => "Sign-in required",
            voiceprompt_client_protocol::websocket::RealtimeState::NotConfigured => "Not configured",
        })?;
        set_text(hwnd, ACCOUNT_VALUE, status.account_label())?;
        set_text(hwnd, CACHE_VALUE, &status.history.len().to_string())?;
        set_text(hwnd, RECOVERY_STATUS, &crate::main_presentation::recovery_status(&status, RECOVERY_COMPACT_FOOTER.with(Cell::get)))?;
        set_text(hwnd, PAUSE, if host.policy.paused() { "Resume auto-copy" } else { "Pause auto-copy" })?;
        let selected = selected_id(hwnd)?;
        let original_available = selected_history_entry(&status.history, selected.as_deref()).is_some_and(|entry| entry.raw_body.is_some());
        for id in CLIENT_ACTIONS {
            let control = api("GetDlgItem", unsafe { GetDlgItem(Some(hwnd), id as i32) })?;
            let enabled = if !fixture_ui_allows(&host.options, id)
                || host.options.auth_only && !matches!(id, SIGN_IN | SIGN_OUT) {
                false
            } else { match id {
                SIGN_IN => status.can_sign_in(),
                SIGN_OUT => status.can_sign_out(),
                COPY_LATEST | COPY_SELECTED => host.clipboard_enabled && !status.history.is_empty(),
                COPY_ORIGINAL => host.clipboard_enabled && original_available,
                REFRESH_HISTORY => !status.history_refreshing && status.auth == voiceprompt_client_protocol::state::AuthState::SignedIn,
                CLEAR_HISTORY => !status.history_refreshing && !status.history.is_empty(),
                RECOVER_SELECTED | DELETE_RECOVERY => !status.recovery.is_empty() && !matches!(status.dictation.stage,
                    crate::dictation::DictationStage::Starting | crate::dictation::DictationStage::Recording | crate::dictation::DictationStage::Finishing),
                _ => true,
            }};
            unsafe { let _ = EnableWindow(control, enabled); }
        }
        let changed = UI_HISTORY.with(|history| {
            history.borrow().as_ref().is_none_or(|previous| !std::sync::Arc::ptr_eq(previous, &status.history))
        });
        if changed {
            let selected = selected_id(hwnd)?;
            let list = api("GetDlgItem (history)", unsafe { GetDlgItem(Some(hwnd), HISTORY_LIST as i32) })?;
            unsafe { let _ = SendMessageW(list, LB_RESETCONTENT, None, None); }
            for entry in status.history.iter() {
                let label = format!("{}. {}", if entry.preview.trim().is_empty() { "(no preview)" } else { &entry.preview },
                    crate::main_presentation::history_metadata(entry, time::OffsetDateTime::now_utc())?);
                let text = wide(OsStr::new(&label))?;
                let result = unsafe { SendMessageW(list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize))) };
                if result.0 < 0 {
                    return Err(NativeError::Verification("History list could not accept an entry."));
                }
            }
            let selected = selected.and_then(|id| status.history.iter().position(|entry| entry.transcript_id == id)).unwrap_or(0);
            UI_HISTORY.with(|history| *history.borrow_mut() = Some(status.history));
            unsafe { let _ = SendMessageW(list, LB_SETCURSEL, Some(WPARAM(selected)), None); }
            show_history_body(hwnd)?;
        }
        let (page, showing_text) = section_state(hwnd)?;
        let history_empty = UI_HISTORY.with(|history|
            history.borrow().as_ref().is_none_or(|entries| entries.is_empty()));
        let empty = page == 0 && !showing_text && history_empty;
        let hint = api("Read empty History hint", unsafe {
            GetDlgItem(Some(hwnd), HISTORY_EMPTY as i32)
        })?;
        unsafe { let _ = ShowWindow(hint, if empty { SW_SHOWNOACTIVATE } else { SW_HIDE }); }
        let list = api("Read populated History list", unsafe {
            GetDlgItem(Some(hwnd), HISTORY_LIST as i32)
        })?;
        unsafe { let _ = ShowWindow(list, if page == 0 && !showing_text && !history_empty { SW_SHOWNOACTIVATE } else { SW_HIDE }); }
        Ok(())
    })
}

fn layout(hwnd: HWND, width: i32, height: i32) -> Result<(), NativeError> {
    if !MAIN_LAYOUT_READY.with(Cell::get) || width == 0 || height == 0 {
        return Ok(());
    }
    let positions =
        crate::main_layout::MainLayout::calculate(width, height, crate::dpi::window_dpi(hwnd))
            .map_err(NativeError::Verification)?;
    let (page, showing_text) = section_state(hwnd)?;
    let mut items = vec![
        (HEADER_LOGO, positions.logo),
        (HEADER_TITLE, positions.title),
        (STATUS_CONTROL, positions.status),
        (MAIN_TABS, positions.tabs),
        (SHOW_HISTORY_TEXT, positions.text_toggle),
        (HISTORY_LIST, positions.history),
        (HISTORY_BODY, positions.history),
        (HISTORY_EMPTY, positions.empty_history),
        (RECOVERY_HELP, positions.recovery_help),
        (RECOVERY_LIST, positions.recovery),
        (RECOVERY_STATUS, positions.recovery_status),
    ];
    items.extend(
        [CONNECTION_LABEL, ACCOUNT_LABEL, CACHE_LABEL]
            .into_iter()
            .zip(positions.status_labels),
    );
    items.extend(
        [CONNECTION_VALUE, ACCOUNT_VALUE, CACHE_VALUE]
            .into_iter()
            .zip(positions.status_values),
    );
    items.extend(
        [SIGN_IN, SIGN_OUT, PAUSE, RECONNECT, OPEN_SETTINGS]
            .into_iter()
            .zip(positions.toolbar),
    );
    items.extend(
        [
            COPY_SELECTED,
            COPY_LATEST,
            COPY_ORIGINAL,
            REFRESH_HISTORY,
            CLEAR_HISTORY,
        ]
        .into_iter()
        .zip(positions.history_actions),
    );
    items.extend(
        [RECOVER_SELECTED, REFRESH_RECOVERY, DELETE_RECOVERY]
            .into_iter()
            .zip(positions.recovery_actions),
    );
    for (id, bounds) in items {
        let child = api("GetDlgItem", unsafe { GetDlgItem(Some(hwnd), id as i32) })?;
        let inset = if matches!(id, HISTORY_LIST | HISTORY_BODY | RECOVERY_LIST) {
            (crate::dpi::window_dpi(hwnd) as i32 + 48) / 96
        } else {
            0
        };
        api("Position adaptive native control", unsafe {
            MoveWindow(
                child,
                bounds.x + inset,
                bounds.y + inset,
                bounds.width - inset * 2,
                bounds.height - inset * 2,
                true,
            )
        })?;
        let visible = match id {
            HISTORY_LIST => {
                page == 0
                    && !showing_text
                    && UI_HISTORY.with(|history| {
                        history
                            .borrow()
                            .as_ref()
                            .is_some_and(|entries| !entries.is_empty())
                    })
            }
            HISTORY_BODY => page == 0 && showing_text,
            HISTORY_EMPTY => {
                page == 0
                    && !showing_text
                    && UI_HISTORY.with(|history| {
                        history
                            .borrow()
                            .as_ref()
                            .is_none_or(|entries| entries.is_empty())
                    })
            }
            SHOW_HISTORY_TEXT | COPY_SELECTED | COPY_LATEST | COPY_ORIGINAL | REFRESH_HISTORY
            | CLEAR_HISTORY => page == 0,
            RECOVERY_HELP | RECOVERY_LIST | RECOVER_SELECTED | REFRESH_RECOVERY
            | RECOVERY_STATUS | DELETE_RECOVERY => page == 1,
            _ => true,
        };
        unsafe {
            let _ = ShowWindow(child, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }
    let recovery = api("Read responsive recovery row height", unsafe {
        GetDlgItem(Some(hwnd), RECOVERY_LIST as i32)
    })?;
    let dpi = crate::dpi::window_dpi(hwnd) as i32;
    let full = (crate::main_layout::RECOVERY_ROW_HEIGHT_DIP * dpi + 48) / 96;
    let inset = (dpi + 48) / 96;
    let row_height = if positions.recovery.height - inset * 2 >= full {
        full
    } else {
        (46 * dpi + 48) / 96
    };
    let previous_height =
        unsafe { SendMessageW(recovery, LB_GETITEMHEIGHT, Some(WPARAM(0)), None) }.0;
    if previous_height != row_height as isize
        && unsafe {
            SendMessageW(
                recovery,
                LB_SETITEMHEIGHT,
                Some(WPARAM(0)),
                Some(LPARAM(row_height as isize)),
            )
        }
        .0 == -1
    {
        return Err(NativeError::Verification(
            "Could not size responsive Recovery rows.",
        ));
    }
    if previous_height != row_height as isize {
        let selected = unsafe { SendMessageW(recovery, LB_GETCURSEL, None, None) }.0;
        if selected >= 0 {
            unsafe {
                let _ = SendMessageW(
                    recovery,
                    LB_SETCURSEL,
                    Some(WPARAM(selected as usize)),
                    None,
                );
            }
        }
    }
    RECOVERY_COMPACT_FOOTER
        .with(|compact| compact.set(positions.recovery_status.width < positions.recovery.width));
    MAIN_VIEW_CHANGED.with(|changed| changed.set(true));
    MAIN_SECTION_STATE.with(|state| state.set(Some((page, showing_text))));
    if !unsafe { windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, true) }.as_bool() {
        return Err(NativeError::Api(
            "Invalidate previous native section geometry",
            windows::core::Error::from_thread(),
        ));
    }
    Ok(())
}

fn section_state(hwnd: HWND) -> Result<(isize, bool), NativeError> {
    let tabs = api("Read native sections", unsafe {
        GetDlgItem(Some(hwnd), MAIN_TABS as i32)
    })?;
    let page = unsafe { SendMessageW(tabs, controls::TCM_GETCURSEL, None, None) }.0;
    if !matches!(page, 0 | 1) {
        return Err(NativeError::Verification(
            "No native History/Recovery section is selected.",
        ));
    }
    let text_view = api("Read selected-text view toggle", unsafe {
        GetDlgItem(Some(hwnd), SHOW_HISTORY_TEXT as i32)
    })?;
    let showing_text = unsafe { SendMessageW(text_view, BM_GETCHECK, None, None) }.0
        == controls::BST_CHECKED.0 as isize;
    Ok((page, showing_text))
}

fn refresh_hover(hwnd: HWND) {
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return;
    }
    let mut point = POINT::default();
    if let Err(error) = unsafe { GetCursorPos(&mut point) } {
        eprintln!("Native hover updates suspended: {error}");
        return;
    }
    let target = unsafe { WindowFromPoint(point) };
    let child = if unsafe { IsChild(hwnd, target) }.as_bool() {
        target
    } else {
        HWND::default()
    };
    let current = (child, point.x, point.y);
    MAIN_HOVER.with(|state| {
        if state.get() == Some(current) {
            return;
        }
        if let Some((previous, _, _)) = state.get() {
            if !previous.0.is_null() {
                unsafe {
                    let _ =
                        windows::Win32::Graphics::Gdi::InvalidateRect(Some(previous), None, false);
                }
            }
        }
        if !child.0.is_null() {
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(child), None, false);
            }
        }
        state.set(Some(current));
    });
}

fn relayout(hwnd: HWND) -> Result<(), NativeError> {
    let mut bounds = RECT::default();
    api("Read native client viewport", unsafe {
        GetClientRect(hwnd, &mut bounds)
    })?;
    layout(hwnd, bounds.right - bounds.left, bounds.bottom - bounds.top)
}

fn create_sections(hwnd: HWND, instance: HINSTANCE) -> Result<(), NativeError> {
    for (id, class, caption, style) in [
        (
            SHOW_HISTORY_TEXT,
            w!("BUTTON"),
            w!("Show selected text"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
        ),
        (
            RECOVERY_HELP,
            w!("STATIC"),
            w!(
                "Interrupted dictations are encrypted for this Windows user and retained for 48 hours (256 MiB limit). Recover finishes into History; it never pastes automatically. Use the original account and backend."
            ),
            WS_CHILD,
        ),
        (
            HISTORY_EMPTY,
            w!("STATIC"),
            w!("No transcripts cached yet.\r\nCompleted transcripts appear here for 48 hours."),
            WS_CHILD | WINDOW_STYLE(SS_CENTER.0),
        ),
        (
            RECOVERY_STATUS,
            w!("STATIC"),
            w!("No unfinished dictations."),
            WS_CHILD,
        ),
        (
            MAIN_TABS,
            controls::WC_TABCONTROLW,
            w!("Sections"),
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WINDOW_STYLE(
                    controls::TCS_OWNERDRAWFIXED
                        | controls::TCS_FIXEDWIDTH
                        | controls::TCS_BUTTONS
                        | controls::TCS_FLATBUTTONS,
                ),
        ),
    ] {
        api("Create native History/Recovery sections", unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                caption,
                style,
                0,
                0,
                1,
                1,
                Some(hwnd),
                Some(HMENU(id as *mut c_void)),
                Some(instance),
                None,
            )
        })?;
    }
    let tabs = api("Read section selector", unsafe {
        GetDlgItem(Some(hwnd), MAIN_TABS as i32)
    })?;
    for (index, label) in ["History", "Recovery"].into_iter().enumerate() {
        let mut text: Vec<_> = label.encode_utf16().chain([0]).collect();
        let item = controls::TCITEMW {
            mask: controls::TCIF_TEXT,
            pszText: windows::core::PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        if unsafe {
            SendMessageW(
                tabs,
                controls::TCM_INSERTITEMW,
                Some(WPARAM(index)),
                Some(LPARAM((&item as *const controls::TCITEMW) as isize)),
            )
        }
        .0 == -1
        {
            return Err(NativeError::Verification(
                "Could not insert a native section.",
            ));
        }
    }
    if !unsafe { shell::SetWindowSubclass(tabs, Some(section_paint), SECTION_PAINT_SUBCLASS, 0) }
        .as_bool()
    {
        return Err(NativeError::Api(
            "Apply original native section presentation",
            windows::core::Error::from_thread(),
        ));
    }
    let mut previous = HWND_TOP;
    for id in [
        MAIN_TABS,
        SHOW_HISTORY_TEXT,
        HISTORY_EMPTY,
        HISTORY_LIST,
        HISTORY_BODY,
        COPY_SELECTED,
        COPY_LATEST,
        COPY_ORIGINAL,
        REFRESH_HISTORY,
        CLEAR_HISTORY,
        RECOVERY_LIST,
        RECOVER_SELECTED,
        REFRESH_RECOVERY,
        DELETE_RECOVERY,
        SIGN_IN,
        SIGN_OUT,
        PAUSE,
        RECONNECT,
        OPEN_SETTINGS,
    ] {
        let child = api("Read native keyboard-order control", unsafe {
            GetDlgItem(Some(hwnd), id as i32)
        })?;
        api("Apply native keyboard navigation order", unsafe {
            SetWindowPos(
                child,
                Some(previous),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        })?;
        previous = child;
    }
    Ok(())
}

unsafe extern "system" fn section_paint(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_NCDESTROY {
        unsafe {
            let _ = shell::RemoveWindowSubclass(window, Some(section_paint), subclass);
            return shell::DefSubclassProc(window, message, wparam, lparam);
        }
    }
    if message != WM_PAINT || !MAIN_LAYOUT_READY.with(Cell::get) {
        return unsafe { shell::DefSubclassProc(window, message, wparam, lparam) };
    }
    let result = MAIN_FONT.with(|slot| {
        let Ok(slot) = slot.try_borrow() else {
            return Ok(None);
        };
        let Some(theme) = slot.as_ref() else {
            return Ok(None);
        };
        let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
        let dc = unsafe { windows::Win32::Graphics::Gdi::BeginPaint(window, &mut paint) };
        let drawing = (|| {
            let mut bounds = RECT::default();
            api("Read original section strip", unsafe {
                GetClientRect(window, &mut bounds)
            })?;
            theme.canvas(dc, bounds)?;
            let selected = unsafe { SendMessageW(window, controls::TCM_GETCURSEL, None, None) }.0;
            for (index, text) in ["History", "Recovery"].into_iter().enumerate() {
                let mut rect = RECT::default();
                if unsafe {
                    SendMessageW(
                        window,
                        controls::TCM_GETITEMRECT,
                        Some(WPARAM(index)),
                        Some(LPARAM((&mut rect as *mut RECT) as isize)),
                    )
                }
                .0 == 0
                {
                    return Err(NativeError::Verification(
                        "Could not read original native section bounds.",
                    ));
                }
                let item = controls::DRAWITEMSTRUCT {
                    CtlType: controls::ODT_TAB,
                    CtlID: MAIN_TABS as u32,
                    itemID: index as u32,
                    itemState: if selected == index as isize {
                        controls::ODS_FLAGS(
                            controls::ODS_SELECTED.0
                                | if unsafe {
                                    windows::Win32::UI::Input::KeyboardAndMouse::GetFocus()
                                } == window
                                {
                                    controls::ODS_FOCUS.0
                                } else {
                                    0
                                },
                        )
                    } else {
                        controls::ODS_FLAGS(0)
                    },
                    hwndItem: window,
                    hDC: dc,
                    rcItem: rect,
                    ..Default::default()
                };
                theme.tab(&item, text)?;
            }
            Ok::<_, NativeError>(())
        })();
        if !unsafe { windows::Win32::Graphics::Gdi::EndPaint(window, &paint) }.as_bool() {
            eprintln!("Original section strip paint cleanup failed.");
        }
        drawing?;
        Ok::<_, NativeError>(Some(LRESULT(0)))
    });
    match result {
        Ok(Some(value)) => value,
        Ok(None) => unsafe { shell::DefSubclassProc(window, message, wparam, lparam) },
        Err(error) => {
            eprintln!("Original native section rendering failed: {error}");
            CALLBACK_ERROR.with(|slot| *slot.borrow_mut() = Some(error));
            unsafe { PostQuitMessage(1) };
            LRESULT(0)
        }
    }
}

fn update_main_font(hwnd: HWND) -> Result<(), NativeError> {
    let dpi = crate::dpi::window_dpi(hwnd);
    MAIN_FONT.with(|slot| {
        if slot.borrow().as_ref().is_none_or(|font| font.dpi != dpi) {
            let theme = crate::main_theme::MainTheme::new(dpi)?;
            theme.base.apply_children(hwnd)?;
            theme.title.apply(api("Read branding title", unsafe {
                GetDlgItem(Some(hwnd), HEADER_TITLE as i32)
            })?);
            for id in [STATUS_CONTROL, CONNECTION_LABEL, ACCOUNT_LABEL, CACHE_LABEL] {
                theme
                    .caption
                    .apply(api("Read original caption control", unsafe {
                        GetDlgItem(Some(hwnd), id as i32)
                    })?);
            }
            theme
                .section_caption
                .apply(api("Read recovery explanation", unsafe {
                    GetDlgItem(Some(hwnd), RECOVERY_HELP as i32)
                })?);
            for id in [HISTORY_EMPTY, RECOVERY_STATUS] {
                theme
                    .section_caption
                    .apply(api("Read section caption", unsafe {
                        GetDlgItem(Some(hwnd), id as i32)
                    })?);
            }
            theme.strong.apply(api("Read recovery list font", unsafe {
                GetDlgItem(Some(hwnd), RECOVERY_LIST as i32)
            })?);
            for id in [CONNECTION_VALUE, ACCOUNT_VALUE] {
                theme
                    .strong
                    .apply(api("Read original status value", unsafe {
                        GetDlgItem(Some(hwnd), id as i32)
                    })?);
            }
            let logo = api("Read original branding control", unsafe {
                GetDlgItem(Some(hwnd), HEADER_LOGO as i32)
            })?;
            let tabs = api("Read original section control", unsafe {
                GetDlgItem(Some(hwnd), MAIN_TABS as i32)
            })?;
            let history = api("Read original history control", unsafe {
                GetDlgItem(Some(hwnd), HISTORY_LIST as i32)
            })?;
            let scale = |value: i32| (value * dpi as i32 + 48) / 96;
            unsafe {
                let _ = SendMessageW(
                    logo,
                    STM_SETICON,
                    Some(WPARAM(crate::icons::AppIcons::branding(dpi)?.0 as usize)),
                    None,
                );
                let _ = SendMessageW(
                    tabs,
                    controls::TCM_SETITEMSIZE,
                    None,
                    Some(LPARAM(
                        (scale(81) | (scale(crate::main_layout::SECTION_ITEM_HEIGHT_DIP) << 16))
                            as isize,
                    )),
                );
                let _ = SendMessageW(
                    history,
                    LB_SETITEMHEIGHT,
                    Some(WPARAM(0)),
                    Some(LPARAM(scale(47) as isize)),
                );
            }
            *slot.borrow_mut() = Some(theme);
        }
        Ok(())
    })
}

fn dispatch(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Result<LRESULT, NativeError> {
    match message {
        #[cfg(test)]
        UI_PREVIEW_MESSAGE => {
            let root = UI_PREVIEW_ROOT
                .with(|slot| slot.borrow().clone())
                .ok_or(NativeError::Verification("No owned visual fixture root."))?;
            let count = CLIENT.with(|slot| {
                let slot = slot.borrow();
                let host = slot
                    .as_ref()
                    .ok_or(NativeError::Verification("No client for visual fixture."))?;
                if !host.options.fixture_runtime
                    || host.clipboard_enabled
                    || host.options.installed
                    || host.options.oauth_config.is_some()
                    || host.status.borrow().auth
                        != voiceprompt_client_protocol::state::AuthState::NotConfigured
                {
                    return Err(NativeError::Verification(
                        "Visual fixture enabled a forbidden capability.",
                    ));
                }
                Ok(host.status.borrow().history.len())
            })?;
            let foreground = unsafe { GetForegroundWindow() };
            if crate::main_presentation::history_format_calls() != 0 {
                return Err(NativeError::Verification(
                    "Hidden main-window startup formatted History rows.",
                ));
            }
            api("Name isolated native client preview", unsafe {
                SetWindowTextW(
                    hwnd,
                    if UI_PREVIEW_KEYBOARD.with(Cell::get) {
                        w!("Isolated Rust keyboard test - NoPorts")
                    } else {
                        w!("Isolated Rust client preview - no physical ports")
                    },
                )
            })?;
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            if unsafe { GetForegroundWindow() } != foreground {
                return Err(NativeError::Verification(
                    "Read-only preview activated the window.",
                ));
            }
            refresh_client_ui(hwnd, true)?;
            for id in CLIENT_ACTIONS {
                let control = api("Verify read-only preview control", unsafe {
                    GetDlgItem(Some(hwnd), id as i32)
                })?;
                let enabled = unsafe {
                    windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(control)
                }
                .as_bool();
                if enabled != (id == OPEN_SETTINGS) {
                    return Err(NativeError::Verification(
                        "Read-only preview exposed an action or disabled Settings navigation.",
                    ));
                }
            }
            let mut bounds = RECT::default();
            api("Read visual client viewport", unsafe {
                GetClientRect(hwnd, &mut bounds)
            })?;
            let data = serde_json::to_vec(&serde_json::json!({"scope":"native-full-client-ui-preview","pid":std::process::id(),
                "window":hwnd.0 as usize,"history_count":count,"physical_ports":false,"auth":"not_configured","dpi":crate::dpi::window_dpi(hwnd),
                "client_width":bounds.right-bounds.left,"client_height":bounds.bottom-bounds.top,"foreground_unchanged":true,
                "hidden_history_format_calls":0,"action_controls_disabled":true,
                "keyboard_only":UI_PREVIEW_KEYBOARD.with(Cell::get),
                "automated_keyboard":UI_PREVIEW_AUTOMATED.with(Cell::get)}))
                .map_err(|_|NativeError::Verification("Encode visual client readiness failed."))?;
            atomic_write(
                &root.join("ui-preview-ready.json"),
                &data,
                WriteMode::CreateNew,
            )?;
            Ok(LRESULT(0))
        }
        FIXTURE_READY_MESSAGE => {
            if let Some((path, data)) = FIXTURE_READY.with(|slot| slot.borrow_mut().take()) {
                atomic_write(&path, &data, WriteMode::CreateNew)?;
            }
            Ok(LRESULT(0))
        }
        WM_CREATE => {
            let instance = HINSTANCE(
                api("GetModuleHandleW", unsafe {
                    GetModuleHandleW(PCWSTR::null())
                })?
                .0,
            );
            let child = api("CreateWindowExW (status)", unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("Transcripts arrive from the cloud and land on your clipboard."),
                    WS_CHILD | WS_VISIBLE,
                    16,
                    16,
                    460,
                    150,
                    Some(hwnd),
                    Some(HMENU(STATUS_CONTROL as *mut c_void)),
                    Some(instance),
                    None,
                )
            })?;
            unsafe {
                let font = GetStockObject(DEFAULT_GUI_FONT);
                let _ = SendMessageW(
                    child,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            for (id, label) in [
                (SIGN_IN, w!("Sign in")),
                (SIGN_OUT, w!("Sign out")),
                (COPY_LATEST, w!("Copy latest")),
                (PAUSE, w!("Pause auto-copy")),
                (RECONNECT, w!("Reconnect")),
                (COPY_SELECTED, w!("Copy selected")),
                (COPY_ORIGINAL, w!("Copy original")),
                (RECOVER_SELECTED, w!("Recover to History")),
                (DELETE_RECOVERY, w!("Discard selected")),
                (REFRESH_RECOVERY, w!("Refresh")),
                (OPEN_SETTINGS, w!("Settings")),
                (REFRESH_HISTORY, w!("Refresh")),
                (CLEAR_HISTORY, w!("Clear history")),
            ] {
                let control = api("CreateWindowExW (button)", unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        w!("BUTTON"),
                        label,
                        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                        0,
                        0,
                        1,
                        1,
                        Some(hwnd),
                        Some(HMENU(id as *mut c_void)),
                        Some(instance),
                        None,
                    )
                })?;
                unsafe {
                    let _ = SendMessageW(
                        control,
                        WM_SETFONT,
                        Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),
                        Some(LPARAM(1)),
                    );
                    let _ = EnableWindow(control, false);
                }
            }
            for (id, class, style) in [
                (
                    HISTORY_LIST,
                    w!("LISTBOX"),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | WS_VSCROLL
                        | WINDOW_STYLE(
                            (LBS_NOTIFY
                                | LBS_OWNERDRAWFIXED
                                | LBS_HASSTRINGS
                                | LBS_NOINTEGRALHEIGHT) as u32,
                        ),
                ),
                (
                    HISTORY_BODY,
                    w!("EDIT"),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | WS_VSCROLL
                        | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
                ),
                (
                    RECOVERY_LIST,
                    w!("LISTBOX"),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | WS_VSCROLL
                        | WINDOW_STYLE(
                            (LBS_NOTIFY
                                | LBS_OWNERDRAWFIXED
                                | LBS_HASSTRINGS
                                | LBS_NOINTEGRALHEIGHT) as u32,
                        ),
                ),
            ] {
                let control = api("CreateWindowExW (history)", unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        class,
                        PCWSTR::null(),
                        style,
                        0,
                        0,
                        1,
                        1,
                        Some(hwnd),
                        Some(HMENU(id as *mut c_void)),
                        Some(instance),
                        None,
                    )
                })?;
                unsafe {
                    let _ = SendMessageW(
                        control,
                        WM_SETFONT,
                        Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }
            create_sections(hwnd, instance)?;
            for (id, caption, style) in [
                (
                    HEADER_LOGO,
                    w!("VoicePrompt logo"),
                    WINDOW_STYLE(SS_ICON.0 | SS_REALSIZECONTROL.0),
                ),
                (
                    HEADER_TITLE,
                    w!("VoicePrompt"),
                    WINDOW_STYLE(SS_ENDELLIPSIS.0),
                ),
                (
                    CONNECTION_LABEL,
                    w!("CONNECTION"),
                    WINDOW_STYLE(SS_ENDELLIPSIS.0),
                ),
                (ACCOUNT_LABEL, w!("ACCOUNT"), WINDOW_STYLE(SS_ENDELLIPSIS.0)),
                (
                    CACHE_LABEL,
                    w!("CACHED TRANSCRIPTS"),
                    WINDOW_STYLE(SS_ENDELLIPSIS.0),
                ),
                (
                    CONNECTION_VALUE,
                    w!("Stopped"),
                    WINDOW_STYLE(SS_ENDELLIPSIS.0),
                ),
                (
                    ACCOUNT_VALUE,
                    w!("Not configured"),
                    WINDOW_STYLE(SS_ENDELLIPSIS.0),
                ),
                (CACHE_VALUE, w!("0"), WINDOW_STYLE(SS_ENDELLIPSIS.0)),
            ] {
                api("Create original branding/status control", unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        w!("STATIC"),
                        caption,
                        WS_CHILD | WS_VISIBLE | style,
                        0,
                        0,
                        1,
                        1,
                        Some(hwnd),
                        Some(HMENU(id as *mut c_void)),
                        Some(instance),
                        None,
                    )
                })?;
            }
            #[cfg(test)]
            if UI_PREVIEW_ROOT.with(|slot| slot.borrow().is_some()) {
                api("Queue isolated native client preview", unsafe {
                    PostMessageW(Some(hwnd), UI_PREVIEW_MESSAGE, WPARAM(0), LPARAM(0))
                })?;
            }
            Ok(LRESULT(0))
        }
        WM_SIZE => {
            let width = (lparam.0 as u32 & 0xffff) as i32;
            let height = ((lparam.0 as u32 >> 16) & 0xffff) as i32;
            layout(hwnd, width, height)?;
            Ok(LRESULT(0))
        }
        WM_MEASUREITEM => {
            if lparam.0 == 0 {
                return Err(NativeError::Verification(
                    "Missing original row measurement.",
                ));
            }
            let mut item =
                unsafe { std::ptr::read_unaligned(lparam.0 as *const controls::MEASUREITEMSTRUCT) };
            if matches!(item.CtlID as usize, HISTORY_LIST | RECOVERY_LIST) {
                let height = if item.CtlID == HISTORY_LIST as u32 {
                    47
                } else {
                    crate::main_layout::RECOVERY_ROW_HEIGHT_DIP
                };
                item.itemHeight = (height as u32 * crate::dpi::window_dpi(hwnd) + 48) / 96;
                unsafe {
                    std::ptr::write_unaligned(lparam.0 as *mut controls::MEASUREITEMSTRUCT, item);
                }
                return Ok(LRESULT(1));
            }
            Ok(unsafe { DefWindowProcW(hwnd, message, wparam, lparam) })
        }
        WM_DRAWITEM => {
            if lparam.0 == 0 {
                return Err(NativeError::Verification("Missing original UI draw state."));
            }
            let item =
                unsafe { std::ptr::read_unaligned(lparam.0 as *const controls::DRAWITEMSTRUCT) };
            let handled = MAIN_FONT.with(|slot| {
                let slot = slot.borrow();
                let Some(theme) = slot.as_ref() else {
                    return Ok(false);
                };
                if item.CtlType == controls::ODT_BUTTON {
                    let mut text = [0; 160];
                    let count = unsafe { GetWindowTextW(item.hwndItem, &mut text) };
                    let text = String::from_utf16(&text[..count as usize])
                        .map_err(|_| NativeError::Verification("Invalid original button label."))?;
                    theme.button(
                        &item,
                        &text,
                        item.CtlID == COPY_SELECTED as u32,
                        matches!(
                            item.CtlID as usize,
                            COPY_SELECTED
                                | COPY_LATEST
                                | COPY_ORIGINAL
                                | REFRESH_HISTORY
                                | CLEAR_HISTORY
                                | RECOVER_SELECTED
                                | REFRESH_RECOVERY
                                | DELETE_RECOVERY
                        ),
                    )?;
                    return Ok(true);
                }
                if item.CtlType == controls::ODT_TAB {
                    let text = match item.itemID {
                        0 => "History",
                        1 => "Recovery",
                        _ => return Ok(false),
                    };
                    theme.tab(&item, text)?;
                    return Ok(true);
                }
                if item.CtlType == controls::ODT_LISTBOX && item.CtlID == HISTORY_LIST as u32 {
                    UI_HISTORY.with(|history| {
                        let history = history.borrow();
                        if let Some(entry) = history
                            .as_ref()
                            .and_then(|entries| entries.get(item.itemID as usize))
                        {
                            theme.history_row(
                                &item,
                                &entry.preview,
                                &crate::main_presentation::history_metadata(
                                    entry,
                                    time::OffsetDateTime::now_utc(),
                                )?,
                            )?;
                        }
                        Ok::<_, NativeError>(())
                    })?;
                    return Ok(true);
                }
                if item.CtlType == controls::ODT_LISTBOX && item.CtlID == RECOVERY_LIST as u32 {
                    UI_RECOVERY.with(|recovery| {
                        if let Some(entry) = recovery
                            .borrow()
                            .as_ref()
                            .and_then(|entries| entries.get(item.itemID as usize))
                        {
                            let compact = item.rcItem.bottom - item.rcItem.top
                                < (crate::main_layout::RECOVERY_ROW_HEIGHT_DIP * theme.dpi as i32
                                    + 48)
                                    / 96;
                            theme.recovery_row(
                                &item,
                                &crate::main_presentation::recovery_lines(entry, compact)?,
                                compact,
                            )?;
                        }
                        Ok::<_, NativeError>(())
                    })?;
                    return Ok(true);
                }
                Ok::<_, NativeError>(false)
            })?;
            if handled {
                Ok(LRESULT(1))
            } else {
                Ok(unsafe { DefWindowProcW(hwnd, message, wparam, lparam) })
            }
        }
        WM_CTLCOLORSTATIC => {
            let child = HWND(lparam.0 as *mut c_void);
            let id = unsafe { GetDlgCtrlID(child) } as usize;
            let brush = MAIN_FONT.with(|slot| {
                slot.borrow().as_ref().map(|theme| {
                    theme.static_colors(
                        windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut c_void),
                        matches!(
                            id,
                            STATUS_CONTROL
                                | CONNECTION_LABEL
                                | ACCOUNT_LABEL
                                | CACHE_LABEL
                                | RECOVERY_HELP
                                | HISTORY_EMPTY
                                | RECOVERY_STATUS
                        ),
                        matches!(
                            id,
                            CONNECTION_LABEL
                                | ACCOUNT_LABEL
                                | CACHE_LABEL
                                | CONNECTION_VALUE
                                | ACCOUNT_VALUE
                                | CACHE_VALUE
                        ),
                    )
                })
            });
            Ok(brush.map_or_else(
                || unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
                |brush| LRESULT(brush.0 as isize),
            ))
        }
        WM_PAINT if MAIN_LAYOUT_READY.with(Cell::get) => {
            let mut bounds = RECT::default();
            api("Read original status-card viewport", unsafe {
                GetClientRect(hwnd, &mut bounds)
            })?;
            let layout = crate::main_layout::MainLayout::calculate(
                bounds.right,
                bounds.bottom,
                crate::dpi::window_dpi(hwnd),
            )
            .map_err(NativeError::Verification)?;
            let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let dc = unsafe { windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut paint) };
            let result = MAIN_FONT.with(|slot| {
                if let Some(theme) = slot.borrow().as_ref() {
                    theme.canvas(dc, bounds)?;
                    let card = layout.status_card;
                    theme.status_card(
                        dc,
                        RECT {
                            left: card.x,
                            top: card.y,
                            right: card.x + card.width,
                            bottom: card.y + card.height,
                        },
                    )?;
                    let list = if section_state(hwnd)?.0 == 0 {
                        layout.history
                    } else {
                        layout.recovery
                    };
                    theme.list_frame(
                        dc,
                        RECT {
                            left: list.x,
                            top: list.y,
                            right: list.x + list.width,
                            bottom: list.y + list.height,
                        },
                    )?;
                }
                Ok::<_, NativeError>(())
            });
            if !unsafe { windows::Win32::Graphics::Gdi::EndPaint(hwnd, &paint) }.as_bool() {
                eprintln!("Original status-card paint cleanup failed.");
            }
            result?;
            Ok(LRESULT(0))
        }
        WM_GETMINMAXINFO => {
            if lparam.0 == 0 {
                return Err(NativeError::Verification(
                    "Missing native minimum-size information.",
                ));
            }
            let mut limits = unsafe { std::ptr::read_unaligned(lparam.0 as *const MINMAXINFO) };
            let dpi = crate::dpi::window_dpi(hwnd) as i32;
            limits.ptMinTrackSize = POINT {
                x: 560 * dpi / 96,
                y: 420 * dpi / 96,
            };
            unsafe {
                std::ptr::write_unaligned(lparam.0 as *mut MINMAXINFO, limits);
            }
            Ok(LRESULT(0))
        }
        WM_NOTIFY => {
            if lparam.0 == 0 {
                return Err(NativeError::Verification(
                    "Missing native section notification.",
                ));
            }
            let header = unsafe { std::ptr::read_unaligned(lparam.0 as *const controls::NMHDR) };
            if header.idFrom == MAIN_TABS && header.code == controls::TCN_SELCHANGE {
                relayout(hwnd)?;
                return Ok(LRESULT(0));
            }
            Ok(unsafe { DefWindowProcW(hwnd, message, wparam, lparam) })
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xffff;
            let notification = (wparam.0 >> 16) & 0xffff;
            if id == 2 && notification == 0 {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            } else if id == SHOW_HISTORY_TEXT && notification == BN_CLICKED as usize {
                relayout(hwnd)?;
            } else if id == HISTORY_LIST && notification == LBN_SELCHANGE as usize {
                show_history_body(hwnd)?;
            } else if id == HISTORY_LIST && notification == LBN_DBLCLK as usize {
                client_command(hwnd, COPY_SELECTED)?;
            } else if notification == BN_CLICKED as usize {
                client_command(hwnd, id)?;
            }
            Ok(LRESULT(0))
        }
        WM_HOTKEY => {
            DICTATION_UI.with(|slot| {
                if let Some(ui) = slot.borrow_mut().as_mut() {
                    if let Err(error) = ui.hotkey(wparam.0 as i32, lparam.0 as usize) {
                        eprintln!("Dictation shortcut action failed: {error}");
                        notify(hwnd, "Dictation could not start", "The shortcut or Escape binding is unavailable, or the client is busy. No text was pasted.", true)?;
                    }
                }
                Ok::<_, NativeError>(())
            })?;
            Ok(LRESULT(0))
        }
        WM_DPICHANGED => {
            if lparam.0 == 0 {
                return Err(NativeError::Verification(
                    "DPI change did not include suggested bounds.",
                ));
            }
            let bounds = unsafe { std::ptr::read_unaligned(lparam.0 as *const RECT) };
            crate::dpi::suggested_bounds(hwnd, bounds)?;
            crate::icons::AppIcons::for_dpi(crate::dpi::window_dpi(hwnd))?.apply(hwnd);
            update_main_font(hwnd)?;
            Ok(LRESULT(0))
        }
        WM_TIMER if wparam.0 == CLIENT_TIMER => {
            refresh_client_ui(hwnd, false)?;
            let reload = RELOAD_CLIENT.with(|slot| slot.borrow_mut().take());
            if let Some(options) = reload {
                let host = CLIENT.with(|slot| slot.borrow_mut().take());
                drop(host);
                UI_HISTORY.with(|slot| *slot.borrow_mut() = None);
                UI_RECOVERY.with(|slot| *slot.borrow_mut() = None);
                attach_client(hwnd, options)?;
                refresh_client_ui(hwnd, false)?;
            }
            Ok(LRESULT(0))
        }
        WM_TIMER if wparam.0 == INSTANCE_TIMER => {
            COMPATIBLE_INSTANCE.with(|slot| {
                if let Some(instance) = slot.borrow().as_ref() {
                    match instance.poll()? {
                        Some(crate::InstanceEvent::Quit) => unsafe {
                            PostQuitMessage(0);
                        },
                        Some(crate::InstanceEvent::Activate) => show_status(hwnd),
                        None => {}
                    }
                }
                Ok::<_, NativeError>(())
            })?;
            Ok(LRESULT(0))
        }
        WM_POWERBROADCAST => {
            let command = match wparam.0 as u32 {
                PBT_APMSUSPEND => Some(ClientCommand::Suspend),
                PBT_APMRESUMEAUTOMATIC => Some(ClientCommand::Resume),
                _ => None,
            };
            if let Some(command) = command {
                CLIENT.with(|client| {
                    if let Some(host) = client.borrow().as_ref() {
                        if host.commands.try_send(command).is_err() {
                            eprintln!("Power-state transition could not be queued.");
                        }
                    }
                });
            }
            Ok(LRESULT(1))
        }
        WM_CLOSE => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            Ok(LRESULT(0))
        }
        WM_SHOWWINDOW => {
            if wparam.0 != 0 {
                MAIN_VIEW_CHANGED.with(|changed| changed.set(true));
            }
            Ok(unsafe { DefWindowProcW(hwnd, message, wparam, lparam) })
        }
        WM_KEYDOWN if wparam.0 == 0x1b => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            Ok(LRESULT(0))
        }
        WM_DESTROY | QUIT_MESSAGE => {
            unsafe {
                PostQuitMessage(0);
            }
            Ok(LRESULT(0))
        }
        WM_TIMER if wparam.0 == EXIT_TIMER => {
            unsafe {
                PostQuitMessage(0);
            }
            Ok(LRESULT(0))
        }
        ACTIVATE_MESSAGE => {
            show_status(hwnd);
            Ok(LRESULT(0))
        }
        TRAY_MESSAGE => {
            match lparam.0 as u32 & 0xffff {
                WM_CONTEXTMENU => context_menu(hwnd)?,
                event if tray_activation(event) => show_status(hwnd),
                _ => {}
            }
            Ok(LRESULT(0))
        }
        _ => Ok(unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }),
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match dispatch(hwnd, message, wparam, lparam) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("Native pilot window callback: {error}");
            CALLBACK_ERROR.with(|slot| {
                let mut stored = slot.borrow_mut();
                if stored.is_none() {
                    *stored = Some(error);
                }
            });
            unsafe {
                PostQuitMessage(1);
            }
            LRESULT(0)
        }
    }
}

struct TemporaryFile(Option<PathBuf>);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            if let Err(error) = std::fs::remove_file(path) {
                eprintln!(
                    "Native pilot cleanup: removing owned readiness staging file failed: {error}"
                );
            }
        }
    }
}

pub(crate) enum WriteMode {
    CreateNew,
    Replace,
}

pub(crate) fn atomic_write(path: &Path, data: &[u8], mode: WriteMode) -> Result<(), NativeError> {
    atomic_write_inner(
        path,
        data,
        mode,
        #[cfg(test)]
        None,
    )
}

#[cfg(test)]
pub(crate) fn atomic_write_observed(
    path: &Path,
    data: &[u8],
    mode: WriteMode,
    trace: &std::sync::Arc<crate::io_trace::Trace>,
) -> Result<(), NativeError> {
    atomic_write_inner(path, data, mode, Some(trace))
}

fn atomic_write_inner(
    path: &Path,
    data: &[u8],
    mode: WriteMode,
    #[cfg(test)] trace: Option<&std::sync::Arc<crate::io_trace::Trace>>,
) -> Result<(), NativeError> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let mut temporary_name = path.as_os_str().to_owned();
    temporary_name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary_path = PathBuf::from(temporary_name);
    let mut temporary = TemporaryFile(None);
    #[cfg(test)]
    let opening = trace.map(|trace| {
        trace.opening(
            &temporary_path,
            "atomic-staging-write",
            crate::io_trace::WRITE_ACCESS,
            crate::io_trace::DEFAULT_SHARE,
            0,
        )
    });
    #[allow(unused_mut)]
    let mut file = {
        #[cfg(test)]
        let _phase = crate::mutation_trace::span("staging-create");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .map_err(|source| NativeError::Io("create atomic staging file", source))?
    };
    #[cfg(test)]
    let mut file = crate::io_trace::TrackedFile::optional(
        file,
        trace,
        &temporary_path,
        "atomic-staging-write",
        crate::io_trace::WRITE_ACCESS,
        crate::io_trace::DEFAULT_SHARE,
        0,
    );
    #[cfg(test)]
    drop(opening);
    temporary.0 = Some(temporary_path.clone());
    {
        #[cfg(test)]
        let mut phase = crate::mutation_trace::span("staging-write");
        #[cfg(test)]
        if let Some(phase) = &mut phase {
            phase.counts(0, 0, data.len() as u64);
        }
        file.write_all(data)
            .map_err(|source| NativeError::Io("write atomic file", source))?;
    }
    {
        #[cfg(test)]
        let _phase = crate::mutation_trace::span("staging-flush");
        file.sync_all()
            .map_err(|source| NativeError::Io("flush atomic file", source))?;
    }
    {
        #[cfg(test)]
        let _phase = crate::mutation_trace::span("staging-close");
        drop(file);
    }
    let from = wide(temporary_path.as_os_str())?;
    let to = wide(path.as_os_str())?;
    // Both names share a directory. Never permit copy/delete fallback across volumes.
    {
        #[cfg(test)]
        let _phase = crate::mutation_trace::span("staging-rename");
        let renamed = match mode {
            WriteMode::CreateNew => api("MoveFileW (publish new file)", unsafe {
                MoveFileW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()))
            }),
            WriteMode::Replace => api("MoveFileExW (replace encrypted cache)", unsafe {
                MoveFileExW(
                    PCWSTR(from.as_ptr()),
                    PCWSTR(to.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }),
        };
        #[cfg(test)]
        if let (Err(error), Some(trace)) = (&renamed, trace) {
            let flags = match mode {
                WriteMode::CreateNew => 0,
                WriteMode::Replace => (MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH).0,
            };
            trace.failed_rename(&temporary_path, path, flags, error);
        }
        renamed?;
    }
    temporary.0 = None;
    Ok(())
}

fn publish_ready(path: &Path) -> Result<(), NativeError> {
    let data = format!(
        "{{\"schema_version\":1,\"status\":\"ready\",\"pid\":{},\"program\":\"VoicePromptRustPilot\",\"scope\":\"native-tray-dpapi-only\"}}\n",
        std::process::id()
    );
    atomic_write(path, data.as_bytes(), WriteMode::CreateNew)
}

pub struct TrayOptions {
    pub instance_key: String,
    pub ready_file: Option<PathBuf>,
    pub fixture_ready_file: Option<PathBuf>,
    pub run_for_seconds: Option<u32>,
    pub self_test: bool,
    pub client: Option<ClientOptions>,
}

struct ClientAttachment;

impl Drop for ClientAttachment {
    fn drop(&mut self) {
        FIXTURE_READY.with(|slot| *slot.borrow_mut() = None);
        crate::settings_ui::close();
        let ui = DICTATION_UI.with(|slot| slot.borrow_mut().take());
        drop(ui);
        let host = CLIENT.with(|slot| slot.borrow_mut().take());
        drop(host);
        UI_HISTORY.with(|slot| *slot.borrow_mut() = None);
        UI_RECOVERY.with(|slot| *slot.borrow_mut() = None);
        let instance = COMPATIBLE_INSTANCE.with(|slot| slot.borrow_mut().take());
        drop(instance);
    }
}

fn attach_client(hwnd: HWND, options: ClientOptions) -> Result<(), NativeError> {
    let host = ClientHost::start(options)?;
    DICTATION_UI.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(ui) = slot.as_mut() {
            ui.replace_host(
                host.commands.clone(),
                host.policy.clone(),
                host.clipboard_enabled,
            );
        } else {
            *slot = Some(crate::dictation_ui::DictationUi::new(
                hwnd,
                host.commands.clone(),
                host.policy.clone(),
                host.clipboard_enabled,
            )?);
        }
        Ok::<_, NativeError>(())
    })?;
    CLIENT.with(|slot| *slot.borrow_mut() = Some(host));
    Ok(())
}

pub fn run_tray(options: TrayOptions) -> Result<(), NativeError> {
    if let Some(client) = options.client.as_ref().filter(|client| client.auth_only) {
        if options.self_test
            || options.run_for_seconds.is_none()
            || options.ready_file.is_some()
            || options.fixture_ready_file.is_some()
        {
            return Err(NativeError::InvalidArgument(
                "Auth-only acceptance requires a bounded isolated application.",
            ));
        }
        crate::fixtures::validate_auth(&client.state_directory, Some(&options.instance_key))?;
    }
    if let Some(path) = &options.fixture_ready_file {
        let client = options
            .client
            .as_ref()
            .filter(|client| client.fixture_runtime)
            .ok_or(NativeError::InvalidArgument(
                "Client measurement readiness requires the signed-out full fixture.",
            ))?;
        if options.self_test
            || options.ready_file.is_some()
            || !path.is_absolute()
            || path
                .try_exists()
                .map_err(|error| NativeError::Io("Inspect fixture readiness destination", error))?
        {
            return Err(NativeError::InvalidArgument(
                "Invalid client measurement readiness destination.",
            ));
        }
        crate::fixtures::validate(&client.state_directory, Some(&options.instance_key))?;
    }
    if let Some(seconds) = options.run_for_seconds {
        if seconds == 0 || seconds > 3600 {
            return Err(NativeError::InvalidArgument(
                "Run duration must be 1-3600 seconds.",
            ));
        }
    }
    if let Some(path) = &options.ready_file {
        if path
            .try_exists()
            .map_err(|source| NativeError::Io("inspect readiness destination", source))?
        {
            return Err(NativeError::InvalidArgument(
                "Readiness file already exists.",
            ));
        }
    }
    let _client_attachment = ClientAttachment;
    let installed = options
        .client
        .as_ref()
        .is_some_and(|client| client.installed);
    if installed {
        let instance = crate::CompatibleInstance::acquire("VoicePrompt.SingleInstance")?;
        if !instance.primary {
            return instance.activate();
        }
        COMPATIBLE_INSTANCE.with(|slot| *slot.borrow_mut() = Some(instance));
    }
    let _instance = if installed {
        None
    } else {
        Some(match SingleInstance::acquire(&options.instance_key) {
            Ok(guard) => guard,
            Err(NativeError::AlreadyRunning)
                if options.ready_file.is_none() && !options.self_test =>
            {
                return request_activation(&options.instance_key);
            }
            Err(error) => return Err(error),
        })
    };
    CALLBACK_ERROR.with(|slot| *slot.borrow_mut() = None);
    api("SetProcessDpiAwarenessContext", unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    })?;
    let _accessibility_apartment = crate::accessibility::Apartment::new()?;
    api(
        "Initialize native tab controls",
        unsafe {
            controls::InitCommonControlsEx(&controls::INITCOMMONCONTROLSEX {
                dwSize: size_of::<controls::INITCOMMONCONTROLSEX>() as u32,
                dwICC: controls::ICC_TAB_CLASSES,
            })
        }
        .ok(),
    )?;
    let instance = HINSTANCE(
        api("GetModuleHandleW", unsafe {
            GetModuleHandleW(PCWSTR::null())
        })?
        .0,
    );
    let name = class_name(&options.instance_key)?;
    let class_icons = crate::icons::AppIcons::for_dpi(96)?;
    let cursor = api("LoadCursorW", unsafe { LoadCursorW(None, IDC_ARROW) })?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hIcon: class_icons.large,
        hCursor: cursor,
        hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
        lpszClassName: PCWSTR(name.as_ptr()),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(NativeError::Api(
            "RegisterClassW",
            windows::core::Error::from_thread(),
        ));
    }
    let registered = RegisteredClass { name, instance };
    let _font_lifetime = MainFontLifetime;
    let previous_focus = unsafe { GetForegroundWindow() };
    let hwnd = api("CreateWindowExW (pilot)", unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(registered.name.as_ptr()),
            if installed {
                w!("VoicePrompt")
            } else {
                w!("VoicePrompt - Rust migration")
            },
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            720,
            540,
            None,
            None,
            Some(instance),
            None,
        )
    })?;
    let _window = OwnedWindow(hwnd);
    let mut accessible_controls = Vec::new();
    for (id, name) in [
        (HEADER_LOGO, "VoicePrompt logo"),
        (HISTORY_LIST, "Cached transcripts, newest first"),
        (HISTORY_BODY, "Selected transcript text"),
        (RECOVERY_LIST, "Recoverable dictations"),
        (
            COPY_SELECTED,
            "Copy the selected transcript to the clipboard",
        ),
        (COPY_LATEST, "Copy the newest transcript to the clipboard"),
        (COPY_ORIGINAL, "Copy original dictation before AI cleanup"),
        (REFRESH_HISTORY, "Refresh history from the backend"),
        (CLEAR_HISTORY, "Delete all locally cached transcripts"),
        (SIGN_IN, "Sign in with Google in the system browser"),
        (SIGN_OUT, "Sign out and delete cached tokens"),
    ] {
        accessible_controls.push((
            api("Read native accessible-name control", unsafe {
                GetDlgItem(Some(hwnd), id as i32)
            })?,
            name,
        ));
    }
    let _accessible_names = crate::accessibility::Labels::new(&accessible_controls)?;
    let icons = crate::icons::AppIcons::for_dpi(crate::dpi::window_dpi(hwnd))?;
    icons.apply(hwnd);
    update_main_font(hwnd)?;
    let dpi = crate::dpi::window_dpi(hwnd) as i32;
    let mut work_area = RECT::default();
    api("Read initial native work area", unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut work_area as *mut RECT).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
        )
    })?;
    #[cfg(test)]
    let desired = if UI_PREVIEW_MINIMUM.with(Cell::get) {
        (560, 420)
    } else {
        (720, 540)
    };
    #[cfg(not(test))]
    let desired = (720, 540);
    let width = (desired.0 * dpi / 96).min(work_area.right - work_area.left);
    let height = (desired.1 * dpi / 96).min(work_area.bottom - work_area.top);
    api("Size native client for initial DPI", unsafe {
        SetWindowPos(
            hwnd,
            None,
            work_area.left + (work_area.right - work_area.left - width) / 2,
            work_area.top + (work_area.bottom - work_area.top - height) / 2,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    })?;
    MAIN_LAYOUT_READY.with(|ready| ready.set(true));
    relayout(hwnd)?;
    if installed && unsafe { SetTimer(Some(hwnd), INSTANCE_TIMER, 100, None) } == 0 {
        return Err(NativeError::Api(
            "Set compatible instance timer",
            windows::core::Error::from_thread(),
        ));
    }
    if let Some(error) = CALLBACK_ERROR.with(|slot| slot.borrow_mut().take()) {
        return Err(error);
    }
    let mut tray = Tray::add(hwnd, icons.small)?;
    let data = b"synthetic native DPAPI startup probe";
    if unprotect(&protect(data)?)? != data {
        return Err(NativeError::Verification(
            "Current-user DPAPI roundtrip failed.",
        ));
    }
    if options.self_test {
        if unsafe { IsWindowVisible(hwnd) }.as_bool()
            || unsafe { GetForegroundWindow() } != previous_focus
        {
            return Err(NativeError::Verification(
                "The hidden native startup changed foreground focus.",
            ));
        }
        if !matches!(
            SingleInstance::acquire(&options.instance_key),
            Err(NativeError::AlreadyRunning)
        ) {
            return Err(NativeError::Verification(
                "Duplicate isolated instances were not rejected.",
            ));
        }
    }
    if let Some(client) = options.client {
        if options.self_test && (client.oauth_config.is_some() || client.clipboard_enabled) {
            return Err(NativeError::InvalidArgument(
                "Self-test cannot use OAuth configuration or the system clipboard.",
            ));
        }
        let auth_only = client.auth_only;
        attach_client(hwnd, client)?;
        CLIENT_TICK.with(|tick| *tick.borrow_mut() = 100);
        if unsafe { SetTimer(Some(hwnd), CLIENT_TIMER, 100, None) } == 0 {
            return Err(NativeError::Api(
                "SetTimer (client UI)",
                windows::core::Error::from_thread(),
            ));
        }
        refresh_client_ui(hwnd, options.self_test)?;
        if auth_only {
            api("Set auth-only window title", unsafe {
                SetWindowTextW(hwnd, w!("VoicePrompt - Google auth-only test"))
            })?;
            show_status(hwnd);
        }
        if options.self_test {
            CLIENT.with(|slot| {
                let client = slot.borrow();
                let host = client
                    .as_ref()
                    .ok_or(NativeError::Verification("Client was not attached."))?;
                if host.clipboard_enabled
                    || host.status.borrow().auth
                        != voiceprompt_client_protocol::state::AuthState::NotConfigured
                {
                    return Err(NativeError::Verification(
                        "Isolated self-test must not sign in or access the clipboard.",
                    ));
                }
                crate::settings_ui::self_test(hwnd, &host.status.borrow().settings)?;
                let list = api("GetDlgItem (history self-test)", unsafe {
                    GetDlgItem(Some(hwnd), HISTORY_LIST as i32)
                })?;
                let count = unsafe { SendMessageW(list, LB_GETCOUNT, None, None) }.0;
                if count != host.status.borrow().history.len() as isize {
                    return Err(NativeError::Verification(
                        "Native history list does not match the isolated cache.",
                    ));
                }
                Ok(())
            })?;
        }
    }
    let stop_after = options.run_for_seconds.or(options.self_test.then_some(1));
    if let Some(seconds) = stop_after {
        if unsafe { SetTimer(Some(hwnd), EXIT_TIMER, seconds * 1000, None) } == 0 {
            return Err(NativeError::Api(
                "SetTimer",
                windows::core::Error::from_thread(),
            ));
        }
    }
    if let Some(path) = &options.ready_file {
        publish_ready(path)?;
    }
    if let Some(path) = &options.fixture_ready_file {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let ready = CLIENT.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .is_some_and(|host| host.status.borrow().runtime_ready)
            });
            if ready {
                break;
            }
            if std::time::Instant::now() >= until {
                return Err(NativeError::Verification(
                    "Full client fixture runtime did not become ready.",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let count = CLIENT.with(|slot| {
            let slot = slot.borrow();
            let host = slot
                .as_ref()
                .ok_or(NativeError::Verification("Missing full client fixture."))?;
            if host.clipboard_enabled
                || host.status.borrow().auth
                    != voiceprompt_client_protocol::state::AuthState::NotConfigured
            {
                return Err(NativeError::Verification(
                    "Measurement fixture enabled physical ports or authentication.",
                ));
            }
            Ok(host.status.borrow().history.len())
        })?;
        let data=serde_json::to_vec(&serde_json::json!({"status":"ready","scope":crate::fixtures::SCOPE,
            "pid":std::process::id(),"implementation":"rust","history_count":count,"auth":"not_configured",
            "physical_ports_enabled":false,"full_components":true,"instance_key":options.instance_key}))
            .map_err(|_|NativeError::Verification("Could not encode client measurement readiness."))?;
        FIXTURE_READY.with(|slot| *slot.borrow_mut() = Some((path.clone(), data)));
        api("Post client fixture readiness turn", unsafe {
            PostMessageW(Some(hwnd), FIXTURE_READY_MESSAGE, WPARAM(0), LPARAM(0))
        })?;
    }
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 == -1 {
            return Err(NativeError::Api(
                "GetMessageW",
                windows::core::Error::from_thread(),
            ));
        }
        if result.0 == 0 {
            break;
        }
        if crate::settings_ui::route(&message)
            || route_history_keys(hwnd, &message)?
            || unsafe { IsDialogMessageW(hwnd, &message) }.as_bool()
        {
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    drop(_client_attachment);
    tray.remove()?;
    if let Some(error) = CALLBACK_ERROR.with(|slot| slot.borrow_mut().take()) {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Bounded actual full-client visual fixture, physical/account ports disabled. Launch through UI tools in an explicitly marked signed-out synthetic root."]
    fn manual_native_client_visual_preview() {
        run_native_client_preview(false);
    }

    #[test]
    #[ignore = "Bounded minimum-reference-size full-client visual fixture, physical/account ports disabled. Launch through UI tools in a fresh marked synthetic root."]
    fn manual_native_client_minimum_visual_preview() {
        run_native_client_preview(true);
    }

    #[test]
    #[ignore = "One separately approved keyboard-only3min NoPorts main/Settings fixture in a fresh marked root; no clipboard/input emission/account/device ports."]
    fn manual_native_client_keyboard_preview() {
        let root = std::env::current_dir().unwrap();
        let marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("keyboard-fixture.json")).unwrap())
                .unwrap();
        assert_eq!(
            marker,
            serde_json::json!({"scope":"single-native-keyboard-only-fixture"})
        );
        UI_PREVIEW_KEYBOARD.with(|value| value.set(true));
        run_native_client_preview(false);
        UI_PREVIEW_KEYBOARD.with(|value| value.set(false));
    }

    #[test]
    #[ignore = "One user-directed automated Computer Use keyboard-only10min NoPorts fixture; no physical/account/destructive ports."]
    fn automated_native_client_keyboard_preview() {
        let root = std::env::current_dir().unwrap();
        let marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("keyboard-fixture.json")).unwrap())
                .unwrap();
        assert_eq!(
            marker,
            serde_json::json!({"scope":"single-native-keyboard-automation-fixture"})
        );
        UI_PREVIEW_KEYBOARD.with(|value| value.set(true));
        UI_PREVIEW_AUTOMATED.with(|value| value.set(true));
        run_native_client_preview(false);
        UI_PREVIEW_AUTOMATED.with(|value| value.set(false));
        UI_PREVIEW_KEYBOARD.with(|value| value.set(false));
    }

    #[test]
    fn read_only_preview_only_allows_settings_navigation_and_normal_controls_are_unchanged() {
        let mut options = ClientOptions {
            state_directory: PathBuf::new(),
            oauth_config: None,
            clipboard_enabled: false,
            installed: false,
            fixture_runtime: true,
            auth_only: false,
        };
        for id in CLIENT_ACTIONS {
            assert_eq!(fixture_ui_allows(&options, id), id == OPEN_SETTINGS);
        }
        options.fixture_runtime = false;
        for id in CLIENT_ACTIONS {
            assert!(fixture_ui_allows(&options, id));
        }
    }

    fn run_native_client_preview(minimum: bool) {
        let root = std::env::current_dir().unwrap();
        crate::fixtures::validate_scope(&root, None, crate::fixtures::SCOPE).unwrap();
        let marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("migration-fixture.json")).unwrap())
                .unwrap();
        let key = marker["instance_key"].as_str().unwrap().to_owned();
        for name in [
            "ui-preview-ready.json",
            "ui-preview-closed.json",
            "ui-host-ready.json",
        ] {
            assert!(
                !root.join(name).exists(),
                "A visual client fixture root cannot be reused."
            );
        }
        let before_settings = std::fs::read(root.join("settings.json")).unwrap();
        let before_history = std::fs::read(root.join("history.json")).unwrap();
        crate::main_presentation::reset_history_format_calls();
        UI_PREVIEW_MINIMUM.with(|value| value.set(minimum));
        crate::settings_ui::preview_minimum(minimum);
        UI_PREVIEW_ROOT.with(|slot| *slot.borrow_mut() = Some(root.clone()));
        run_tray(TrayOptions {
            instance_key: key,
            ready_file: None,
            fixture_ready_file: Some(root.join("ui-host-ready.json")),
            run_for_seconds: Some(if UI_PREVIEW_AUTOMATED.with(Cell::get) {
                600
            } else {
                180
            }),
            self_test: false,
            client: Some(ClientOptions {
                state_directory: root.clone(),
                oauth_config: None,
                clipboard_enabled: false,
                installed: false,
                fixture_runtime: true,
                auth_only: false,
            }),
        })
        .unwrap();
        let visible_history_formats = crate::main_presentation::history_format_calls();
        let history: Vec<voiceprompt_client_protocol::history::HistoryEntry> =
            voiceprompt_client_protocol::history::parse_history(&before_history).unwrap();
        assert!(visible_history_formats >= history.len());
        UI_PREVIEW_ROOT.with(|slot| *slot.borrow_mut() = None);
        UI_PREVIEW_MINIMUM.with(|value| value.set(false));
        crate::settings_ui::preview_minimum(false);
        assert_eq!(
            std::fs::read(root.join("settings.json")).unwrap(),
            before_settings
        );
        assert_eq!(
            std::fs::read(root.join("history.json")).unwrap(),
            before_history
        );
        assert!(!root.join("tokens.bin").exists());
        atomic_write(
            &root.join("ui-preview-closed.json"),
            serde_json::json!({
                "scope":"native-full-client-ui-preview","pid":std::process::id(),"closed":true,
                "physical_ports":false,"settings_history_unchanged":true,"token_cache_absent":true,
                "visible_history_format_calls":visible_history_formats,
            })
            .to_string()
            .as_bytes(),
            WriteMode::CreateNew,
        )
        .unwrap();
    }

    #[test]
    fn original_copy_tracks_selected_row_presence_including_empty_original_like_wpf() {
        let entries: Vec<_> = (0..2)
            .map(|index| voiceprompt_client_protocol::history::HistoryEntry {
                transcript_id: index.to_string(),
                recording_id: "synthetic".into(),
                preview: "Synthetic".into(),
                body: "Synthetic final".into(),
                raw_body: (index == 1).then(String::new),
                refinement_fallback_blocks: 0,
                completed_at: time::OffsetDateTime::UNIX_EPOCH,
                cached_at: time::OffsetDateTime::UNIX_EPOCH,
                character_count: "Synthetic final".len(),
            })
            .collect();
        assert!(selected_history_entry(&entries, None).is_none());
        assert!(selected_history_entry(&entries, Some("missing")).is_none());
        assert!(
            selected_history_entry(&entries, Some("0"))
                .unwrap()
                .raw_body
                .is_none()
        );
        assert_eq!(
            selected_history_entry(&entries, Some("1"))
                .unwrap()
                .raw_body
                .as_deref(),
            Some("")
        );
    }

    #[test]
    fn history_keyboard_actions_match_actual_wpf_without_touching_input_or_clipboard() {
        let none = HOT_KEY_MODIFIERS::default();
        for key in [VK_RETURN, VK_SPACE] {
            assert_eq!(
                history_key_action(true, key.0 as usize, none),
                Some(COPY_SELECTED)
            );
            assert_eq!(history_key_action(false, key.0 as usize, none), None);
            assert_eq!(
                history_key_action(true, key.0 as usize, MOD_SHIFT),
                Some(COPY_SELECTED)
            );
            assert_eq!(
                history_key_action(true, key.0 as usize, MOD_CONTROL),
                Some(COPY_SELECTED)
            );
        }
        assert_eq!(
            history_key_action(true, VK_C.0 as usize, MOD_CONTROL),
            Some(COPY_SELECTED)
        );
        assert_eq!(
            history_key_action(true, VK_C.0 as usize, MOD_CONTROL | MOD_SHIFT),
            None
        );
        assert_eq!(
            history_key_action(false, VK_C.0 as usize, MOD_CONTROL),
            None
        );
        assert_eq!(history_key_action(true, VK_C.0 as usize, none), None);
        for focused in [false, true] {
            assert_eq!(
                history_key_action(focused, VK_F5.0 as usize, none),
                Some(REFRESH_HISTORY)
            );
            assert_eq!(history_key_action(focused, VK_F5.0 as usize, MOD_ALT), None);
        }
        assert_eq!(
            history_key_action(
                true,
                windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE.0 as usize,
                none
            ),
            None
        );
    }

    #[test]
    fn tray_matches_reference_menu_state_and_default_action_without_showing_ui() {
        for paused in [false, true] {
            for can_copy in [false, true] {
                let menu = context_menu_items(true, paused, can_copy).unwrap();
                unsafe {
                    assert_eq!(GetMenuItemCount(Some(menu.0)), 5);
                    assert_eq!(
                        GetMenuDefaultItem(menu.0, 0, GET_MENU_DEFAULT_ITEM_FLAGS::default()),
                        MENU_OPEN as u32
                    );
                    assert_eq!(GetMenuItemID(menu.0, 4), MENU_EXIT as u32);
                    assert_eq!(
                        GetMenuState(menu.0, COPY_LATEST as u32, MF_BYCOMMAND) & MF_GRAYED.0 == 0,
                        can_copy
                    );
                    assert_eq!(
                        GetMenuState(menu.0, PAUSE as u32, MF_BYCOMMAND) & MF_CHECKED.0 != 0,
                        paused
                    );
                    let mut label = [0; 64];
                    let length =
                        GetMenuStringW(menu.0, PAUSE as u32, Some(&mut label), MF_BYCOMMAND);
                    assert_eq!(
                        String::from_utf16(&label[..length as usize]).unwrap(),
                        "&Pause notifications"
                    );
                }
            }
        }
    }

    #[test]
    fn live_tray_status_and_activation_match_reference_without_input() {
        let mut status = crate::client::ClientStatus::default();
        assert_eq!(
            tray_status_text(&status),
            "VoicePrompt \u{2014} Not configured"
        );
        status.auth = voiceprompt_client_protocol::state::AuthState::SignedIn;
        status.realtime = voiceprompt_client_protocol::websocket::RealtimeState::Reconnecting;
        assert_eq!(
            tray_status_text(&status),
            "VoicePrompt \u{2014} Reconnecting"
        );
        assert!(tray_status_text(&status).encode_utf16().count() <= 63);
        assert!(tray_activation(WM_LBUTTONDBLCLK));
        assert!(tray_activation(TRAY_KEY_SELECT));
        assert!(tray_activation(NIN_BALLOONUSERCLICK));
        assert!(
            !tray_activation(NIN_SELECT),
            "A single click must not open the window."
        );
        assert!(!tray_activation(WM_CONTEXTMENU));
    }

    #[test]
    fn dpapi_roundtrips_synthetic_data_and_rejects_corruption() {
        for data in [
            Vec::new(),
            b"synthetic token fixture".to_vec(),
            vec![0; 65536],
        ] {
            let encrypted = protect(&data).unwrap();
            assert_ne!(encrypted, data);
            assert_eq!(unprotect(&encrypted).unwrap(), data);
            let mut corrupt = encrypted;
            corrupt.truncate(corrupt.len() / 2);
            assert!(unprotect(&corrupt).is_err());
        }
        assert!(unprotect(&[]).is_err());
    }

    #[test]
    fn instance_ownership_is_isolated_and_released() {
        let key = format!("test-{}", std::process::id());
        let first = SingleInstance::acquire(&key).unwrap();
        assert!(matches!(
            SingleInstance::acquire(&key),
            Err(NativeError::AlreadyRunning)
        ));
        let second = SingleInstance::acquire(&format!("{key}-other")).unwrap();
        drop(second);
        drop(first);
        assert!(SingleInstance::acquire(&key).is_ok());
    }

    #[test]
    fn instance_keys_cannot_escape_the_pilot_namespace() {
        for key in [
            "",
            "..\\VoicePrompt.SingleInstance",
            "Local\\system",
            "bad key",
            "\0",
        ] {
            assert!(validate_instance_key(key).is_err());
        }
        assert!(validate_instance_key(&"a".repeat(65)).is_err());
        assert!(validate_instance_key("interactive-2026_test").is_ok());
    }
}
