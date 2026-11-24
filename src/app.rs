use std::{
    cell::Cell,
    error::Error,
    io,
    mem::size_of,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
};

use clipserve::{
    capture::{Capture, CaptureState},
    config::{Config, config_path},
    http::Server,
    store::{SharedStore, Store, StoredImage, url},
};
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

use crate::{
    autostart,
    clipboard::{self, Data, Opened, Prepared, Worker},
    desktop::{Desktop, View},
    settings::Reply,
    tray::{Status, Tray},
};

const WAKE: u32 = WM_APP + 1;
const RETRY: usize = 1;
const RETRY_DELAYS: [u32; 3] = [15, 35, 75];

thread_local! {
    static CLIPBOARD_CHANGED: Cell<bool> = const { Cell::new(false) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static TRAY_RESTARTED: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy)]
pub enum Action {
    ToggleCapture,
    Copy,
    RecentImages,
    Settings,
    ToggleAutostart,
    Exit,
}

pub enum Command {
    Tray(Action),
    Image(ImageAction, String, u32),
    ClearImages,
    Apply(Config, Reply),
    Prepared(Prepared),
    Error(String),
    HttpStopped,
}

pub enum ImageAction {
    Copy,
    Restore,
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
    dialog(None, message, MB_OK | MB_ICONERROR);
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

fn dialog(
    owner: Option<HWND>,
    message: &str,
    flags: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) -> windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT {
    let message = HSTRING::from(message);
    // The optional dialog owner is on the calling tray thread and the string outlives the call.
    unsafe {
        MessageBoxW(
            owner,
            PCWSTR(message.as_ptr()),
            w!("ClipServe"),
            flags | MB_SETFOREGROUND,
        )
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
    let mut app = App::new(window.0, exe, wake)?;
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
                    if let Some((write, attempt)) = app.retry.take() {
                        app.write(write, attempt);
                    }
                }
                _ => {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }
}

enum Write {
    Capture(Capture, StoredImage),
    Copy(u32, String),
    Restore(u32, Data),
}

struct App {
    hwnd: HWND,
    exe: PathBuf,
    config: Config,
    store: SharedStore,
    state: CaptureState,
    server: Option<Server>,
    retry: Option<(Write, usize)>,
    worker: Worker,
    desktop: Desktop,
    tray: Tray,
    wake: Wake,
}

impl App {
    fn new(hwnd: HWND, exe: PathBuf, wake: Wake) -> Result<Self, Box<dyn Error>> {
        let tray = Tray::new(wake.clone())?;
        let (config, config_error) = match Config::load(&config_path(&exe)) {
            Ok(config) => (config, None),
            Err(err) => (
                Config::default(),
                Some(format!(
                    "Could not load or create config.json beside ClipServe.exe. Safe defaults are in use.\n\n{}",
                    error_detail(&err)
                )),
            ),
        };
        let autostart_error = if config_error.is_none() {
            autostart::Change::apply(&exe, config.start_with_windows)
                .err()
                .map(autostart_error)
        } else {
            None
        };
        let worker_wake = wake.clone();
        let store = Arc::new(Mutex::new(Store::new(config.max_images)));
        let desktop = Desktop::start(wake.clone(), Arc::clone(&store))?;
        let mut app = Self {
            hwnd,
            exe,
            config,
            tray,
            store,
            state: CaptureState::new(config.capture_on_startup, clipboard::sequence()),
            server: None,
            retry: None,
            worker: Worker::start(move |job| worker_wake.send(Command::Prepared(job)))?,
            desktop,
            wake,
        };
        let bind_error = match app.bind(config) {
            Ok(server) => {
                app.install(server);
                None
            }
            Err(err) => Some(err),
        };
        app.update_tray(false);
        for err in [config_error, autostart_error, bind_error]
            .into_iter()
            .flatten()
        {
            app.error(&err);
        }
        Ok(app)
    }

