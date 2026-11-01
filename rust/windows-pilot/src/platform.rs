use std::cell::RefCell;
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
    LRESULT, LocalFree, POINT, WPARAM,
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
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETFOCUS, NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const ENTROPY: &[u8] = b"VoicePrompt.DPAPI.v1";
const MAX_PLAINTEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CIPHERTEXT_BYTES: usize = MAX_PLAINTEXT_BYTES + 64 * 1024;
const TRAY_MESSAGE: u32 = WM_APP + 1;
const ACTIVATE_MESSAGE: u32 = WM_APP + 2;
const QUIT_MESSAGE: u32 = WM_APP + 3;
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

thread_local! {
    static CALLBACK_ERROR: RefCell<Option<NativeError>> = const { RefCell::new(None) };
    static CLIENT: RefCell<Option<ClientHost>> = const { RefCell::new(None) };
    static UI_HISTORY: RefCell<Option<std::sync::Arc<Vec<voiceprompt_client_protocol::history::HistoryEntry>>>> =
        const { RefCell::new(None) };
}

#[derive(Debug)]
pub enum NativeError {
    InvalidArgument(&'static str),
    AlreadyRunning,
    Api(&'static str, windows::core::Error),
    Io(&'static str, io::Error),
    Verification(&'static str),
    Protocol(ProtocolError),
}

impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument(message) | Self::Verification(message) => f.write_str(message),
            Self::AlreadyRunning => {
                f.write_str("This isolated Rust pilot instance is already running.")
            }
            Self::Api(operation, source) => write!(f, "{operation} returned failure: {source}"),
            Self::Io(operation, source) => write!(f, "{operation} failed: {source}"),
            Self::Protocol(error) => error.fmt(f),
        }
    }
}

impl Error for NativeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Api(_, source) => Some(source),
            Self::Io(_, source) => Some(source),
            Self::Protocol(source) => Some(source),
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
    transform(data, true)
}

pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, NativeError> {
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
        use std::os::windows::fs::MetadataExt;
        for path in self.path.ancestors() {
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
        let tip: Vec<u16> = "VoicePrompt Rust pilot - no cloud or dictation yet"
            .encode_utf16()
            .collect();
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

fn context_menu(hwnd: HWND) -> Result<(), NativeError> {
    let menu = OwnedMenu(api("CreatePopupMenu", unsafe { CreatePopupMenu() })?);
    let mut position = POINT::default();
    unsafe {
        api(
            "AppendMenuW",
            AppendMenuW(menu.0, MF_STRING, MENU_OPEN, w!("Open pilot status")),
        )?;
        let connected = CLIENT.with(|client| client.borrow().is_some());
        if connected {
            api(
                "AppendMenuW",
                AppendMenuW(menu.0, MF_STRING, COPY_LATEST, w!("Copy latest")),
            )?;
            let paused = CLIENT.with(|client| {
                client
                    .borrow()
                    .as_ref()
                    .is_some_and(|host| host.policy.paused())
            });
            api(
                "AppendMenuW",
                AppendMenuW(
                    menu.0,
                    MF_STRING | if paused { MF_CHECKED } else { MF_UNCHECKED },
                    PAUSE,
                    w!("Pause automatic copy and notifications"),
                ),
            )?;
            api(
                "AppendMenuW",
                AppendMenuW(menu.0, MF_STRING, RECONNECT, w!("Reconnect")),
            )?;
        }
        api(
            "AppendMenuW",
            AppendMenuW(menu.0, MF_SEPARATOR, 0, PCWSTR::null()),
        )?;
        api(
            "AppendMenuW",
            AppendMenuW(menu.0, MF_STRING, MENU_EXIT, w!("Exit Rust pilot")),
        )?;
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

fn client_command(hwnd: HWND, command: usize) -> Result<(), NativeError> {
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

fn set_text(hwnd: HWND, id: usize, text: &str) -> Result<(), NativeError> {
    let child = api("GetDlgItem", unsafe { GetDlgItem(Some(hwnd), id as i32) })?;
    let text = wide(OsStr::new(text))?;
    api("SetWindowTextW", unsafe {
        SetWindowTextW(child, PCWSTR(text.as_ptr()))
    })
}

fn notify(hwnd: HWND, title: &str, body: &str, warning: bool) -> Result<(), NativeError> {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_INFO,
        dwInfoFlags: if warning { NIIF_WARNING } else { NIIF_INFO },
        ..Default::default()
    };
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

fn show_history_body(hwnd: HWND) -> Result<(), NativeError> {
    let selected = selected_id(hwnd)?;
    UI_HISTORY.with(|history| {
        let history = history.borrow();
        let entry = history.as_ref().and_then(|entries| {
            entries
                .iter()
                .find(|entry| Some(&entry.transcript_id) == selected.as_ref())
        });
        set_text(
            hwnd,
            HISTORY_BODY,
            entry.map_or("", |entry| entry.body.as_str()),
        )
    })
}

fn refresh_client_ui(hwnd: HWND) -> Result<(), NativeError> {
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
            }
        }
        if !host.status.has_changed().map_err(|_| {
            NativeError::Verification("The client status channel stopped.")
        })? && UI_HISTORY.with(|history| history.borrow().is_some()) {
            return Ok(());
        }
        let status = host.status.borrow_and_update().clone();
        set_text(hwnd, STATUS_CONTROL, &format!(
            "Isolated Rust client - dictation and Recovery are not implemented yet.\r\nSign-in: {:?}{} | Realtime: {:?}\r\n{}\r\nNo installed VoicePrompt data is discovered automatically.",
            status.auth, if status.signing_in { " (browser flow pending)" } else { "" },
            status.realtime, status.issue.unwrap_or(""),
        ))?;
        set_text(hwnd, PAUSE, if host.policy.paused() { "Resume auto-copy" } else { "Pause auto-copy" })?;
        for id in [SIGN_IN, SIGN_OUT, COPY_LATEST, PAUSE, RECONNECT, COPY_SELECTED, COPY_ORIGINAL] {
            let control = api("GetDlgItem", unsafe { GetDlgItem(Some(hwnd), id as i32) })?;
            let enabled = match id {
                SIGN_IN => status.auth != voiceprompt_client_protocol::state::AuthState::NotConfigured && !status.signing_in,
                SIGN_OUT => status.auth == voiceprompt_client_protocol::state::AuthState::SignedIn || status.signing_in,
                COPY_LATEST | COPY_SELECTED | COPY_ORIGINAL => !status.history.is_empty(),
                _ => true,
            };
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
                let label = format!("{}  {}", entry.completed_at, entry.preview.chars().take(120).collect::<String>());
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
        Ok(())
    })
}

fn layout(hwnd: HWND, width: i32, height: i32) -> Result<(), NativeError> {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as i32;
    let scale = |value: i32| value * dpi / 96;
    let left = scale(16);
    let content = (width - left * 2).max(1);
    for (id, x, y, w, h) in [
        (STATUS_CONTROL, left, scale(12), content, scale(92)),
        (SIGN_IN, left, scale(112), scale(80), scale(28)),
        (SIGN_OUT, scale(102), scale(112), scale(80), scale(28)),
        (COPY_LATEST, scale(188), scale(112), scale(94), scale(28)),
        (PAUSE, scale(288), scale(112), scale(130), scale(28)),
        (RECONNECT, scale(424), scale(112), scale(90), scale(28)),
        (HISTORY_LIST, left, scale(152), content, scale(128)),
        (
            HISTORY_BODY,
            left,
            scale(292),
            content,
            (height - scale(346)).max(1),
        ),
        (
            COPY_SELECTED,
            left,
            (height - scale(42)).max(scale(310)),
            scale(130),
            scale(28),
        ),
        (
            COPY_ORIGINAL,
            scale(154),
            (height - scale(42)).max(scale(310)),
            scale(130),
            scale(28),
        ),
    ] {
        let child = api("GetDlgItem", unsafe { GetDlgItem(Some(hwnd), id as i32) })?;
        api("MoveWindow", unsafe { MoveWindow(child, x, y, w, h, true) })?;
    }
    Ok(())
}

fn dispatch(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Result<LRESULT, NativeError> {
    match message {
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
                    w!(
                        "Native Rust migration pilot\r\n\r\nReady: tray, isolated instance, current-user DPAPI.\r\nNot yet connected: Google login, PubSub, dictation, history.\r\n\r\nThe installed VoicePrompt and its data remain unchanged."
                    ),
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
            ] {
                let control = api("CreateWindowExW (button)", unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        w!("BUTTON"),
                        label,
                        WS_CHILD | WS_VISIBLE | WS_TABSTOP,
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
                        | WINDOW_STYLE(LBS_NOTIFY as u32),
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
            ] {
                let control = api("CreateWindowExW (history)", unsafe {
                    CreateWindowExW(
                        WS_EX_CLIENTEDGE,
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
            Ok(LRESULT(0))
        }
        WM_SIZE => {
            let width = (lparam.0 as u32 & 0xffff) as i32;
            let height = ((lparam.0 as u32 >> 16) & 0xffff) as i32;
            layout(hwnd, width, height)?;
            Ok(LRESULT(0))
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xffff;
            let notification = (wparam.0 >> 16) & 0xffff;
            if id == HISTORY_LIST && notification == LBN_SELCHANGE as usize {
                show_history_body(hwnd)?;
            } else if notification == BN_CLICKED as usize {
                client_command(hwnd, id)?;
            }
            Ok(LRESULT(0))
        }
        WM_TIMER if wparam.0 == CLIENT_TIMER => {
            refresh_client_ui(hwnd)?;
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
                WM_LBUTTONDBLCLK | NIN_SELECT | 1025 => show_status(hwnd),
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

enum WriteMode {
    CreateNew,
    Replace,
}

fn atomic_write(path: &Path, data: &[u8], mode: WriteMode) -> Result<(), NativeError> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let mut temporary_name = path.as_os_str().to_owned();
    temporary_name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary_path = PathBuf::from(temporary_name);
    let mut temporary = TemporaryFile(None);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_path)
        .map_err(|source| NativeError::Io("create atomic staging file", source))?;
    temporary.0 = Some(temporary_path.clone());
    file.write_all(data)
        .map_err(|source| NativeError::Io("write atomic file", source))?;
    file.sync_all()
        .map_err(|source| NativeError::Io("flush atomic file", source))?;
    drop(file);
    let from = wide(temporary_path.as_os_str())?;
    let to = wide(path.as_os_str())?;
    // Both names share a directory. Never permit copy/delete fallback across volumes.
    match mode {
        WriteMode::CreateNew => api("MoveFileW (publish new file)", unsafe {
            MoveFileW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()))
        })?,
        WriteMode::Replace => api("MoveFileExW (replace encrypted cache)", unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        })?,
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
    pub run_for_seconds: Option<u32>,
    pub self_test: bool,
    pub client: Option<ClientOptions>,
}

struct ClientAttachment;

impl Drop for ClientAttachment {
    fn drop(&mut self) {
        let host = CLIENT.with(|slot| slot.borrow_mut().take());
        drop(host);
        UI_HISTORY.with(|slot| *slot.borrow_mut() = None);
    }
}

pub fn run_tray(options: TrayOptions) -> Result<(), NativeError> {
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
    let _instance = match SingleInstance::acquire(&options.instance_key) {
        Ok(guard) => guard,
        Err(NativeError::AlreadyRunning) if options.ready_file.is_none() && !options.self_test => {
            return request_activation(&options.instance_key);
        }
        Err(error) => return Err(error),
    };
    CALLBACK_ERROR.with(|slot| *slot.borrow_mut() = None);
    let _client_attachment = ClientAttachment;
    api("SetProcessDpiAwarenessContext", unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    })?;
    let instance = HINSTANCE(
        api("GetModuleHandleW", unsafe {
            GetModuleHandleW(PCWSTR::null())
        })?
        .0,
    );
    let name = class_name(&options.instance_key)?;
    let icon = api("LoadIconW", unsafe { LoadIconW(None, IDI_APPLICATION) })?;
    let cursor = api("LoadCursorW", unsafe { LoadCursorW(None, IDC_ARROW) })?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hIcon: icon,
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
    let previous_focus = unsafe { GetForegroundWindow() };
    let hwnd = api("CreateWindowExW (pilot)", unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(registered.name.as_ptr()),
            w!("VoicePrompt Rust pilot - native foundation"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            570,
            560,
            None,
            None,
            Some(instance),
            None,
        )
    })?;
    let _window = OwnedWindow(hwnd);
    if let Some(error) = CALLBACK_ERROR.with(|slot| slot.borrow_mut().take()) {
        return Err(error);
    }
    let mut tray = Tray::add(hwnd, icon)?;
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
        let host = ClientHost::start(client)?;
        CLIENT.with(|slot| *slot.borrow_mut() = Some(host));
        if unsafe { SetTimer(Some(hwnd), CLIENT_TIMER, 100, None) } == 0 {
            return Err(NativeError::Api(
                "SetTimer (client UI)",
                windows::core::Error::from_thread(),
            ));
        }
        refresh_client_ui(hwnd)?;
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
        if unsafe { IsDialogMessageW(hwnd, &message) }.as_bool() {
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
