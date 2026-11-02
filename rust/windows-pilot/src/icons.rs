use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_SHARED, LoadImageW, SM_CXICON, SM_CXSMICON,
    SM_CYICON, SM_CYSMICON, SendMessageW, WM_SETICON,
};
use windows::core::PCWSTR;

use crate::NativeError;

pub(crate) struct AppIcons {
    pub small: HICON,
    pub large: HICON,
}

fn load(width: i32, height: i32) -> Result<HICON, NativeError> {
    let module = unsafe { GetModuleHandleW(None) }
        .map_err(|error| NativeError::Api("Get original icon resource module", error))?;
    let icon = unsafe {
        LoadImageW(
            Some(module.into()),
            PCWSTR(std::ptr::without_provenance(1)),
            IMAGE_ICON,
            width,
            height,
            LR_SHARED,
        )
    }
    .map_err(|error| NativeError::Api("Load original VoicePrompt icon resource", error))?;
    // LR_SHARED icons are owned by the resource module, not by a window.
    Ok(HICON(icon.0))
}

impl AppIcons {
    pub(crate) fn for_dpi(dpi: u32) -> Result<Self, NativeError> {
        if !(48..=768).contains(&dpi) {
            return Err(NativeError::InvalidArgument(
                "Unsupported application icon DPI.",
            ));
        }
        let metric = |name| unsafe { GetSystemMetricsForDpi(name, dpi) };
        Ok(Self {
            small: load(metric(SM_CXSMICON), metric(SM_CYSMICON))?,
            large: load(metric(SM_CXICON), metric(SM_CYICON))?,
        })
    }

    pub(crate) fn apply(&self, window: HWND) {
        unsafe {
            let _ = SendMessageW(
                window,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(self.small.0 as isize)),
            );
            let _ = SendMessageW(
                window,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(self.large.0 as isize)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WM_GETICON, WS_POPUP,
    };
    use windows::core::w;

    #[test]
    fn original_icons_bind_both_window_sizes_at_each_supported_scale() {
        let foreground = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Synthetic icon binding"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
        }
        .unwrap();
        for dpi in [96, 120, 144, 192, 288] {
            let icons = AppIcons::for_dpi(dpi).unwrap();
            icons.apply(window);
            for (size, expected) in [(ICON_SMALL, icons.small), (ICON_BIG, icons.large)] {
                let actual =
                    unsafe { SendMessageW(window, WM_GETICON, Some(WPARAM(size as usize)), None) };
                assert_eq!(actual.0, expected.0 as isize, "DPI {dpi}, size {size}");
            }
        }
        assert!(AppIcons::for_dpi(0).is_err());
        assert!(AppIcons::for_dpi(769).is_err());
        assert_eq!(
            unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() },
            foreground
        );
        unsafe { DestroyWindow(window) }.unwrap();
    }
}
