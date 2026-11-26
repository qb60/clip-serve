use std::{cell::Cell, error::Error, io, mem::size_of, rc::Rc, sync::mpsc};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HLOCAL, HWND,
            LPARAM, LRESULT, LocalFree, WPARAM,
        },
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::{
            DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener},
            Diagnostics::Debug::OutputDebugStringW,
            LibraryLoader::GetModuleHandleW,
            Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken},
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
            KillTimer, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MSG, MessageBoxW, PostMessageW,
            RegisterClassW, RegisterWindowMessageW, SetTimer, TranslateMessage, WM_APP,
            WM_CLIPBOARDUPDATE, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
        },
    },
    core::{HSTRING, PCWSTR, PWSTR, w},
};

use crate::app::{App, Command};

pub mod autostart;
mod bitmap;
pub mod clipboard;

pub const EXECUTABLE_NAME: &str = "ClipServe.exe";
pub const AUTOSTART_LABEL: &str = "Start with Windows";

const WAKE: u32 = WM_APP + 1;
const RETRY: usize = 1;

thread_local! {
    static CLIPBOARD_CHANGED: Cell<bool> = const { Cell::new(false) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static TRAY_RESTARTED: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone)]
pub struct Wake {
    tx: mpsc::Sender<Command>,
    window: usize,
}

impl Wake {
    pub fn send(&self, command: Command) {
        if self.tx.send(command).is_ok() {
            // The receiver and HWND belong to the tray thread. PostMessage copies the values.
            unsafe {
                if PostMessageW(Some(HWND(self.window as _)), WAKE, WPARAM(0), LPARAM(0)).is_err() {
                    log::warn!("Could not wake the tray thread.");
                }
            }
        }
    }
}

struct DebugLog;

impl log::Log for DebugLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn && metadata.target().starts_with("clipserve::")
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let message =
                HSTRING::from(format!("ClipServe {}: {}\n", record.level(), record.args()));
            // OutputDebugString consumes the string during the call.
            unsafe { OutputDebugStringW(PCWSTR(message.as_ptr())) };
        }
    }

    fn flush(&self) {}
}

pub fn show_error(message: &str) {
    dialog(None, message);
}

pub fn error_detail(err: &(dyn Error + 'static)) -> String {
    if let Some(err) = err.downcast_ref::<windows::core::Error>() {
        format!("Windows error {}.", err.code())
    } else if let Some(err) = err.downcast_ref::<io::Error>()
        && let Some(code) = err.raw_os_error()
    {
        format!("{} (Windows error {code}).", err.kind())
    } else {
        err.to_string()
    }
}

fn dialog(owner: Option<HWND>, message: &str) {
    let message = HSTRING::from(message);
    // The optional dialog owner is on the calling tray thread and the string outlives the call.
    unsafe {
        let _ = MessageBoxW(
            owner,
            PCWSTR(message.as_ptr()),
            w!("ClipServe"),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // This guard owns a real kernel handle, never a pseudohandle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn instance() -> windows::core::Result<Option<Handle>> {
    // Windows returns the SID in a caller-owned, aligned buffer. The converted SID is LocalFree'd.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let token = Handle(token);
        let mut size = 0;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut size);
        if size < size_of::<TOKEN_USER>() as u32 {
            return Err(windows::core::Error::from_thread());
        }
        let mut buffer = vec![0_usize; (size as usize).div_ceil(size_of::<usize>())];
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )?;
        let user = buffer.as_ptr().cast::<TOKEN_USER>().read();
        let mut sid = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid)?;
        let sid_text = sid.to_string();
        LocalFree(Some(HLOCAL(sid.as_ptr().cast())));
        let name = HSTRING::from(format!(r"Global\ClipServe-{}", sid_text?));
        let handle = Handle(CreateMutexW(None, false, PCWSTR(name.as_ptr()))?);
        let already_exists = GetLastError() == ERROR_ALREADY_EXISTS;
        Ok((!already_exists).then_some(handle))
    }
}

struct ListenerWindow(HWND);

