use super::*;
use std::path::Path;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::platform::{WriteMode, atomic_write};

const SCOPE: &str = "single-native-guarded-paste-fixture";
const MARKER: &str = "VoicePrompt synthetic guarded paste test 2026";
const RUN: usize = 7001;
const ARM: u32 = WM_APP + 71;
const CLOSE: u32 = WM_APP + 72;
const CLASS: PCWSTR = w!("VoicePromptRustPilot.SingleGuardedPasteTest");

#[derive(Default)]
struct Report {
    attempted: bool,
    target_matched: bool,
    clipboard_write_attempted: bool,
    clipboard_marker_published: bool,
    clipboard_changed: bool,
    input_sent: bool,
    marker_observed: bool,
    finished: bool,
    closed: bool,
}

impl Report {
    fn save(&self, root: &Path, mode: WriteMode) -> Result<(), crate::NativeError> {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "scope":SCOPE,"pid":std::process::id(),"attempted":self.attempted,
            "target_matched":self.target_matched,
            "clipboard_write_attempted":self.clipboard_write_attempted,
            "clipboard_marker_published":self.clipboard_marker_published,
            "clipboard_changed":self.clipboard_changed,"input_sent":self.input_sent,
            "marker_observed":self.marker_observed,"finished":self.finished,
            "closed":self.closed,"old_clipboard_read":false,"microphone_or_hotkeys":false,
            "account_or_backend":false,"whole_migration_acceptance":false,
        }))
        .map_err(|_| crate::NativeError::Verification("Encode manual desktop result failed."))?;
        atomic_write(&root.join("manual-paste-result.json"), &bytes, mode)
    }
}

