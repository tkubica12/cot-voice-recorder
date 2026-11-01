use crate::ProtocolError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gesture {
    pub key: u16,
    pub modifiers: u32,
}

impl Gesture {
    pub fn parse(text: &str) -> Result<Self, ProtocolError> {
        let mut parts = text.split('+').map(str::trim).peekable();
        let mut modifiers = 0;
        let mut key = None;
        while let Some(part) = parts.next() {
            let modifier = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => Some(2),
                "alt" => Some(1),
                "shift" => Some(4),
                "win" | "windows" => Some(8),
                _ => None,
            };
            if let Some(flag) = modifier {
                if key.is_some() || modifiers & flag != 0 {
                    return Err(invalid());
                }
                modifiers |= flag;
            } else {
                if key.is_some() || parts.peek().is_some() {
                    return Err(invalid());
                }
                key = key_code(part);
            }
        }
        let key = key.ok_or_else(invalid)?;
        if modifiers & 3 == 0 || matches!(key, 0x1b | 0x10..=0x12 | 0x5b | 0x5c) {
            return Err(invalid());
        }
        Ok(Self { key, modifiers })
    }

    pub fn matches_message(&self, value: usize) -> bool {
        (value & 0xffff) as u32 == self.modifiers && ((value >> 16) & 0xffff) as u16 == self.key
    }
    pub fn chord_down(&self, down: impl Fn(u16) -> bool) -> bool {
        down(self.key)
            && (self.modifiers & 2 != 0) == down(0x11)
            && (self.modifiers & 1 != 0) == down(0x12)
            && (self.modifiers & 4 != 0) == down(0x10)
            && (self.modifiers & 8 != 0) == (down(0x5b) || down(0x5c))
    }
}

fn invalid() -> ProtocolError {
    ProtocolError::InvalidField("dictation shortcut")
}

fn key_code(value: &str) -> Option<u16> {
    let upper = value.to_ascii_uppercase();
    if upper.len() == 1 {
        let byte = upper.as_bytes()[0];
        if byte.is_ascii_alphanumeric() {
            return Some(u16::from(byte));
        }
        if let Some(number) = upper
            .strip_prefix('D')
            .and_then(|value| value.parse::<u16>().ok())
        {
            if number <= 9 {
                return Some(0x30 + number);
            }
        }
        if let Some(number) = upper
            .strip_prefix("NUMPAD")
            .and_then(|value| value.parse::<u16>().ok())
        {
            if number <= 9 {
                return Some(0x60 + number);
            }
        }
    }
    if let Some(number) = upper
        .strip_prefix('F')
        .and_then(|value| value.parse::<u16>().ok())
    {
        if (1..=24).contains(&number) {
            return Some(0x70 + number - 1);
        }
    }
    Some(match upper.as_str() {
        "SPACE" => 0x20,
        "TAB" => 9,
        "ENTER" | "RETURN" => 13,
        "BACK" | "BACKSPACE" => 8,
        "INSERT" => 0x2d,
        "DELETE" => 0x2e,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PRIOR" => 0x21,
        "PAGEDOWN" | "NEXT" => 0x22,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "OEM1" | "OEMSEMICOLON" => 0xba,
        "OEMPLUS" => 0xbb,
        "OEMCOMMA" => 0xbc,
        "OEMMINUS" => 0xbd,
        "OEMPERIOD" => 0xbe,
        "OEM2" | "OEMQUESTION" => 0xbf,
        "OEM3" | "OEMTILDE" => 0xc0,
        "OEM4" | "OEMOPENBRACKETS" => 0xdb,
        "OEM5" | "OEMPIPE" => 0xdc,
        "OEM6" | "OEMCLOSEBRACKETS" => 0xdd,
        "OEM7" | "OEMQUOTES" => 0xde,
        "OEM102" | "OEMBACKSLASH" => 0xe2,
        "MULTIPLY" => 0x6a,
        "ADD" => 0x6b,
        "SEPARATOR" => 0x6c,
        "SUBTRACT" => 0x6d,
        "DECIMAL" => 0x6e,
        "DIVIDE" => 0x6f,
        "CAPITAL" | "CAPSLOCK" => 0x14,
        "PAUSE" => 0x13,
        "PRINTSCREEN" | "SNAPSHOT" => 0x2c,
        "APPS" => 0x5d,
        "NUMLOCK" => 0x90,
        "SCROLL" => 0x91,
        "HELP" => 0x2f,
        _ => return None,
    })
}

#[derive(Default)]
pub struct PressGate(bool);
impl PressGate {
    pub fn press(&mut self) -> bool {
        if self.0 {
            false
        } else {
            self.0 = true;
            true
        }
    }
    pub fn observe(&mut self, down: bool) {
        if !down {
            self.0 = false;
        }
    }
    pub fn held(&self) -> bool {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gesture_and_latch_preserve_physical_press_and_exact_modifiers() {
        let hold = Gesture::parse("Ctrl+Alt+Space").unwrap();
        assert_eq!(
            hold,
            Gesture {
                key: 32,
                modifiers: 3
            }
        );
        assert!(hold.matches_message((32 << 16) | 3));
        assert!(!hold.matches_message((32 << 16) | 7));
        assert!(hold.chord_down(|key| matches!(key, 32 | 0x11 | 0x12)));
        assert!(!hold.chord_down(|key| matches!(key, 32 | 0x11 | 0x12 | 0x10)));
        let mut gate = PressGate::default();
        assert!(gate.press());
        gate.observe(true);
        assert!(!gate.press());
        gate.observe(false);
        assert!(gate.press());
        for invalid in [
            "Space",
            "Ctrl+Ctrl+K",
            "Ctrl+Escape",
            "Alt++",
            "Ctrl+unknown",
        ] {
            assert!(Gesture::parse(invalid).is_err());
        }
    }
}
