#!/bin/bash
# Create a DMG installer for Savfox macOS app.
# Usage: ./scripts/create-dmg.sh [--sign IDENTITY]
set -euo pipefail

APP_NAME="Savfox"
VERSION=$(grep '^version\s*=' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
APP_DIR="target/${APP_NAME}.app"
DMG_NAME="Savfox-${VERSION}.dmg"
DMG_DIR="target/dmg"
SIGN_IDENTITY=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --sign) SIGN_IDENTITY="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

if [[ ! -d "$APP_DIR" ]]; then
    echo "Error: App bundle not found at $APP_DIR"
    echo "Run ./scripts/package-mac-app.sh first."
    exit 1
fi

# Codesign if identity provided
if [[ -n "$SIGN_IDENTITY" ]]; then
    echo "Signing app bundle with identity: $SIGN_IDENTITY"
    codesign --force --deep --sign "$SIGN_IDENTITY" "$APP_DIR"
fi

# Create staging directory
rm -rf "$DMG_DIR"
mkdir -p "$DMG_DIR"
cp -r "$APP_DIR" "$DMG_DIR/"

# Create symlink to Applications
ln -s /Applications "$DMG_DIR/Applications"

# Create DMG
echo "Creating DMG: $DMG_NAME"
hdiutil create \
    -volname "$APP_NAME" \
    -srcfolder "$DMG_DIR" \
    -ov \
    -format UDZO \
    "target/$DMG_NAME"

rm -rf "$DMG_DIR"

echo ""
echo "DMG created: target/$DMG_NAME"

# Notarize if signed
if [[ -n "$SIGN_IDENTITY" ]]; then
    echo ""
    echo "To notarize, run:"
    echo "  xcrun notarytool submit target/$DMG_NAME --apple-id YOUR_APPLE_ID --team-id YOUR_TEAM_ID --password YOUR_APP_PASSWORD"
fi
