use voiceprompt_client_protocol::hotkeys::{Gesture, PressGate};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, RegisterHotKey, UnregisterHotKey,
};

use crate::NativeError;
use crate::desktop::key_down;

const CANCEL_ID: i32 = 0x563f;

#[derive(Clone, Copy)]
struct Registration {
    id: i32,
    gesture: Gesture,
}

pub enum HotkeyEvent {
    HoldStart,
    HoldStop,
    Toggle,
    Cancel,
}

pub struct Hotkeys {
    window: HWND,
    hold: Option<Registration>,
    toggle: Option<Registration>,
    next_id: i32,
    cancel: bool,
    hold_active: bool,
    hold_gate: PressGate,
    toggle_gate: PressGate,
    cancel_gate: PressGate,
}

impl Hotkeys {
    pub fn new(window: HWND) -> Self {
        Self {
            window,
            hold: None,
            toggle: None,
            next_id: 0x5640,
            cancel: false,
            hold_active: false,
            hold_gate: PressGate::default(),
            toggle_gate: PressGate::default(),
            cancel_gate: PressGate::default(),
        }
    }

    pub fn configure(
        &mut self,
        enabled: bool,
        hold: &str,
        toggle: &str,
    ) -> Result<(), NativeError> {
        if !enabled {
            for registration in [self.hold.take(), self.toggle.take()].into_iter().flatten() {
                self.unregister(registration.id);
            }
            self.hold_active = false;
            return Ok(());
        }
        let hold = Gesture::parse(hold)?;
        let toggle = Gesture::parse(toggle)?;
        if hold == toggle {
            return Err(NativeError::InvalidArgument(
                "Hold and toggle shortcuts must differ.",
            ));
        }
        if self
            .hold
            .is_some_and(|registration| registration.gesture == hold)
            && self
                .toggle
                .is_some_and(|registration| registration.gesture == toggle)
        {
            return Ok(());
        }
        let mut staged = Vec::new();
        let result = (|| {
            let next_hold = self.stage(hold, &mut staged)?;
            let next_toggle = self.stage(toggle, &mut staged)?;
            Ok::<_, NativeError>((next_hold, next_toggle))
        })();
        let (next_hold, next_toggle) = match result {
            Ok(result) => result,
            Err(error) => {
                for registration in staged {
                    self.unregister(registration.id);
                }
                return Err(error);
            }
        };
        for old in [self.hold, self.toggle].into_iter().flatten() {
            if old.id != next_hold.id && old.id != next_toggle.id {
                self.unregister(old.id);
            }
        }
        if self
            .hold
            .is_none_or(|old| old.gesture.key != next_hold.gesture.key)
        {
            self.hold_gate.observe(false);
        }
        if self
            .toggle
            .is_none_or(|old| old.gesture.key != next_toggle.gesture.key)
        {
            self.toggle_gate.observe(false);
        }
        if key_down(hold.key) {
            self.hold_gate.press();
        }
        if key_down(toggle.key) {
            self.toggle_gate.press();
        }
        self.hold = Some(next_hold);
        self.toggle = Some(next_toggle);
        Ok(())
    }

    fn stage(
        &mut self,
        gesture: Gesture,
        staged: &mut Vec<Registration>,
    ) -> Result<Registration, NativeError> {
        if let Some(old) = [self.hold, self.toggle]
            .into_iter()
            .flatten()
            .find(|old| old.gesture == gesture)
        {
            return Ok(old);
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .filter(|value| *value < 0xbfff)
            .ok_or(NativeError::Verification(
                "Hotkey registration IDs exhausted.",
            ))?;
        unsafe {
            RegisterHotKey(
                Some(self.window),
                id,
                HOT_KEY_MODIFIERS(gesture.modifiers | 0x4000),
                u32::from(gesture.key),
            )
        }
        .map_err(|error| NativeError::Api("RegisterHotKey (choose an unused shortcut)", error))?;
        let registration = Registration { id, gesture };
        staged.push(registration);
        Ok(registration)
    }

    fn unregister(&self, id: i32) {
        if let Err(error) = unsafe { UnregisterHotKey(Some(self.window), id) } {
            eprintln!("Hotkey unregister failed: {error}");
        }
    }

    pub fn enable_cancel(&mut self, enabled: bool) -> Result<(), NativeError> {
        if self.cancel == enabled {
            return Ok(());
        }
        if enabled {
            unsafe {
                RegisterHotKey(
                    Some(self.window),
                    CANCEL_ID,
                    HOT_KEY_MODIFIERS(0x4000),
                    0x1b,
                )
            }
            .map_err(|error| {
                NativeError::Api(
                    "Escape shortcut unavailable; dictation cannot safely start",
                    error,
                )
            })?;
        } else {
            self.unregister(CANCEL_ID);
        }
        self.cancel = enabled;
        Ok(())
    }

    pub fn message(&mut self, id: i32, value: usize) -> Option<HotkeyEvent> {
        if self.cancel && id == CANCEL_ID {
            let gesture = Gesture {
                key: 0x1b,
                modifiers: 0,
            };
            return (gesture.matches_message(value) && key_down(0x1b) && self.cancel_gate.press())
                .then_some(HotkeyEvent::Cancel);
        }
        let hold = self.hold?;
        let toggle = self.toggle?;
        let is_hold = id == hold.id;
        let registration = if is_hold {
            hold
        } else if id == toggle.id {
            toggle
        } else {
            return None;
        };
        if !registration.gesture.matches_message(value) || !key_down(registration.gesture.key) {
            return None;
        }
        let gate = if is_hold {
            &mut self.hold_gate
        } else {
            &mut self.toggle_gate
        };
        let first = gate.press();
        if hold.gesture.key == toggle.gesture.key {
            if is_hold {
                self.toggle_gate.press();
            } else {
                self.hold_gate.press();
            }
        }
        if !first || !registration.gesture.chord_down(key_down) {
            return None;
        }
        if is_hold {
            self.hold_active = true;
            Some(HotkeyEvent::HoldStart)
        } else {
            Some(HotkeyEvent::Toggle)
        }
    }

    pub fn poll(&mut self) -> Option<HotkeyEvent> {
        self.hold_gate
            .observe(self.hold.is_some_and(|hold| key_down(hold.gesture.key)));
        self.toggle_gate.observe(
            self.toggle
                .is_some_and(|toggle| key_down(toggle.gesture.key)),
        );
        self.cancel_gate.observe(key_down(0x1b));
        if self.hold_active
            && self
                .hold
                .is_none_or(|hold| !hold.gesture.chord_down(key_down))
        {
            self.hold_active = false;
            return Some(HotkeyEvent::HoldStop);
        }
        None
    }
    pub fn needs_poll(&self) -> bool {
        self.hold_gate.held()
            || self.toggle_gate.held()
            || self.cancel_gate.held()
            || self.hold_active
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        if let Err(error) = self.configure(false, "", "") {
            eprintln!("Hotkey cleanup: {error}");
        }
        if let Err(error) = self.enable_cancel(false) {
            eprintln!("Escape cleanup: {error}");
        }
    }
}
