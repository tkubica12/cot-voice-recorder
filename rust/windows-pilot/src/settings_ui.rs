use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;

use tokio::sync::mpsc;
use voiceprompt_client_protocol::settings::AppSettings;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_WINDOW, DEFAULT_GUI_FONT, GetStockObject, GetSysColorBrush,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::NativeError;
use crate::client::ClientCommand;

thread_local! {
    static SETTINGS: RefCell<Option<SettingsUi>> = const { RefCell::new(None) };
}

const URL: i32 = 2001;
const PAUSED: i32 = 2002;
const AUTO_START: i32 = 2003;
const ENABLED: i32 = 2004;
const HOLD: i32 = 2005;
const TOGGLE: i32 = 2006;
const LANGUAGE: i32 = 2007;
const REFINE: i32 = 2008;
const STREAMING: i32 = 2009;
const WAIT: i32 = 2010;
const SAVE: i32 = 2011;

struct SettingsUi {
    window: HWND,
    instance: HINSTANCE,
    commands: mpsc::Sender<ClientCommand>,
    original: AppSettings,
    font: crate::dpi::DpiFont,
}

fn native<T>(operation: &'static str, result: windows::core::Result<T>) -> Result<T, NativeError> {
    result.map_err(|error| NativeError::Api(operation, error))
}

