use std::{
    error::Error,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use clipserve::{
    capture::{Capture, CaptureState},
    config::{Config, config_path},
    http::Server,
    store::{SharedStore, Store, StoredImage, url},
};

use crate::{
    clipboard::{Prepared, Worker},
    desktop::{Desktop, View},
    platform::{
        AUTOSTART_LABEL, EXECUTABLE_NAME, HostWindow, Wake, autostart,
        clipboard::{self, Data, Opened},
        error_detail,
    },
    tray::{Status, Tray},
};

const RETRY_DELAYS: [u32; 3] = [15, 35, 75];

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

#[derive(Clone, Copy)]
pub enum ImageAction {
    Copy,
    Restore,
}

type Reply = async_channel::Sender<Result<(), String>>;

enum Write {
    Capture(Capture, StoredImage),
    Copy(u32, String),
    Restore(u32, Data),
}

pub(crate) struct App {
    window: HostWindow,
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
    pub(crate) fn new(
        window: HostWindow,
        exe: PathBuf,
        wake: Wake,
    ) -> Result<Self, Box<dyn Error>> {
        let tray = Tray::new(wake.clone())?;
        let (config, config_error) = match Config::load(&config_path(&exe)) {
            Ok(config) => (config, None),
            Err(err) => (
                Config::default(),
                Some(format!(
                    "Could not load or create config.json beside {EXECUTABLE_NAME}. Safe defaults are in use.\n\n{}",
                    error_detail(&err)
                )),
            ),
        };
        let autostart_error = if config_error.is_none() {
            autostart::Change::apply(&exe, config.autostart)
                .err()
                .map(autostart_error)
        } else {
            None
        };
        let worker_wake = wake.clone();
        let store = Arc::new(Mutex::new(Store::new(config.max_images)));
        let desktop = Desktop::start(wake.clone(), Arc::clone(&store))?;
        let mut app = Self {
            window,
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

    pub(crate) fn command(&mut self, command: Command) -> bool {
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
                let _ = reply.try_send(result);
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
                    autostart: !self.config.autostart,
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

    pub(crate) fn clipboard_changed(&mut self) {
        let sequence = clipboard::sequence();
        if let Some(job) = self.state.observe(sequence, self.window.owns_clipboard()) {
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

    pub(crate) fn update_tray(&mut self, force: bool) {
        let count = self.store.lock().unwrap().len();
        let healthy = self.state.endpoint().is_some();
        self.desktop.refresh_recent(healthy);
        self.tray.update(
            Status {
                enabled: self.state.enabled,
                healthy,
                count,
                autostart: self.config.autostart,
            },
            force,
        );
    }

    fn error(&self, message: &str) {
        self.window.show_error(message);
    }

    fn cancel_retry(&mut self) {
        if self.retry.take().is_some() {
            self.window.cancel_retry();
        }
    }

    pub(crate) fn retry_write(&mut self) {
        if let Some((write, attempt)) = self.retry.take() {
            self.write(write, attempt);
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
        let opened = match self.window.open_clipboard() {
            Ok(opened) => opened,
            Err(_) if attempt < RETRY_DELAYS.len() => {
                // Only contended writes arm this timer. Capture and the tray stay event-driven.
                if self.window.arm_retry(RETRY_DELAYS[attempt]) {
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
    data: io::Result<Data>,
    state: &mut CaptureState,
) -> Result<(), String> {
    let data = data.map_err(|_| "Could not prepare data for the clipboard.".to_owned())?;
    let sequence = opened.replace(data)?;
    state.record_write(sequence);
    Ok(())
}

fn save_config(exe: &Path, next: Config) -> Result<(), String> {
    let path = config_path(exe);
    let pending = next.stage(&path).map_err(config_write_error)?;
    let autostart_change =
        autostart::Change::apply(exe, next.autostart).map_err(autostart_error)?;
    if let Err(err) = pending.persist(&path) {
        let mut message = config_write_error(err.error);
        if let Some(change) = autostart_change
            && let Err(err) = change.rollback()
        {
            message.push_str(&format!(
                "\n\nCould not restore the previous {AUTOSTART_LABEL} entry.\n\n{err}"
            ));
        }
        return Err(message);
    }
    Ok(())
}

fn config_write_error(err: io::Error) -> String {
    format!(
        "Could not save config.json beside {EXECUTABLE_NAME}.\n\n{}",
        error_detail(&err)
    )
}

fn autostart_error(err: io::Error) -> String {
    format!(
        "Could not update {AUTOSTART_LABEL}.\n\n{}",
        error_detail(&err)
    )
}
