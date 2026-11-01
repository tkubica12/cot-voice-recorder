use crate::NativeError;
use std::path::Path;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ,
    REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        let result = unsafe { RegCloseKey(self.0) };
        if result.0 != 0 {
            eprintln!("Autostart registry close failed: {}", result.0);
        }
    }
}

fn check(operation: &'static str, result: WIN32_ERROR) -> Result<(), NativeError> {
    result
        .ok()
        .map_err(|error| NativeError::Api(operation, error))
}

pub(crate) fn command(
    executable: &Path,
    state: &Path,
    config: Option<&Path>,
    installed: bool,
) -> Result<String, NativeError> {
    fn quote(path: &Path) -> Result<String, NativeError> {
        let path = path.to_str().ok_or(NativeError::InvalidArgument(
            "Autostart paths require valid Unicode.",
        ))?;
        if path.contains('"') || path.contains('\0') {
            return Err(NativeError::InvalidArgument("Invalid autostart path."));
        }
        Ok(format!("\"{}\"", path.trim_end_matches('\\')))
    }
    if installed {
        return Ok(format!("{} --installed", quote(executable)?));
    }
    let mut command = format!("{} --state-directory {}", quote(executable)?, quote(state)?);
    if let Some(config) = config {
        command.push_str(&format!(" --oauth-config {}", quote(config)?));
    }
    Ok(command)
}

pub(crate) fn set(enabled: bool, command: &str, installed: bool) -> Result<(), NativeError> {
    let mut key = HKEY::default();
    check("Open per-user autostart registry", unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    })?;
    let key = Key(key);
    let name = if installed {
        w!("VoicePrompt")
    } else {
        w!("VoicePromptRustPilot")
    };
    if enabled {
        let encoded: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes: Vec<u8> = encoded
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        check("Save per-user autostart", unsafe {
            RegSetValueExW(key.0, name, None, REG_SZ, Some(&bytes))
        })
    } else {
        let result = unsafe { RegDeleteValueW(key.0, name) };
        if result == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            check("Remove per-user autostart", result)
        }
    }
}

pub(crate) fn enabled(installed: bool) -> Result<bool, NativeError> {
    let mut key = HKEY::default();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            None,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if opened == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    check("Inspect per-user autostart registry", opened)?;
    let key = Key(key);
    let mut kind = Default::default();
    let mut bytes = 0;
    let name = if installed {
        w!("VoicePrompt")
    } else {
        w!("VoicePromptRustPilot")
    };
    let result =
        unsafe { RegQueryValueExW(key.0, name, None, Some(&mut kind), None, Some(&mut bytes)) };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    check("Read per-user autostart state", result)?;
    if kind != REG_SZ && kind != REG_EXPAND_SZ {
        eprintln!(
            "Autostart Run value has a non-string type; it is not a supported launch command."
        );
        return Ok(false);
    }
    Ok(bytes > 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn autostart_arguments_preserve_explicit_state_and_only_one_installed_mechanism() {
        assert_eq!(
            command(
                Path::new("D:\\program files\\VoicePrompt.exe"),
                Path::new("D:\\state"),
                None,
                true
            )
            .unwrap(),
            "\"D:\\program files\\VoicePrompt.exe\" --installed"
        );
        assert!(
            command(
                Path::new("D:\\app.exe"),
                Path::new("D:\\fixture\\"),
                None,
                false
            )
            .unwrap()
            .ends_with("\"D:\\fixture\"")
        );
        assert!(
            command(
                Path::new("D:\\bad\".exe"),
                Path::new("D:\\state"),
                None,
                false
            )
            .is_err()
        );
    }
}
