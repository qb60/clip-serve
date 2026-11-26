#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod platform;
#[cfg(windows)]
mod recent;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match platform::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            platform::show_error(&format!(
                "ClipServe could not start or continue.\n\n{}",
                platform::error_detail(err.as_ref())
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
