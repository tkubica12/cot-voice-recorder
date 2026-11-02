use super::*;
use std::mem::size_of;
use windows::Win32::Foundation::{ERROR_SUCCESS, GetLastError, SetLastError};
use windows::Win32::System::Threading::{GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources};

struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Canvas {
    fn new(width: i32, height: i32) -> Self {
        let dc = unsafe { CreateCompatibleDC(None) };
        assert!(!dc.0.is_null());
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = std::ptr::null_mut();
        let bitmap =
            unsafe { CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut pixels, None, 0) }
                .unwrap();
        assert!(!pixels.is_null());
        let previous = unsafe { SelectObject(dc, bitmap.into()) };
        assert!(!previous.0.is_null() && previous.0 as isize != -1);
        Self {
            dc,
            bitmap,
            previous,
        }
    }

    fn pixel(&self, x: i32, y: i32) -> COLORREF {
        let color = unsafe { GetPixel(self.dc, x, y) };
        assert_ne!(color.0, u32::MAX);
        color
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.dc, self.previous);
            assert!(DeleteObject(self.bitmap.into()).as_bool());
            assert!(DeleteDC(self.dc).as_bool());
        }
    }
}

fn render(canvas: &Canvas, theme: &MainTheme) {
    let scale = |value| theme.scale(value);
    let bounds = |left, top, right, bottom| RECT {
        left: scale(left),
        top: scale(top),
        right: scale(right),
        bottom: scale(bottom),
    };
    theme.canvas(canvas.dc, bounds(0, 0, 320, 160)).unwrap();
    theme
        .tab(
            &DRAWITEMSTRUCT {
                hDC: canvas.dc,
                rcItem: bounds(0, 0, 96, crate::main_layout::SECTION_ITEM_HEIGHT_DIP),
                itemState: ODS_SELECTED,
                ..Default::default()
            },
            "History",
        )
        .unwrap();
    theme
        .history_row(
            &DRAWITEMSTRUCT {
                hDC: canvas.dc,
                rcItem: bounds(0, 40, 300, 87),
                itemState: ODS_SELECTED,
                ..Default::default()
            },
            "Synthetic final text.",
            "Synthetic date · 21 chars · 48 h left",
        )
        .unwrap();
    theme
        .button(
            &DRAWITEMSTRUCT {
                hDC: canvas.dc,
                rcItem: bounds(0, 100, 110, 130),
                ..Default::default()
            },
            "Copy selected",
            true,
            true,
        )
        .unwrap();
}

#[test]
fn original_segoe_font_roles_palette_and_selected_rasters_match_at_each_dpi() {
    assert_eq!(INK.0, 0x00111111);
    assert_eq!(GRAY.0, 0x0080726b);
    assert_eq!(LINE.0, 0x00ebe7e5);
    assert_eq!(SURFACE.0, 0x00f8f7f7);
    assert_eq!(WHITE.0, 0x00ffffff);
    assert_eq!(ACCENT.0, 0x001673f9);
    for dpi in [96, 120, 144, 192, 288] {
        let theme = MainTheme::new(dpi).unwrap();
        for (font, size, weight) in [
            (&theme.base, 13, 400),
            (&theme.caption, 11, 400),
            (&theme.section_caption, 11, 600),
            (&theme.title, 17, 600),
            (&theme.strong, 13, 600),
        ] {
            let mut description = LOGFONTW::default();
            assert_eq!(
                unsafe {
                    GetObjectW(
                        font.handle().into(),
                        size_of::<LOGFONTW>() as i32,
                        Some((&mut description as *mut LOGFONTW).cast()),
                    )
                },
                size_of::<LOGFONTW>() as i32
            );
            assert_eq!(description.lfHeight, -((size * dpi as i32 + 48) / 96));
            assert_eq!(description.lfWeight, weight);
            let end = description
                .lfFaceName
                .iter()
                .position(|value| *value == 0)
                .unwrap();
            assert_eq!(
                String::from_utf16(&description.lfFaceName[..end]).unwrap(),
                "Segoe UI"
            );
        }
        let canvas = Canvas::new(theme.scale(320), theme.scale(160));
        let previous: Vec<_> = [OBJ_FONT, OBJ_PEN, OBJ_BRUSH]
            .map(|kind| unsafe { GetCurrentObject(canvas.dc, kind) })
            .into();
        render(&canvas, &theme);
        assert_eq!(
            canvas.pixel(
                theme.scale(30),
                theme.scale(crate::main_layout::SECTION_ITEM_HEIGHT_DIP) - 1
            ),
            ACCENT,
            "Selected tab underline must fit inside the viewport."
        );
        assert_eq!(canvas.pixel(theme.scale(1), theme.scale(44)), ACCENT);
        assert_eq!(canvas.pixel(theme.scale(200), theme.scale(43)), SURFACE);
        assert_eq!(canvas.pixel(theme.scale(200), theme.scale(87) - 1), ACCENT);
        assert_eq!(canvas.pixel(theme.scale(8), theme.scale(104)), ACCENT);
        for (kind, previous) in [OBJ_FONT, OBJ_PEN, OBJ_BRUSH].into_iter().zip(previous) {
            assert_eq!(unsafe { GetCurrentObject(canvas.dc, kind) }, previous);
        }
    }
    assert!(MainTheme::new(0).is_err());
    assert!(DpiFont::sized(96, 0, 400).is_err());
    assert!(DpiFont::sized(96, 13, 0).is_err());
}

