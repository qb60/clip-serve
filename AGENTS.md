# ClipServe

ClipServe is a Windows tray app that turns clipboard images into HTTP links.

## Code map

- `src/app.rs` coordinates Win32 events, tray actions, clipboard operations, and the HTTP listener.
- `src/clipboard.rs` reads and writes the Windows clipboard. `src/capture.rs` handles capture state and image processing.
- `src/store.rs` keeps images and thumbnails in memory. `src/http.rs` serves individual PNGs. Loopback HTTP tests are in `tests/http.rs`.
- `src/desktop.rs` runs the GPUI windows. `src/settings.rs` handles settings, `src/recent.rs` handles the gallery and preview, and `src/tray.rs` defines the tray menu.
- `src/config.rs` validates and saves configuration. `src/autostart.rs` manages Windows startup.

## Constraints

- Keep ClipServe portable as one Windows x64 executable with `config.json` alongside it. Use English for UI text, errors, comments, and documentation.
- Keep images in RAM and evict the oldest first. Keep the existing URL for an image while it is stored. Serve images only by individual URL, with no HTTP listing.
- Before modifying the clipboard, check its sequence number. Keep full-size image processing off the UI thread.
- Follow [BUILD.md](BUILD.md) for the checks and Windows cross-build used by CI. Verify interactive clipboard and tray behavior on Windows.
