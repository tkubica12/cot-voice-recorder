use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect, WindowFromPoint};

use crate::NativeError;
use crate::dpi::DpiFont;

#[cfg(test)]
#[path = "main_theme_tests.rs"]
mod tests;

pub(crate) const INK: COLORREF = COLORREF(0x00111111);
pub(crate) const GRAY: COLORREF = COLORREF(0x0080726b);
pub(crate) const LINE: COLORREF = COLORREF(0x00ebe7e5);
pub(crate) const SURFACE: COLORREF = COLORREF(0x00f8f7f7);
pub(crate) const WHITE: COLORREF = COLORREF(0x00ffffff);
pub(crate) const ACCENT: COLORREF = COLORREF(0x001673f9);

struct Brush(HBRUSH);
impl Brush {
    fn new(color: COLORREF) -> Result<Self, NativeError> {
        let handle = unsafe { CreateSolidBrush(color) };
        if handle.0.is_null() {
            return Err(NativeError::Api(
                "Create original-theme brush",
                windows::core::Error::from_thread(),
            ));
        }
        Ok(Self(handle))
    }
}
impl Drop for Brush {
    fn drop(&mut self) {
        if !unsafe { DeleteObject(self.0.into()) }.as_bool() {
            eprintln!("Original-theme brush cleanup failed.");
        }
    }
}

struct Pen(HPEN);
impl Pen {
    fn new(color: COLORREF, width: i32) -> Result<Self, NativeError> {
        let handle = unsafe { CreatePen(PS_SOLID, width, color) };
        if handle.0.is_null() {
            return Err(NativeError::Api(
                "Create original-theme border",
                windows::core::Error::from_thread(),
            ));
        }
        Ok(Self(handle))
    }
}
impl Drop for Pen {
    fn drop(&mut self) {
        if !unsafe { DeleteObject(self.0.into()) }.as_bool() {
            eprintln!("Original-theme border cleanup failed.");
        }
    }
}

struct SavedDc(HDC, i32);
impl SavedDc {
    fn new(dc: HDC) -> Result<Self, NativeError> {
        let saved = unsafe { SaveDC(dc) };
        if saved == 0 {
            return Err(NativeError::Api(
                "Save original-theme drawing state",
                windows::core::Error::from_thread(),
            ));
        }
        Ok(Self(dc, saved))
    }
}
impl Drop for SavedDc {
    fn drop(&mut self) {
        if !unsafe { RestoreDC(self.0, self.1) }.as_bool() {
            eprintln!("Original-theme drawing-state restore failed.");
        }
    }
}

pub(crate) struct MainTheme {
    pub dpi: u32,
    pub base: DpiFont,
    pub caption: DpiFont,
    pub section_caption: DpiFont,
    pub title: DpiFont,
    pub strong: DpiFont,
    white: Brush,
    surface: Brush,
    accent: Brush,
    line: Pen,
    accent_line: Pen,
}

impl MainTheme {
    pub fn new(dpi: u32) -> Result<Self, NativeError> {
        Ok(Self {
            dpi,
            base: DpiFont::sized(dpi, 13, 400)?,
            caption: DpiFont::sized(dpi, 11, 400)?,
            section_caption: DpiFont::sized(dpi, 11, 600)?,
            title: DpiFont::sized(dpi, 17, 600)?,
            strong: DpiFont::sized(dpi, 13, 600)?,
            white: Brush::new(WHITE)?,
            surface: Brush::new(SURFACE)?,
            accent: Brush::new(ACCENT)?,
            line: Pen::new(LINE, ((dpi + 48) / 96).max(1) as i32)?,
            accent_line: Pen::new(ACCENT, ((2 * dpi + 48) / 96).max(1) as i32)?,
        })
    }

    fn scale(&self, value: i32) -> i32 {
        (value * self.dpi as i32 + 48) / 96
    }

