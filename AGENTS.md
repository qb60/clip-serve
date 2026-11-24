# ClipServe

- Keep ClipServe portable as one Windows x64 executable. Use English for UI text, errors, comments, and documentation.
- Run `docker build --platform linux/amd64 --progress plain --output type=local,dest=dist .` for the same checks and Windows cross-build as CI.
- Keep interactive Windows clipboard and tray checks manual.
- Keep the local SPEC.md out of commits.
