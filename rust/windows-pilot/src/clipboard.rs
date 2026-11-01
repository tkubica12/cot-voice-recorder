use std::mem::size_of;

use voiceprompt_client_protocol::delivery::ClipboardError;
use windows::Win32::Foundation::{
    ERROR_SUCCESS, GetLastError, GlobalFree, HANDLE, HGLOBAL, HWND, SetLastError,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

struct GlobalBuffer(Option<HGLOBAL>);

impl Drop for GlobalBuffer {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            if let Err(error) = unsafe { GlobalFree(Some(handle)) } {
                eprintln!("Clipboard allocation cleanup failed: {error}");
            }
        }
    }
}

struct OpenedClipboard;

impl Drop for OpenedClipboard {
    fn drop(&mut self) {
        if let Err(error) = unsafe { CloseClipboard() } {
            eprintln!("CloseClipboard failed: {error}");
        }
    }
}

fn unicode_text(text: &str) -> Result<Vec<u16>, ClipboardError> {
    if text.is_empty() || text.len() > 16 * 1024 * 1024 || text.contains('\0') {
        return Err(ClipboardError::InvalidText);
    }
    Ok(text.encode_utf16().chain(std::iter::once(0)).collect())
}

pub(crate) fn write(owner: HWND, text: &str) -> Result<(), ClipboardError> {
    let encoded = unicode_text(text)?;
    let bytes = encoded
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or(ClipboardError::InvalidText)?;
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }.map_err(|error| {
        eprintln!("Clipboard GlobalAlloc failed: {error}");
        ClipboardError::Platform
    })?;
    let mut buffer = GlobalBuffer(Some(handle));
    let memory = unsafe { GlobalLock(handle) };
    if memory.is_null() {
        eprintln!(
            "Clipboard GlobalLock failed: {}",
            windows::core::Error::from_thread()
        );
        return Err(ClipboardError::Platform);
    }
    // The moveable allocation is locked and exactly fits the terminated UTF-16 buffer.
    unsafe {
        std::ptr::copy_nonoverlapping(encoded.as_ptr(), memory.cast::<u16>(), encoded.len());
        SetLastError(ERROR_SUCCESS);
        if GlobalUnlock(handle).is_err() && GetLastError() != ERROR_SUCCESS {
            eprintln!(
                "Clipboard GlobalUnlock failed: {}",
                windows::core::Error::from_thread()
            );
            return Err(ClipboardError::Platform);
        }
    }
    if unsafe { OpenClipboard(Some(owner)) }.is_err() {
        return Err(ClipboardError::Busy);
    }
    let _opened = OpenedClipboard;
    unsafe { EmptyClipboard() }.map_err(|error| {
        eprintln!("EmptyClipboard failed: {error}");
        ClipboardError::Platform
    })?;
    unsafe { SetClipboardData(13, Some(HANDLE(handle.0))) }.map_err(|error| {
        eprintln!("SetClipboardData failed: {error}");
        ClipboardError::Platform
    })?;
    // CF_UNICODETEXT is rendered immediately; only successful publication transfers ownership.
    buffer.0 = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_buffer_is_exact_terminated_utf16_without_touching_system_clipboard() {
        assert_eq!(
            unicode_text("A\u{1f600}\r\n").unwrap(),
            [65, 0xd83d, 0xde00, 13, 10, 0]
        );
        assert_eq!(unicode_text("bad\0text"), Err(ClipboardError::InvalidText));
        assert_eq!(unicode_text(""), Err(ClipboardError::InvalidText));
    }
}
