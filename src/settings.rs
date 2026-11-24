use std::{
    io,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use clipserve::{config::Config, store};
use eframe::egui::{self, Context, ViewportCommand};
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE},
};
use winit::{
    platform::windows::EventLoopBuilderExtWindows,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
};

use crate::app::{Command, Wake};

#[derive(Default)]
struct Window {
    busy: bool,
    exiting: bool,
    visible: Option<(Context, usize)>,
}

pub struct Settings {
    open: Option<mpsc::Sender<Config>>,
    window: Arc<Mutex<Window>>,
    thread: Option<JoinHandle<()>>,
}

impl Settings {
    pub fn start(wake: Wake) -> io::Result<Self> {
        let (open, requests) = mpsc::channel();
        let window = Arc::new(Mutex::new(Window::default()));
        let thread_window = Arc::clone(&window);
        let thread = thread::Builder::new()
            .name("Settings".into())
            .spawn(move || {
                for config in requests {
                    if thread_window.lock().unwrap().exiting {
                        break;
                    }
                    let result = run(config, &wake, &thread_window);
                    {
                        let mut window = thread_window.lock().unwrap();
                        window.visible = None;
                        window.busy = false;
                    }
                    if let Err(err) = result {
                        wake.send(Command::Error(format!("Could not open Settings.\n\n{err}")));
                    }
                }
            })?;
        Ok(Self {
            open: Some(open),
            window,
            thread: Some(thread),
        })
    }

    pub fn open(&self, config: Config) -> io::Result<()> {
        let mut window = self.window.lock().unwrap();
        if let Some((ctx, _)) = &window.visible {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        } else if !window.busy {
            self.open
                .as_ref()
                .unwrap()
                .send(config)
                .map_err(|_| io::Error::other("The Settings thread stopped."))?;
            window.busy = true;
        }
        Ok(())
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        {
            let mut window = self.window.lock().unwrap();
            window.exiting = true;
            if let Some((_, hwnd)) = &window.visible {
                close(*hwnd);
            }
        }
        self.open.take();
        if self.thread.take().unwrap().join().is_err() {
            log::error!("The Settings thread panicked.");
        }
    }
}

