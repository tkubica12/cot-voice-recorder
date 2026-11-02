use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use super::*;
use windows::Win32::Foundation::RECT;

fn publish(root: &Path, name: &str, data: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(name))
        .unwrap();
    file.write_all(data).unwrap();
    file.sync_all().unwrap();
}

fn pump_for(duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "Bounded visual-only fixture. Launch through approved UI tools in a fresh explicitly marked root; no physical input or account ports."]
fn manual_reference_overlay_visual_fixture() {
    unsafe {
        windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    }
    .unwrap();
    let root = std::env::current_dir().unwrap();
    let marker: serde_json::Value =
        serde_json::from_reader(File::open(root.join("overlay-fixture.json")).unwrap()).unwrap();
    assert_eq!(
        marker,
        serde_json::json!({"scope":"native-overlay-visual-fixture"})
    );
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        1,
        "Fresh visual-only fixture root required."
    );
    publish(&root, "started.json", serde_json::json!({"pid":std::process::id(),"scope":"native-overlay-visual-fixture","physical_ports":false}).to_string().as_bytes());
    let (commands, _receiver) = mpsc::channel(2);
    let mut ui = DictationUi::new(
        HWND::default(),
        commands,
        Arc::new(DeliveryPolicy::default()),
        false,
    )
    .unwrap();
    ui.generation = 17;
    ui.busy = true;
    let mut status = ClientStatus::default();
    let started = Instant::now();
    status.dictation.generation = 17;
    status.dictation.recording_started = Some(started);
    let states = [
        ("listening-empty", DictationStage::Recording, "", 1),
        (
            "listening-preview",
            DictationStage::Recording,
            "Synthetic live preview. These words grow while you speak.",
            2,
        ),
        (
            "listening-wrap",
            DictationStage::Recording,
            "Synthetic continuous preview wraps over several lines. Each new recognized phrase replaces the live preview without activating another application. This deliberately long sample checks the original three-line layout and character ellipsis rather than hiding or shrinking the text. No microphone or clipboard is used.",
            3,
        ),
        (
            "finishing",
            DictationStage::Finishing,
            "Synthetic live preview. These words grow while you speak.",
            4,
        ),
        (
            "saved",
            DictationStage::PasteSkipped,
            "Only confirmed final text is delivered.",
            5,
        ),
    ];
    let mut reports = Vec::new();
    for (name, stage, preview, seconds) in states {
        status.dictation.stage = stage;
        status.dictation.preview = preview.into();
        let foreground = unsafe { GetForegroundWindow() };
        let now = started + Duration::from_secs(seconds);
        ui.poll_at(&status, now).unwrap();
        assert_eq!(
            unsafe { GetForegroundWindow() },
            foreground,
            "Overlay must not activate."
        );
        assert!(unsafe { IsWindowVisible(ui.overlay.window) }.as_bool());
        let frame = ui.overlay.fixture_frame().unwrap();
        let mut bounds = RECT::default();
        unsafe { GetWindowRect(ui.overlay.window, &mut bounds) }.unwrap();
        for dpi in [96, 144, 192] {
            let (size, bytes) = ui.overlay.fixture_raster(dpi).unwrap();
            let base = format!("{name}-{dpi}");
            publish(&root, &format!("{base}.bgra"), &bytes);
            publish(
                &root,
                &format!("{base}.json"),
                serde_json::json!({
                    "width":size.cx,"height":size.cy,"dpi":dpi,"pixel_format":"Pbgra32",
                    "header":frame.header,"preview":frame.preview,"recording":frame.recording,
                })
                .to_string()
                .as_bytes(),
            );
        }
        reports.push(serde_json::json!({"state":name,"visible":true,"foreground_unchanged":true,
            "bounds":[bounds.left,bounds.top,bounds.right,bounds.bottom],"dpi":crate::dpi::window_dpi(ui.overlay.window)}));
        if stage != DictationStage::PasteSkipped {
            pump_for(Duration::from_secs(10));
        } else {
            let terminal_started = Instant::now();
            pump_for(Duration::from_millis(899));
            ui.poll_at(
                &status,
                now + terminal_started.elapsed().min(Duration::from_millis(899)),
            )
            .unwrap();
            assert!(unsafe { IsWindowVisible(ui.overlay.window) }.as_bool());
            pump_for(Duration::from_millis(20));
            ui.poll_at(&status, now + terminal_started.elapsed())
                .unwrap();
            assert!(!unsafe { IsWindowVisible(ui.overlay.window) }.as_bool());
        }
    }
    ui.poll_at(&status, started + Duration::from_millis(5900))
        .unwrap();
    assert!(!unsafe { IsWindowVisible(ui.overlay.window) }.as_bool());
    ui.poll_at(&status, started + Duration::from_secs(8))
        .unwrap();
    assert!(!unsafe { IsWindowVisible(ui.overlay.window) }.as_bool());
    assert!(ui.paste.is_none());
    let window = ui.overlay.window;
    drop(ui);
    assert!(!unsafe { IsWindow(Some(window)) }.as_bool());
    publish(
        &root,
        "result.json",
        serde_json::json!({
            "scope":"native-overlay-visual-fixture","pid":std::process::id(),"physical_ports":false,
            "states":reports,"dismissed_after_900ms":true,"unchanged_terminal_not_reshown":true,
            "window_destroyed":true,"complete":true,
        })
        .to_string()
        .as_bytes(),
    );
}
