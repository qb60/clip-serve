use std::{
    collections::{HashMap, HashSet},
    io,
    sync::{Arc, mpsc},
    thread,
};

use clipserve::{capture::preview_png, store::SharedStore};
use eframe::egui::{
    self, ColorImage, Context, TextureHandle, TextureOptions, Ui, ViewportCommand, ViewportId,
};

use crate::{
    app::{Command, ImageAction, Wake},
    clipboard,
};

const TILE_WIDTH: f32 = 150.0;
const TILE_HEIGHT: f32 = 112.0;

pub struct RecentImages {
    store: SharedStore,
    wake: Wake,
    selected: Option<String>,
    textures: HashMap<String, TextureHandle>,
    preview: Preview,
    confirm_clear: bool,
}

impl RecentImages {
    pub fn new(store: SharedStore, wake: Wake) -> Self {
        Self {
            store,
            wake,
            selected: None,
            textures: HashMap::new(),
            preview: Preview::default(),
            confirm_clear: false,
        }
    }

    pub fn hide_dialogs(&mut self) {
        self.preview.close();
        self.confirm_clear = false;
    }

    pub fn ui(&mut self, ui: &mut Ui, can_copy: bool, work_area: Option<egui::Rect>) {
        let entries: Vec<_> = self
            .store
            .lock()
            .unwrap()
            .recent()
            .map(|entry| (entry.id.clone(), Arc::clone(&entry.image.thumbnail)))
            .collect();
        let ids: HashSet<_> = entries.iter().map(|(id, _)| id.as_str()).collect();
        self.textures.retain(|id, _| ids.contains(id.as_str()));
        if self.selected.as_deref().is_some_and(|id| !ids.contains(id)) {
            self.selected = None;
        }
        if self
            .preview
            .id
            .as_deref()
            .is_some_and(|id| !ids.contains(id))
        {
            self.preview.close();
        }

        let footer =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(16, 10));
        egui::Panel::bottom("recent_actions")
            .resizable(false)
            .exact_size(52.0)
            .frame(footer)
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    image_buttons(ui, &self.wake, self.selected.as_deref(), can_copy);
                    let preview = ui.add_enabled(
                        self.selected.is_some(),
                        egui::Button::new("Preview").min_size(egui::vec2(80.0, 30.0)),
                    );
                    if preview.clicked()
                        && let Some(id) = &self.selected
                    {
                        self.preview.open(id, ui.ctx());
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !entries.is_empty(),
                                egui::Button::new("Clear stored images")
                                    .min_size(egui::vec2(132.0, 30.0)),
                            )
                            .clicked()
                        {
                            self.confirm_clear = true;
                            ui.ctx().request_repaint();
                        }
                    });
                });
            });

        let frame = egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::same(14));
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            if entries.is_empty() {
                ui.centered_and_justified(|ui| ui.label("No images captured yet."));
                return;
            }
            let spacing = ui.spacing().item_spacing.x;
            let scrollbar = ui.spacing().scroll.allocated_width();
            let columns = ((ui.available_width() - scrollbar + spacing) / (TILE_WIDTH + spacing))
                .floor()
                .max(1.0) as usize;
            let caption = egui::TextStyle::Monospace.resolve(ui.style());
            let row_height = TILE_HEIGHT + ui.text_style_height(&egui::TextStyle::Monospace) + 8.0;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_rows(
                    ui,
                    row_height,
                    entries.len().div_ceil(columns),
                    |ui, rows| {
                        for row in rows {
                            let start = row * columns;
                            let end = (start + columns).min(entries.len());
                            let count = (end - start) as f32;
                            let (_, row_rect) = ui.allocate_space(egui::vec2(
                                count * TILE_WIDTH + (count - 1.0) * spacing,
                                row_height,
                            ));
                            for (col, (id, thumbnail)) in entries[start..end].iter().enumerate() {
                                let rect = egui::Rect::from_min_size(
                                    row_rect.min
                                        + egui::vec2(col as f32 * (TILE_WIDTH + spacing), 0.0),
                                    egui::vec2(TILE_WIDTH, row_height),
                                );
                                let texture =
                                    self.textures.entry(id.clone()).or_insert_with(|| {
                                        let size = [
                                            thumbnail.width() as usize,
                                            thumbnail.height() as usize,
                                        ];
                                        let image = ColorImage::from_rgba_unmultiplied(
                                            size,
                                            thumbnail.as_raw(),
                                        );
                                        ui.ctx().load_texture(id, image, TextureOptions::LINEAR)
                                    });
                                let selected = self.selected.as_ref() == Some(id);
                                let tile = image_tile(ui, rect, id, texture, selected, &caption);
                                if tile.clicked() || tile.double_clicked() {
                                    self.selected = Some(id.clone());
                                    if tile.double_clicked() {
                                        self.preview.open(id, ui.ctx());
                                    }
                                    ui.ctx().request_repaint();
                                }
                            }
                        }
                    },
                );
        });

        if self.confirm_clear {
            let modal =
                egui::Modal::new(egui::Id::unique("clipserve_clear_images")).show(ui.ctx(), |ui| {
                    ui.set_max_width(300.0);
                    ui.heading("Clear stored images?");
                    ui.label("All existing image URLs will stop working.");
                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        (
                            ui.button("Clear images").clicked(),
                            ui.button("Cancel").clicked(),
                        )
                    })
                    .inner
                });
            let (clear, cancel) = modal.inner;
            if clear {
                self.wake.send(Command::ClearImages);
                self.selected = None;
                self.preview.close();
            }
            if clear || cancel || modal.should_close() {
                self.confirm_clear = false;
            }
        }

        self.preview
            .show(ui.ctx(), &self.store, &self.wake, can_copy, work_area);
    }
}

