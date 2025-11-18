# Multi-stage Dockerfile for Eidetica
# Stage 1: Build the application
FROM rust:1-slim AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
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
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user
RUN useradd -m -u 1000 eidetica

# Copy the binary from builder
COPY --from=builder /build/target/release/eidetica /usr/local/bin/eidetica

# Set ownership
RUN chown eidetica:eidetica /usr/local/bin/eidetica

# Switch to non-root user
USER eidetica
WORKDIR /home/eidetica

# Environment variables
ENV RUST_LOG=info

# Expose default port (adjust if needed)
EXPOSE 3000

# Health check (adjust endpoint as needed)
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD [ "eidetica", "--help" ]

# Run the application
ENTRYPOINT ["/usr/local/bin/eidetica"]
