# Multi-stage Dockerfile for Eidetica
# Stage 1: Build the application
FROM rust:1-slim AS builder

# Install build dependencies
# hadolint ignore=DL3008
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy manifests and source
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
COPY examples/ ./examples/

# Build the application in release mode
RUN cargo build --release -p eidetica-bin

# Stage 2: Create minimal runtime image
FROM debian:bookworm-slim

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
