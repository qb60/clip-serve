# Build and release ClipServe

## Build

On Linux x64, build with Docker and BuildKit. The container runs formatting, Clippy, and tests before building and packaging the Windows executable. The first build downloads the Windows SDK and CRT under the [Microsoft license used by cargo-xwin](https://go.microsoft.com/fwlink/?LinkId=2086102).

```bash
docker build --platform linux/amd64 --progress plain --output type=local,dest=dist .
(cd dist && sha256sum -c SHA256SUMS.txt)
```

For a native Windows build, install stable Rust and Visual Studio Build Tools with **Desktop development with C++** and a Windows SDK.

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc
```

## Publish a GitHub release

Create an empty GitHub repository and enable Actions. Replace `OWNER` and `REPO`, then push `main`.

```bash
git remote add origin https://github.com/OWNER/REPO.git
git push -u origin main
```

After CI passes, push a tag matching the version in `Cargo.toml`. For later releases, update the package version and `Cargo.lock` before tagging.

```bash
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
```

The release workflow publishes `ClipServe.exe`, `ClipServe-windows-x64.zip`, and `SHA256SUMS.txt` under **Releases**.
