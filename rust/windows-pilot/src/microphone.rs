use std::fmt;
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use windows::Win32::Media::Audio::{
    HWAVEIN, MIDI_WAVE_OPEN_TYPE, WAVEFORMATEX, WAVEHDR, waveInAddBuffer, waveInClose, waveInOpen,
    waveInPrepareHeader, waveInReset, waveInStart, waveInUnprepareHeader,
};

const BUFFER_BYTES: usize = 1280;
const BUFFER_COUNT: usize = 8;
const WHDR_DONE_FLAG: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureError {
    Device(u32),
    Overflow,
    InvalidBuffer,
    Worker,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Microphone capture failed: {self:?}; saved audio is retained."
        )
    }
}
impl std::error::Error for CaptureError {}

pub struct CaptureBuffer(pub Vec<u8>);
impl Drop for CaptureBuffer {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

pub struct Microphone {
    stop: Arc<AtomicBool>,
    pub error: watch::Receiver<Option<CaptureError>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct Buffer {
    data: Box<[u8; BUFFER_BYTES]>,
    header: Box<WAVEHDR>,
    prepared: bool,
}

struct Device {
    handle: HWAVEIN,
    buffers: Vec<Buffer>,
}

fn mm(result: u32) -> Result<(), CaptureError> {
    if result == 0 {
        Ok(())
    } else {
        Err(CaptureError::Device(result))
    }
}

impl Device {
    fn open() -> Result<Self, CaptureError> {
        let format = WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: 1,
            nSamplesPerSec: 16000,
            nAvgBytesPerSec: 32000,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut handle = HWAVEIN::default();
        // Match the current WaveInEvent capture format without resampling or changing device roles.
        mm(unsafe {
            waveInOpen(
                Some(&mut handle),
                u32::MAX,
                &format,
                None,
                None,
                MIDI_WAVE_OPEN_TYPE(0),
            )
        })?;
        let mut device = Self {
            handle,
            buffers: Vec::with_capacity(BUFFER_COUNT),
        };
        for _ in 0..BUFFER_COUNT {
            let mut data = Box::new([0; BUFFER_BYTES]);
            let header = Box::new(WAVEHDR {
                lpData: windows::core::PSTR(data.as_mut_ptr()),
                dwBufferLength: BUFFER_BYTES as u32,
                ..Default::default()
            });
            device.buffers.push(Buffer {
                data,
                header,
                prepared: false,
            });
            let buffer = device.buffers.last_mut().ok_or(CaptureError::Worker)?;
            mm(unsafe {
                waveInPrepareHeader(handle, buffer.header.as_mut(), size_of::<WAVEHDR>() as u32)
            })?;
            buffer.prepared = true;
            mm(unsafe {
                waveInAddBuffer(handle, buffer.header.as_mut(), size_of::<WAVEHDR>() as u32)
            })?;
        }
        mm(unsafe { waveInStart(handle) })?;
        Ok(device)
    }

    fn reset(&mut self) -> Result<(), CaptureError> {
        mm(unsafe { waveInReset(self.handle) })
    }

