use std::mem::size_of;

use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub window: usize,
    pub focus: usize,
    pub thread: u32,
    pub process: u32,
}

fn focus(thread: u32) -> usize {
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    match unsafe { GetGUIThreadInfo(thread, &mut info) } {
        Ok(()) => info.hwndFocus.0 as usize,
        Err(error) => {
            eprintln!("Paste focus inspection failed: {error}");
            0
        }
    }
}

impl Target {
    pub fn capture() -> Self {
        let window = unsafe { GetForegroundWindow() };
        let mut process = 0;
        let thread = unsafe { GetWindowThreadProcessId(window, Some(&mut process)) };
        Self {
            window: window.0 as usize,
            focus: focus(thread),
            thread,
            process,
        }
    }
}

pub struct DesktopGuard {
    target: Target,
    changed: bool,
    clipboard: u32,
}

impl DesktopGuard {
    pub fn new(target: Target) -> Self {
        Self {
            target,
            changed: false,
            clipboard: 0,
        }
    }
    pub fn observe(&mut self) -> bool {
        self.changed |= !target_matches(self.target, Target::capture());
        !self.changed
    }
    pub fn mark_clipboard(&mut self, sequence: u32) {
        self.clipboard = sequence;
    }
    pub fn modifiers_released() -> bool {
        [0x10, 0x11, 0x12, 0x5b, 0x5c]
            .iter()
            .all(|key| !key_down(*key))
    }
    pub fn try_paste(&mut self) -> bool {
        if !self.observe()
            || !Self::modifiers_released()
            || self.clipboard == 0
            || self.clipboard != unsafe { GetClipboardSequenceNumber() }
        {
            return false;
        }
        let input = paste_inputs();
        let sent = unsafe { SendInput(&input, size_of::<INPUT>() as i32) };
        if sent == input.len() as u32 {
            return true;
        }
        eprintln!("Paste SendInput was incomplete; clipboard and History remain available.");
        if sent > 0 {
            let release = [key(0x56, true), key(0x11, true)];
            let released = unsafe { SendInput(&release, size_of::<INPUT>() as i32) };
            if released != release.len() as u32 {
                eprintln!("Paste modifier release failed.");
            }
        }
        false
    }
}

pub(crate) fn key_down(key: u16) -> bool {
    (unsafe { GetAsyncKeyState(i32::from(key)) }) < 0
}

fn target_matches(expected: Target, current: Target) -> bool {
    expected.window != 0
        && expected.focus != 0
        && expected.thread != 0
        && expected.process != 0
        && expected == current
}

fn key(code: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(code),
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                ..Default::default()
            },
        },
    }
}

fn paste_inputs() -> [INPUT; 4] {
    [
        key(0x11, false),
        key(0x56, false),
        key(0x56, true),
        key(0x11, true),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paste_has_exactly_one_ctrl_v_no_enter_and_missing_or_changed_focus_is_rejected() {
        let input = paste_inputs();
        let codes: Vec<_> = input
            .iter()
            .map(|input| unsafe { input.Anonymous.ki.wVk.0 })
            .collect();
        assert_eq!(codes, [0x11, 0x56, 0x56, 0x11]);
        let expected = Target {
            window: 1,
            focus: 2,
            thread: 3,
            process: 4,
        };
        assert!(target_matches(expected, expected));
        assert!(!target_matches(
            expected,
            Target {
                focus: 5,
                ..expected
            }
        ));
        assert!(!target_matches(
            Target {
                focus: 0,
                ..expected
            },
            expected
        ));
    }
}
