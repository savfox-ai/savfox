#!/bin/bash
# Savfox installer script
# Usage: curl -fsSL https://raw.githubusercontent.com/chrislearn/savfox/main/scripts/install.sh | bash
set -euo pipefail

REPO="chrislearn/savfox"
INSTALL_DIR="${SAVFOX_INSTALL_DIR:-/usr/local/bin}"
BINARY="savfox"

# Detect OS and architecture
detect_platform() {
    local os arch

    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os="linux" ;;
        Darwin) os="darwin" ;;
        *)      echo "Unsupported OS: $os" >&2; exit 1 ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch="amd64" ;;
        aarch64|arm64) arch="arm64" ;;
        *)             echo "Unsupported architecture: $arch" >&2; exit 1 ;;
    esac

    echo "${os}-${arch}"
}

# Get latest release tag from GitHub
get_latest_version() {
    curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' \
        | sed -E 's/.*"([^"]+)".*/\1/'
}

main() {
    local platform version archive_name url tmp_dir

    platform="$(detect_platform)"
    version="${SAVFOX_VERSION:-$(get_latest_version)}"

    if [ -z "$version" ]; then
        echo "Error: Could not determine latest version." >&2
        exit 1
    fi

    echo "Installing Savfox ${version} for ${platform}..."

    archive_name="savfox-${platform}.tar.gz"
    url="https://github.com/${REPO}/releases/download/${version}/${archive_name}"

    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "$tmp_dir"' EXIT

    echo "Downloading ${url}..."
    curl -fsSL "$url" -o "${tmp_dir}/${archive_name}"

    echo "Extracting..."
    tar xzf "${tmp_dir}/${archive_name}" -C "$tmp_dir"

    echo "Installing to ${INSTALL_DIR}/${BINARY}..."
    if [ -w "$INSTALL_DIR" ]; then
        mv "${tmp_dir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    else
        sudo mv "${tmp_dir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    fi
    chmod +x "${INSTALL_DIR}/${BINARY}"

    echo ""
    echo "Savfox ${version} installed successfully!"
    echo "Run 'savfox --help' to get started."
}

main "$@"
