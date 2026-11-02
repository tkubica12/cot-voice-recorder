use super::*;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{GetObjectW, LOGFONTW, ScreenToClient};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetForegroundWindow, GetWindowRect, IsWindowVisible,
    WINDOW_EX_STYLE, WM_GETFONT, WS_CHILD, WS_POPUP, WS_TABSTOP,
};
use windows::core::w;

struct HiddenWindow(HWND);
impl Drop for HiddenWindow {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) }.unwrap();
    }
}

fn hidden() -> HiddenWindow {
    HiddenWindow(
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Synthetic non-activating DPI fixture"),
                WS_POPUP,
                0,
                0,
                800,
                600,
                None,
                None,
                None,
                None,
            )
        }
        .unwrap(),
    )
}

fn child_bounds(parent: HWND, child: HWND) -> (i32, i32, i32, i32) {
    let mut bounds = RECT::default();
    unsafe { GetWindowRect(child, &mut bounds) }.unwrap();
    let mut origin = POINT {
        x: bounds.left,
        y: bounds.top,
    };
    assert!(unsafe { ScreenToClient(parent, &mut origin) }.as_bool());
    (
        origin.x,
        origin.y,
        bounds.right - bounds.left,
        bounds.bottom - bounds.top,
    )
}

#[test]
fn flat_native_controls_scale_96_144_192_and_back_without_becoming_visible_or_active() {
    let parent = hidden();
    let child = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!("synthetic"),
            WS_CHILD | WS_TABSTOP,
            12,
            24,
            120,
            32,
            Some(parent.0),
            None,
            None,
            None,
        )
    }
    .unwrap();
    assert_eq!(child_bounds(parent.0, child), (12, 24, 120, 32));
    scale_children(parent.0, 96, 144).unwrap();
    assert_eq!(child_bounds(parent.0, child), (18, 36, 180, 48));
    scale_children(parent.0, 144, 192).unwrap();
    assert_eq!(child_bounds(parent.0, child), (24, 48, 240, 64));
    scale_children(parent.0, 192, 96).unwrap();
    assert_eq!(child_bounds(parent.0, child), (12, 24, 120, 32));
    suggested_bounds(
        parent.0,
        RECT {
            left: 20,
            top: 40,
            right: 820,
            bottom: 640,
        },
    )
    .unwrap();
    assert!(!unsafe { IsWindowVisible(parent.0) }.as_bool());
    assert!(!unsafe { IsWindowVisible(child) }.as_bool());
    let foreground = unsafe { GetForegroundWindow() };
    assert_ne!(foreground, parent.0);
    assert_ne!(foreground, child);
}

#[test]
fn actual_system_font_sizes_and_child_bindings_follow_explicit_dpi_without_os_setting_changes() {
    let parent = hidden();
    let children: Vec<_> = [w!("STATIC"), w!("EDIT"), w!("BUTTON")]
        .into_iter()
        .map(|class| {
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class,
                    w!("synthetic"),
                    WS_CHILD | WS_TABSTOP,
                    12,
                    24,
                    120,
                    32,
                    Some(parent.0),
                    None,
                    None,
                    None,
                )
            }
            .unwrap()
        })
        .collect();
    let mut heights = Vec::new();
    let fonts: Vec<_> = [96, 144, 192]
        .into_iter()
        .map(|dpi| DpiFont::create(dpi).unwrap())
        .collect();
    for font in &fonts {
        let mut description = LOGFONTW::default();
        assert_eq!(
            unsafe {
                GetObjectW(
                    font.handle.into(),
                    size_of::<LOGFONTW>() as i32,
                    Some((&mut description as *mut LOGFONTW).cast()),
                )
            },
            size_of::<LOGFONTW>() as i32
        );
        heights.push(description.lfHeight.abs());
        font.apply_children(parent.0).unwrap();
        for child in &children {
            assert_eq!(
                unsafe { SendMessageW(*child, WM_GETFONT, None, None) }.0,
                font.handle.0 as isize
            );
        }
    }
    assert!(heights[0] > 0 && heights[1] > heights[0] && heights[2] > heights[1]);
    assert!((heights[1] * 2 - heights[0] * 3).abs() <= 2);
    assert!((heights[2] - heights[0] * 2).abs() <= 1);
    drop(parent);
    drop(fonts);
}
