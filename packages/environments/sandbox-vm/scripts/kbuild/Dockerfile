FROM debian:bookworm-slim AS build_kernel_base
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    bc \
    bison \
    flex \
    libssl-dev \
    libelf-dev \
    dwarves \
    curl \
    xz-utils \
    ca-certificates \
    python3 \
    clang \
    lld \
    llvm \
    pkg-config \
    rsync \
    file && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /build/linux
RUN curl -fsSL -o linux.tar.xz https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.10.tar.xz && \
    tar -xJf linux.tar.xz --strip-components=1 && rm linux.tar.xz

COPY ./microvm-kernel-ci-aarch64-6.10.config /tmp/config.aarch64

FROM build_kernel_base AS build_kernel_arm64
WORKDIR /build/linux
RUN make mrproper && \
    cp /tmp/config.aarch64 .config && \
    make LLVM=1 ARCH=arm64 olddefconfig && \
    make LLVM=1 ARCH=arm64 -j"$(nproc)" Image && \
    mkdir -p /opt && cp arch/arm64/boot/Image /opt/kernel.arm64
