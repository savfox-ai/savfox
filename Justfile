# Savfox Justfile
# Usage: just <recipe>
# Install just: winget install Casey.Just

# Use PowerShell on Windows
set windows-shell := ["pwsh", "-NoProfile", "-Command"]

# Default values — override with: just port=9000 token=mytoken gateway
port  := "18881"
token := "test123"
dev_backend_port := "18881"
frontend_port := "18080"

# ── Gateway ─────────────────────────────────────────────────────────────────

# Build web frontend if needed + run gateway server (debug)
gateway:
    pwsh -NoProfile -File scripts/build-web.ps1
    cargo run --bin savfox -- gateway --port {{port}} --token {{token}}

# Build web frontend if needed + run gateway server (release)
gateway-release:
    pwsh -NoProfile -File scripts/build-web.ps1 -Release
    cargo run --release --bin savfox -- gateway --port {{port}} --token {{token}}

# Run gateway server without rebuilding the web frontend
gateway-skip-web:
    cargo run --bin savfox -- gateway --port {{port}} --token {{token}}

# Run the gateway backend only, on the fixed dev port expected by the Dioxus proxy config
gateway-backend:
    cargo run --bin savfox -- gateway --port {{dev_backend_port}} --token {{token}}

# ── Web frontend ─────────────────────────────────────────────────────────────

# One-shot web build (debug)
web-build:
    pwsh -NoProfile -File scripts/build-web.ps1

# One-shot web build (release)
web-build-release:
    pwsh -NoProfile -File scripts/build-web.ps1 -Release

# Live-reload web frontend (run alongside `just gateway-skip-web` in another terminal)
web-serve:
    Push-Location crates/gateway-dioxus; dx serve --web --port {{frontend_port}} --open false; Pop-Location

# Live-reload Dioxus frontend with dev-server proxying /api, /health, and /ws to the local gateway backend
gateway-frontend:
    Push-Location crates/gateway-dioxus; dx serve --web --port {{frontend_port}} --open false; Pop-Location

# ── General Cargo ─────────────────────────────────────────────────────────────

# Check entire workspace
check:
    cargo check --workspace

# Clippy entire workspace
lint:
    cargo clippy --workspace

# Format entire workspace
fmt:
    cargo +nightly fmt --all

# Run all tests
test:
    cargo test --workspace

# ── Install ─────────────────────────────────────────────────────────────────

# Build release and install savfox CLI tools to ~/bin
install:
    cargo build
    New-Item -ItemType Directory -Force -Path "$HOME/bin" | Out-Null
    Get-ChildItem target/debug -Filter "savfox*.exe" | Copy-Item -Destination "$HOME/bin" -Force

# ── Utilities ────────────────────────────────────────────────────────────────

# Print available recipes
help:
    @just --list
