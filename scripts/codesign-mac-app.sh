#!/bin/bash
# Code-sign the Savfox macOS binary and app bundle.
# Usage: ./scripts/codesign-mac-app.sh --identity "Developer ID Application: Your Name (TEAMID)"
set -euo pipefail

IDENTITY=""
ENTITLEMENTS=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --identity) IDENTITY="$2"; shift 2 ;;
        --entitlements) ENTITLEMENTS="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

if [[ -z "$IDENTITY" ]]; then
    echo "Usage: $0 --identity \"Developer ID Application: ...\" [--entitlements path.plist]"
    echo ""
    echo "Available identities:"
    security find-identity -v -p codesigning | grep "Developer ID" || echo "  (none found)"
    exit 1
fi

APP_DIR="target/Savfox.app"
BINARY="target/release/savfox"

# Sign standalone binary
if [[ -f "$BINARY" ]]; then
    echo "Signing binary: $BINARY"
    if [[ -n "$ENTITLEMENTS" ]]; then
        codesign --force --options runtime --sign "$IDENTITY" --entitlements "$ENTITLEMENTS" "$BINARY"
    else
        codesign --force --options runtime --sign "$IDENTITY" "$BINARY"
    fi
    echo "  Verified: $(codesign -v "$BINARY" 2>&1 && echo "OK" || echo "FAIL")"
fi

# Sign app bundle
if [[ -d "$APP_DIR" ]]; then
    echo "Signing app bundle: $APP_DIR"
    if [[ -n "$ENTITLEMENTS" ]]; then
        codesign --force --deep --options runtime --sign "$IDENTITY" --entitlements "$ENTITLEMENTS" "$APP_DIR"
    else
        codesign --force --deep --options runtime --sign "$IDENTITY" "$APP_DIR"
    fi
    echo "  Verified: $(codesign -v "$APP_DIR" 2>&1 && echo "OK" || echo "FAIL")"
fi

echo ""
echo "Code signing complete."
