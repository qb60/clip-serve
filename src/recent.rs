use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::Arc,
};

use clipserve::{
    capture::preview_png,
    store::{Entry, SharedStore},
};
use gpui::{
    App, AppContext, ClickEvent, Context, IntoElement, PromptButton, PromptLevel, Render,
    RenderImage, SharedString, UniformListScrollHandle, Window, WindowHandle, div, img, prelude::*,
    px, size, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable, Root, Sizable, StyledExt,
    button::Button,
    scroll::{Scrollbar, ScrollbarMode},
};

use crate::{
    app::{Command, ImageAction},
    desktop::window_options,
    platform::{Wake, clipboard},
};

const TILE_WIDTH: f32 = 150.;
const TILE_GAP: f32 = 8.;
const GRID_INSET: f32 = 12.;
pub const GALLERY_WIDTH: f32 = grid_width(4) + 2. * GRID_INSET + TILE_GAP;

pub struct RecentImages {
    store: SharedStore,
    wake: Wake,
    entries: Vec<Entry>,
    thumbnails: HashMap<String, Arc<RenderImage>>,
    scroll: UniformListScrollHandle,
    selected: Option<String>,
    preview: Option<(String, WindowHandle<Root>)>,
    confirming: bool,
    can_copy: bool,
}

impl RecentImages {
    pub fn new(store: SharedStore, wake: Wake, can_copy: bool, cx: &mut Context<Self>) -> Self {
        let entries = store.lock().unwrap().recent().cloned().collect();
        cx.on_release(|gallery, cx| gallery.close_preview(cx))
            .detach();
        Self {
            store,
            wake,
            entries,
            thumbnails: HashMap::new(),
            scroll: UniformListScrollHandle::new(),
            selected: None,
            preview: None,
            confirming: false,
            can_copy,
        }
    }

    pub fn refresh(&mut self, can_copy: bool, window: &mut Window, cx: &mut Context<Self>) {
        let copy_changed = std::mem::replace(&mut self.can_copy, can_copy) != can_copy;
        self.entries = self.store.lock().unwrap().recent().cloned().collect();
        let ids: HashSet<_> = self.entries.iter().map(|entry| entry.id.as_str()).collect();
        self.thumbnails.retain(|id, image| {
            if ids.contains(id.as_str()) {
                true
            } else {
                let _ = window.drop_image(Arc::clone(image));
                false
            }
        });
        if self.selected.as_deref().is_some_and(|id| !ids.contains(id)) {
            self.selected = None;
        }
        if self
            .preview
            .as_ref()
            .is_some_and(|(id, _)| !ids.contains(id.as_str()))
        {
            self.close_preview(cx);
        } else if copy_changed && let Some((_, handle)) = &self.preview {
            let _ = handle.update(cx, |root, _, cx| {
                if let Ok(preview) = root.view().clone().downcast::<Preview>() {
                    preview.update(cx, |preview, cx| {
                        preview.can_copy = can_copy;
                        cx.notify();
                    });
                }
            });
        }
        cx.notify();
    }

