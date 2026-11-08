# syntax=docker/dockerfile:1
# ---------------------------------------------------------------------------
# Stage 1: Build the Glory + Sapid web UI (WASM)
# ---------------------------------------------------------------------------
FROM rust:bookworm AS web-builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        clang lld git jq python3 && \
    rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-unknown-unknown

WORKDIR /work/savfox-ai/savfox
COPY .cargo/ .cargo/
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# The CLI must match the actual root lock copied above, not a second version constant.
RUN version="$(python3 -c 'import pathlib,re,sys,tomllib; rows=[p["version"] for p in tomllib.loads(pathlib.Path("Cargo.lock").read_text())["package"] if p["name"]=="wasm-bindgen"]; (len(rows)==1 and isinstance(rows[0],str) and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?",rows[0])) or sys.exit("Cargo.lock must select exactly one valid wasm-bindgen version"); print(rows[0],end="")')" && \
    cargo install wasm-bindgen-cli --version "=$version" --locked && \
    test "$(wasm-bindgen --version)" = "wasm-bindgen $version"

RUN cargo build --locked --release -p savfox-gateway-glory --lib --target wasm32-unknown-unknown && \
    mkdir -p /web/wasm /web/assets && \
    wasm-bindgen target/wasm32-unknown-unknown/release/savfox_gateway_glory.wasm \
        --target web --out-name savfox_gateway_glory --out-dir /web/wasm && \
    cp crates/gateway-glory/index.html /web/index.html && \
    cp -r crates/gateway-glory/assets/. /web/assets/ && \
    sapid_manifest="$(cargo metadata --locked --format-version 1 --filter-platform wasm32-unknown-unknown | \
      jq -er '.packages[] | select(.name == "sapid" and .version == "0.7.1" and .source == "registry+https://github.com/rust-lang/crates.io-index") | .manifest_path')" && \
    cp "$(dirname "$sapid_manifest")/styles/sapid.css" /web/assets/sapid.css

# ---------------------------------------------------------------------------
# Stage 2: Build the Rust binary
# ---------------------------------------------------------------------------
FROM rust:bookworm AS builder

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        clang lld pkg-config libssl-dev git && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /work/savfox-ai/savfox
COPY . .

# Copy pre-built web assets into the location rust-embed expects
COPY --from=web-builder /web/ \
     crates/gateway-server/static/

RUN cargo build --locked --release --bin savfox && \
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
