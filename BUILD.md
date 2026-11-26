# Build and release ClipServe

## Build

On Linux x64, build with Docker and BuildKit. The container runs formatting, Clippy, and tests before building and packaging the Windows executable. The first build downloads the Windows SDK and CRT under the [Microsoft license used by cargo-xwin](https://go.microsoft.com/fwlink/?LinkId=2086102).

```bash
docker build --platform linux/amd64 --progress plain --output type=local,dest=dist .
(cd dist && sha256sum -c SHA256SUMS.txt)
```

On Linux, the Docker build embeds GPUI's Direct3D shader source. ClipServe compiles it once on the desktop thread at startup and reuses the bytecode. Native Windows release builds precompile the shaders with the Windows SDK's `fxc.exe`.

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

On `main`, use the release script with Bash, Git, and Cargo. It updates `Cargo.toml` and `Cargo.lock`, commits the version change, and creates the annotated tag locally.

```bash
scripts/release.sh 0.1.3
git push origin main
```

After CI passes, push the tag printed by the script.

```bash
git push origin v0.1.3
```

The release workflow publishes `ClipServe.exe`, `ClipServe-windows-x64.zip`, and `SHA256SUMS.txt` under **Releases**.
