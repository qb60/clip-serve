use image::ImageFormat;
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem},
};

use crate::{
    app::{Action, Command},
    platform::{AUTOSTART_LABEL, Wake},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub enabled: bool,
    pub healthy: bool,
    pub count: usize,
    pub autostart: bool,
}

pub struct Tray {
    icon: TrayIcon,
    enabled: CheckMenuItem,
    copy: MenuItem,
    autostart: CheckMenuItem,
    status: Option<Status>,
}

impl Tray {
    pub fn new(wake: Wake) -> Result<Self, Box<dyn std::error::Error>> {
        let enabled = CheckMenuItem::new("Enable capture", true, false, None);
        let copy = MenuItem::new("Copy latest image URL", false, None);
        let recent = MenuItem::new("Recent images...", true, None);
        let settings = MenuItem::new("Settings...", true, None);
        let autostart = CheckMenuItem::new(AUTOSTART_LABEL, true, false, None);
        let exit = MenuItem::new("Exit", true, None);
        let menu = Menu::with_items(&[&enabled, &copy, &recent, &settings, &autostart, &exit])?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_icon(status_icon(false, true)?)
            .with_tooltip("ClipServe")
            .build()?;

        let items = [
            (enabled.id().clone(), Action::ToggleCapture),
            (copy.id().clone(), Action::Copy),
            (recent.id().clone(), Action::RecentImages),
            (settings.id().clone(), Action::Settings),
            (autostart.id().clone(), Action::ToggleAutostart),
            (exit.id().clone(), Action::Exit),
        ];
        let menu_wake = wake.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, command)) = items.iter().find(|(id, _)| *id == event.id) {
                menu_wake.send(Command::Tray(*command));
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                wake.send(Command::Tray(Action::ToggleCapture));
            }
        }));
        Ok(Self {
            icon,
            enabled,
            copy,
            autostart,
            status: None,
        })
    }

    pub fn update(&mut self, status: Status, force: bool) {
        // A check item can toggle itself before the app accepts a change.
        self.enabled.set_checked(status.enabled);
        self.autostart.set_checked(status.autostart);
        self.copy.set_enabled(status.healthy && status.count != 0);
        if self.status == Some(status) && !force {
            return;
        }
        let state = match (status.healthy, status.enabled) {
            (false, _) => "suspended (server unavailable)",
            (true, true) => "enabled",
            (true, false) => "disabled",
        };
        let icon_changed = self
            .status
            .is_none_or(|old| old.enabled != status.enabled || old.healthy != status.healthy);
        if (force || icon_changed)
            && let Err(err) = status_icon(status.enabled, status.healthy)
                .and_then(|icon| Ok(self.icon.set_icon(Some(icon))?))
        {
            log::warn!("Could not update the tray icon: {err}");
        }
        if let Err(err) = self.icon.set_tooltip(Some(format!(
            "ClipServe · Capture {state} · {} images",
            status.count
        ))) {
            log::warn!("Could not update the tray tooltip: {err}");
        }
        self.status = Some(status);
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
        TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
    }
}

pub const DISABLED_ICON: &[u8] = include_bytes!("../assets/disabled.png");

fn status_icon(enabled: bool, healthy: bool) -> Result<Icon, Box<dyn std::error::Error>> {
    let bytes: &[u8] = match (healthy, enabled) {
        (false, _) => include_bytes!("../assets/error.png"),
        (true, true) => include_bytes!("../assets/enabled.png"),
        (true, false) => DISABLED_ICON,
    };
    let rgba = image::load_from_memory_with_format(bytes, ImageFormat::Png)?.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Icon::from_rgba(rgba.into_raw(), width, height)?)
}