impl ListenerWindow {
    fn new() -> windows::core::Result<Self> {
        // This thread owns the window and pumps its queue until it destroys the window.
        unsafe {
            let instance = HINSTANCE(GetModuleHandleW(None)?.0);
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: w!("ClipServeClipboardListener"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(windows::core::Error::from_thread());
            }
            TASKBAR_CREATED.set(RegisterWindowMessageW(w!("TaskbarCreated")));
            let window = Self(CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.lpszClassName,
                w!("ClipServe"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )?);
            AddClipboardFormatListener(window.0)?;
            Ok(window)
        }
    }
}

impl Drop for ListenerWindow {
    fn drop(&mut self) {
        // Unregister before destroying, on the original window thread.
        unsafe {
            let _ = RemoveClipboardFormatListener(self.0);
            let _ = DestroyWindow(self.0);
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE || TASKBAR_CREATED.get() != 0 && msg == TASKBAR_CREATED.get() {
        if msg == WM_CLIPBOARDUPDATE {
            CLIPBOARD_CHANGED.set(true);
        } else {
            TRAY_RESTARTED.set(true);
        }
        // Sent messages may be dispatched inside GetMessage; post a wake to return to the loop.
        unsafe {
            let _ = PostMessageW(Some(hwnd), WAKE, WPARAM(0), LPARAM(0));
        }
        LRESULT(0)
    } else {
        // This callback receives the original Win32 message for this HWND.
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let _ = log::set_logger(&DebugLog);
    log::set_max_level(log::LevelFilter::Warn);
    let Some(_instance) = instance()? else {
        show_error("ClipServe is already running.");
        return Ok(());
    };
    let exe = std::env::current_exe()?;
    let window = ListenerWindow::new()?;
    let (tx, rx) = mpsc::channel();
    let wake = Wake {
        tx,
        window: window.0.0 as usize,
    };
    let mut app = App::new(HostWindow(window.0), exe, wake)?;
    loop {
        while let Ok(command) = rx.try_recv() {
            if !app.command(command) {
                return Ok(());
            }
        }
        if CLIPBOARD_CHANGED.replace(false) {
            app.clipboard_changed();
        }
        if TRAY_RESTARTED.replace(false) {
            app.update_tray(true);
        }
        let mut message = MSG::default();
        // Both the window and MSG dispatch belong to this thread.
        unsafe {
            match GetMessageW(&mut message, None, 0, 0).0 {
                -1 => return Err(windows::core::Error::from_thread().into()),
                0 => return Ok(()),
                _ if message.hwnd == window.0
                    && message.message == WM_TIMER
                    && message.wParam.0 == RETRY =>
                {
                    let _ = KillTimer(Some(window.0), RETRY);
                    app.retry_write();
                }
                _ => {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
pub struct HostWindow(HWND);

impl HostWindow {
    pub fn owns_clipboard(self) -> bool {
        clipboard::owned_by(self.0)
    }

    pub fn open_clipboard(self) -> io::Result<clipboard::Opened> {
        clipboard::Opened::new(self.0)
    }

    pub fn arm_retry(self, delay_ms: u32) -> bool {
        // The timer belongs to the tray thread's window.
        unsafe { SetTimer(Some(self.0), RETRY, delay_ms, None) != 0 }
    }

    pub fn cancel_retry(self) {
        // The timer belongs to the tray thread's window.
        unsafe {
            let _ = KillTimer(Some(self.0), RETRY);
        }
    }

    pub fn show_error(self, message: &str) {
        dialog(Some(self.0), message);
    }
}

pub fn warm_up_desktop() {
    #[cfg(clipserve_embedded_shaders)]
    if let Err(err) = gpui_windows::warm_up_shaders() {
        log::warn!("Could not precompile the Direct3D shaders: {err:#}");
    }
}

pub fn desktop_application() -> Result<gpui::Application, Box<dyn Error>> {
    let platform = gpui_windows::WindowsPlatform::new(false)?;
    Ok(gpui::Application::with_platform(Rc::new(platform)))
}
