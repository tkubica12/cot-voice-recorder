use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;

use tokio::sync::mpsc;
use voiceprompt_client_protocol::settings::AppSettings;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_WINDOW, DEFAULT_GUI_FONT, GetMonitorInfoW, GetStockObject, GetSysColorBrush,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::NativeError;
use crate::client::ClientCommand;

thread_local! {
    static SETTINGS: RefCell<Option<SettingsUi>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    static PREVIEW_MINIMUM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn preview_minimum(value: bool) {
    PREVIEW_MINIMUM.with(|flag| flag.set(value));
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
const HINT: i32 = 2012;

struct SettingsUi {
    window: HWND,
    instance: HINSTANCE,
    commands: mpsc::Sender<ClientCommand>,
    original: AppSettings,
    font: crate::dpi::DpiFont,
    fields: [HWND; 10],
    labels: [Option<HWND>; 10],
    hint: HWND,
    save_button: HWND,
    scroll: i32,
    wheel_remainder: i32,
    can_save: bool,
}

fn work_area(window: HWND) -> Result<RECT, NativeError> {
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Err(NativeError::Api(
            "Read Settings monitor work area",
            windows::core::Error::from_thread(),
        ));
    }
    if info.rcWork.right <= info.rcWork.left || info.rcWork.bottom <= info.rcWork.top {
        return Err(NativeError::Verification(
            "Settings monitor has no usable work area.",
        ));
    }
    Ok(info.rcWork)
}

fn fit_bounds(bounds: RECT, work: RECT) -> RECT {
    let width = (bounds.right - bounds.left).min(work.right - work.left);
    let height = (bounds.bottom - bounds.top).min(work.bottom - work.top);
    let left = bounds.left.clamp(work.left, work.right - width);
    let top = bounds.top.clamp(work.top, work.bottom - height);
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
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
        can_save: bool,
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
                if can_save {
                    w!("VoicePrompt - Settings")
                } else {
                    w!("Isolated Rust Settings - no save or physical ports")
                },
                WS_OVERLAPPEDWINDOW | WS_VSCROLL | WS_CLIPCHILDREN,
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
        let mut ui = Self {
            window,
            instance,
            commands,
            original: settings.clone(),
            font,
            fields: [HWND::default(); 10],
            labels: [None; 10],
            hint: HWND::default(),
            save_button: HWND::default(),
            scroll: 0,
            wheel_remainder: 0,
            can_save,
        };
        let dpi = ui.font.dpi as i32;
        let scale = |value: i32| value * dpi / 96;
        for (index, (id, label, class, style)) in [
            (URL, "Backend URL", w!("EDIT"), ES_AUTOHSCROLL as u32),
            (
                PAUSED,
                "Pause automatic copy and notifications",
                w!("BUTTON"),
                BS_AUTOCHECKBOX as u32,
            ),
            (
                AUTO_START,
                "Start automatically at Windows sign-in",
                w!("BUTTON"),
                BS_AUTOCHECKBOX as u32,
            ),
            (
                ENABLED,
                "Enable dictation shortcuts",
                w!("BUTTON"),
                BS_AUTOCHECKBOX as u32,
            ),
            (
                HOLD,
                "Hold-to-talk shortcut",
                w!("EDIT"),
                ES_AUTOHSCROLL as u32,
            ),
            (
                TOGGLE,
                "Hands-free shortcut",
                w!("EDIT"),
                ES_AUTOHSCROLL as u32,
            ),
            (
                LANGUAGE,
                "Language: auto / cs / en",
                w!("EDIT"),
                ES_AUTOHSCROLL as u32,
            ),
            (
                REFINE,
                "Polish dictation with LLM",
                w!("BUTTON"),
                BS_AUTOCHECKBOX as u32,
            ),
            (
                STREAMING,
                "Use live MAI streaming",
                w!("BUTTON"),
                BS_AUTOCHECKBOX as u32,
            ),
            (
                WAIT,
                "Final cleanup wait (500-5000 ms, step 500)",
                w!("EDIT"),
                ES_AUTOHSCROLL as u32,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let is_edit = matches!(id, URL | HOLD | TOGGLE | LANGUAGE | WAIT);
            if is_edit != crate::settings_layout::EDIT_FIELDS[index] {
                return Err(NativeError::Verification(
                    "Settings field role does not match its layout.",
                ));
            }
            if is_edit {
                let label_text = encoded(label);
                let child = native("Create Settings label", unsafe {
                    CreateWindowExW(
                        Default::default(),
                        w!("STATIC"),
                        PCWSTR(label_text.as_ptr()),
                        WS_CHILD | WS_VISIBLE,
                        0,
                        0,
                        1,
                        1,
                        Some(window),
                        None,
                        Some(instance),
                        None,
                    )
                })?;
                ui.labels[index] = Some(child);
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
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | WINDOW_STYLE(style | if is_edit { 0 } else { BS_MULTILINE as u32 }),
                    0,
                    0,
                    1,
                    1,
                    Some(window),
                    Some(HMENU(id as *mut c_void)),
                    Some(instance),
                    None,
                )
            })?;
            ui.fields[index] = child;
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
        ui.hint = native("Create Settings guidance", unsafe {
            CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                PCWSTR(hint.as_ptr()),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                1,
                1,
                Some(window),
                Some(HMENU(HINT as *mut c_void)),
                Some(instance),
                None,
            )
        })?;
        ui.save_button = native("Create Settings save button", unsafe {
            CreateWindowExW(
                Default::default(),
                w!("BUTTON"),
                w!("Save Settings"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                0,
                0,
                1,
                1,
                Some(window),
                Some(HMENU(SAVE as *mut c_void)),
                Some(instance),
                None,
            )
        })?;
        ui.load(settings)?;
        unsafe {
            let _ = EnableWindow(ui.save_button, can_save);
        }
        ui.font.apply_children(window)?;
        crate::icons::AppIcons::for_dpi(ui.font.dpi)?.apply(window);
        let work = work_area(owner)?;
        #[cfg(test)]
        let desired = if !can_save && PREVIEW_MINIMUM.with(std::cell::Cell::get) {
            (340, 260)
        } else {
            (570, 550)
        };
        #[cfg(not(test))]
        let desired = (570, 550);
        let width = scale(desired.0).min(work.right - work.left);
        let height = scale(desired.1).min(work.bottom - work.top);
        crate::dpi::suggested_bounds(
            window,
            RECT {
                left: work.left + (work.right - work.left - width) / 2,
                top: work.top + (work.bottom - work.top - height) / 2,
                right: work.left + (work.right - work.left + width) / 2,
                bottom: work.top + (work.bottom - work.top + height) / 2,
            },
        )?;
        ui.relayout()?;
        Ok(ui)
    }

    fn geometry(&self) -> Result<crate::settings_layout::SettingsLayout, NativeError> {
        let mut bounds = RECT::default();
        native("Read Settings viewport", unsafe {
            GetClientRect(self.window, &mut bounds)
        })?;
        crate::settings_layout::SettingsLayout::calculate(
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            crate::dpi::window_dpi(self.window),
        )
        .map_err(NativeError::Verification)
    }

    fn relayout(&mut self) -> Result<(), NativeError> {
        let layout = self.geometry()?;
        self.scroll = layout.clamp_scroll(self.scroll);
        let scroll = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS | SIF_DISABLENOSCROLL,
            nMin: 0,
            nMax: layout.content_height - 1,
            nPage: layout.viewport_height as u32,
            nPos: self.scroll,
            ..Default::default()
        };
        // SetScrollInfo returns the prior/current position, so zero is not an error code.
        unsafe {
            let _ = SetScrollInfo(self.window, SB_VERT, &scroll, true);
        }
        let mut actual = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_POS | SIF_RANGE | SIF_PAGE,
            ..Default::default()
        };
        native("Verify Settings scroll position", unsafe {
            GetScrollInfo(self.window, SB_VERT, &mut actual)
        })?;
        if actual.nPos != self.scroll
            || actual.nMin != 0
            || actual.nMax != scroll.nMax
            || actual.nPage != scroll.nPage.min(layout.content_height as u32)
        {
            return Err(NativeError::Verification(
                "Settings scrollbar did not accept its bounded position.",
            ));
        }
        for (window, rect) in self
            .fields
            .into_iter()
            .zip(layout.fields)
            .chain(
                self.labels
                    .into_iter()
                    .zip(layout.labels)
                    .filter_map(|(window, rect)| window.zip(rect)),
            )
            .chain([(self.hint, layout.hint), (self.save_button, layout.save)])
        {
            native("Position responsive Settings control", unsafe {
                MoveWindow(
                    window,
                    rect.x,
                    rect.y - self.scroll,
                    rect.width,
                    rect.height,
                    true,
                )
            })?;
        }
        if !unsafe { windows::Win32::Graphics::Gdi::InvalidateRect(Some(self.window), None, true) }
            .as_bool()
        {
            return Err(NativeError::Api(
                "Invalidate Settings viewport",
                windows::core::Error::from_thread(),
            ));
        }
        Ok(())
    }

    fn scroll_to(&mut self, position: i32) -> Result<(), NativeError> {
        let position = self.geometry()?.clamp_scroll(position);
        if position != self.scroll {
            self.scroll = position;
            self.relayout()?;
        }
        Ok(())
    }

    fn reveal_focus(&mut self) -> Result<(), NativeError> {
        let focus = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() };
        if !self.fields.contains(&focus) && focus != self.save_button {
            return Ok(());
        }
        let layout = self.geometry()?;
        let bounds = self
            .fields
            .iter()
            .position(|field| *field == focus)
            .map_or(layout.save, |index| layout.fields[index]);
        self.scroll_to(layout.reveal(self.scroll, bounds))
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
        if !self.can_save {
            return Err(NativeError::Verification(
                "Validation fixture cannot save Settings.",
            ));
        }
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
    if message == WM_GETMINMAXINFO && lparam.0 != 0 {
        let mut bounds = unsafe { std::ptr::read_unaligned(lparam.0 as *const MINMAXINFO) };
        let dpi = crate::dpi::window_dpi(window) as i32;
        bounds.ptMinTrackSize.x = 340 * dpi / 96;
        bounds.ptMinTrackSize.y = 220 * dpi / 96;
        unsafe {
            std::ptr::write_unaligned(lparam.0 as *mut MINMAXINFO, bounds);
        }
        return LRESULT(0);
    }
    if message == WM_SIZE {
        if wparam.0 != SIZE_MINIMIZED as usize {
            update_viewport(window, |ui| {
                ui.relayout()?;
                ui.reveal_focus()
            });
        }
        return LRESULT(0);
    }
    if message == WM_VSCROLL {
        update_viewport(window, |ui| {
            let layout = ui.geometry()?;
            let line = (24 * crate::dpi::window_dpi(window) as i32 + 48) / 96;
            let page = (layout.viewport_height - line).max(line);
            let command = SCROLLBAR_COMMAND((wparam.0 & 0xffff) as i32);
            let position = match command {
                SB_LINEUP => ui.scroll.saturating_sub(line),
                SB_LINEDOWN => ui.scroll.saturating_add(line),
                SB_PAGEUP => ui.scroll.saturating_sub(page),
                SB_PAGEDOWN => ui.scroll.saturating_add(page),
                SB_TOP => 0,
                SB_BOTTOM => layout.maximum_scroll(),
                SB_THUMBPOSITION | SB_THUMBTRACK => {
                    let mut info = SCROLLINFO {
                        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                        fMask: SIF_TRACKPOS,
                        ..Default::default()
                    };
                    native("Read Settings scrollbar thumb", unsafe {
                        GetScrollInfo(window, SB_VERT, &mut info)
                    })?;
                    info.nTrackPos
                }
                _ => ui.scroll,
            };
            ui.scroll_to(position)
        });
        return LRESULT(0);
    }
    if message == WM_MOUSEWHEEL {
        update_viewport(window, |ui| {
            ui.wheel_remainder += ((wparam.0 >> 16) as u16 as i16) as i32;
            let notches = ui.wheel_remainder / 120;
            ui.wheel_remainder %= 120;
            let mut lines = 0u32;
            native("Read user wheel scrolling preference", unsafe {
                SystemParametersInfoW(
                    SPI_GETWHEELSCROLLLINES,
                    0,
                    Some((&mut lines as *mut u32).cast()),
                    Default::default(),
                )
            })?;
            let layout = ui.geometry()?;
            let step = if lines == u32::MAX {
                i64::from(layout.viewport_height)
            } else {
                i64::from((24 * crate::dpi::window_dpi(window) as i32 + 48) / 96) * i64::from(lines)
            };
            let position = (i64::from(ui.scroll) - i64::from(notches) * step)
                .clamp(0, i64::from(layout.maximum_scroll()));
            ui.scroll_to(i32::try_from(position).map_err(|_| {
                NativeError::Verification("Settings scroll position is outside supported bounds.")
            })?)
        });
        return LRESULT(0);
    }
    if message == WM_DPICHANGED && lparam.0 != 0 {
        let bounds = unsafe {
            std::ptr::read_unaligned(lparam.0 as *const windows::Win32::Foundation::RECT)
        };
        update_viewport(window, |ui| {
            let font = crate::dpi::DpiFont::create(crate::dpi::window_dpi(window))?;
            ui.scroll = i32::try_from(
                (i64::from(ui.scroll) * i64::from(font.dpi) + i64::from(ui.font.dpi) / 2)
                    / i64::from(ui.font.dpi),
            ).map_err(|_| NativeError::Verification("Settings DPI scroll offset is outside supported bounds."))?;
            font.apply_children(window)?;
            crate::icons::AppIcons::for_dpi(font.dpi)?.apply(window);
            ui.font = font;
            crate::dpi::suggested_bounds(window, fit_bounds(bounds, work_area(window)?))?;
            ui.relayout()?;
            ui.reveal_focus()
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

fn update_viewport(window: HWND, update: impl FnOnce(&mut SettingsUi) -> Result<(), NativeError>) {
    let result = SETTINGS.with(|slot| {
        // Synchronous Create/SetWindowPos messages precede storage or reenter the explicit layout.
        let Ok(mut slot) = slot.try_borrow_mut() else {
            return Ok(());
        };
        let Some(ui) = slot.as_mut() else {
            return Ok(());
        };
        if ui.window != window {
            return Err(NativeError::Verification(
                "Settings viewport belongs to another window.",
            ));
        }
        update(ui)
    });
    if let Err(error) = result {
        eprintln!("Settings viewport needs attention: {error}");
        if let Err(error) = set(
            window,
            HINT,
            "Settings layout could not be updated. Close and reopen Settings; nothing was saved.",
        ) {
            eprintln!("Settings viewport error could not be displayed: {error}");
        }
    }
}

pub(crate) fn open(
    owner: HWND,
    commands: mpsc::Sender<ClientCommand>,
    settings: Arc<AppSettings>,
    can_save: bool,
) -> Result<(), NativeError> {
    SETTINGS.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(ui) = slot.as_mut() {
            ui.commands = commands;
            ui.original = settings.as_ref().clone();
            ui.can_save = can_save;
            unsafe {
                let _ = EnableWindow(ui.save_button, can_save);
            }
            ui.load(&settings)?;
        } else {
            *slot = Some(SettingsUi::create(owner, commands, &settings, can_save)?);
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
    let window = SETTINGS.with(|slot| slot.borrow().as_ref().map(|ui| ui.window));
    let Some(window) = window else {
        return false;
    };
    if !unsafe { IsWindowVisible(window) }.as_bool()
        || !(message.hwnd == window || unsafe { IsChild(window, message.hwnd) }.as_bool())
    {
        return false;
    }
    let handled = unsafe { IsDialogMessageW(window, message) }.as_bool();
    if handled {
        update_viewport(window, SettingsUi::reveal_focus);
    }
    handled
}

pub(crate) fn close() {
    let ui = SETTINGS.with(|slot| slot.borrow_mut().take());
    drop(ui);
}

pub(crate) fn self_test(owner: HWND, settings: &AppSettings) -> Result<(), NativeError> {
    let (sender, _receiver) = mpsc::channel(1);
    let previous = unsafe { GetForegroundWindow() };
    let mut ui = SettingsUi::create(owner, sender, settings, false)?;
    let dpi = crate::dpi::window_dpi(ui.window) as i32;
    for (width, height) in [(570, 550), (340, 260)] {
        native("Resize hidden Settings viewport", unsafe {
            SetWindowPos(
                ui.window,
                None,
                0,
                0,
                width * dpi / 96,
                height * dpi / 96,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        })?;
        ui.relayout()?;
        let layout = ui.geometry()?;
        for (field, expected) in ui
            .fields
            .into_iter()
            .zip(layout.fields)
            .chain([(ui.save_button, layout.save)])
        {
            ui.scroll_to(layout.reveal(0, expected))?;
            let mut bounds = RECT::default();
            native("Inspect reachable hidden Settings field", unsafe {
                GetWindowRect(field, &mut bounds)
            })?;
            let mut origin = windows::Win32::Foundation::POINT {
                x: bounds.left,
                y: bounds.top,
            };
            if !unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(ui.window, &mut origin) }
                .as_bool()
                || origin.x != expected.x
                || origin.y != expected.y - ui.scroll
                || origin.y < 0
                || origin.y + bounds.bottom - bounds.top > layout.viewport_height
            {
                return Err(NativeError::Verification(
                    "A Settings field could not be revealed in its viewport.",
                ));
            }
        }
        ui.scroll_to(i32::MAX)?;
        if ui.scroll != layout.maximum_scroll() {
            return Err(NativeError::Verification(
                "Settings could not reach its final action.",
            ));
        }
        let mut save = RECT::default();
        native("Inspect scrolled Settings action", unsafe {
            GetWindowRect(ui.save_button, &mut save)
        })?;
        let mut point = windows::Win32::Foundation::POINT {
            x: save.left,
            y: save.top,
        };
        if !unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(ui.window, &mut point) }
            .as_bool()
            || point.y < 0
            || point.y + save.bottom - save.top > layout.viewport_height
        {
            return Err(NativeError::Verification(
                "Settings final action remained outside its viewport.",
            ));
        }
        ui.scroll_to(0)?;
        if ui.values()?.encode()? != settings.encode()? {
            return Err(NativeError::Verification(
                "Settings scrolling changed its fields.",
            ));
        }
    }
    if ui.save().is_ok() {
        return Err(NativeError::Verification(
            "Validation Settings allowed a save command.",
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_fitting_handles_negative_coordinates_and_small_high_dpi_work_areas() {
        for work in [
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1040,
            },
            RECT {
                left: -1280,
                top: -900,
                right: 0,
                bottom: -180,
            },
            RECT {
                left: 1920,
                top: 0,
                right: 2520,
                bottom: 400,
            },
        ] {
            for bounds in [
                RECT {
                    left: work.left - 500,
                    top: work.top - 500,
                    right: work.left + 1200,
                    bottom: work.top + 1200,
                },
                RECT {
                    left: work.right - 100,
                    top: work.bottom - 100,
                    right: work.right + 600,
                    bottom: work.bottom + 600,
                },
            ] {
                let fit = fit_bounds(bounds, work);
                assert!(fit.left >= work.left && fit.top >= work.top);
                assert!(fit.right <= work.right && fit.bottom <= work.bottom);
                assert!(fit.right > fit.left && fit.bottom > fit.top);
            }
        }
    }
}