#[test]
fn full_and_compact_recovery_rows_keep_all_lines_inside_the_selected_surface() {
    for dpi in [96, 144, 192, 288] {
        let theme = MainTheme::new(dpi).unwrap();
        for (compact, height, lines) in [
            (
                false,
                crate::main_layout::RECOVERY_ROW_HEIGHT_DIP,
                vec![
                    "Synthetic date".into(),
                    "2 chunks pending".into(),
                    "Language: cs".into(),
                    "Cloud audit upload pending: True".into(),
                ],
            ),
            (
                true,
                46,
                vec![
                    "Synthetic date; Language: cs".into(),
                    "2 chunks pending; Audit pending: True".into(),
                ],
            ),
        ] {
            assert!(6 + lines.len() as i32 * 19 <= height);
            let canvas = Canvas::new(theme.scale(320), theme.scale(height));
            let item = DRAWITEMSTRUCT {
                hDC: canvas.dc,
                rcItem: RECT {
                    left: 0,
                    top: 0,
                    right: theme.scale(320),
                    bottom: theme.scale(height),
                },
                itemState: ODS_SELECTED,
                ..Default::default()
            };
            theme.recovery_row(&item, &lines, compact).unwrap();
            assert_eq!(canvas.pixel(theme.scale(1), theme.scale(4)), ACCENT);
            assert_eq!(canvas.pixel(theme.scale(300), theme.scale(3)), SURFACE);
            assert_eq!(
                canvas.pixel(theme.scale(200), theme.scale(height) - 1),
                ACCENT
            );
        }
    }
}

#[test]
#[ignore = "Separate synthetic graphics-only process avoids concurrent GDI counts. No window, cursor query, input, clipboard, account or device."]
fn synthetic_main_theme_cycles_release_fonts_brushes_pens_and_bitmaps() {
    let handles = || {
        unsafe { SetLastError(ERROR_SUCCESS) };
        let count = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        if count == 0 {
            assert_eq!(unsafe { GetLastError() }, ERROR_SUCCESS);
        }
        count
    };
    let cycle = || {
        let theme = MainTheme::new(144).unwrap();
        let canvas = Canvas::new(theme.scale(320), theme.scale(160));
        render(&canvas, &theme);
    };
    for _ in 0..20 {
        cycle();
    }
    let before = handles();
    for _ in 0..1000 {
        cycle();
    }
    let after = handles();
    assert_eq!(after, before);
    println!(
        "{{\"scope\":\"synthetic-main-theme-graphics-only\",\"warmup_cycles\":20,\"measured_cycles\":1000,\"gdi_before\":{before},\"gdi_after\":{after},\"physical_ports\":false,\"window_created\":false}}"
    );
}
