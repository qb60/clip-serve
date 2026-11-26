use std::{
    io,
    sync::Arc,
    thread::{self, JoinHandle},
};

use clipserve::{config::Config, store::SharedStore};
use gpui::{
    App, AppContext, Bounds, DisplayId, Pixels, QuitMode, Size, TitlebarOptions, WindowBounds,
    WindowHandle, WindowOptions, px, size,
};
use gpui_component::{Root, Theme};

use crate::{
    app::Command,
    platform::{self, Wake},
    recent::{GALLERY_WIDTH, RecentImages},
    settings::Editor,
};

#[derive(Clone, Copy)]
pub enum View {
    Settings(Config),
    RecentImages,
}

enum Request {
    Open(View),
    Refresh(bool),
    Quit,
}

pub struct Desktop {
    requests: async_channel::Sender<Request>,
    thread: Option<JoinHandle<()>>,
}

impl Desktop {
    pub fn start(wake: Wake, store: SharedStore) -> io::Result<Self> {
        let (requests, receive) = async_channel::unbounded();
        let thread = thread::Builder::new()
            .name("Desktop".into())
            .spawn(move || {
                platform::warm_up_desktop();
                let mut can_copy = false;
                let first = loop {
                    match receive.recv_blocking() {
                        Ok(Request::Open(view)) => break view,
                        Ok(Request::Refresh(healthy)) => can_copy = healthy,
                        Ok(Request::Quit) | Err(_) => return,
                    }
                };
                let application = match platform::desktop_application() {
                    Ok(application) => application,
                    Err(err) => {
                        wake.send(Command::Error(format!(
                            "Could not initialize the desktop interface.\n\n{err:#}"
                        )));
                        return;
                    }
                };
                application
                    .with_assets(gpui_kit_assets::Assets)
                    .with_quit_mode(QuitMode::Explicit)
                    .run(move |cx| {
                        gpui_component::init(cx);
                        Theme::update(cx, |theme| theme.font_size = px(14.));
                        let mut windows = DesktopWindows {
                            settings: None,
                            recent: None,
                            can_copy,
                            store,
                            wake,
                        };
                        windows.open(first, cx);
                        cx.spawn(async move |cx| {
                            while let Ok(request) = receive.recv().await {
                                let quit = matches!(request, Request::Quit);
                                cx.update(|cx| match request {
                                    Request::Open(view) => windows.open(view, cx),
                                    Request::Refresh(can_copy) => windows.refresh(can_copy, cx),
                                    Request::Quit => cx.quit(),
                                });
                                if quit {
                                    break;
                                }
                            }
                        })
                        .detach();
                    });
            })?;
        Ok(Self {
            requests,
            thread: Some(thread),
        })
    }

    pub fn open(&self, view: View) -> io::Result<()> {
        self.requests
            .try_send(Request::Open(view))
            .map_err(|_| io::Error::other("The desktop thread stopped."))
    }

    pub fn refresh_recent(&self, can_copy: bool) {
        let _ = self.requests.try_send(Request::Refresh(can_copy));
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        let _ = self.requests.try_send(Request::Quit);
        if self.thread.take().unwrap().join().is_err() {
            log::error!("The desktop thread panicked.");
        }
    }
}

struct DesktopWindows {
    settings: Option<WindowHandle<Root>>,
    recent: Option<WindowHandle<Root>>,
    can_copy: bool,
    store: SharedStore,
    wake: Wake,
}

impl DesktopWindows {
    fn open(&mut self, view: View, cx: &mut App) {
        let (current, title) = match view {
            View::Settings(_) => (&mut self.settings, "ClipServe Settings"),
            View::RecentImages => (&mut self.recent, "ClipServe Recent Images"),
        };
        if current.is_some_and(|handle| {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        }) {
            return;
        }
        let result = match view {
            View::Settings(config) => cx.open_window(
                window_options(title, size(px(390.), px(240.)), None, None, cx),
                |window, cx| {
                    let editor = cx.new(|cx| Editor::new(config, self.wake.clone(), window, cx));
                    cx.new(|cx| Root::new(editor, window, cx))
                },
            ),
            View::RecentImages => {
                let options = window_options(
                    title,
                    size(px(GALLERY_WIDTH), px(492.)),
                    Some(size(px(480.), px(320.))),
                    None,
                    cx,
                );
                let initial_size = options.window_bounds.unwrap().get_bounds().size;
                cx.open_window(options, |window, cx| {
                    if window.viewport_size().width < initial_size.width - px(1.) {
                        window.resize(initial_size);
                    }
                    let recent = cx.new(|cx| {
                        RecentImages::new(
                            Arc::clone(&self.store),
                            self.wake.clone(),
                            self.can_copy,
                            cx,
                        )
                    });
                    cx.new(|cx| Root::new(recent, window, cx))
                })
            }
        };
        match result {
            Ok(handle) => *current = Some(handle),
            Err(err) => self
                .wake
                .send(Command::Error(format!("Could not open {title}.\n\n{err}"))),
        }
    }

    fn refresh(&mut self, can_copy: bool, cx: &mut App) {
        self.can_copy = can_copy;
        if let Some(handle) = self.recent {
            let _ = handle.update(cx, |root, window, cx| {
                if let Ok(recent) = root.view().clone().downcast::<RecentImages>() {
                    recent.update(cx, |recent, cx| recent.refresh(can_copy, window, cx));
                }
                window.refresh();
            });
        }
    }
}

pub(crate) fn window_options(
    title: impl Into<gpui::SharedString>,
    size: Size<Pixels>,
    minimum: Option<Size<Pixels>>,
    display: Option<DisplayId>,
    cx: &App,
) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(display, size, cx))),
        window_min_size: minimum,
        is_resizable: minimum.is_some(),
        display_id: display,
        ..Default::default()
    }
}
