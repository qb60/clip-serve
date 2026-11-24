use std::sync::mpsc;

use clipserve::{config::Config, store};
use eframe::egui::{self, Context, ViewportCommand};

use crate::app::{Command, Wake};

pub struct Reply {
    pub result: mpsc::Sender<Result<(), String>>,
    pub ctx: Context,
}

struct ApplyRequest {
    receive: mpsc::Receiver<Result<(), String>>,
    config: Config,
    close: bool,
}

pub struct Editor {
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
    pub fn new(saved: Config, wake: Wake) -> Self {
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

    pub fn logic(&mut self, ctx: &Context, visible: bool) {
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
                if *close && visible {
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

    pub fn ui(&mut self, ui: &mut egui::Ui) {
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
                                ui.add(
                                    egui::TextEdit::singleline(value).desired_width(f32::INFINITY),
                                )
                                .labelled_by(label.id);
                                ui.end_row();
                            }
                        });
                    ui.add_space(8.0);
                    ui.checkbox(
                        &mut self.capture_on_startup,
                        "Enable capture when ClipServe starts",
                    );
                    ui.checkbox(&mut self.start_with_windows, "Start with Windows");
                    ui.separator();
                    let config = self.config();
                    let endpoint_changed = config
                        .as_ref()
                        .is_ok_and(|config| config.endpoint() != self.saved.endpoint());
                    ui.horizontal(|ui| {
                        ui.label("URL preview");
                        if self.pending.is_some() {
                            ui.spinner();
                        } else if endpoint_changed {
                            ui.weak("Existing URLs may stop working.");
                        } else if !self.failed && !self.message.is_empty() {
                            ui.weak(&self.message);
                        }
                    });
                    match config {
                        Ok(config) => {
                            ui.monospace(store::url(config.endpoint(), "k7Fp2xQ9"));
                        }
                        Err(_) => {
                            ui.label("Enter a valid address, port and image count.");
                        }
                    }
                });
                if self.failed && !self.message.is_empty() {
                    ui.colored_label(ui.visuals().error_fg_color, &self.message);
                }
            });
        });
    }
}