    fn command(&mut self, command: Command) -> bool {
        match command {
            Command::Tray(Action::Exit) => return false,
            Command::Tray(action) => self.action(action),
            Command::Image(action, id, sequence) => self.use_image(action, id, sequence),
            Command::ClearImages => {
                self.state
                    .set_enabled(self.state.enabled, clipboard::sequence());
                self.cancel_retry();
                self.store.lock().unwrap().clear();
            }
            Command::Apply(config, reply) => {
                let result = self.apply(config);
                let _ = reply.result.send(result);
                reply.ctx.request_repaint();
            }
            Command::Prepared(Prepared::Capture(job, result)) => {
                if self.state.validate(job, clipboard::sequence()).is_some() {
                    match result {
                        Ok(image) => self.commit(Write::Capture(job, image)),
                        Err(err) => {
                            self.error(&format!("Could not capture the clipboard image.\n\n{err}"))
                        }
                    }
                }
            }
            Command::Prepared(Prepared::Restore(sequence, result)) => {
                if sequence == clipboard::sequence() {
                    match result {
                        Ok(data) => self.commit(Write::Restore(sequence, data)),
                        Err(err) => self.error(&format!("Could not restore the image.\n\n{err}")),
                    }
                }
            }
            Command::Error(message) => self.error(&message),
            Command::HttpStopped => {
                if self.state.endpoint().is_none() {
                    self.update_tray(false);
                    self.error("The HTTP server stopped. Capture is suspended. Open Settings and select Apply to restart it.");
                }
            }
        }
        self.update_tray(false);
        true
    }

    fn action(&mut self, action: Action) {
        match action {
            Action::ToggleCapture => self
                .state
                .set_enabled(!self.state.enabled, clipboard::sequence()),
            Action::Copy => {
                let latest = self.store.lock().unwrap().latest();
                if let Some(entry) = latest {
                    self.use_image(ImageAction::Copy, entry.id, clipboard::sequence());
                }
            }
            Action::RecentImages => {
                if let Err(err) = self.desktop.open(View::RecentImages) {
                    self.error(&format!("Could not open Recent images.\n\n{err}"));
                }
            }
            Action::Settings => {
                if let Err(err) = self.desktop.open(View::Settings(self.config)) {
                    self.error(&format!("Could not open Settings.\n\n{err}"));
                }
            }
            Action::ToggleAutostart => {
                let config = Config {
                    start_with_windows: !self.config.start_with_windows,
                    ..self.config
                };
                if let Err(err) = self.save(config) {
                    self.error(&err);
                } else {
                    self.config = config;
                }
            }
            Action::Exit => unreachable!("Exit is handled by the message loop"),
        }
    }

    fn use_image(&mut self, action: ImageAction, id: String, sequence: u32) {
        if clipboard::sequence() != sequence {
            return;
        }
        let png = self.store.lock().unwrap().get(&id);
        let Some(png) = png else {
            self.error("That image is no longer available.");
            return;
        };
        self.state.set_enabled(self.state.enabled, sequence);
        match action {
            ImageAction::Copy => self.commit(Write::Copy(sequence, id)),
            ImageAction::Restore => {
                self.cancel_retry();
                self.worker.restore(sequence, png);
            }
        }
    }

    fn clipboard_changed(&mut self) {
        let sequence = clipboard::sequence();
        if let Some(job) = self.state.observe(sequence, clipboard::owned_by(self.hwnd)) {
            self.worker.capture(job);
        }
    }

    fn bind(&self, config: Config) -> Result<Server, String> {
        let wake = self.wake.clone();
        Server::prepare(config.endpoint(), Arc::clone(&self.store), move || {
            wake.send(Command::HttpStopped)
        })
        .map_err(|err| {
            format!(
                "Could not listen on {}.\n\n{}",
                config.endpoint(),
                error_detail(&err)
            )
        })
    }

    fn install(&mut self, server: Server) {
        server.activate();
        self.state.set_server(
            Some((server.endpoint(), server.health())),
            clipboard::sequence(),
        );
        self.server = Some(server);
    }

    fn apply(&mut self, config: Config) -> Result<(), String> {
        let config = config.validate().map_err(|err| err.to_string())?;
        let next = if self.state.endpoint() == Some(config.endpoint()) {
            None
        } else {
            Some(self.bind(config)?)
        };
        self.save(config)?;
        self.config = config;
        self.store.lock().unwrap().set_limit(config.max_images);
        if let Some(server) = next {
            self.install(server);
        }
        Ok(())
    }

    fn save(&self, config: Config) -> Result<(), String> {
        save_config(&self.exe, config)
    }

