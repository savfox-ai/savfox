#!/bin/bash
# Package Savfox as a macOS .app bundle.
# Usage: ./scripts/package-mac-app.sh [--release]
set -euo pipefail

APP_NAME="Savfox"
BUNDLE_ID="ai.savfox.gateway"
VERSION=$(grep '^version\s*=' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
PROFILE="debug"

if [[ "${1:-}" == "--release" ]]; then
    PROFILE="release"
fi

# Build the binary
echo "Building savfox ($PROFILE)..."
if [[ "$PROFILE" == "release" ]]; then
    cargo build --release --bin savfox
else
    cargo build --bin savfox
fi

BINARY="target/${PROFILE}/savfox"
if [[ ! -f "$BINARY" ]]; then
    echo "Error: Binary not found at $BINARY"
    exit 1
fi

# Create .app bundle structure
APP_DIR="target/${APP_NAME}.app"
rm -rf "$APP_DIR"
mkdir -p "${APP_DIR}/Contents/MacOS"
mkdir -p "${APP_DIR}/Contents/Resources"

# Copy binary
cp "$BINARY" "${APP_DIR}/Contents/MacOS/savfox"

# Create Info.plist
cat > "${APP_DIR}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleExecutable</key>
    <string>savfox</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

echo ""
echo "App bundle created at: $APP_DIR"
echo "Version: $VERSION"
echo ""
echo "To create a DMG, run: ./scripts/create-dmg.sh"
