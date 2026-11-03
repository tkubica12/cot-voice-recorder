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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PasteBlock {
    FocusChanged,
    ModifiersHeld,
    ClipboardNotPublished,
    ClipboardChanged { expected: u32, actual: u32 },
}

impl PasteBlock {
    pub fn label(self) -> &'static str {
        match self {
            Self::FocusChanged => "focus_window_thread_or_process_changed",
            Self::ModifiersHeld => "modifiers_not_released",
            Self::ClipboardNotPublished => "clipboard_sequence_not_recorded",
            Self::ClipboardChanged { .. } => "clipboard_sequence_changed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PasteOutcome {
    Blocked(PasteBlock),
    Sent {
        events: u32,
    },
    Incomplete {
        events: u32,
        expected: u32,
        release_events: Option<u32>,
    },
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
        self.observe_target(Target::capture())
    }
    fn observe_target(&mut self, current: Target) -> bool {
        self.changed |= !target_matches(self.target, current);
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
        matches!(self.try_paste_detailed(), PasteOutcome::Sent { .. })
    }
    fn preflight(
        &mut self,
        current: Target,
        read_modifiers_released: impl FnOnce() -> bool,
        read_sequence: impl FnOnce() -> u32,
    ) -> Result<(), PasteBlock> {
        if !self.observe_target(current) {
            return Err(PasteBlock::FocusChanged);
        }
        if !read_modifiers_released() {
            return Err(PasteBlock::ModifiersHeld);
        }
        if self.clipboard == 0 {
            return Err(PasteBlock::ClipboardNotPublished);
        }
        let sequence = read_sequence();
        if self.clipboard != sequence {
            return Err(PasteBlock::ClipboardChanged {
                expected: self.clipboard,
                actual: sequence,
            });
        }
        Ok(())
    }
    pub(crate) fn try_paste_detailed(&mut self) -> PasteOutcome {
        let outcome = self.attempt_with(
            Target::capture(),
            Self::modifiers_released,
            || unsafe { GetClipboardSequenceNumber() },
            |input| unsafe { SendInput(input, size_of::<INPUT>() as i32) },
        );
        match outcome {
            PasteOutcome::Blocked(reason) => eprintln!("Paste suppressed: {}.", reason.label()),
            PasteOutcome::Incomplete {
                events,
                expected,
                release_events,
            } => {
                eprintln!(
                    "Paste SendInput accepted {events}/{expected} events; clipboard and History remain available."
                );
                if let Some(released) = release_events {
                    if released != 2 {
                        eprintln!("Paste modifier release accepted {released}/2 events.");
                    }
                }
            }
            PasteOutcome::Sent { .. } => {}
        }
        outcome
    }

    fn attempt_with(
        &mut self,
        current: Target,
        read_modifiers_released: impl FnOnce() -> bool,
        read_sequence: impl FnOnce() -> u32,
        send: impl FnMut(&[INPUT]) -> u32,
    ) -> PasteOutcome {
        if let Err(reason) = self.preflight(current, read_modifiers_released, read_sequence) {
            return PasteOutcome::Blocked(reason);
        }
        emit_paste_with(send)
    }
}

