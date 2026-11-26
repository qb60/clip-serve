use clipserve::{config::Config, store};
use gpui::{Context, Entity, IntoElement, Render, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
};

use crate::app::{Command, Wake};

pub type Reply = async_channel::Sender<Result<(), String>>;

pub struct Editor {
    wake: Wake,
    saved: Config,
    fields: [Entity<InputState>; 3],
    capture_on_startup: bool,
    start_with_windows: bool,
    pending: bool,
    message: String,
    failed: bool,
    _subscriptions: [Subscription; 3],
}

impl Editor {
    pub fn new(saved: Config, wake: Wake, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fields = [
            saved.listen_address.to_string(),
            saved.port.to_string(),
            saved.max_images.to_string(),
        ]
        .map(|value| cx.new(|cx| InputState::new(window, cx).default_value(value)));
        let subscriptions = fields.each_ref().map(|field| {
            cx.subscribe(field, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })
        });
        Self {
            wake,
            saved,
            fields,
            capture_on_startup: saved.capture_on_startup,
            start_with_windows: saved.start_with_windows,
            pending: false,
            message: String::new(),
            failed: false,
            _subscriptions: subscriptions,
        }
    }

    fn config(&self, cx: &gpui::App) -> Result<Config, String> {
        let [address, port, max_images] =
            self.fields.each_ref().map(|field| field.read(cx).value());
        Config {
            listen_address: address
                .trim()
                .parse()
                .map_err(|_| "Enter a local IPv4 address.")?,
            port: port
                .trim()
                .parse()
                .map_err(|_| "Port must be between 1 and 65535.")?,
            max_images: max_images
                .trim()
                .parse()
                .map_err(|_| "Maximum images must be between 1 and 1000.")?,
            capture_on_startup: self.capture_on_startup,
            start_with_windows: self.start_with_windows,
        }
        .validate()
        .map_err(|err| err.to_string())
    }

    fn apply(&mut self, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        match self.config(cx) {
            Ok(config) => {
                let (reply, receive) = async_channel::bounded(1);
                self.pending = true;
                self.failed = false;
                self.message.clear();
                self.wake.send(Command::Apply(config, reply));
                let editor = cx.weak_entity();
                window
                    .spawn(cx, async move |cx| {
                        let result = receive.recv().await.unwrap_or_else(|_| {
                            Err("ClipServe could not apply the settings.".into())
                        });
                        let _ = cx.update(|window, cx| {
                            let _ = editor.update(cx, |this, cx| {
                                this.pending = false;
                                match result {
                                    Ok(()) => {
                                        this.saved = config;
                                        this.message = "Settings applied.".into();
                                        if close {
                                            window.remove_window();
                                        }
                                    }
                                    Err(err) => {
                                        this.message = err;
                                        this.failed = true;
                                    }
                                }
                                cx.notify();
                            });
                        });
                    })
                    .detach();
            }
            Err(err) => {
                self.message = err;
                self.failed = true;
            }
        }
        cx.notify();
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let error = theme.danger;
        let config = self.config(cx);
        let status = if self.pending {
            "Applying settings…"
        } else if config
            .as_ref()
            .is_ok_and(|config| config.endpoint() != self.saved.endpoint())
        {
            "Existing URLs may stop working."
        } else if self.failed {
            ""
        } else {
            &self.message
        };
        let preview = config.map_or_else(
            |_| "Enter a valid address, port and image count.".to_owned(),
            |config| store::url(config.endpoint(), "k7Fp2xQ9"),
        );
        let fields = ["Local IPv4 address", "Port", "Maximum images"]
            .into_iter()
            .zip(self.fields.iter())
            .map(|(label, field)| {
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w(px(140.))
                            .flex_none()
                            .whitespace_nowrap()
                            .child(label),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(field)
                                .small()
                                .aria_label(label)
                                .disabled(self.pending),
                        ),
                    )
            });
        div()
            .v_flex()
            .size_full()
            .child(
                div()
                    .id("settings-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .py_3()
                    .child(
                        div()
                            .v_flex()
                            .gap_1()
                            .children(fields)
                            .child(
                                Checkbox::new("capture")
                                    .label("Enable capture when ClipServe starts")
                                    .checked(self.capture_on_startup)
                                    .disabled(self.pending)
                                    .on_change(cx.listener(|this, value: &bool, _, cx| {
                                        this.capture_on_startup = *value;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Checkbox::new("autostart")
                                    .label("Start with Windows")
                                    .checked(self.start_with_windows)
                                    .disabled(self.pending)
                                    .on_change(cx.listener(|this, value: &bool, _, cx| {
                                        this.start_with_windows = *value;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .border_t_1()
                                    .border_color(border)
                                    .pt_2()
                                    .child(
                                        div()
                                            .h_flex()
                                            .gap_2()
                                            .whitespace_nowrap()
                                            .child("URL preview")
                                            .child(
                                                div().text_color(muted).child(status.to_owned()),
                                            ),
                                    )
                                    .child(preview),
                            )
                            .when(self.failed, |ui| {
                                ui.child(div().text_color(error).child(self.message.clone()))
                            }),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .justify_end()
                    .gap_2()
                    .border_t_1()
                    .border_color(border)
                    .px_4()
                    .py_2()
                    .child(
                        Button::new("ok")
                            .label("OK")
                            .min_w(px(84.))
                            .primary()
                            .disabled(self.pending)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.apply(true, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .min_w(px(84.))
                            .outline()
                            .disabled(self.pending)
                            .on_click(|_, window, _| window.remove_window()),
                    )
                    .child(
                        Button::new("apply")
                            .label("Apply")
                            .min_w(px(84.))
                            .outline()
                            .disabled(self.pending)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.apply(false, window, cx)),
                            ),
                    ),
            )
    }
}
