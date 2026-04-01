#!/usr/bin/env bash
set -euo pipefail

# Savfox Gateway Installer
# Usage: curl -fsSL https://raw.githubusercontent.com/savfox-ai/savfox/main/deploy/install.sh | bash

VERSION="${SAVFOX_VERSION:-latest}"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
DATA_DIR="${SAVFOX_HOME:-$HOME/.savfox}"

# Detect OS and architecture
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
esac

case "$OS" in
    linux) TARGET="${ARCH}-unknown-linux-musl" ;;
    darwin) TARGET="${ARCH}-apple-darwin" ;;
    *) echo "Unsupported OS: $OS"; exit 1 ;;
esac

echo "Installing Savfox for ${TARGET}..."

# Download
if [ "$VERSION" = "latest" ]; then
    DOWNLOAD_URL="https://github.com/savfox-ai/savfox/releases/latest/download/savfox-${TARGET}"
else
    DOWNLOAD_URL="https://github.com/savfox-ai/savfox/releases/download/v${VERSION}/savfox-${TARGET}"
fi

TMPFILE="$(mktemp)"
trap 'rm -f "$TMPFILE"' EXIT

echo "Downloading from ${DOWNLOAD_URL}..."
curl -fSL -o "$TMPFILE" "$DOWNLOAD_URL"
chmod +x "$TMPFILE"

# Install
if [ -w "$INSTALL_DIR" ]; then
    mv "$TMPFILE" "${INSTALL_DIR}/savfox"
else
    echo "Installing to ${INSTALL_DIR} (requires sudo)..."
    sudo mv "$TMPFILE" "${INSTALL_DIR}/savfox"
fi

# Create data directory
mkdir -p "$DATA_DIR"

echo ""
echo "Savfox installed successfully!"
echo "  Binary: ${INSTALL_DIR}/savfox"
echo "  Data:   ${DATA_DIR}"
echo ""
echo "Quick start:"
echo "  savfox gateway --port 18881"
echo ""
echo "For more information: https://github.com/savfox-ai/savfox"
