use std::mem::size_of;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, PCWSTR, w};

use crate::NativeError;

pub(crate) const WIDTH: i32 = 440;
pub(crate) const HEIGHT: i32 = 112;
pub(crate) const PLACEHOLDER: &str = "Recognized words will appear here as you speak.";

fn api<T>(operation: &'static str, result: windows::core::Result<T>) -> Result<T, NativeError> {
    result.map_err(|error| NativeError::Api(operation, error))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub header: String,
    pub preview: String,
    pub recording: bool,
}

impl Frame {
    pub fn new(header: &str, preview: &str, recording: bool) -> Self {
        Self {
            header: header.into(),
            preview: if preview.trim().is_empty() {
                PLACEHOLDER
            } else {
                preview
            }
            .into(),
            recording,
        }
    }
}

struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    #[cfg(test)]
    pixels: *mut u8,
    size: SIZE,
}

impl Dib {
    fn new(dpi: u32) -> Result<Self, NativeError> {
        if !(48..=768).contains(&dpi) {
            return Err(NativeError::InvalidArgument("Unsupported overlay DPI."));
        }
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.0.is_null() {
            return Err(NativeError::Api(
                "Create overlay memory DC",
                windows::core::Error::from_thread(),
            ));
        }
        let scale = |value| (value * dpi as i32 + 48) / 96;
        let mut dib = Self {
            dc,
            bitmap: HBITMAP::default(),
            previous: HGDIOBJ::default(),
            #[cfg(test)]
            pixels: std::ptr::null_mut(),
            size: SIZE {
                cx: scale(WIDTH),
                cy: scale(HEIGHT),
            },
        };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: dib.size.cx,
                biHeight: -dib.size.cy,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = std::ptr::null_mut();
        dib.bitmap = api("Create overlay premultiplied BGRA bitmap", unsafe {
            CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut pixels, None, 0)
        })?;
        dib.previous = unsafe { SelectObject(dc, dib.bitmap.into()) };
        if dib.previous.0.is_null() || dib.previous.0 as isize == -1 || pixels.is_null() {
            return Err(NativeError::Api(
                "Select overlay bitmap",
                windows::core::Error::from_thread(),
            ));
        }
        #[cfg(test)]
        {
            dib.pixels = pixels.cast();
        }
        Ok(dib)
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.0.is_null() && self.previous.0 as isize != -1 {
                let _ = SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.0.is_null() && !DeleteObject(self.bitmap.into()).as_bool() {
                eprintln!("Overlay bitmap cleanup failed.");
            }
            if !DeleteDC(self.dc).as_bool() {
                eprintln!("Overlay memory DC cleanup failed.");
            }
        }
    }
}

struct Surface {
    target: ID2D1DCRenderTarget,
    write: IDWriteFactory,
    header: IDWriteTextFormat,
    preview: IDWriteTextFormat,
    dib: Dib,
    dpi: u32,
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

fn color(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a,
    }
}

impl Surface {
    fn new(dpi: u32) -> Result<Self, NativeError> {
        let dib = Dib::new(dpi)?;
        let factory: ID2D1Factory = api("Create native overlay drawing factory", unsafe {
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
        })?;
        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi as f32,
            dpiY: dpi as f32,
            ..Default::default()
        };
        let target = api("Create software overlay drawing target", unsafe {
            factory.CreateDCRenderTarget(&properties)
        })?;
        api("Bind overlay bitmap", unsafe {
            target.BindDC(
                dib.dc,
                &RECT {
                    left: 0,
                    top: 0,
                    right: dib.size.cx,
                    bottom: dib.size.cy,
                },
            )
        })?;
        let write: IDWriteFactory = api("Create native overlay text factory", unsafe {
            DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
        })?;
        let format = |size| {
            api("Create Segoe UI overlay text", unsafe {
                write.CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_REGULAR,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-us"),
                )
            })
        };
        let header = format(12.0)?;
        api("Set single-line overlay header", unsafe {
            header.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)
        })?;
        let preview = format(14.0)?;
        let last_line: IDWriteTextFormat1 = api(
            "Select native character-trimmed preview format",
            preview.cast(),
        )?;
        // WPF keeps the fitting prefix of the last word before the character ellipsis.
        api(
            "Keep character trimming on the final preview line",
            unsafe { last_line.SetLastLineWrapping(false) },
        )?;
        api("Set overlay preview line height", unsafe {
            preview.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 20.0, 16.0)
        })?;
        let ellipsis = api("Create overlay character ellipsis", unsafe {
            write.CreateEllipsisTrimmingSign(&preview)
        })?;
        api("Set overlay character ellipsis", unsafe {
            preview.SetTrimming(
                &DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    ..Default::default()
                },
                &ellipsis,
            )
        })?;
        unsafe {
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }
        Ok(Self {
            target,
            write,
            header,
            preview,
            dib,
            dpi,
        })
    }

    fn render(&self, frame: &Frame) -> Result<(), NativeError> {
        let title: Vec<_> = frame.header.encode_utf16().collect();
        let preview: Vec<_> = frame.preview.encode_utf16().collect();
        let title_layout = api("Measure native overlay header", unsafe {
            self.write
                .CreateTextLayout(&title, &self.header, 400.0, 32.0)
        })?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        api("Read overlay header height", unsafe {
            title_layout.GetMetrics(&mut metrics)
        })?;
        let background = api("Create overlay panel brush", unsafe {
            self.target
                .CreateSolidColorBrush(&color(30, 30, 34, 1.0), None)
        })?;
        let white = api("Create overlay text brush", unsafe {
            self.target
                .CreateSolidColorBrush(&color(255, 255, 255, 1.0), None)
        })?;
        // The reference controller supplies level=0: a half-opacity OrangeRed dot, not a waveform.
        let dot_color = if frame.recording {
            color(255, 69, 0, 0.5)
        } else {
            color(30, 144, 255, 1.0)
        };
        let dot_brush = api("Create overlay state brush", unsafe {
            self.target.CreateSolidColorBrush(&dot_color, None)
        })?;
        let mut dot = D2D1_ELLIPSE {
            radiusX: 4.0,
            radiusY: 4.0,
            ..Default::default()
        };
        dot.point.X = 16.0;
        dot.point.Y = 10.0 + metrics.height / 2.0;
        let preview_top = 10.0 + metrics.height + 8.0;
        unsafe {
            self.target.BeginDraw();
            self.target.Clear(Some(&D2D1_COLOR_F::default()));
            self.target.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: rect(0.0, 0.0, WIDTH as f32, HEIGHT as f32),
                    radiusX: 17.0,
                    radiusY: 17.0,
                },
                &background,
            );
            self.target.FillEllipse(&dot, &dot_brush);
            self.target.DrawText(
                &title,
                &self.header,
                &rect(30.0, 10.0, 430.0, 10.0 + metrics.height),
                &white,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            self.target.DrawText(
                &preview,
                &self.preview,
                &rect(12.0, preview_top, 428.0, preview_top + 60.0),
                &white,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
        api("Complete native overlay drawing", unsafe {
            self.target.EndDraw(None, None)
        })
    }
}