    fn update_tray(&mut self, force: bool) {
        let count = self.store.lock().unwrap().len();
        let healthy = self.state.endpoint().is_some();
        self.desktop.refresh_recent(healthy);
        self.tray.update(
            Status {
                enabled: self.state.enabled,
                healthy,
                count,
                autostart: self.config.start_with_windows,
            },
            force,
        );
    }

    fn error(&self, message: &str) {
        dialog(Some(self.hwnd), message, MB_OK | MB_ICONERROR);
    }

    fn cancel_retry(&mut self) {
        if self.retry.take().is_some() {
            // The timer belongs to the tray thread's window.
            unsafe {
                let _ = KillTimer(Some(self.hwnd), RETRY);
            }
        }
    }

    fn commit(&mut self, write: Write) {
        self.cancel_retry();
        self.write(write, 0);
    }

    fn write(&mut self, write: Write, attempt: usize) {
        let sequence = clipboard::sequence();
        let current = match &write {
            Write::Capture(job, _) => self.state.validate(*job, sequence).is_some(),
            Write::Copy(expected, _) | Write::Restore(expected, _) => *expected == sequence,
        };
        if !current {
            return;
        }
        let opened = match Opened::new(self.hwnd) {
            Ok(opened) => opened,
            Err(_) if attempt < RETRY_DELAYS.len() => {
                // Only contended writes arm this timer. Capture and the tray stay event-driven.
                let timer =
                    unsafe { SetTimer(Some(self.hwnd), RETRY, RETRY_DELAYS[attempt], None) };
                if timer != 0 {
                    self.retry = Some((write, attempt + 1));
                } else {
                    self.error("Could not retry the clipboard write.");
                }
                return;
            }
            Err(_) => {
                self.error("Could not write to the clipboard. It stayed busy.");
                return;
            }
        };
        if clipboard::sequence() != sequence {
            return;
        }
        let result: Result<(), String> = match write {
            Write::Capture(job, image) => {
                let Some(endpoint) = self.state.validate(job, sequence) else {
                    return;
                };
                let state = &mut self.state;
                self.store.lock().unwrap().publish(image, |id| {
                    write_data(&opened, Data::text(&url(endpoint, id)), state)
                })
            }
            Write::Copy(_, id) => {
                let Some(endpoint) = self.state.endpoint() else {
                    drop(opened);
                    self.error("Could not copy a URL while the HTTP server is unavailable.");
                    return;
                };
                if self.store.lock().unwrap().get(&id).is_some() {
                    write_data(&opened, Data::text(&url(endpoint, &id)), &mut self.state)
                } else {
                    Err("That image is no longer available.".into())
                }
            }
            Write::Restore(_, data) => write_data(&opened, Ok(data), &mut self.state),
        };
        drop(opened);
        if let Err(err) = result {
            self.error(&err);
        }
        self.update_tray(false);
    }
}

fn write_data(
    opened: &Opened,
    data: windows::core::Result<Data>,
    state: &mut CaptureState,
) -> Result<(), String> {
    let data = data.map_err(|_| "Could not prepare data for the clipboard.".to_owned())?;
    let sequence = opened
        .replace(data)
        .map_err(|_| "Windows could not replace the clipboard contents.".to_owned())?;
    state.record_write(sequence);
    Ok(())
}

fn save_config(exe: &Path, next: Config) -> Result<(), String> {
    let path = config_path(exe);
    let pending = next.stage(&path).map_err(config_write_error)?;
    let registry =
        autostart::Change::apply(exe, next.start_with_windows).map_err(autostart_error)?;
    if let Err(err) = pending.persist(&path) {
        let mut message = config_write_error(err.error);
        if let Some(change) = registry
            && let Err(err) = change.rollback()
        {
            message.push_str(&format!(
                "\n\nCould not restore the previous Start with Windows entry.\n\n{err}"
            ));
        }
        return Err(message);
    }
    Ok(())
}

fn config_write_error(err: io::Error) -> String {
    format!(
        "Could not save config.json beside ClipServe.exe.\n\n{}",
        error_detail(&err)
    )
}

fn autostart_error(err: io::Error) -> String {
    format!(
        "Could not update Start with Windows.\n\n{}",
        error_detail(&err)
    )
}
