# Build + test environment for the loader spike.
# Pinned by digest; Debian packages come from the trixie release the image ships.
FROM rust:1.99.1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7
LABEL org.glassine.spike=loader
RUN apt-get update && apt-get install -y --no-install-recommends \
        clang lld llvm make curl ca-certificates xz-utils \
        qemu-system-x86 ovmf mtools dosfstools python3 python3-pil \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-uefi && rustup component add rust-src
ENV CARGO_HOME=/work/.cache/cargo