fn encoded(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
fn set(window: HWND, id: i32, text: &str) -> Result<(), NativeError> {
    let child = native("Get Settings control", unsafe {
        GetDlgItem(Some(window), id)
    })?;
    let value = encoded(text);
    native("Set Settings control text", unsafe {
        SetWindowTextW(child, PCWSTR(value.as_ptr()))
    })
}
fn checked(window: HWND, id: i32, value: bool) -> Result<(), NativeError> {
    let child = native("Get Settings checkbox", unsafe {
        GetDlgItem(Some(window), id)
    })?;
    unsafe {
        let _ = SendMessageW(child, BM_SETCHECK, Some(WPARAM(usize::from(value))), None);
    }
    Ok(())
}
fn read(window: HWND, id: i32) -> Result<String, NativeError> {
    let child = native("Get Settings text", unsafe { GetDlgItem(Some(window), id) })?;
    let length = unsafe { GetWindowTextLengthW(child) };
    if !(0..=8192).contains(&length) {
        return Err(NativeError::InvalidArgument(
            "Settings text exceeds supported bounds.",
        ));
    }
    let mut value = vec![0; length as usize + 1];
    let count = unsafe { GetWindowTextW(child, &mut value) };
    if count != length {
        return Err(NativeError::Verification(
            "Settings text could not be read.",
        ));
    }
    String::from_utf16(&value[..count as usize])
        .map_err(|_| NativeError::InvalidArgument("Invalid Settings UTF-16 text."))
}
fn is_checked(window: HWND, id: i32) -> Result<bool, NativeError> {
    let child = native("Get Settings checkbox", unsafe {
        GetDlgItem(Some(window), id)
    })?;
    Ok(unsafe { SendMessageW(child, BM_GETCHECK, None, None) }.0 == 1)
}

impl SettingsUi {
    fn create(
        owner: HWND,
        commands: mpsc::Sender<ClientCommand>,
        settings: &AppSettings,
    ) -> Result<Self, NativeError> {
        let font = crate::dpi::DpiFont::create(crate::dpi::window_dpi(owner))?;
        let instance = HINSTANCE(
            native("Get Settings module", unsafe {
                GetModuleHandleW(PCWSTR::null())
            })?
            .0,
        );
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            hCursor: native("Load Settings cursor", unsafe {
                LoadCursorW(None, IDC_ARROW)
            })?,
            hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
            lpszClassName: w!("VoicePromptRustPilot.Settings"),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(NativeError::Api(
                "Register Settings window",
                windows::core::Error::from_thread(),
            ));
        }
        let window = match unsafe {
            CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.lpszClassName,
                w!("VoicePrompt - Settings"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                570,
                550,
                Some(owner),
                None,
                Some(instance),
                None,
            )
        } {
            Ok(window) => window,
            Err(error) => {
                if let Err(error) = unsafe { UnregisterClassW(class.lpszClassName, Some(instance)) }
                {
                    eprintln!("Settings class cleanup: {error}");
                }
                return Err(NativeError::Api("Create Settings window", error));
            }
        };
        let ui = Self {
            window,
            instance,
            commands,
            original: settings.clone(),
            font,
        };
        let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(window) }.max(96) as i32;
        let scale = |value: i32| value * dpi / 96;
        for (id, label, class, style, y) in [
            (URL, "Backend URL", w!("EDIT"), ES_AUTOHSCROLL as u32, 18),
            (
                PAUSED,
                "Pause automatic copy and notifications",
                w!("BUTTON"),
                BS_AUTOCHECKBOX as u32,
                54,
            ),
            (
                AUTO_START,
                "Start automatically at Windows sign-in",
                w!("BUTTON"),
                BS_AUTOCHECKBOX as u32,
                88,
            ),
            (
                ENABLED,
                "Enable dictation shortcuts",
                w!("BUTTON"),
                BS_AUTOCHECKBOX as u32,
                122,
            ),
            (
                HOLD,
                "Hold-to-talk shortcut",
                w!("EDIT"),
                ES_AUTOHSCROLL as u32,
                158,
            ),
            (
                TOGGLE,
                "Hands-free shortcut",
                w!("EDIT"),
                ES_AUTOHSCROLL as u32,
                194,
            ),
            (
                LANGUAGE,
                "Language: auto / cs / en",
                w!("EDIT"),
                ES_AUTOHSCROLL as u32,
                230,
            ),
            (
                REFINE,
                "Polish dictation with LLM",
                w!("BUTTON"),
                BS_AUTOCHECKBOX as u32,
                266,
            ),
            (
                STREAMING,
                "Use live MAI streaming",
                w!("BUTTON"),
                BS_AUTOCHECKBOX as u32,
                300,
            ),
            (
                WAIT,
                "Final cleanup wait (500-5000 ms, step 500)",
                w!("EDIT"),
                ES_AUTOHSCROLL as u32,
                336,
            ),
        ] {
            let is_edit = matches!(id, URL | HOLD | TOGGLE | LANGUAGE | WAIT);
            if is_edit {
                let label_text = encoded(label);
                let child = native("Create Settings label", unsafe {
                    CreateWindowExW(
                        Default::default(),
                        w!("STATIC"),
                        PCWSTR(label_text.as_ptr()),
                        WS_CHILD | WS_VISIBLE,
                        scale(16),
                        scale(y),
                        scale(250),
                        scale(25),
                        Some(window),
                        None,
                        Some(instance),
                        None,
                    )
                })?;
                unsafe {
                    let _ = SendMessageW(
                        child,
                        WM_SETFONT,
                        Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),
                        None,
                    );
                }
            }
            let text = encoded(if !is_edit { label } else { "" });
            let child = native("Create Settings input", unsafe {
                CreateWindowExW(
                    if is_edit {
                        WS_EX_CLIENTEDGE
                    } else {
                        Default::default()
                    },
                    class,
                    PCWSTR(text.as_ptr()),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(style),
                    scale(if is_edit { 275 } else { 16 }),
                    scale(y),
                    scale(if is_edit { 245 } else { 500 }),
                    scale(26),
                    Some(window),
                    Some(HMENU(id as *mut c_void)),
                    Some(instance),
                    None,
                )
            })?;
            unsafe {
                let _ = SendMessageW(
                    child,
                    WM_SETFONT,
                    Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),
                    None,
                );
            }
        }
        let hint = encoded(
            "Apply while idle. Backend changes reconnect VoicePrompt.\r\nNo configuration or credentials are included in the public installer.",
        );
        native("Create Settings guidance", unsafe {
            CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                PCWSTR(hint.as_ptr()),
                WS_CHILD | WS_VISIBLE,
                scale(16),
                scale(374),
                scale(500),
                scale(42),
                Some(window),
                None,
                Some(instance),
                None,
            )
        })?;
        native("Create Settings save button", unsafe {
            CreateWindowExW(
                Default::default(),
                w!("BUTTON"),
                w!("Save Settings"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                scale(16),
                scale(430),
                scale(145),
                scale(30),
                Some(window),
                Some(HMENU(SAVE as *mut c_void)),
                Some(instance),
                None,
            )
        })?;
        ui.load(settings)?;
        ui.font.apply_children(window)?;
        crate::icons::AppIcons::for_dpi(ui.font.dpi)?.apply(window);
        unsafe {
            SetWindowPos(
                window,
                None,
                0,
                0,
                scale(570),
                scale(550),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .map_err(|error| NativeError::Api("Scale Settings window", error))?;
        }
        Ok(ui)
    }

    fn load(&self, settings: &AppSettings) -> Result<(), NativeError> {
        for (id, value) in [
            (URL, settings.backend_base_url.as_str()),
            (HOLD, settings.dictation_shortcut.as_str()),
            (TOGGLE, settings.dictation_toggle_shortcut.as_str()),
            (LANGUAGE, settings.dictation_language.as_str()),
            (
                WAIT,
                &settings.dictation_final_wait_milliseconds.to_string(),
            ),
        ] {
            set(self.window, id, value)?;
        }
        for (id, value) in [
            (PAUSED, settings.notifications_paused),
            (AUTO_START, settings.auto_start),
            (ENABLED, settings.dictation_enabled),
            (REFINE, settings.dictation_refinement_enabled),
            (STREAMING, settings.dictation_streaming_enabled),
        ] {
            checked(self.window, id, value)?;
        }
        Ok(())
    }
    fn values(&self) -> Result<AppSettings, NativeError> {
        let mut settings = self.original.clone();
        settings.backend_base_url = read(self.window, URL)?.trim().trim_end_matches('/').into();
        settings.notifications_paused = is_checked(self.window, PAUSED)?;
        settings.auto_start = is_checked(self.window, AUTO_START)?;
        settings.dictation_enabled = is_checked(self.window, ENABLED)?;
        settings.dictation_shortcut = read(self.window, HOLD)?;
        settings.dictation_toggle_shortcut = read(self.window, TOGGLE)?;
        settings.dictation_language = read(self.window, LANGUAGE)?;
        settings.dictation_refinement_enabled = is_checked(self.window, REFINE)?;
        settings.dictation_streaming_enabled = is_checked(self.window, STREAMING)?;
        settings.dictation_final_wait_milliseconds =
            read(self.window, WAIT)?.parse().map_err(|_| {
                NativeError::InvalidArgument("Final wait must be 500-5000 ms in steps of 500.")
            })?;
        settings.validate()?;
        Ok(settings)
    }
    fn save(&self) -> Result<(), NativeError> {
        let settings = self.values()?;
        self.commands
            .try_send(ClientCommand::SaveSettings(settings))
            .map_err(|_| {
                NativeError::Verification("The client is busy; settings were not queued.")
            })?;
        unsafe {
            let _ = ShowWindow(self.window, SW_HIDE);
        }
        Ok(())
    }
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DPICHANGED && lparam.0 != 0 {
        let bounds = unsafe {
            std::ptr::read_unaligned(lparam.0 as *const windows::Win32::Foundation::RECT)
        };
        if let Err(error) = crate::dpi::suggested_bounds(window, bounds) {
            eprintln!("Settings DPI bounds: {error}");
        }
        match crate::icons::AppIcons::for_dpi(crate::dpi::window_dpi(window)) {
            Ok(icons) => icons.apply(window),
            Err(error) => eprintln!("Settings icon DPI update failed: {error}"),
        }
        SETTINGS.with(|slot| {
            if let Some(ui) = slot.borrow_mut().as_mut() {
                match crate::dpi::DpiFont::create(crate::dpi::window_dpi(window)) {
                    Ok(font) => {
                        if let Err(error) =
                            crate::dpi::scale_children(window, ui.font.dpi, font.dpi)
                        {
                            eprintln!("Settings control scale: {error}");
                        }
                        if let Err(error) = font.apply_children(window) {
                            eprintln!("Settings font update: {error}");
                        }
                        ui.font = font;
                    }
                    Err(error) => eprintln!("Settings DPI font: {error}"),
                }
            }
        });
        return LRESULT(0);
    }
    if message == WM_CLOSE || message == WM_COMMAND && (wparam.0 & 0xffff) == 2 {
        unsafe {
            let _ = ShowWindow(window, SW_HIDE);
        }
        return LRESULT(0);
    }
    if message == WM_COMMAND && matches!(wparam.0 & 0xffff, 1 | 2011) {
        SETTINGS.with(|slot| {
            if let Some(ui) = slot.borrow().as_ref() {
                if let Err(error) = ui.save() {
                    eprintln!("Settings could not be applied: {error}");
                    let text = encoded(&error.to_string());
                    unsafe {
                        let _ = MessageBoxW(
                            Some(window),
                            PCWSTR(text.as_ptr()),
                            w!("Settings need attention"),
                            MB_OK | MB_ICONWARNING,
                        );
                    }
                }
            }
        });
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

pub(crate) fn open(
    owner: HWND,
    commands: mpsc::Sender<ClientCommand>,
    settings: Arc<AppSettings>,
) -> Result<(), NativeError> {
    SETTINGS.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(ui) = slot.as_mut() {
            ui.commands = commands;
            ui.original = settings.as_ref().clone();
            ui.load(&settings)?;
        } else {
            *slot = Some(SettingsUi::create(owner, commands, &settings)?);
        }
        if let Some(ui) = slot.as_ref() {
            unsafe {
                let _ = ShowWindow(ui.window, SW_SHOW);
                let _ = SetForegroundWindow(ui.window);
            }
        }
        Ok(())
    })
}

