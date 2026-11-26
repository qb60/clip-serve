FROM rust:1.99.1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7 AS build

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates clang curl gcc libc6-dev lld llvm jq zip \
    && rm -rf /var/lib/apt/lists/*
RUN rustup component add clippy rustfmt && rustup target add x86_64-pc-windows-msvc
RUN curl --fail --silent --show-error --location https://github.com/rust-cross/cargo-xwin/releases/download/v0.24.1/cargo-xwin-v0.24.1.x86_64-unknown-linux-musl.tar.gz --output /tmp/cargo-xwin.tar.gz \
    && echo '207ec5436db138b60d90cb41513ce2e3db4893a9c4e25da6209790c1a2db88cf  /tmp/cargo-xwin.tar.gz' | sha256sum --check \
    && tar -xzf /tmp/cargo-xwin.tar.gz -C /usr/local/cargo/bin cargo-xwin \
    && rm /tmp/cargo-xwin.tar.gz
RUN apt-get update && apt-get install -y --no-install-recommends xz-utils && rm -rf /var/lib/apt/lists/*
RUN curl --fail --show-error --silent --location --retry 3 https://github.com/trcrsired/windows-msvc-sysroot/releases/download/2026-01-16/windows-msvc-sysroot.tar.xz -o /tmp/msvc-sdk.tar.xz \
    && echo 'ba765d78c02fee46564ec1c4a8d711c828260ece21627721e4d95a0161ae0894  /tmp/msvc-sdk.tar.xz' | sha256sum --check \
    && tar -xJf /tmp/msvc-sdk.tar.xz -C /opt \
    && rm /tmp/msvc-sdk.tar.xz \
    && test -d /opt/windows-msvc-sysroot/include
RUN apt-get update && apt-get install -y --no-install-recommends patch && rm -rf /var/lib/apt/lists/*
ENV XWIN_CROSS_COMPILER=clang XWIN_MSVC_SYSROOT=/opt/windows-msvc-sysroot

WORKDIR /work
COPY Cargo.toml Cargo.lock build.rs ./
COPY .cargo .cargo
COPY assets assets
COPY src src
COPY tests tests
COPY scripts/gpui-embedded-shaders.patch ./

RUN --mount=type=cache,id=clipserve-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=clipserve-cargo-target,target=/work/target,sharing=locked \
    cargo fmt --check \
    && cargo clippy --locked --all-targets -- -D warnings \
    && cargo test --locked \
    && cargo fetch --locked --target x86_64-pc-windows-msvc \
    && cp -R /usr/local/cargo/registry/src/*/gpui-pre-windows-0.3.14 /work/gpui-pre-windows \
    && patch -d /work/gpui-pre-windows -p1 < gpui-embedded-shaders.patch \
    && cd /work/gpui-pre-windows/src \
    && sed '/^#include "alpha_correction.hlsl"/d' shaders.hlsl > shaders-body.hlsl \
    && cat alpha_correction.hlsl shaders-body.hlsl > shaders.hlsl \
    && cd /work \
    && CLIPSERVE_EMBEDDED_SHADERS=1 cargo xwin clippy --config 'paths=["/work/gpui-pre-windows"]' --locked --all-targets --target x86_64-pc-windows-msvc -- -D warnings \
    && CLIPSERVE_EMBEDDED_SHADERS=1 cargo xwin build --config 'paths=["/work/gpui-pre-windows"]' --release --locked --target x86_64-pc-windows-msvc \
    && cp target/x86_64-pc-windows-msvc/release/clipserve.exe ClipServe.exe

COPY README.md ./
ARG RELEASE_TAG
RUN cargo metadata --locked --no-deps --format-version 1 > /tmp/metadata.json \
    && version="$(jq -er '.packages[] | select(.name == "clipserve") | .version' /tmp/metadata.json)" \
    && if [ -n "$RELEASE_TAG" ] && [ "$RELEASE_TAG" != "v$version" ]; then \
        echo "Release tag $RELEASE_TAG does not match Cargo.toml version $version." >&2; exit 1; \
    fi \
    && mkdir /out \
    && cp ClipServe.exe /out/ClipServe.exe \
    && cp README.md /out/README.md \
    && cp /work/gpui-pre-windows/LICENSE-APACHE /out/LICENSE-GPUI.txt \
    && cd /out \
    && zip -q ClipServe-windows-x64.zip ClipServe.exe README.md LICENSE-GPUI.txt \
    && rm README.md LICENSE-GPUI.txt \
    && sha256sum ClipServe.exe ClipServe-windows-x64.zip > SHA256SUMS.txt \
    && wc -c ClipServe.exe

FROM scratch AS artifacts
COPY --from=build /out/ /
