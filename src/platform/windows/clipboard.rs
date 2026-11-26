use std::{io, mem, ptr, thread, time::Duration};

use clipserve::{capture::prepare_image, store::StoredImage};
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

    pub fn text(text: &str) -> io::Result<Self> {
        let bytes: Vec<u8> = text
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        Ok(Self::new(CF_UNICODETEXT.0 as u32, &bytes)?)
    }

    pub fn image(png: &[u8]) -> io::Result<Self> {
        Ok(Self::new(CF_DIBV5.0 as u32, &super::bitmap::dibv5(png)?)?)
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
    pub fn new(owner: HWND) -> io::Result<Self> {
        // The owner lives on the thread that opens and closes the clipboard.
        unsafe { OpenClipboard(Some(owner))? };
        Ok(Self)
    }

    pub fn replace(&self, data: Data) -> Result<u32, String> {
        // The caller checked the sequence under this open transaction and prepared the handle.
        unsafe {
            EmptyClipboard()
                .and_then(|()| SetClipboardData(data.format, Some(HANDLE(data.memory.0))))
                .map_err(|_| "Windows could not replace the clipboard contents.".to_owned())?;
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

pub fn read_image(expected: u32) -> Option<io::Result<StoredImage>> {
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
                return Some(prepare_image(data.width, data.height, data.bytes));
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