fn image_tile(
    ui: &Ui,
    rect: egui::Rect,
    id: &str,
    texture: &TextureHandle,
    selected: bool,
    caption: &egui::FontId,
) -> egui::Response {
    let tile = ui.interact(rect, ui.scope_id().with(id), egui::Sense::click());
    let filename = format!("{id}.png");
    tile.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, tile.enabled(), selected, &filename)
    });
    let visuals = ui.style().interact_selectable(&tile, selected);
    let painter = ui.painter();
    painter.rect(
        rect,
        visuals.corner_radius,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let image_center = egui::pos2(rect.center().x, rect.min.y + TILE_HEIGHT * 0.5);
    painter.image(
        texture.id(),
        egui::Rect::from_center_size(image_center, texture.size_vec2()),
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    painter.text(
        egui::pos2(rect.center().x, rect.min.y + TILE_HEIGHT + 4.0),
        egui::Align2::CENTER_TOP,
        filename,
        caption.clone(),
        visuals.text_color(),
    );
    tile
}

fn image_buttons(ui: &mut Ui, wake: &Wake, id: Option<&str>, can_copy: bool) {
    let size = egui::vec2(104.0, 30.0);
    let restore = ui.add_enabled(
        id.is_some(),
        egui::Button::new("Restore image").min_size(size),
    );
    let copy = ui.add_enabled(
        id.is_some() && can_copy,
        egui::Button::new("Copy URL").min_size(size),
    );
    if let Some(id) = id {
        let action = if copy.clicked() {
            Some(ImageAction::Copy)
        } else {
            restore.clicked().then_some(ImageAction::Restore)
        };
        if let Some(action) = action {
            wake.send(Command::Image(action, id.to_owned(), clipboard::sequence()));
        }
    }
}

#[derive(Default)]
struct Preview {
    id: Option<String>,
    image: Option<Result<TextureHandle, String>>,
    pending: Option<(String, mpsc::Receiver<io::Result<image::RgbaImage>>)>,
}

impl Preview {
    fn viewport() -> ViewportId {
        ViewportId::from_hash_of("clipserve_image_preview")
    }

    fn open(&mut self, id: &str, ctx: &Context) {
        if self.id.is_some() {
            ctx.send_viewport_cmd_to(Self::viewport(), ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(Self::viewport(), ViewportCommand::Focus);
        }
        if self.id.as_deref() != Some(id) {
            self.id = Some(id.to_owned());
            self.image = None;
        }
    }

    fn close(&mut self) {
        self.id = None;
        self.image = None;
    }

    fn load(&mut self, ctx: &Context, store: &SharedStore) {
        if let Some((id, receive)) = &self.pending {
            let result = match receive.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err(io::Error::other("The preview worker stopped."))
                }
            };
            if self.id.as_ref() == Some(id) {
                self.image = Some(
                    result
                        .map(|pixels| {
                            let size = [pixels.width() as usize, pixels.height() as usize];
                            let image = ColorImage::from_rgba_unmultiplied(size, pixels.as_raw());
                            ctx.load_texture("image_preview", image, TextureOptions::LINEAR)
                        })
                        .map_err(|err| format!("Could not preview the image.\n{err}")),
                );
            }
            self.pending = None;
        }
        if let Some(id) = &self.id
            && self.image.is_none()
        {
            let Some(png) = store.lock().unwrap().get(id) else {
                self.close();
                return;
            };
            let (reply, receive) = mpsc::channel();
            let repaint = ctx.clone();
            match thread::Builder::new()
                .name("Image preview".into())
                .spawn(move || {
                    let _ = reply.send(preview_png(&png));
                    repaint.request_repaint_of(ViewportId::ROOT);
                }) {
                Ok(_) => self.pending = Some((id.clone(), receive)),
                Err(err) => {
                    self.image = Some(Err(format!("Could not start the image preview.\n{err}")))
                }
            }
        }
    }

    fn show(
        &mut self,
        ctx: &Context,
        store: &SharedStore,
        wake: &Wake,
        can_copy: bool,
        work_area: Option<egui::Rect>,
    ) {
        self.load(ctx, store);
        let Some(id) = &self.id else { return };
        let ideal = egui::vec2(960.0, 720.0);
        let area = work_area.or_else(|| {
            ctx.input(|input| input.viewport().monitor_size)
                .map(|size| egui::Rect::from_min_size(egui::Pos2::ZERO, size))
        });
        let size = area.map_or(ideal, |area| ideal.min(area.size() * 0.85));
        let mut viewport = egui::ViewportBuilder::default()
            .with_title(format!("ClipServe Preview — {id}.png"))
            .with_inner_size(size)
            .with_min_inner_size([320.0, 240.0]);
        if let Some(area) = area {
            viewport = viewport.with_position(area.center() - size * 0.5);
        }
        let close = ctx.show_viewport_immediate(Self::viewport(), viewport, |ui, _| {
            egui::Panel::bottom("preview_actions")
                .resizable(false)
                .exact_size(52.0)
                .show(ui, |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        image_buttons(ui, wake, Some(id), can_copy);
                    });
                });
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| match &self.image {
                    Some(Ok(texture)) => {
                        ui.add(
                            egui::Image::new((texture.id(), texture.size_vec2()))
                                .fit_to_exact_size(ui.available_size()),
                        );
                    }
                    Some(Err(message)) => {
                        ui.label(message);
                    }
                    None => {
                        ui.spinner();
                    }
                });
            });
            ui.input(|input| {
                input.viewport().close_requested() || input.key_pressed(egui::Key::Escape)
            })
        });
        if close {
            self.close();
            ctx.request_repaint();
        }
    }
}
