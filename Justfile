# Savfox Justfile
# Usage: just <recipe>
# Install just: winget install Casey.Just

# Use PowerShell on Windows
set windows-shell := ["pwsh", "-NoProfile", "-Command"]

# Default values - override ports with: just port=9000 gateway
# Optional gateway token: set SAVFOX_GATEWAY_TOKEN. If unset, the gateway
# generates and persists a token at startup.
port  := "18881"
dev_backend_port := "18881"
frontend_port := "18080"

# ── Gateway ─────────────────────────────────────────────────────────────────

# Build web frontend if needed + run gateway server (debug)
gateway:
    pwsh -NoProfile -File scripts/build-web.ps1
    $gatewayArgs = @('run', '--bin', 'savfox', '--', 'gateway', '--port', '{{port}}'); if ($env:SAVFOX_GATEWAY_TOKEN) { $gatewayArgs += @('--token', $env:SAVFOX_GATEWAY_TOKEN) }; cargo @gatewayArgs

# Build web frontend if needed + run gateway server (release)
gateway-release:
    pwsh -NoProfile -File scripts/build-web.ps1 -Release
    $gatewayArgs = @('run', '--release', '--bin', 'savfox', '--', 'gateway', '--port', '{{port}}'); if ($env:SAVFOX_GATEWAY_TOKEN) { $gatewayArgs += @('--token', $env:SAVFOX_GATEWAY_TOKEN) }; cargo @gatewayArgs

# Run gateway server without rebuilding the web frontend
gateway-skip-web:
    $gatewayArgs = @('run', '--bin', 'savfox', '--', 'gateway', '--port', '{{port}}'); if ($env:SAVFOX_GATEWAY_TOKEN) { $gatewayArgs += @('--token', $env:SAVFOX_GATEWAY_TOKEN) }; cargo @gatewayArgs

# Run the Arkret-enabled gateway backend on the fixed dev port expected by the Dioxus proxy config
gateway-backend:
    $gatewayArgs = @('run', '--bin', 'savfox', '--features', 'savfox-gateway-server/arkret', '--', 'gateway', '--port', '{{dev_backend_port}}'); if ($env:SAVFOX_GATEWAY_TOKEN) { $gatewayArgs += @('--token', $env:SAVFOX_GATEWAY_TOKEN) }; cargo @gatewayArgs

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
    cargo clippy --workspace --all-targets -- -D warnings

# Format entire workspace
fmt:
    cargo +nightly fmt --all

# Check formatting without writing changes
fmt-check:
    cargo +nightly fmt --all -- --check

# Run all tests
test:
    cargo test --workspace

# Regenerate the config JSON schema fixture
write-config-schema:
    cargo run -p savfox-core --bin savfox-write-config-schema

# Targeted tests: core/runtime crates
test-core:
    cargo test -p savfox-core -p savfox-config -p savfox-model -p savfox-http-client -p savfox-api-client

# Targeted tests: protocol and editor-facing services
test-protocol:
    cargo test -p savfox-protocol -p savfox-app-server-protocol -p savfox-app-server -p savfox-mcp-server

# Targeted tests: interactive TUI surface
test-tui:
    cargo test -p savfox-tui

# Targeted tests: gateway, shared gateway types, and channels
test-gateway:
    cargo test -p savfox-gateway-server -p savfox-gateway-shared -p savfox-channels

# Targeted tests: channel adapters only
test-channels:
    cargo test -p savfox-channels

# Targeted build check: web frontend
test-web:
    Push-Location crates/gateway-dioxus; dx build --web; Pop-Location

# ── Install ─────────────────────────────────────────────────────────────────

# Build release and install savfox CLI tools to ~/bin
install:
    cargo build
    New-Item -ItemType Directory -Force -Path "$HOME/bin" | Out-Null
    Get-ChildItem target/debug -Filter "savfox*.exe" | Copy-Item -Destination "$HOME/bin" -Force

# Build debug and copy exes to ~/bin (fast dev iteration)
dist-dev:
    cargo build
    New-Item -ItemType Directory -Force -Path "$HOME/bin" | Out-Null
    Get-ChildItem target/debug -Filter "savfox*.exe" | Copy-Item -Destination "$HOME/bin" -Force
    Write-Host "Copied to ~/bin:" -ForegroundColor Green
    Get-ChildItem "$HOME/bin/savfox*.exe" | ForEach-Object { Write-Host "  $_" }

# ── Utilities ────────────────────────────────────────────────────────────────

# Print available recipes
help:
    @just --list
