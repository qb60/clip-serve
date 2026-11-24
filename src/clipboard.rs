use std::{
    io, mem, ptr,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

use clipserve::capture::{Capture, dibv5, encode_png};
use windows::{
    Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardOwner, GetClipboardSequenceNumber,
                OpenClipboard, SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::{CF_DIBV5, CF_UNICODETEXT},
        },
    },
    core::Result as WinResult,
};

pub fn sequence() -> u32 {
    // This query needs no open clipboard or caller-owned handles.
    unsafe { GetClipboardSequenceNumber() }
}

pub fn owned_by(window: HWND) -> bool {
    // The clipboard owner is queried for comparison only.
    unsafe { GetClipboardOwner() }.is_ok_and(|owner| owner == window)
}

pub struct Data {
    format: u32,
    memory: HGLOBAL,
}

// GMEM_MOVEABLE memory is process-owned and unlocked before being sent to the tray thread.
unsafe impl Send for Data {}

impl Data {
    fn new(format: u32, bytes: &[u8]) -> WinResult<Self> {
        // The allocation belongs to Data unless Windows accepts it in SetClipboardData.
        unsafe {
            let value = Self {
                format,
                memory: GlobalAlloc(GMEM_MOVEABLE, bytes.len())?,
            };
            let dest = GlobalLock(value.memory);
            if dest.is_null() {
                return Err(windows::core::Error::from_thread());
            }
            ptr::copy_nonoverlapping(bytes.as_ptr(), dest.cast(), bytes.len());
            // A zero result also means the final successful unlock.
            let _ = GlobalUnlock(value.memory);
            Ok(value)
        }
    }

    pub fn text(text: &str) -> WinResult<Self> {
        let bytes: Vec<u8> = text
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        Self::new(CF_UNICODETEXT.0 as u32, &bytes)
    }

    fn image(png: &[u8]) -> io::Result<Self> {
        Ok(Self::new(CF_DIBV5.0 as u32, &dibv5(png)?)?)
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        // The handle has not been transferred to Windows.
        unsafe {
            let _ = GlobalFree(Some(self.memory));
        }
    }
}

pub struct Opened;

impl Opened {
    pub fn new(owner: HWND) -> WinResult<Self> {
        // The owner lives on the thread that opens and closes the clipboard.
        unsafe { OpenClipboard(Some(owner))? };
        Ok(Self)
    }

    pub fn replace(&self, data: Data) -> WinResult<u32> {
        // The caller checked the sequence under this open transaction and prepared the handle.
        unsafe {
            EmptyClipboard()?;
            SetClipboardData(data.format, Some(HANDLE(data.memory.0)))?;
            mem::forget(data);
            Ok(GetClipboardSequenceNumber())
        }
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        // The clipboard was opened on this thread.
        unsafe {
            if CloseClipboard().is_err() {
                log::error!("Could not close the clipboard.");
            }
        }
    }
}

enum Job {
    Capture(Capture),
    Restore(u32, Arc<[u8]>),
}

pub enum Prepared {
    Capture(Capture, io::Result<Arc<[u8]>>),
    Restore(u32, io::Result<Data>),
}

#[derive(Default)]
struct Pending {
    capture: Option<Capture>,
    restore: Option<(u32, Arc<[u8]>)>,
    stop: bool,
}

pub struct Worker {
    inbox: Arc<(Mutex<Pending>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn start(send: impl Fn(Prepared) + Send + 'static) -> io::Result<Self> {
        let inbox = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let worker_inbox = Arc::clone(&inbox);
        let thread = thread::Builder::new()
            .name("Clipboard images".into())
            .spawn(move || {
                let (pending, ready) = &*worker_inbox;
                loop {
                    let job = {
                        let mut next = ready
                            .wait_while(pending.lock().unwrap(), |next| {
                                !next.stop && next.restore.is_none() && next.capture.is_none()
                            })
                            .unwrap();
                        if next.stop {
                            break;
                        }
                        if let Some((sequence, png)) = next.restore.take() {
                            Job::Restore(sequence, png)
                        } else {
                            Job::Capture(next.capture.take().unwrap())
                        }
                    };
                    match job {
                        Job::Capture(job) => {
                            if let Some(result) = read_image(job.sequence) {
                                send(Prepared::Capture(job, result));
                            }
                        }
                        Job::Restore(expected, png) if sequence() == expected => {
                            send(Prepared::Restore(expected, Data::image(&png)));
                        }
                        Job::Restore(..) => {}
                    }
                }
            })?;
        Ok(Self {
            inbox,
            thread: Some(thread),
        })
    }

    pub fn capture(&self, job: Capture) {
        let (pending, ready) = &*self.inbox;
        pending.lock().unwrap().capture = Some(job);
        ready.notify_one();
    }

    pub fn restore(&self, sequence: u32, png: Arc<[u8]>) {
        let (pending, ready) = &*self.inbox;
        pending.lock().unwrap().restore = Some((sequence, png));
        ready.notify_one();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let (pending, ready) = &*self.inbox;
        pending.lock().unwrap().stop = true;
        ready.notify_one();
        if self.thread.take().unwrap().join().is_err() {
            log::error!("The clipboard worker panicked.");
        }
    }
}

fn read_image(expected: u32) -> Option<io::Result<Arc<[u8]>>> {
    use arboard::Error;

    let delays = [15, 35, 75].into_iter().map(Some).chain([None]);
    for delay in delays {
        if sequence() != expected {
            return None;
        }
        let result = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_image());
        match result {
            Ok(data) => {
                if sequence() != expected {
                    return None;
                }
                return Some(encode_png(data.width, data.height, &data.bytes));
            }
            Err(Error::ContentNotAvailable) => return None,
            Err(Error::ClipboardOccupied | Error::Unknown { .. }) if delay.is_some() => {
                thread::sleep(Duration::from_millis(delay.unwrap()));
            }
            Err(Error::ClipboardOccupied) => {
                return Some(Err(io::Error::other("The clipboard stayed busy.")));
            }
            Err(_) => {
                return Some(Err(io::Error::other(
                    "Windows could not read the clipboard image.",
                )));
            }
        }
    }
    unreachable!("the last attempt always returns");
}
