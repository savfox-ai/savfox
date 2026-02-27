# Stage 1: Build
FROM rust:1.89-bookworm AS builder

WORKDIR /build
COPY . .

# Install wasm-pack for the web UI build
RUN cargo install wasm-pack

# Build the CLI binary (which includes the gateway server)
RUN cargo build --release -p savfox-cli

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN groupadd -r savfox && useradd -r -g savfox -m savfox

COPY --from=builder /build/target/release/savfox /usr/local/bin/savfox

# Create data directories
RUN mkdir -p /data/savfox && chown savfox:savfox /data/savfox

USER savfox
WORKDIR /data/savfox

ENV SAVFOX_HOME=/data/savfox
ENV RUST_LOG=info

EXPOSE 18881

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:18881/health || exit 1

ENTRYPOINT ["savfox"]
CMD ["gateway", "--port", "18881", "--host", "0.0.0.0"]
