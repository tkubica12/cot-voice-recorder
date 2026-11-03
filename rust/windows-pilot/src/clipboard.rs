use std::mem::size_of;

use voiceprompt_client_protocol::delivery::ClipboardError;
use windows::Win32::Foundation::{
    ERROR_SUCCESS, GetLastError, GlobalFree, HANDLE, HGLOBAL, HWND, SetLastError,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardOwner, GetClipboardSequenceNumber, OpenClipboard,
    SetClipboardData,
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

struct OpenedClipboard {
    open: bool,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl OpenedClipboard {
    fn close(&mut self) -> Result<(), ClipboardError> {
        unsafe { CloseClipboard() }.map_err(|error| {
            eprintln!("Clipboard close failed; publication cannot be verified: {error}");
            ClipboardError::Platform
        })?;
        self.open = false;
        Ok(())
    }
}

impl Drop for OpenedClipboard {
    fn drop(&mut self) {
        if self.open {
            if let Err(error) = unsafe { CloseClipboard() } {
                eprintln!("CloseClipboard cleanup failed: {error}");
            }
        }
    }
}

pub(crate) fn committed_sequence(
    close: impl FnOnce() -> Result<(), ClipboardError>,
    sequence: impl FnOnce() -> u32,
    still_owned: impl FnOnce() -> Result<bool, ClipboardError>,
) -> Result<u32, ClipboardError> {
    close()?;
    let sequence = sequence();
    if sequence == 0 {
        eprintln!("Clipboard publication sequence is unavailable after close.");
        return Err(ClipboardError::Platform);
    }
    if !still_owned()? {
        eprintln!("Clipboard ownership changed after publication; copying was not verified.");
        return Err(ClipboardError::Platform);
    }
    Ok(sequence)
}

fn unicode_text(text: &str) -> Result<Vec<u16>, ClipboardError> {
    if text.is_empty() || text.len() > 16 * 1024 * 1024 || text.contains('\0') {
        return Err(ClipboardError::InvalidText);
    }
    Ok(text.encode_utf16().chain(std::iter::once(0)).collect())
}

pub(crate) fn write(owner: HWND, text: &str) -> Result<(), ClipboardError> {
    write_with_sequence(owner, text).map(|_| ())
}

pub(crate) fn write_with_sequence(owner: HWND, text: &str) -> Result<u32, ClipboardError> {
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
    let mut opened = OpenedClipboard {
        open: true,
        _thread: std::marker::PhantomData,
    };
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
    committed_sequence(
        || opened.close(),
        || unsafe { GetClipboardSequenceNumber() },
        || match unsafe { GetClipboardOwner() } {
            Ok(current) => Ok(current == owner),
            Err(error) => {
                if error.code().0 == 0 {
                    eprintln!("Clipboard has no verified owner after publication.");
                } else {
                    eprintln!("Clipboard ownership metadata is unavailable: {error}");
                }
                Err(ClipboardError::Platform)
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returned_sequence_is_sampled_after_completed_close_without_clipboard_ports() {
        let sequence = std::cell::Cell::new(25);
        let operations = std::cell::RefCell::new(Vec::new());
        let published = committed_sequence(
            || {
                operations.borrow_mut().push("close");
                sequence.set(28);
                Ok(())
            },
            || {
                operations.borrow_mut().push("sequence");
                sequence.get()
            },
            || {
                operations.borrow_mut().push("ownership");
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(published, 28);
        assert_eq!(*operations.borrow(), ["close", "sequence", "ownership"]);
    }

    #[test]
    fn failed_close_unavailable_sequence_and_changed_owner_are_never_publication_success() {
        assert_eq!(
            committed_sequence(
                || Err(ClipboardError::Platform),
                || panic!("Failed close must not vend a sequence."),
                || panic!("Failed close must not query ownership."),
            ),
            Err(ClipboardError::Platform)
        );
        assert_eq!(
            committed_sequence(
                || Ok(()),
                || 0,
                || panic!("Missing sequence must not query ownership."),
            ),
            Err(ClipboardError::Platform)
        );
        assert_eq!(
            committed_sequence(|| Ok(()), || 28, || Ok(false),),
            Err(ClipboardError::Platform)
        );
        assert_eq!(
            committed_sequence(|| Ok(()), || 28, || Err(ClipboardError::Platform),),
            Err(ClipboardError::Platform)
        );
    }

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
