use crate::NativeError;
use std::mem::size_of;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, DeleteObject, HFONT};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SWP_NOACTIVATE, SWP_NOZORDER,
    SendMessageW, SetWindowPos, WM_SETFONT,
};

#[cfg(test)]
#[path = "dpi_tests.rs"]
mod tests;

pub(crate) struct DpiFont {
    handle: HFONT,
    pub dpi: u32,
}

impl DpiFont {
    pub fn create(dpi: u32) -> Result<Self, NativeError> {
        let mut metrics = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        unsafe {
            SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                size_of::<NONCLIENTMETRICSW>() as u32,
                Some((&mut metrics as *mut NONCLIENTMETRICSW).cast()),
                0,
                dpi,
            )
        }
        .map_err(|error| NativeError::Api("Read DPI-specific system UI font", error))?;
        let font = unsafe { CreateFontIndirectW(&metrics.lfMessageFont) };
        if font.0.is_null() {
            return Err(NativeError::Api(
                "Create DPI-aware UI font",
                windows::core::Error::from_thread(),
            ));
        }
        Ok(Self { handle: font, dpi })
    }
    pub fn apply_children(&self, window: HWND) -> Result<(), NativeError> {
        unsafe extern "system" fn apply(window: HWND, parameter: LPARAM) -> windows::core::BOOL {
            unsafe {
                let _ = SendMessageW(
                    window,
                    WM_SETFONT,
                    Some(WPARAM(parameter.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            true.into()
        }
        // EnumChildWindows is synchronous; its return value is not meaningful per Win32 contract.
        unsafe {
            let _ = EnumChildWindows(Some(window), Some(apply), LPARAM(self.handle.0 as isize));
        }
        Ok(())
    }
}
impl Drop for DpiFont {
    fn drop(&mut self) {
        if !unsafe { DeleteObject(self.handle.into()) }.as_bool() {
            eprintln!("DPI UI font cleanup failed.");
        }
    }
}

pub(crate) fn window_dpi(window: HWND) -> u32 {
    unsafe { GetDpiForWindow(window) }.max(96)
}

pub(crate) fn suggested_bounds(window: HWND, bounds: RECT) -> Result<(), NativeError> {
    unsafe {
        SetWindowPos(
            window,
            None,
            bounds.left,
            bounds.top,
            (bounds.right - bounds.left).max(1),
            (bounds.bottom - bounds.top).max(1),
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
    }
    .map_err(|error| NativeError::Api("Apply per-monitor DPI bounds", error))
}

pub(crate) fn scale_children(window: HWND, old_dpi: u32, new_dpi: u32) -> Result<(), NativeError> {
    struct Layout {
        window: HWND,
        old: u32,
        new: u32,
        error: Option<NativeError>,
    }
    unsafe extern "system" fn resize(child: HWND, parameter: LPARAM) -> windows::core::BOOL {
        let layout = unsafe { &mut *(parameter.0 as *mut Layout) };
        let mut bounds = RECT::default();
        let result = (|| {
            unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(child, &mut bounds) }
                .map_err(|error| NativeError::Api("Inspect DPI child bounds", error))?;
            let mut origin = windows::Win32::Foundation::POINT {
                x: bounds.left,
                y: bounds.top,
            };
            if !unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(layout.window, &mut origin) }
                .as_bool()
            {
                return Err(NativeError::Api(
                    "Map DPI control coordinates",
                    windows::core::Error::from_thread(),
                ));
            }
            let scale = |value: i32| {
                (i64::from(value) * i64::from(layout.new) / i64::from(layout.old)) as i32
            };
            unsafe {
                SetWindowPos(
                    child,
                    None,
                    scale(origin.x),
                    scale(origin.y),
                    scale(bounds.right - bounds.left).max(1),
                    scale(bounds.bottom - bounds.top).max(1),
                    SWP_NOACTIVATE | SWP_NOZORDER,
                )
            }
            .map_err(|error| NativeError::Api("Scale native child controls", error))
        })();
        if let Err(error) = result {
            layout.error = Some(error);
            return false.into();
        }
        true.into()
    }
    let mut layout = Layout {
        window,
        old: old_dpi.max(1),
        new: new_dpi,
        error: None,
    };
    unsafe {
        let _ = EnumChildWindows(
            Some(window),
            Some(resize),
            LPARAM((&mut layout as *mut Layout) as isize),
        );
    }
    match layout.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