pub(crate) fn route(message: &MSG) -> bool {
    SETTINGS.with(|slot| {
        slot.borrow().as_ref().is_some_and(|ui| {
            unsafe { IsWindowVisible(ui.window) }.as_bool()
                && (message.hwnd == ui.window
                    || unsafe { IsChild(ui.window, message.hwnd) }.as_bool())
                && unsafe { IsDialogMessageW(ui.window, message) }.as_bool()
        })
    })
}

pub(crate) fn close() {
    let ui = SETTINGS.with(|slot| slot.borrow_mut().take());
    drop(ui);
}

pub(crate) fn self_test(owner: HWND, settings: &AppSettings) -> Result<(), NativeError> {
    let (sender, _receiver) = mpsc::channel(1);
    let previous = unsafe { GetForegroundWindow() };
    let ui = SettingsUi::create(owner, sender, settings)?;
    if ui.values()?.encode()? != settings.encode()?
        || unsafe { IsWindowVisible(ui.window) }.as_bool()
        || unsafe { GetForegroundWindow() } != previous
    {
        return Err(NativeError::Verification(
            "Native Settings controls did not preserve the fixture or hidden focus.",
        ));
    }
    Ok(())
}

impl Drop for SettingsUi {
    fn drop(&mut self) {
        if let Err(error) = unsafe { DestroyWindow(self.window) } {
            eprintln!("Settings window cleanup: {error}");
        }
        if let Err(error) =
            unsafe { UnregisterClassW(w!("VoicePromptRustPilot.Settings"), Some(self.instance)) }
        {
            eprintln!("Settings class cleanup: {error}");
        }
    }
}