pub(crate) struct Overlay {
    pub window: HWND,
    surface: Option<Surface>,
    last_frame: Option<Frame>,
    position: Option<(i32, i32)>,
    #[cfg(test)]
    pub(crate) suppress_presentation: bool,
}

impl Overlay {
    pub fn new() -> Result<Self, NativeError> {
        let window = api(
            "Create nonactivating click-through dictation overlay",
            unsafe {
                CreateWindowExW(
                    WS_EX_LAYERED
                        | WS_EX_TRANSPARENT
                        | WS_EX_NOACTIVATE
                        | WS_EX_TOOLWINDOW
                        | WS_EX_TOPMOST,
                    w!("STATIC"),
                    w!("VoicePrompt dictation"),
                    WS_POPUP,
                    0,
                    0,
                    WIDTH,
                    HEIGHT,
                    None,
                    None,
                    None,
                    None,
                )
            },
        )?;
        let overlay = Self {
            window,
            surface: None,
            last_frame: None,
            position: None,
            #[cfg(test)]
            suppress_presentation: false,
        };
        crate::icons::AppIcons::for_dpi(crate::dpi::window_dpi(window))?.apply(window);
        Ok(overlay)
    }

    pub fn present(&mut self, frame: Frame) -> Result<(), NativeError> {
        let dpi = crate::dpi::window_dpi(self.window);
        let mut area = RECT::default();
        api("Read primary work area for dictation overlay", unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some((&mut area as *mut RECT).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
            )
        })?;
        if self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.dpi != dpi)
        {
            self.surface = Some(Surface::new(dpi)?);
            self.last_frame = None;
            crate::icons::AppIcons::for_dpi(dpi)?.apply(self.window);
        }
        let surface = self.surface.as_ref().expect("created above");
        let changed = self.last_frame.as_ref() != Some(&frame);
        if changed {
            surface.render(&frame)?;
            let accessible: Vec<_> = format!(
                "VoicePrompt dictation - {}\r\n{}",
                frame.header, frame.preview
            )
            .encode_utf16()
            .chain([0])
            .collect();
            api("Update accessible dictation text", unsafe {
                SetWindowTextW(self.window, PCWSTR(accessible.as_ptr()))
            })?;
        }
        let position = POINT {
            x: area.left + (area.right - area.left - surface.dib.size.cx) / 2,
            y: area.bottom - surface.dib.size.cy - (18 * dpi as i32 + 48) / 96,
        };
        #[cfg(test)]
        if self.suppress_presentation {
            self.last_frame = Some(frame);
            self.position = Some((position.x, position.y));
            return Ok(());
        }
        if !changed && self.position == Some((position.x, position.y)) {
            return Ok(());
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
            ..Default::default()
        };
        api("Publish native layered dictation overlay", unsafe {
            UpdateLayeredWindow(
                self.window,
                None,
                Some(&position),
                Some(&surface.dib.size),
                Some(surface.dib.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        })?;
        api("Show dictation overlay without activation", unsafe {
            SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                position.x,
                position.y,
                surface.dib.size.cx,
                surface.dib.size.cy,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        })?;
        self.last_frame = Some(frame);
        self.position = Some((position.x, position.y));
        Ok(())
    }

    pub fn hide(&mut self) {
        unsafe {
            let _ = ShowWindow(self.window, SW_HIDE);
        }
        self.surface = None;
        self.last_frame = None;
        self.position = None;
    }

    #[cfg(test)]
    pub(crate) fn fixture_frame(&self) -> Option<&Frame> {
        self.last_frame.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn fixture_raster(&self, dpi: u32) -> Result<(SIZE, Vec<u8>), NativeError> {
        let frame = self
            .last_frame
            .as_ref()
            .ok_or(NativeError::Verification("No overlay frame to export."))?;
        let surface = Surface::new(dpi)?;
        surface.render(frame)?;
        let size = surface.dib.size;
        let bytes = unsafe {
            std::slice::from_raw_parts(surface.dib.pixels, (size.cx * size.cy * 4) as usize)
        }
        .to_vec();
        Ok((size, bytes))
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        if let Err(error) = unsafe { DestroyWindow(self.window) } {
            eprintln!("Overlay window cleanup failed: {error}");
        }
    }
}

#[cfg(test)]
#[path = "dictation_overlay_tests.rs"]
mod tests;
