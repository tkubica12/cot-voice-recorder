use super::*;

fn pixel(surface: &Surface, x: usize, y: usize) -> [u8; 4] {
    let offset = (y * surface.dib.size.cx as usize + x) * 4;
    unsafe { std::slice::from_raw_parts(surface.dib.pixels.add(offset), 4) }
        .try_into()
        .unwrap()
}

#[test]
fn renderer_preserves_reference_geometry_alpha_colors_and_live_text_at_each_dpi() {
    for dpi in [96, 120, 144, 192, 288] {
        let surface = Surface::new(dpi).unwrap();
        let last_line: IDWriteTextFormat1 = surface.preview.cast().unwrap();
        assert!(!unsafe { last_line.GetLastLineWrapping() }.as_bool());
        let scale = |value| (value * dpi as usize + 48) / 96;
        surface
            .render(&Frame::new("Listening 00:01 - release to finish", "", true))
            .unwrap();
        assert_eq!(surface.dib.size.cx as usize, scale(440));
        assert_eq!(surface.dib.size.cy as usize, scale(112));
        assert_eq!(
            pixel(&surface, 0, 0),
            [0; 4],
            "Transparent rounded corner at {dpi}"
        );
        assert_eq!(pixel(&surface, scale(220), scale(108)), [34, 30, 30, 255]);
        let recording_dot = pixel(&surface, scale(16), scale(18));
        assert!(recording_dot[2] > 140 && recording_dot[1] < 51 && recording_dot[0] < 19);
        surface
            .render(&Frame::new(
                "Finishing transcription - Esc discards",
                "Synthetic confirmed text.",
                false,
            ))
            .unwrap();
        assert_eq!(pixel(&surface, scale(16), scale(18)), [255, 144, 30, 255]);
    }
}

#[test]
#[ignore = "Separate synthetic graphics-only process avoids concurrent test noise in process-wide GDI handle counts. No visible window or physical port."]
fn synthetic_render_hide_cycles_release_all_gdi_handles() {
    use windows::Win32::System::Threading::{GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources};
    let handles = || {
        unsafe {
            windows::Win32::Foundation::SetLastError(windows::Win32::Foundation::ERROR_SUCCESS)
        };
        let count = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        if count == 0 {
            assert_eq!(
                unsafe { windows::Win32::Foundation::GetLastError() },
                windows::Win32::Foundation::ERROR_SUCCESS
            );
        }
        count
    };
    let foreground = unsafe { GetForegroundWindow() };
    let mut overlay = Overlay::new().unwrap();
    overlay.suppress_presentation = true;
    assert_eq!(unsafe { GetForegroundWindow() }, foreground);
    let frame = Frame::new(
        "Listening 01:23 - release to finish",
        "Synthetic preview 0123456789. No audio, account, clipboard or visible presentation.",
        true,
    );
    for _ in 0..20 {
        overlay.present(frame.clone()).unwrap();
        overlay.hide();
    }
    let before = handles();
    for _ in 0..1000 {
        overlay.present(frame.clone()).unwrap();
        overlay.hide();
    }
    let after = handles();
    assert_eq!(
        after, before,
        "Every overlay DC/bitmap must be released on hide."
    );
    assert!(!unsafe { IsWindowVisible(overlay.window) }.as_bool());
    println!(
        "{{\"scope\":\"synthetic-overlay-graphics-only\",\"warmup_cycles\":20,\"measured_cycles\":1000,\"gdi_before\":{before},\"gdi_after\":{after},\"physical_ports\":false,\"window_shown\":false}}"
    );
}

#[test]
fn hidden_overlay_is_layered_click_through_nonactivating_and_uses_original_icons() {
    let before = unsafe { GetForegroundWindow() };
    let mut overlay = Overlay::new().unwrap();
    let style = WINDOW_EX_STYLE(unsafe { GetWindowLongPtrW(overlay.window, GWL_EXSTYLE) } as u32);
    for required in [
        WS_EX_LAYERED,
        WS_EX_TRANSPARENT,
        WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST,
    ] {
        assert!(style.contains(required));
    }
    assert!(!unsafe { IsWindowVisible(overlay.window) }.as_bool());
    assert!(
        overlay.surface.is_none(),
        "No graphics/font allocation in signed-out idle."
    );
    overlay.hide();
    assert_eq!(unsafe { GetForegroundWindow() }, before);
}
