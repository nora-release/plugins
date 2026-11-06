# Build stage: compile vmd
FROM docker.io/library/golang:1.25-alpine AS builder
WORKDIR /src
COPY . .
RUN go mod download
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /vmd ./cmd/vmd

# Runtime stage
FROM docker.io/library/debian:trixie-slim

# Install only essential system tools
RUN apt-get update && apt-get install -y --no-install-recommends \
    openssh-client \
    net-tools \
    curl \
    iproute2 \
    ripgrep \
    fd-find \
    iputils-ping \
    python3 \
    python3-pip \
    python3-venv \
    python-is-python3 \
    file \
    jq \
    e2fsprogs \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && find /usr /var -xdev -type d -name __pycache__ -prune -exec rm -rf '{}' + \
    && rm -rf \
        /usr/lib/python*/test \
        /usr/share/doc \
        /usr/share/man \
        /usr/share/info \
        /usr/share/lintian \
        /var/cache/debconf \
        /var/log/* \
    && rm -f /usr/lib/python*/EXTERNALLY-MANAGED

# Copy vmd binary as init from builder stage
# vmd serves as init (PID 1) and sandbox daemon
COPY --from=builder --chmod=755 /vmd /sbin/init
