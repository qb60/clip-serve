#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod recent;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match app::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            app::show_error(&format!(
                "ClipServe could not start or continue.\n\n{}",
                app::error_detail(err.as_ref())
            ));
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("ClipServe requires an interactive Windows session.");
    std::process::ExitCode::FAILURE
}