struct TestWindow {
    window: HWND,
    instance: HINSTANCE,
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        if let Err(error) = unsafe { DestroyWindow(self.window) } {
            eprintln!("Manual desktop window cleanup failed: {error}");
        }
        if let Err(error) = unsafe { UnregisterClassW(CLASS, Some(self.instance)) } {
            eprintln!("Manual desktop class cleanup failed: {error}");
        }
    }
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let posted = match message {
        WM_COMMAND if wparam.0 & 0xffff == RUN && wparam.0 >> 16 == 0 => Some(ARM),
        WM_CLOSE => Some(CLOSE),
        _ => None,
    };
    if let Some(message) = posted {
        if let Err(error) = unsafe { PostMessageW(Some(window), message, WPARAM(0), LPARAM(0)) } {
            eprintln!("Manual desktop command could not be queued: {error}");
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn text(window: HWND, value: &str) -> windows::core::Result<()> {
    let wide: Vec<_> = value.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { SetWindowTextW(window, PCWSTR(wide.as_ptr())) }
}

fn timer(window: HWND, id: usize, milliseconds: u32) -> Result<(), std::io::Error> {
    if unsafe { SetTimer(Some(window), id, milliseconds, None) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::current_dir()?;
    crate::fixtures::validate_scope(&root, None, SCOPE)?;
    let mut report = Report::default();
    report.save(&root, WriteMode::CreateNew)?;
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
    let class = WNDCLASSW {
        lpfnWndProc: Some(procedure),
        hInstance: instance,
        lpszClassName: CLASS,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }?,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let window = TestWindow {
        window: unsafe {
            CreateWindowExW(
                WS_EX_APPWINDOW,
                CLASS,
                w!("VoicePrompt - Guarded paste test"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                700,
                330,
                None,
                None,
                Some(instance),
                None,
            )
        }?,
        instance,
    };
    let status = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!(
                "One authorized clipboard overwrite + automatic Ctrl+V.\r\nClick Run once personally, then do not change focus or keys for one second.\r\nNo microphone, accounts, backend, Enter or Save. No retry."
            ),
            WS_CHILD | WS_VISIBLE,
            20,
            20,
            650,
            90,
            Some(window.window),
            None,
            Some(instance),
            None,
        )
    }?;
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("Synthetic target (not saved)"),
            WS_CHILD | WS_VISIBLE,
            20,
            120,
            650,
            24,
            Some(window.window),
            None,
            Some(instance),
            None,
        )
    }?;
    let edit = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            20,
            150,
            650,
            35,
            Some(window.window),
            None,
            Some(instance),
            None,
        )
    }?;
    let button = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Run once"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            20,
            210,
            160,
            35,
            Some(window.window),
            Some(HMENU(RUN as *mut _)),
            Some(instance),
            None,
        )
    }?;
    timer(window.window, 3, 180000)?;
    let mut guard = None;
    let mut previous_clipboard = 0;
    let mut message = MSG::default();
    loop {
        let received = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if received == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        if received == 0 || message.message == CLOSE {
            break;
        }
        match (message.message, message.wParam.0) {
            (ARM, _) if !report.attempted => {
                report.attempted = true;
                let _ = unsafe { EnableWindow(button, false) };
                if unsafe { GetForegroundWindow() } != window.window
                    || unsafe { GetWindowTextLengthW(edit) } != 0
                    || !DesktopGuard::modifiers_released()
                {
                    report.finished = true;
                    text(
                        status,
                        "Blocked before clipboard: own window, blank target and released modifiers required. No retry.",
                    )?;
                } else {
                    unsafe { SetFocus(Some(edit)) }?;
                    let target = Target::capture();
                    report.target_matched = target.window == window.window.0 as usize
                        && target.focus == edit.0 as usize
                        && target.process == unsafe { GetCurrentProcessId() }
                        && target.thread == unsafe { GetCurrentThreadId() };
                    if report.target_matched {
                        previous_clipboard = unsafe { GetClipboardSequenceNumber() };
                        guard = Some(DesktopGuard::new(target));
                        text(
                            status,
                            "Armed: keep this window and its field focused for one second.",
                        )?;
                        timer(window.window, 1, 500)?;
                    } else {
                        report.finished = true;
                        text(
                            status,
                            "Blocked before clipboard: target mismatch. No retry.",
                        )?;
                    }
                }
            }
            (WM_TIMER, 1) if !report.finished => {
                unsafe { KillTimer(Some(window.window), 1) }?;
                let Some(mut pending) = guard.take() else {
                    return Err("Manual desktop timer had no owned target.".into());
                };
                if !pending.observe()
                    || !DesktopGuard::modifiers_released()
                    || previous_clipboard != unsafe { GetClipboardSequenceNumber() }
                    || unsafe { GetWindowTextLengthW(edit) } != 0
                {
                    report.finished = true;
                    text(
                        status,
                        "Blocked before clipboard: focus, modifiers, target or clipboard changed. No retry.",
                    )?;
                } else {
                    report.clipboard_write_attempted = true;
                    match crate::clipboard::write_with_sequence(window.window, MARKER) {
                        Ok(sequence) => {
                            report.clipboard_marker_published = true;
                            pending.mark_clipboard(sequence);
                            report.input_sent = pending.try_paste();
                            if report.input_sent {
                                timer(window.window, 2, 250)?;
                            } else {
                                report.finished = true;
                                text(
                                    status,
                                    "Marker copied, automatic paste blocked. No retry; marker remains on clipboard.",
                                )?;
                            }
                        }
                        Err(error) => {
                            eprintln!("Manual desktop clipboard publication failed: {error:?}");
                            report.finished = true;
                            text(
                                status,
                                "Clipboard publication failed; clipboard may have changed. No retry.",
                            )?;
                        }
                    }
                    report.clipboard_changed =
                        previous_clipboard != unsafe { GetClipboardSequenceNumber() };
                }
            }
            (WM_TIMER, 2) if !report.finished => {
                unsafe { KillTimer(Some(window.window), 2) }?;
                let mut value = [0; 256];
                let count = unsafe { GetWindowTextW(edit, &mut value) };
                report.marker_observed = count > 0
                    && value[..count as usize] == MARKER.encode_utf16().collect::<Vec<_>>();
                report.finished = true;
                text(
                    status,
                    if report.marker_observed {
                        "PASS: exact synthetic marker observed in own field after one automatic Ctrl+V. Marker remains on clipboard."
                    } else {
                        "FAILED: exact marker not observed. No retry; clipboard marker remains."
                    },
                )?;
            }
            (WM_TIMER, 3) => break,
            _ => unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            },
        }
        report.save(&root, WriteMode::Replace)?;
    }
    drop(window);
    report.closed = true;
    report.save(&root, WriteMode::Replace)?;
    if !report.finished || !report.marker_observed || !report.input_sent {
        return Err(
            "One manual desktop attempt did not pass; no automatic retry authorized.".into(),
        );
    }
    Ok(())
}

#[test]
#[ignore = "Needs one explicit user-approved clipboard overwrite/automatic Ctrl+V in own test field; never CI."]
fn manual_native_guarded_paste_once() {
    run().unwrap();
}

#[test]
fn manual_desktop_fixture_requires_an_owned_fresh_scope_before_any_ui_or_clipboard_action() {
    let directory = crate::recovery_store::tests::TemporaryRoot::new();
    assert!(crate::fixtures::validate_scope(&directory.0, None, SCOPE).is_err());
    let marker = directory.0.join("migration-fixture.json");
    for (scope, accepted) in [(crate::fixtures::SCOPE, false), (SCOPE, true)] {
        std::fs::write(
            &marker,
            serde_json::to_vec(&serde_json::json!({
                "scope":scope, "instance_key":"0123456789abcdef0123456789abcdef",
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            crate::fixtures::validate_scope(&directory.0, None, SCOPE).is_ok(),
            accepted
        );
    }
    let report = Report::default();
    report.save(&directory.0, WriteMode::CreateNew).unwrap();
    assert!(report.save(&directory.0, WriteMode::CreateNew).is_err());
    assert!(!report.attempted && !report.clipboard_write_attempted && !report.input_sent);
}