fn close(hwnd: usize) {
    // PostMessage does not dereference the handle. Windows rejects a closed window.
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

fn run(config: Config, wake: &Wake, window: &Mutex<Window>) -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([480.0, 264.0])
        .with_resizable(false)
        .with_maximize_button(false);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(crate::tray::DISABLED_ICON) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        run_and_return: true,
        event_loop_builder: Some(Box::new(|builder| {
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };
    eframe::run_native(
        "ClipServe Settings",
        options,
        Box::new(|cc| {
            let handle = cc.window_handle()?;
            let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                unreachable!("settings use a Win32 window");
            };
            let hwnd = handle.hwnd.get() as usize;
            let mut window = window.lock().unwrap();
            window.visible = Some((cc.egui_ctx.clone(), hwnd));
            if window.exiting {
                close(hwnd);
            }
            Ok(Box::new(Editor::new(config, wake.clone())))
        }),
    )
}

pub struct Reply {
    pub result: mpsc::Sender<Result<(), String>>,
    pub ctx: Context,
}

struct ApplyRequest {
    receive: mpsc::Receiver<Result<(), String>>,
    config: Config,
    close: bool,
}

struct Editor {
    wake: Wake,
    saved: Config,
    address: String,
    port: String,
    max_images: String,
    capture_on_startup: bool,
    start_with_windows: bool,
    pending: Option<ApplyRequest>,
    message: String,
    failed: bool,
}

impl Editor {
    fn new(saved: Config, wake: Wake) -> Self {
        Self {
            wake,
            saved,
            address: saved.listen_address.to_string(),
            port: saved.port.to_string(),
            max_images: saved.max_images.to_string(),
            capture_on_startup: saved.capture_on_startup,
            start_with_windows: saved.start_with_windows,
            pending: None,
            message: String::new(),
            failed: false,
        }
    }

    fn config(&self) -> Result<Config, String> {
        Config {
            listen_address: self
                .address
                .trim()
                .parse()
                .map_err(|_| "Enter a local IPv4 address.")?,
            port: self
                .port
                .trim()
                .parse()
                .map_err(|_| "Port must be between 1 and 65535.")?,
            max_images: self
                .max_images
                .trim()
                .parse()
                .map_err(|_| "Maximum images must be between 1 and 1000.")?,
            capture_on_startup: self.capture_on_startup,
            start_with_windows: self.start_with_windows,
        }
        .validate()
        .map_err(|err| err.to_string())
    }

    fn apply(&mut self, ctx: &Context, close: bool) {
        match self.config() {
            Ok(config) => {
                let (result, receive) = mpsc::channel();
                self.message.clear();
                self.pending = Some(ApplyRequest {
                    receive,
                    config,
                    close,
                });
                self.wake.send(Command::Apply(
                    config,
                    Reply {
                        result,
                        ctx: ctx.clone(),
                    },
                ));
            }
            Err(err) => {
                self.message = err;
                self.failed = true;
            }
        }
    }
}

impl eframe::App for Editor {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        let Some(ApplyRequest {
            receive,
            config,
            close,
        }) = &self.pending
        else {
            return;
        };
        let result = match receive.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("ClipServe could not apply the settings.".into())
            }
        };
        match result {
            Ok(()) => {
                self.saved = *config;
                self.message = "Settings applied.".into();
                self.failed = false;
                if *close {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
            Err(err) => {
                self.message = err;
                self.failed = true;
            }
        }
        self.pending = None;
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let footer =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(16, 10));
        egui::Panel::bottom("settings_actions")
            .resizable(false)
            .exact_size(52.0)
            .frame(footer)
            .show(ui, |ui| {
                ui.add_enabled_ui(self.pending.is_none(), |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let button_size = egui::vec2(84.0, 30.0);
                        if ui
                            .add(egui::Button::new("Apply").min_size(button_size))
                            .clicked()
                        {
                            self.apply(ui.ctx(), false);
                        }
                        if ui
                            .add(egui::Button::new("Cancel").min_size(button_size))
                            .clicked()
                        {
                            ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                        }
                        if ui
                            .add(egui::Button::new("OK").min_size(button_size))
                            .clicked()
                        {
                            self.apply(ui.ctx(), true);
                        }
                    });
                });
            });

        let body =
            egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::symmetric(16, 12));
        egui::CentralPanel::default().frame(body).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_enabled_ui(self.pending.is_none(), |ui| {
                    egui::Grid::new("settings")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            for (label, value) in [
                                ("Local IPv4 address", &mut self.address),
                                ("Port", &mut self.port),
                                ("Maximum images", &mut self.max_images),
                            ] {
                                let label = ui.label(label);
                                ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY))
                                    .labelled_by(label.id);
                                ui.end_row();
                            }
                        });
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.capture_on_startup, "Enable capture when ClipServe starts");
                    ui.checkbox(&mut self.start_with_windows, "Start with Windows");
                    ui.separator();
                    ui.label("URL preview");
                    match self.config() {
                        Ok(config) => {
                            ui.monospace(store::url(config.endpoint(), "k7Fp2xQ9"));
                            if config.endpoint() != self.saved.endpoint() {
                                ui.label("Changing the address or port can make previously copied URLs unreachable.");
                            }
                        }
                        Err(_) => { ui.label("Enter a valid address, port and image count."); }
                    }
                });
                if self.pending.is_some() {
                    ui.spinner();
                } else if self.failed {
                    ui.colored_label(ui.visuals().error_fg_color, &self.message);
                } else if !self.message.is_empty() {
                    ui.label(&self.message);
                }
            });
        });
    }
}
