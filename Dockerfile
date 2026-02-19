# Multi-stage Dockerfile for Eidetica
ARG DEBIAN_RELEASE=trixie
ARG RUST_VERSION=1.93
ARG CARGO_CHEF_VERSION=0.1.73

# Stage 1: Base builder image with cargo-chef
FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS chef
ARG CARGO_CHEF_VERSION
# hadolint ignore=DL3008
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-chef --locked --version ${CARGO_CHEF_VERSION}
WORKDIR /build

# Stage 2: Compute dependency recipe
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Stage 3: Build dependencies (cached) then application
FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json -p eidetica-bin
COPY . .
RUN cargo build --release -p eidetica-bin

# Stage 4: Minimal runtime image
ARG DEBIAN_RELEASE
FROM debian:${DEBIAN_RELEASE}-slim

# Install runtime dependencies
# hadolint ignore=DL3008
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user
RUN useradd -m -u 1000 eidetica

# Copy the binary from builder
COPY --from=builder /build/target/release/eidetica /usr/local/bin/eidetica

# Set ownership
RUN chown eidetica:eidetica /usr/local/bin/eidetica

# Create config directory
RUN mkdir -p /config && chown eidetica:eidetica /config

# Switch to non-root user
USER eidetica
WORKDIR /config

# Environment variables
ENV EIDETICA_DATA_DIR=/config
ENV EIDETICA_HOST=0.0.0.0

# Expose default port
EXPOSE 3000

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD [ "eidetica", "health" ]

# Run the application
ENTRYPOINT ["/usr/local/bin/eidetica"]