fn emit_paste_with(mut send: impl FnMut(&[INPUT]) -> u32) -> PasteOutcome {
    let input = paste_inputs();
    let sent = send(&input);
    if sent == input.len() as u32 {
        return PasteOutcome::Sent { events: sent };
    }
    let release_events = if sent > 0 {
        let release = [key(0x56, true), key(0x11, true)];
        let released = send(&release);
        Some(released)
    } else {
        None
    };
    PasteOutcome::Incomplete {
        events: sent,
        expected: input.len() as u32,
        release_events,
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
#[path = "desktop_manual_tests.rs"]
mod manual_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_uses_completed_publication_sequence_and_still_blocks_later_changes_without_ports() {
        let sequence = std::cell::Cell::new(25);
        let published = crate::clipboard::committed_sequence(
            || {
                sequence.set(28);
                Ok(())
            },
            || sequence.get(),
            || Ok(true),
        )
        .unwrap();
        let target = Target {
            window: 1,
            focus: 2,
            thread: 3,
            process: 4,
        };
        let mut guard = DesktopGuard::new(target);
        guard.mark_clipboard(published);
        let mut synthetic_emissions = 0;
        assert_eq!(
            guard.attempt_with(
                target,
                || true,
                || sequence.get(),
                |events| {
                    synthetic_emissions += 1;
                    events.len() as u32
                }
            ),
            PasteOutcome::Sent { events: 4 }
        );
        assert_eq!(synthetic_emissions, 1);
        sequence.set(29);
        assert_eq!(
            guard.attempt_with(
                target,
                || true,
                || sequence.get(),
                |_| panic!("A later clipboard change must never reach the emitter."),
            ),
            PasteOutcome::Blocked(PasteBlock::ClipboardChanged {
                expected: 28,
                actual: 29
            })
        );
        assert_eq!(synthetic_emissions, 1);
    }

    #[test]
    fn typed_guard_reasons_use_no_ports_and_preserve_lazy_order_and_sticky_focus() {
        let target = Target {
            window: 1,
            focus: 2,
            thread: 3,
            process: 4,
        };
        let mut guard = DesktopGuard::new(target);
        assert_eq!(
            guard.preflight(
                target,
                || true,
                || panic!("No published sequence: never read clipboard metadata.")
            ),
            Err(PasteBlock::ClipboardNotPublished)
        );
        guard.mark_clipboard(12);
        assert_eq!(
            guard.preflight(
                target,
                || false,
                || panic!("Held modifiers: never read clipboard metadata.")
            ),
            Err(PasteBlock::ModifiersHeld)
        );
        assert_eq!(
            guard.preflight(target, || true, || 13),
            Err(PasteBlock::ClipboardChanged {
                expected: 12,
                actual: 13
            })
        );
        assert_eq!(guard.preflight(target, || true, || 12), Ok(()));
        let changed = Target { focus: 9, ..target };
        assert_eq!(
            guard.preflight(
                changed,
                || panic!("Focus changed: never query modifiers."),
                || panic!("Focus changed: no clipboard query.")
            ),
            Err(PasteBlock::FocusChanged)
        );
        assert_eq!(
            guard.preflight(target, || true, || 12),
            Err(PasteBlock::FocusChanged)
        );
        assert_eq!(
            PasteBlock::FocusChanged.label(),
            "focus_window_thread_or_process_changed"
        );
        assert_eq!(PasteBlock::ModifiersHeld.label(), "modifiers_not_released");
        assert_eq!(
            PasteBlock::ClipboardNotPublished.label(),
            "clipboard_sequence_not_recorded"
        );
        assert_eq!(
            PasteBlock::ClipboardChanged {
                expected: 12,
                actual: 13
            }
            .label(),
            "clipboard_sequence_changed"
        );
        for (current, released, published, sequence, reason) in [
            (changed, true, true, 12, PasteBlock::FocusChanged),
            (target, false, true, 12, PasteBlock::ModifiersHeld),
            (target, true, false, 12, PasteBlock::ClipboardNotPublished),
            (
                target,
                true,
                true,
                13,
                PasteBlock::ClipboardChanged {
                    expected: 12,
                    actual: 13,
                },
            ),
        ] {
            let mut gate = DesktopGuard::new(target);
            if published {
                gate.mark_clipboard(12);
            }
            assert_eq!(
                gate.attempt_with(
                    current,
                    || released,
                    || sequence,
                    |_| panic!("A rejected guard must never reach an input emitter.")
                ),
                PasteOutcome::Blocked(reason)
            );
        }
    }

    #[test]
    fn incomplete_native_emission_is_not_a_guard_rejection_or_complete_paste() {
        for accepted in 0..=4 {
            let mut calls = Vec::new();
            let outcome = emit_paste_with(|inputs| {
                calls.push(
                    inputs
                        .iter()
                        .map(|input| unsafe {
                            (input.Anonymous.ki.wVk.0, input.Anonymous.ki.dwFlags)
                        })
                        .collect::<Vec<_>>(),
                );
                if calls.len() == 1 {
                    accepted
                } else {
                    inputs.len() as u32
                }
            });
            assert_eq!(
                calls[0].iter().map(|(key, _)| *key).collect::<Vec<_>>(),
                [0x11, 0x56, 0x56, 0x11]
            );
            match accepted {
                4 => {
                    assert_eq!(outcome, PasteOutcome::Sent { events: 4 });
                    assert_eq!(calls.len(), 1);
                }
                0 => {
                    assert_eq!(
                        outcome,
                        PasteOutcome::Incomplete {
                            events: 0,
                            expected: 4,
                            release_events: None
                        }
                    );
                    assert_eq!(calls.len(), 1);
                }
                _ => {
                    assert_eq!(
                        outcome,
                        PasteOutcome::Incomplete {
                            events: accepted,
                            expected: 4,
                            release_events: Some(2)
                        }
                    );
                    assert_eq!(calls.len(), 2);
                    assert_eq!(calls[1], [(0x56, KEYEVENTF_KEYUP), (0x11, KEYEVENTF_KEYUP)]);
                }
            }
        }
        let mut count = 0;
        let cleanup_failed = emit_paste_with(|_| {
            count += 1;
            if count == 1 { 3 } else { 1 }
        });
        assert_eq!(
            cleanup_failed,
            PasteOutcome::Incomplete {
                events: 3,
                expected: 4,
                release_events: Some(1)
            }
        );
        assert_eq!(
            count, 2,
            "Cleanup must not retry the paste or release indefinitely."
        );
    }
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
