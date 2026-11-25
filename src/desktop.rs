use std::{
    io,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use clipserve::{config::Config, store::SharedStore};
use eframe::egui::{self, Context, ViewportCommand};
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow},
    UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE},
};
use winit::{
    platform::windows::EventLoopBuilderExtWindows,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
};

use crate::{
    app::{Command, Wake},
    recent::RecentImages,
    settings::Editor,
};

#[derive(Clone, Copy)]
pub enum View {
    Settings(Config),
    RecentImages,
}

impl View {
    fn title(self) -> &'static str {
        match self {
            Self::Settings(_) => "ClipServe Settings",
            Self::RecentImages => "ClipServe Recent Images",
        }
    }

    fn size(self) -> [f32; 2] {
        match self {
            Self::Settings(_) => [480.0, 264.0],
            Self::RecentImages => [680.0, 492.0],
        }
    }

    fn recent(self) -> bool {
        matches!(self, Self::RecentImages)
    }

    fn min_size(self) -> [f32; 2] {
        if self.recent() {
            [480.0, 320.0]
        } else {
            self.size()
        }
    }
}

#[derive(Default)]
struct Window {
    busy: bool,
    exiting: bool,
    visible: Option<(Context, usize)>,
    next_view: Option<View>,
    recent: bool,
    can_copy: bool,
}

pub struct Desktop {
    open: Option<mpsc::Sender<View>>,
    window: Arc<Mutex<Window>>,
    thread: Option<JoinHandle<()>>,
}

impl Desktop {
    pub fn start(wake: Wake, store: SharedStore) -> io::Result<Self> {
        let (open, requests) = mpsc::channel();
        let window = Arc::new(Mutex::new(Window::default()));
        let thread_window = Arc::clone(&window);
        let thread = thread::Builder::new()
            .name("Desktop".into())
            .spawn(move || {
                for request in requests {
                    let mut next = Some(request);
                    while let Some(view) = next {
                        if thread_window.lock().unwrap().exiting {
                            return;
                        }
                        let result = run(view, &wake, &store, &thread_window);
                        {
                            let mut window = thread_window.lock().unwrap();
                            window.visible = None;
                            next = window.next_view.take();
                            window.busy = next.is_some();
                        }
                        if let Err(err) = result {
                            wake.send(Command::Error(format!(
                                "Could not open {}.\n\n{err}",
                                view.title()
                            )));
                        }
                    }
                }
            })?;
        Ok(Self {
            open: Some(open),
            window,
            thread: Some(thread),
        })
    }

    pub fn open(&self, view: View) -> io::Result<()> {
        let mut window = self.window.lock().unwrap();
        if window.busy {
            window.next_view = Some(view);
            if let Some((ctx, _)) = &window.visible {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
        } else {
            self.open
                .as_ref()
                .unwrap()
                .send(view)
                .map_err(|_| io::Error::other("The desktop thread stopped."))?;
            window.busy = true;
        }
        Ok(())
    }

    pub fn refresh_recent(&self, can_copy: bool) {
        let mut window = self.window.lock().unwrap();
        window.can_copy = can_copy;
        if window.recent
            && let Some((ctx, _)) = &window.visible
        {
            ctx.request_repaint();
        }
    }
}

impl Drop for Desktop {
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
            log::error!("The desktop thread panicked.");
        }
    }
}

