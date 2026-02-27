#!/usr/bin/env bash
# Savfox installer — one-line install:
#   curl -fsSL https://savfox.ai/install.sh | bash
#
# Environment variables:
#   SAVFOX_VERSION  — specific version to install (default: latest)
#   SAVFOX_DIR      — installation directory (default: ~/.savfox/bin)
#   SAVFOX_REPO     — GitHub repo (default: savfox-ai/savfox)

set -euo pipefail

REPO="${SAVFOX_REPO:-savfox-ai/savfox}"
INSTALL_DIR="${SAVFOX_DIR:-$HOME/.savfox/bin}"
VERSION="${SAVFOX_VERSION:-latest}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info() { printf "${BLUE}info${NC}  %s\n" "$1"; }
warn() { printf "${YELLOW}warn${NC}  %s\n" "$1"; }
error() { printf "${RED}error${NC} %s\n" "$1" >&2; }
success() { printf "${GREEN}✓${NC} %s\n" "$1"; }

# Detect OS and architecture
detect_platform() {
    local os arch

    case "$(uname -s)" in
        Linux*)   os="linux" ;;
        Darwin*)  os="darwin" ;;
        MINGW*|MSYS*|CYGWIN*) os="windows" ;;
        *)
            error "Unsupported operating system: $(uname -s)"
            exit 1
            ;;
    esac

    case "$(uname -m)" in
        x86_64|amd64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *)
            error "Unsupported architecture: $(uname -m)"
            exit 1
            ;;
    esac

    PLATFORM="${os}"
    ARCH="${arch}"

    if [ "$os" = "windows" ]; then
        BINARY_NAME="savfox.exe"
        ARCHIVE_EXT="zip"
    else
        BINARY_NAME="savfox"
        ARCHIVE_EXT="tar.gz"
    fi
}

# Get the download URL for the specified version
get_download_url() {
    local version="$1"
    local base_url="https://github.com/${REPO}/releases"

    if [ "$version" = "latest" ]; then
        # Fetch latest release tag
        version=$(curl -fsSL "${base_url}/latest" -o /dev/null -w '%{redirect_url}' | grep -oE '[^/]+$')
        if [ -z "$version" ]; then
            error "Could not determine latest version"
            exit 1
        fi
        info "Latest version: ${version}"
    fi

    local filename="savfox-${version}-${ARCH}-${PLATFORM}.${ARCHIVE_EXT}"
    echo "${base_url}/download/${version}/${filename}"
}

# Download and install
install() {
    local url="$1"
    local tmpdir

    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT

    info "Downloading from ${url}..."

    if command -v curl &>/dev/null; then
        curl -fsSL "$url" -o "${tmpdir}/savfox-archive"
    elif command -v wget &>/dev/null; then
        wget -qO "${tmpdir}/savfox-archive" "$url"
    else
        error "Neither curl nor wget found. Please install one of them."
        exit 1
    fi

    info "Extracting..."

    if [ "$ARCHIVE_EXT" = "zip" ]; then
        unzip -q "${tmpdir}/savfox-archive" -d "${tmpdir}/extracted"
    else
        mkdir -p "${tmpdir}/extracted"
        tar -xzf "${tmpdir}/savfox-archive" -C "${tmpdir}/extracted"
    fi

    # Find the binary
    local binary
    binary=$(find "${tmpdir}/extracted" -name "${BINARY_NAME}" -type f | head -1)

    if [ -z "$binary" ]; then
        error "Binary '${BINARY_NAME}' not found in archive"
        exit 1
    fi

    # Install to target directory
    mkdir -p "$INSTALL_DIR"
    cp "$binary" "${INSTALL_DIR}/${BINARY_NAME}"
    chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

    success "Installed to ${INSTALL_DIR}/${BINARY_NAME}"
}

# Add to PATH if needed
setup_path() {
    if echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
        return
    fi

    warn "${INSTALL_DIR} is not in your PATH"

    local shell_config=""
    case "${SHELL:-}" in
        */zsh)  shell_config="$HOME/.zshrc" ;;
        */bash) shell_config="$HOME/.bashrc" ;;
        */fish) shell_config="$HOME/.config/fish/config.fish" ;;
    esac

    if [ -n "$shell_config" ]; then
        if [ "${SHELL:-}" = "*/fish" ]; then
            echo "fish_add_path ${INSTALL_DIR}" >> "$shell_config"
        else
            echo "export PATH=\"${INSTALL_DIR}:\$PATH\"" >> "$shell_config"
        fi
        info "Added ${INSTALL_DIR} to PATH in ${shell_config}"
        info "Run 'source ${shell_config}' or restart your shell"
    else
        info "Add this to your shell config:"
        info "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    fi
}

# Verify installation
verify() {
    if "${INSTALL_DIR}/${BINARY_NAME}" --version &>/dev/null; then
        local version
        version=$("${INSTALL_DIR}/${BINARY_NAME}" --version 2>&1 || true)
        success "Savfox installed successfully: ${version}"
    else
        warn "Binary installed but could not verify. You may need to install system dependencies."
    fi
}

main() {
    echo ""
    echo "  ╔═══════════════════════════╗"
    echo "  ║   Savfox Installer        ║"
    echo "  ╚═══════════════════════════╝"
    echo ""

    detect_platform
    info "Platform: ${PLATFORM}/${ARCH}"
    info "Install directory: ${INSTALL_DIR}"

    local url
    url=$(get_download_url "$VERSION")

    install "$url"
    setup_path
    verify

    echo ""
    info "Get started:"
    info "  savfox exec \"Hello, world!\""
    info "  savfox gateway --port 18881"
    echo ""
}

main "$@"
