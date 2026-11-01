use crate::NativeError;
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, ReleaseMutex, SetEvent, WaitForSingleObject,
};
use windows::core::PCWSTR;

pub struct CompatibleInstance {
    mutex: HANDLE,
    activate: HANDLE,
    quit: HANDLE,
    pub primary: bool,
}

pub enum InstanceEvent {
    Activate,
    Quit,
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            eprintln!("Compatible instance handle cleanup: {error}");
        }
    }
}
fn name(base: &str, suffix: &str) -> Result<Vec<u16>, NativeError> {
    if base.is_empty()
        || base.len() > 128
        || !base
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'.' | b'-' | b'_'))
    {
        return Err(NativeError::InvalidArgument(
            "Invalid compatible instance namespace.",
        ));
    }
    Ok(format!("Local\\{base}.{suffix}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect())
}

impl CompatibleInstance {
    pub fn acquire(base: &str) -> Result<Self, NativeError> {
        let mutex_name = name(base, "mutex")?;
        let mutex = Handle(
            unsafe { CreateMutexW(None, true, PCWSTR(mutex_name.as_ptr())) }.map_err(|error| {
                NativeError::Api("Create compatible single-instance mutex", error)
            })?,
        );
        let primary = unsafe { GetLastError() } != ERROR_ALREADY_EXISTS;
        let activate_name = name(base, "activate")?;
        let activate = Handle(
            unsafe { CreateEventW(None, false, false, PCWSTR(activate_name.as_ptr())) }
                .map_err(|error| NativeError::Api("Create compatible activation event", error))?,
        );
        let quit_name = name(base, "quit")?;
        let quit = Handle(
            unsafe { CreateEventW(None, false, false, PCWSTR(quit_name.as_ptr())) }
                .map_err(|error| NativeError::Api("Create compatible quit event", error))?,
        );
        let result = Self {
            mutex: mutex.0,
            activate: activate.0,
            quit: quit.0,
            primary,
        };
        std::mem::forget(mutex);
        std::mem::forget(activate);
        std::mem::forget(quit);
        Ok(result)
    }
    pub fn activate(&self) -> Result<(), NativeError> {
        unsafe { SetEvent(self.activate) }
            .map_err(|error| NativeError::Api("Signal compatible activation", error))
    }
    pub fn request_quit(&self) -> Result<(), NativeError> {
        unsafe { SetEvent(self.quit) }
            .map_err(|error| NativeError::Api("Signal compatible quit", error))
    }
    pub fn wait_for_exit(&self, timeout: Duration) -> Result<(), NativeError> {
        if self.primary {
            return Ok(());
        }
        let waited = unsafe {
            WaitForSingleObject(
                self.mutex,
                timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32,
            )
        };
        if matches!(waited.0, 0 | 0x80) {
            unsafe { ReleaseMutex(self.mutex) }
                .map_err(|error| NativeError::Api("Release compatible wait ownership", error))
        } else {
            Err(NativeError::Verification(
                "The existing VoicePrompt did not shut down within the upgrade timeout. No force-kill is allowed.",
            ))
        }
    }
    pub fn poll(&self) -> Result<Option<InstanceEvent>, NativeError> {
        for (handle, event) in [
            (self.quit, InstanceEvent::Quit),
            (self.activate, InstanceEvent::Activate),
        ] {
            let result = unsafe { WaitForSingleObject(handle, 0) };
            match result.0 {
                0 => return Ok(Some(event)),
                258 => {}
                _ => {
                    return Err(NativeError::Api(
                        "Poll compatible instance event",
                        windows::core::Error::from_thread(),
                    ));
                }
            }
        }
        Ok(None)
    }
}
impl Drop for CompatibleInstance {
    fn drop(&mut self) {
        if self.primary {
            if let Err(error) = unsafe { ReleaseMutex(self.mutex) } {
                eprintln!("Compatible instance ownership cleanup: {error}");
            }
        }
        for handle in [self.activate, self.quit, self.mutex] {
            if let Err(error) = unsafe { CloseHandle(handle) } {
                eprintln!("Compatible instance handle cleanup: {error}");
            }
        }
    }
}
