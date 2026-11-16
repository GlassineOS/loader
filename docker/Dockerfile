# Build and test environment of the loader. Pinned by digest; the Debian packages come
# from the trixie release the image ships. OVMF and AAVMF come from these packages at
# test time and are never committed.
FROM rust:1.99.1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7
RUN apt-get update && apt-get install -y --no-install-recommends \
        clang lld llvm make curl ca-certificates xz-utils git \
        qemu-system-x86 qemu-system-arm ovmf qemu-efi-aarch64 \
        mtools dosfstools gdisk python3 python3-pil \
        sbsigntool openssl python3-virt-firmware \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-uefi aarch64-unknown-uefi
ENV CARGO_HOME=/work/.cache/cargo
