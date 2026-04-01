# ---------------------------------------------------------------------------
# Stage 1: Build the Dioxus web UI (WASM)
# ---------------------------------------------------------------------------
FROM rust:1.93-bookworm AS web-builder

RUN cargo install dioxus-cli

WORKDIR /src
COPY .cargo/ .cargo/
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

WORKDIR /src/crates/gateway-dioxus
RUN dx build --release

# ---------------------------------------------------------------------------
# Stage 2: Build the Rust binary
# ---------------------------------------------------------------------------
FROM rust:1.93-bookworm AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        pkg-config libssl-dev git && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# Copy pre-built web assets into the location rust-embed expects
COPY --from=web-builder /src/crates/gateway-dioxus/dist/ \
     crates/gateway-dioxus/dist/

RUN cargo build --release --bin savfox && \
    strip target/release/savfox

# ---------------------------------------------------------------------------
# Stage 3: Minimal runtime image
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates libssl3 curl && \
    rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 1000 savfox && \
    useradd --uid 1000 --gid savfox --shell /bin/bash --create-home savfox && \
    mkdir -p /home/savfox/.savfox && \
    chown -R savfox:savfox /home/savfox/.savfox

COPY --from=builder /src/target/release/savfox /usr/local/bin/savfox

USER savfox
WORKDIR /home/savfox

EXPOSE 18881

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:18881/health || exit 1

CMD ["savfox", "gateway", "--host", "0.0.0.0", "--port", "18881"]