fn close(hwnd: usize) {
    // PostMessage does not dereference the handle. Windows rejects a closed window.
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

fn monitor_work_area(frame: &eframe::Frame, pixels_per_point: f32) -> Option<egui::Rect> {
    let handle = frame.window_handle().ok()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // The frame handle is valid for this call and MONITORINFO is initialized with its size.
    let found = unsafe {
        let monitor = MonitorFromWindow(HWND(handle.hwnd.get() as _), MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(monitor, &mut info).as_bool()
    };
    found.then(|| {
        let work = info.rcWork;
        egui::Rect::from_min_max(
            egui::pos2(
                work.left as f32 / pixels_per_point,
                work.top as f32 / pixels_per_point,
            ),
            egui::pos2(
                work.right as f32 / pixels_per_point,
                work.bottom as f32 / pixels_per_point,
            ),
        )
    })
}

fn apply_windows_style(ctx: &Context) {
    use egui::{Color32, Stroke};

    if let Some(font) = std::env::var_os("SystemRoot").and_then(|root| {
        let path = std::path::PathBuf::from(root)
            .join("Fonts")
            .join("segoeui.ttf");
        std::fs::read(path).ok()
    }) {
        ctx.add_font(egui::FontInsert::new(
            "Segoe UI",
            egui::FontData::from_owned(font),
            vec![egui::InsertFontFamily {
                family: egui::FontFamily::Proportional,
                priority: egui::FontPriority::Highest,
            }],
        ));
    }

    ctx.all_styles_mut(|style| {
        let visuals = &mut style.visuals;
        visuals.text_options.subpixel_binning = false;
        let (panel, field, idle, hovered, border, accent, selection) = if visuals.dark_mode {
            (
                32,
                26,
                45,
                56,
                112,
                Color32::from_rgb(96, 205, 255),
                Color32::from_rgb(31, 65, 88),
            )
        } else {
            visuals.widgets.noninteractive.fg_stroke.color = Color32::BLACK;
            visuals.widgets.inactive.fg_stroke.color = Color32::BLACK;
            (
                248,
                255,
                255,
                241,
                156,
                Color32::from_rgb(0, 95, 184),
                Color32::from_rgb(223, 236, 248),
            )
        };
        visuals.panel_fill = Color32::from_gray(panel);
        visuals.window_fill = visuals.panel_fill;
        visuals.text_edit_bg_color = Some(Color32::from_gray(field));
        visuals.selection.bg_fill = selection;
        visuals.selection.stroke = Stroke::new(2.0, accent);

        let widgets = &mut visuals.widgets;
        for (widget, fill, outline) in [
            (
                &mut widgets.inactive,
                idle,
                Stroke::new(1.0, Color32::from_gray(border)),
            ),
            (&mut widgets.hovered, hovered, Stroke::new(1.0, accent)),
            (&mut widgets.active, hovered, Stroke::new(2.0, accent)),
            (&mut widgets.open, hovered, Stroke::new(1.0, accent)),
        ] {
            widget.bg_fill = Color32::from_gray(fill);
            widget.weak_bg_fill = widget.bg_fill;
            widget.bg_stroke = outline;
            widget.corner_radius = 4.into();
            widget.expansion = 0.0;
        }
    });
}

fn run(view: View, wake: &Wake, store: &SharedStore, window: &Mutex<Window>) -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(view.size())
        .with_min_inner_size(view.min_size())
        .with_resizable(view.recent())
        .with_minimize_button(true)
        .with_maximize_button(view.recent());
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
        view.title(),
        options,
        Box::new(|cc| {
            apply_windows_style(&cc.egui_ctx);
            let handle = cc.window_handle()?;
            let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                unreachable!("ClipServe uses a Win32 window");
            };
            let hwnd = handle.hwnd.get() as usize;
            let mut state = window.lock().unwrap();
            state.visible = Some((cc.egui_ctx.clone(), hwnd));
            state.recent = view.recent();
            if state.exiting {
                close(hwnd);
            }
            let mut gui = Gui {
                window,
                wake: wake.clone(),
                view,
                editor: None,
                recent: RecentImages::new(Arc::clone(store), wake.clone()),
                can_copy: state.can_copy,
            };
            gui.open(view);
            Ok(Box::new(gui))
        }),
    )
}

struct Gui<'a> {
    window: &'a Mutex<Window>,
    wake: Wake,
    view: View,
    editor: Option<Editor>,
    recent: RecentImages,
    can_copy: bool,
}

impl Gui<'_> {
    fn open(&mut self, view: View) {
        if let View::Settings(config) = view {
            self.editor
                .get_or_insert_with(|| Editor::new(config, self.wake.clone()));
        }
        self.view = view;
    }
}

impl eframe::App for Gui<'_> {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        let mut window = self.window.lock().unwrap();
        self.can_copy = window.can_copy;
        if let Some(view) = window.next_view.take() {
            if self.view.recent() != view.recent() {
                ctx.send_viewport_cmd(ViewportCommand::Title(view.title().into()));
                if !view.recent() {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                    self.recent.hide_dialogs();
                }
                ctx.send_viewport_cmd(ViewportCommand::Resizable(view.recent()));
                ctx.send_viewport_cmd(ViewportCommand::EnableButtons {
                    close: true,
                    minimized: true,
                    maximize: view.recent(),
                });
                ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(view.min_size().into()));
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(view.size().into()));
            }
            self.open(view);
            window.recent = view.recent();
        }
        drop(window);
        if let Some(editor) = &mut self.editor {
            editor.logic(ctx, !self.view.recent());
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        match self.view {
            View::Settings(_) => self.editor.as_mut().unwrap().ui(ui),
            View::RecentImages => self.recent.ui(
                ui,
                self.can_copy,
                monitor_work_area(frame, ui.ctx().pixels_per_point()),
            ),
        }
    }
}
