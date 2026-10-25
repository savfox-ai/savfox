# syntax=docker/dockerfile:1
# ---------------------------------------------------------------------------
# Stage 1: Build the Glory + Sapid web UI (WASM)
# ---------------------------------------------------------------------------
FROM rust:bookworm AS web-builder
ARG WASM_BINDGEN_VERSION=0.2.129

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        clang lld && \
    rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-unknown-unknown && \
    cargo install wasm-bindgen-cli --version "${WASM_BINDGEN_VERSION}" --locked

COPY --from=glory /Cargo.toml /Cargo.lock /work/glory-rs/glory/
COPY --from=glory /crates /work/glory-rs/glory/crates/
COPY --from=sapid /Cargo.toml /README.md /work/glory-rs/sapid/
COPY --from=sapid /src /work/glory-rs/sapid/src/
COPY --from=sapid /styles /work/glory-rs/sapid/styles/
WORKDIR /work/savfox-ai/savfox
COPY .cargo/ .cargo/
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

RUN cargo build --release -p savfox-gateway-glory --lib --target wasm32-unknown-unknown && \
    mkdir -p /web/wasm /web/assets && \
    wasm-bindgen target/wasm32-unknown-unknown/release/savfox_gateway_glory.wasm \
        --target web --out-name savfox_gateway_glory --out-dir /web/wasm && \
    cp crates/gateway-glory/index.html /web/index.html && \
    cp -r crates/gateway-glory/assets/. /web/assets/ && \
    cp /work/glory-rs/sapid/styles/sapid.css /web/assets/sapid.css

# ---------------------------------------------------------------------------
# Stage 2: Build the Rust binary
# ---------------------------------------------------------------------------
FROM rust:bookworm AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        clang lld pkg-config libssl-dev git && \
    rm -rf /var/lib/apt/lists/*

COPY --from=glory /Cargo.toml /Cargo.lock /work/glory-rs/glory/
COPY --from=glory /crates /work/glory-rs/glory/crates/
COPY --from=sapid /Cargo.toml /README.md /work/glory-rs/sapid/
COPY --from=sapid /src /work/glory-rs/sapid/src/
WORKDIR /work/savfox-ai/savfox
COPY . .

# Copy pre-built web assets into the location rust-embed expects
COPY --from=web-builder /web/ \
     crates/gateway-server/static/

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

COPY --from=builder /work/savfox-ai/savfox/target/release/savfox /usr/local/bin/savfox

USER savfox
WORKDIR /home/savfox

EXPOSE 18881

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:18881/health || exit 1

CMD ["savfox", "gateway", "--host", "0.0.0.0", "--port", "18881"]
