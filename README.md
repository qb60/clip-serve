# ClipServe

ClipServe is a portable Windows 10/11 x64 app that turns clipboard images into HTTP links.

Run `ClipServe.exe` and open its tray menu. By default, capture starts off and the server listens on `127.0.0.1:8888`. For access from other devices, choose a reachable local IPv4 address in **Settings...**.

- Left-click the tray icon to toggle capture. With capture on, copying an image replaces it with a URL.
- Open **Recent images...** to browse saved images. Double-click a thumbnail or choose **Preview** to see it larger. Use **Copy URL** or **Restore image** to put the link or image back on the clipboard.
- Use **Copy latest image URL** for quick access to the newest image. Erase the history with **Clear stored images** in the gallery.
- ClipServe keeps the latest 10 images in memory by default. You can change the limit in **Settings...**. Exiting ClipServe clears all images.

## Windows Firewall

If Windows Firewall prompts you on first launch, allow access on both **Private** and **Public** networks. Otherwise, connections from virtual machines and other devices may be blocked.