    pub fn static_colors(&self, dc: HDC, caption: bool, surface: bool) -> HBRUSH {
        unsafe {
            let _ = SetTextColor(dc, if caption { GRAY } else { INK });
            let _ = SetBkColor(dc, if surface { SURFACE } else { WHITE });
            let _ = SetBkMode(dc, TRANSPARENT);
        }

        if surface {
            self.surface.0
        } else {
            self.white.0
        }
    }

    pub fn canvas(&self, dc: HDC, rect: RECT) -> Result<(), NativeError> {
        self.fill(dc, rect, &self.white)
    }

    fn text(
        &self,
        dc: HDC,
        font: &DpiFont,
        text: &str,
        mut rect: RECT,
        color: COLORREF,
        flags: DRAW_TEXT_FORMAT,
    ) -> Result<(), NativeError> {
        if text.is_empty() {
            return Ok(());
        }
        let mut encoded: Vec<_> = text.encode_utf16().collect();
        unsafe {
            let _ = SelectObject(dc, font.handle().into());
            let _ = SetTextColor(dc, color);
            let _ = SetBkMode(dc, TRANSPARENT);
        }
        if unsafe { DrawTextW(dc, &mut encoded, &mut rect, flags) } == 0 {
            return Err(NativeError::Api(
                "Draw original-theme text",
                windows::core::Error::from_thread(),
            ));
        }
        Ok(())
    }

    fn fill(&self, dc: HDC, rect: RECT, brush: &Brush) -> Result<(), NativeError> {
        if unsafe { FillRect(dc, &rect, brush.0) } == 0 {
            return Err(NativeError::Api(
                "Fill original-theme surface",
                windows::core::Error::from_thread(),
            ));
        }
        Ok(())
    }

    pub fn status_card(&self, dc: HDC, rect: RECT) -> Result<(), NativeError> {
        let _saved = SavedDc::new(dc)?;
        unsafe {
            let _ = SelectObject(dc, self.surface.0.into());
            let _ = SelectObject(dc, self.line.0.into());
        }
        if !unsafe {
            RoundRect(
                dc,
                rect.left,
                rect.top,
                rect.right,
                rect.bottom,
                self.scale(8),
                self.scale(8),
            )
        }
        .as_bool()
        {
            return Err(NativeError::Api(
                "Draw original status card",
                windows::core::Error::from_thread(),
            ));
        }
        Ok(())
    }

    pub fn list_frame(&self, dc: HDC, rect: RECT) -> Result<(), NativeError> {
        let _saved = SavedDc::new(dc)?;
        unsafe {
            let _ = SelectObject(dc, GetStockObject(NULL_BRUSH));
            let _ = SelectObject(dc, self.line.0.into());
        }
        if !unsafe { Rectangle(dc, rect.left, rect.top, rect.right, rect.bottom) }.as_bool() {
            return Err(NativeError::Api(
                "Draw original flat list border",
                windows::core::Error::from_thread(),
            ));
        }
        Ok(())
    }

    pub fn button(
        &self,
        item: &DRAWITEMSTRUCT,
        text: &str,
        accent: bool,
        section_action: bool,
    ) -> Result<(), NativeError> {
        let _saved = SavedDc::new(item.hDC)?;
        let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
        let focused = item.itemState.0 & ODS_FOCUS.0 != 0;
        let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
        let hot = hovered(item.hwndItem);
        let brush = if accent && !disabled && !pressed && !hot {
            &self.accent
        } else if pressed || hot {
            &self.surface
        } else {
            &self.white
        };
        let pen = if focused || (accent && !disabled) {
            &self.accent_line
        } else {
            &self.line
        };
        self.fill(item.hDC, item.rcItem, &self.white)?;
        unsafe {
            let _ = SelectObject(item.hDC, brush.0.into());
            let _ = SelectObject(item.hDC, pen.0.into());
        }
        let mut bounds = item.rcItem;
        bounds.right -= 1;
        bounds.bottom -= 1;
        if !unsafe {
            RoundRect(
                item.hDC,
                bounds.left,
                bounds.top,
                bounds.right,
                bounds.bottom,
                self.scale(8),
                self.scale(8),
            )
        }
        .as_bool()
        {
            return Err(NativeError::Api(
                "Draw original-theme button",
                windows::core::Error::from_thread(),
            ));
        }
        let color = if disabled {
            GRAY
        } else if accent && !pressed && !hot {
            WHITE
        } else {
            INK
        };
        self.text(
            item.hDC,
            if section_action {
                &self.strong
            } else {
                &self.base
            },
            text,
            bounds,
            color,
            DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_HIDEPREFIX,
        )?;
        Ok(())
    }