    fn open_preview(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((open, handle)) = &self.preview
            && open == &id
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }
        let Some(png) = self.store.lock().unwrap().get(&id) else {
            return;
        };
        self.close_preview(cx);
        let display = window.display(cx);
        let ideal = size(px(960.), px(720.));
        let preview_size = display.as_ref().map_or(ideal, |display| {
            let screen = display.bounds().size;
            size(
                ideal.width.min(screen.width * 0.85),
                ideal.height.min(screen.height * 0.85),
            )
        });
        let options = window_options(
            format!("ClipServe Preview — {id}.png"),
            preview_size,
            Some(size(px(320.), px(240.))),
            display.map(|display| display.id()),
            cx,
        );
        let wake = self.wake.clone();
        let store = Arc::clone(&self.store);
        let can_copy = self.can_copy;
        let result = cx.open_window(options, |window, cx| {
            let preview = cx.new(|cx| {
                let decode = cx
                    .background_executor()
                    .spawn(async move { preview_png(&png).map(render_image) });
                cx.spawn(async move |preview, cx| {
                    let result = decode.await;
                    let _ = preview.update(cx, |this: &mut Preview, cx| {
                        this.image = Some(
                            result.map_err(|err| format!("Could not preview the image.\n{err}")),
                        );
                        cx.notify();
                    });
                })
                .detach();
                Preview {
                    image: None,
                    id: id.clone(),
                    wake,
                    store,
                    can_copy,
                }
            });
            cx.new(|cx| Root::new(preview, window, cx))
        });
        match result {
            Ok(handle) => self.preview = Some((id, handle)),
            Err(err) => self.wake.send(Command::Error(format!(
                "Could not open the image preview.\n\n{err}"
            ))),
        }
    }

    fn close_preview(&mut self, cx: &mut App) {
        if let Some((_, handle)) = self.preview.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn confirm_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirming || self.entries.is_empty() {
            return;
        }
        self.confirming = true;
        let reply = window.prompt(
            PromptLevel::Warning,
            "Clear stored images?",
            Some("All existing image URLs will stop working."),
            &[
                PromptButton::cancel("Cancel"),
                PromptButton::ok("Clear images"),
            ],
            cx,
        );
        cx.spawn(async move |gallery, cx| {
            let confirmed = matches!(reply.await, Ok(1));
            let _ = gallery.update(cx, |this, cx| {
                this.confirming = false;
                if confirmed {
                    this.wake.send(Command::ClearImages);
                    this.selected = None;
                    this.close_preview(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for RecentImages {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let columns = gallery_columns(f32::from(window.viewport_size().width));
        let selected = self.selected.as_deref();
        let no_images = self.entries.is_empty();
        div()
            .v_flex()
            .size_full()
            .child(if no_images {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child("No images captured yet.")
                    .into_any_element()
            } else {
                let tiles = uniform_list(
                    "images",
                    self.entries.len().div_ceil(columns),
                    cx.processor(move |this, rows: Range<usize>, _, cx| {
                        let background = cx.theme().background;
                        let selected_color = cx.theme().accent;
                        let mut rendered = Vec::with_capacity(rows.len());
                        for row in rows {
                            let start = row * columns;
                            let end = (start + columns).min(this.entries.len());
                            let mut tiles = div()
                                .h_flex()
                                .flex_none()
                                .w(px(grid_width(columns)))
                                .gap(px(TILE_GAP))
                                .pb(px(TILE_GAP));
                            for entry in &this.entries[start..end] {
                                let id = entry.id.clone();
                                let texture =
                                    this.thumbnails.entry(id.clone()).or_insert_with(|| {
                                        render_image(entry.image.thumbnail.as_ref().clone())
                                    });
                                let is_selected = this.selected.as_ref() == Some(&id);
                                tiles = tiles.child(
                                    div()
                                        .id(SharedString::from(id.clone()))
                                        .role(gpui::Role::Button)
                                        .tab_stop(true)
                                        .v_flex()
                                        .items_center()
                                        .w(px(TILE_WIDTH))
                                        .flex_none()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(if is_selected {
                                            cx.theme().ring
                                        } else {
                                            border
                                        })
                                        .bg(if is_selected {
                                            selected_color
                                        } else {
                                            background
                                        })
                                        .hover(|style| style.bg(selected_color))
                                        .cursor_pointer()
                                        .on_click(cx.listener(
                                            move |this, event: &ClickEvent, window, cx| {
                                                this.selected = Some(id.clone());
                                                if event.click_count() >= 2 {
                                                    this.open_preview(id.clone(), window, cx);
                                                }
                                                cx.notify();
                                            },
                                        ))
                                        .child(img(Arc::clone(texture)).w_full().h(px(112.)))
                                        .child(div().pb_1().child(format!("{}.png", entry.id))),
                                );
                            }
                            rendered.push(div().h_flex().w_full().justify_center().child(tiles));
                        }
                        rendered
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full()
                .px(px(GRID_INSET))
                .pt_3();
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(tiles)
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(GRID_INSET))
                            .child(
                                Scrollbar::vertical(&self.scroll)
                                    .mode(ScrollbarMode::Always)
                                    .viewport_from_layout()
                                    .styles(|styles| {
                                        styles.track(|track| track.width(px(GRID_INSET)))
                                    }),
                            ),
                    )
                    .into_any_element()
            })
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .gap_2()
                    .border_t_1()
                    .border_color(border)
                    .px_3()
                    .py_2()
                    .child(
                        Button::new("clear")
                            .large()
                            .label("Clear stored images")
                            .outline()
                            .disabled(no_images || self.confirming)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.confirm_clear(window, cx)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("preview")
                            .large()
                            .label("Preview")
                            .outline()
                            .disabled(selected.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(id) = this.selected.clone() {
                                    this.open_preview(id, window, cx);
                                }
                            })),
                    )
                    .children(image_buttons(&self.wake, selected, self.can_copy)),
            )
    }
}

fn gallery_columns(width: f32) -> usize {
    ((width - 2. * GRID_INSET + TILE_GAP) / (TILE_WIDTH + TILE_GAP))
        .floor()
        .max(1.) as usize
}

const fn grid_width(columns: usize) -> f32 {
    columns as f32 * (TILE_WIDTH + TILE_GAP) - TILE_GAP
}

fn image_buttons(wake: &Wake, id: Option<&str>, can_copy: bool) -> [Button; 2] {
    [
        ("copy", "Copy URL", ImageAction::Copy, can_copy),
        ("restore", "Restore image", ImageAction::Restore, true),
    ]
    .map(|(key, label, action, enabled)| {
        let wake = wake.clone();
        let id = id.map(str::to_owned);
        Button::new(key)
            .large()
            .label(label)
            .outline()
            .disabled(!enabled || id.is_none())
            .on_click(move |_, _, _| {
                if let Some(id) = &id {
                    wake.send(Command::Image(action, id.clone(), clipboard::sequence()));
                }
            })
    })
}

fn render_image(mut pixels: image::RgbaImage) -> Arc<RenderImage> {
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Arc::new(RenderImage::new([image::Frame::new(pixels)]))
}

struct Preview {
    image: Option<Result<Arc<RenderImage>, String>>,
    id: String,
    wake: Wake,
    store: SharedStore,
    can_copy: bool,
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let stored = self.store.lock().unwrap().get(&self.id).is_some();
        let content = match &self.image {
            Some(Ok(image)) => img(Arc::clone(image)).size_full().into_any_element(),
            Some(Err(error)) => div().child(error.clone()).into_any_element(),
            None => div().child("Loading image…").into_any_element(),
        };
        div()
            .v_flex()
            .size_full()
            .id("preview-window")
            .on_key_down(|event, window, cx| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                    cx.stop_propagation();
                }
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_3()
                    .child(content),
            )
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .justify_end()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .px_3()
                    .py_2()
                    .children(image_buttons(
                        &self.wake,
                        stored.then_some(self.id.as_str()),
                        self.can_copy,
                    )),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_columns_fit_the_viewport() {
        assert_eq!(gallery_columns(300.), 1);
        assert_eq!(gallery_columns(480.), 2);
        assert_eq!(gallery_columns(GALLERY_WIDTH), 4);
        let four_column_minimum = grid_width(4) + 2. * GRID_INSET;
        assert_eq!(gallery_columns(four_column_minimum), 4);
        assert_eq!(gallery_columns(four_column_minimum - 1.), 3);
    }

    #[test]
    fn render_image_preserves_alpha_and_converts_to_bgra() {
        let image = render_image(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([1, 2, 3, 4]),
        ));
        assert_eq!(image.as_bytes(0), Some([3, 2, 1, 4].as_slice()));
    }
}