    fn drain(
        &mut self,
        next: &mut usize,
        output: &mpsc::Sender<CaptureBuffer>,
        requeue: bool,
    ) -> Result<(), CaptureError> {
        for _ in 0..self.buffers.len() {
            let buffer = &mut self.buffers[*next];
            // WinMM owns the prepared header until DONE or a successful synchronous reset.
            let flags =
                unsafe { std::ptr::read_volatile(std::ptr::addr_of!(buffer.header.dwFlags)) };
            if flags & WHDR_DONE_FLAG == 0 {
                break;
            }
            let count = unsafe {
                std::ptr::read_volatile(std::ptr::addr_of!(buffer.header.dwBytesRecorded))
            } as usize;
            if count > BUFFER_BYTES || count % 2 != 0 {
                return Err(CaptureError::InvalidBuffer);
            }
            if count > 0 {
                enqueue(output, &buffer.data[..count])?;
            }
            buffer.data.fill(0);
            if requeue {
                mm(unsafe {
                    waveInAddBuffer(
                        self.handle,
                        buffer.header.as_mut(),
                        size_of::<WAVEHDR>() as u32,
                    )
                })?;
            } else {
                buffer.header.dwFlags &= !WHDR_DONE_FLAG;
            }
            *next = (*next + 1) % self.buffers.len();
        }
        Ok(())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if let Err(error) = self.reset() {
            eprintln!("Microphone reset during cleanup: {error}");
        }
        for buffer in &mut self.buffers {
            if buffer.prepared {
                match mm(unsafe {
                    waveInUnprepareHeader(
                        self.handle,
                        buffer.header.as_mut(),
                        size_of::<WAVEHDR>() as u32,
                    )
                }) {
                    Ok(()) => buffer.prepared = false,
                    Err(error) => eprintln!("Microphone unprepare during cleanup: {error}"),
                }
            }
        }
        let retained = self.buffers.iter().any(|buffer| buffer.prepared);
        if retained {
            // A broken driver still owns these pointers. Retain allocations instead of freeing live memory.
            let buffers = std::mem::take(&mut self.buffers);
            for buffer in buffers {
                if buffer.prepared {
                    std::mem::forget(buffer);
                }
            }
            eprintln!(
                "Microphone cleanup retained driver-owned buffers; device could not close safely."
            );
        } else if let Err(error) = mm(unsafe { waveInClose(self.handle) }) {
            eprintln!("Microphone close during cleanup: {error}");
        }
    }
}

fn enqueue(output: &mpsc::Sender<CaptureBuffer>, pcm: &[u8]) -> Result<(), CaptureError> {
    output
        .try_send(CaptureBuffer(pcm.to_vec()))
        .map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => CaptureError::Overflow,
            mpsc::error::TrySendError::Closed(_) => CaptureError::Worker,
        })
}

impl Microphone {
    pub fn start() -> Result<(Self, mpsc::Receiver<CaptureBuffer>), CaptureError> {
        let (output, receiver) = mpsc::channel(50);
        let (error_sender, error) = watch::channel(None);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (started, ready) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("voiceprompt-microphone".into())
            .spawn(move || {
                let mut device = match Device::open() {
                    Ok(device) => device,
                    Err(error) => {
                        let _ = started.send(Err(error));
                        return;
                    }
                };
                if started.send(Ok(())).is_err() {
                    return;
                }
                let mut next = 0;
                let result = loop {
                    if stopped.load(Ordering::Acquire) {
                        break device
                            .reset()
                            .and_then(|()| device.drain(&mut next, &output, false));
                    }
                    if let Err(error) = device.drain(&mut next, &output, true) {
                        break Err(error);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                };
                if let Err(error) = result {
                    eprintln!("Microphone: {error}");
                    error_sender.send_replace(Some(error));
                }
            })
            .map_err(|_| CaptureError::Worker)?;
        match ready.recv() {
            Ok(Ok(())) => Ok((
                Self {
                    stop,
                    error,
                    thread: Some(thread),
                },
                receiver,
            )),
            result => {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                Err(match result {
                    Ok(Err(error)) => error,
                    _ => CaptureError::Worker,
                })
            }
        }
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn finish(mut self) -> Result<(), CaptureError> {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| CaptureError::Worker)?;
        }
        match *self.error.borrow() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for Microphone {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                eprintln!("Microphone worker failed during shutdown.");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_capture_queue_preserves_order_and_overload_is_not_a_silent_drop() {
        let (sender, mut receiver) = mpsc::channel(2);
        enqueue(&sender, &[1, 0]).unwrap();
        enqueue(&sender, &[2, 0]).unwrap();
        assert_eq!(enqueue(&sender, &[3, 0]), Err(CaptureError::Overflow));
        assert_eq!(receiver.try_recv().unwrap().0, [1, 0]);
        assert_eq!(receiver.try_recv().unwrap().0, [2, 0]);
    }
}