    pub fn tab(&self, item: &DRAWITEMSTRUCT, text: &str) -> Result<(), NativeError> {
        let _saved = SavedDc::new(item.hDC)?;
        let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
        let focused = item.itemState.0 & ODS_FOCUS.0 != 0;
        self.fill(
            item.hDC,
            item.rcItem,
            if focused || hovered_rect(item.hwndItem, item.rcItem) {
                &self.surface
            } else {
                &self.white
            },
        )?;
        if selected {
            self.fill(
                item.hDC,
                RECT {
                    top: item.rcItem.bottom - self.scale(2),
                    ..item.rcItem
                },
                &self.accent,
            )?;
        }
        self.text(
            item.hDC,
            if selected { &self.strong } else { &self.base },
            text,
            item.rcItem,
            if selected { INK } else { GRAY },
            DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX,
        )
    }

    pub fn history_row(
        &self,
        item: &DRAWITEMSTRUCT,
        preview: &str,
        metadata: &str,
    ) -> Result<(), NativeError> {
        let _saved = SavedDc::new(item.hDC)?;
        let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
        self.fill(
            item.hDC,
            item.rcItem,
            if selected || hovered_rect(item.hwndItem, item.rcItem) {
                &self.surface
            } else {
                &self.white
            },
        )?;
        if selected {
            self.fill(
                item.hDC,
                RECT {
                    right: item.rcItem.left + self.scale(3),
                    ..item.rcItem
                },
                &self.accent,
            )?;
            self.fill(
                item.hDC,
                RECT {
                    top: item.rcItem.bottom - self.scale(1),
                    ..item.rcItem
                },
                &self.accent,
            )?;
        }
        let left = item.rcItem.left + self.scale(if selected { 11 } else { 8 });
        let right = item.rcItem.right - self.scale(8);
        let top = item.rcItem.top + self.scale(6);
        self.text(
            item.hDC,
            &self.strong,
            if preview.trim().is_empty() {
                "(no preview)"
            } else {
                preview
            },
            RECT {
                left,
                top,
                right,
                bottom: top + self.scale(19),
            },
            INK,
            DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        )?;
        self.text(
            item.hDC,
            &self.section_caption,
            metadata,
            RECT {
                left,
                top: top + self.scale(19),
                right,
                bottom: item.rcItem.bottom - self.scale(5),
            },
            GRAY,
            DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        )
    }
}

fn hovered(window: HWND) -> bool {
    if window.0.is_null() {
        return false;
    }
    let mut point = windows::Win32::Foundation::POINT::default();
    if let Err(error) = unsafe { GetCursorPos(&mut point) } {
        eprintln!("Original-theme hover unavailable: {error}");
        return false;
    }
    if unsafe { WindowFromPoint(point) } != window {
        return false;
    }
    let mut bounds = RECT::default();
    if let Err(error) = unsafe { GetWindowRect(window, &mut bounds) } {
        eprintln!("Original-theme hover bounds unavailable: {error}");
        return false;
    }
    point.x >= bounds.left
        && point.x < bounds.right
        && point.y >= bounds.top
        && point.y < bounds.bottom
}

fn hovered_rect(window: HWND, rect: RECT) -> bool {
    if !hovered(window) {
        return false;
    }
    let mut point = windows::Win32::Foundation::POINT::default();
    if let Err(error) = unsafe { GetCursorPos(&mut point) } {
        eprintln!("Original-theme row hover unavailable: {error}");
        return false;
    }
    if !unsafe { ScreenToClient(window, &mut point) }.as_bool() {
        eprintln!("Original-theme hover coordinate conversion failed.");
        return false;
    }
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}
