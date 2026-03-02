#!/bin/bash
set -e

VERSION=${1:-"0.3.0"}
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
DIST_DIR="$PROJECT_ROOT/../dist"

echo "Packaging npm packages for version ${VERSION}..."

PACKAGE_DIR="$PROJECT_ROOT/../npm-packages"
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR"

echo ""
echo "Creating main package..."
MAIN_PKG_DIR="$PACKAGE_DIR/savfox"
mkdir -p "$MAIN_PKG_DIR/bin"
mkdir -p "$MAIN_PKG_DIR/vendor"

cp "$PROJECT_ROOT/package.json" "$MAIN_PKG_DIR/"
sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"${VERSION}\"/" "$MAIN_PKG_DIR/package.json"

cp "$PROJECT_ROOT/bin/savfox.js" "$MAIN_PKG_DIR/bin/"
chmod +x "$MAIN_PKG_DIR/bin/savfox.js"

if [ -d "$DIST_DIR" ]; then
  cp -r "$DIST_DIR"/* "$MAIN_PKG_DIR/vendor/"
fi

echo ""
echo "Creating platform-specific packages..."

declare -A PLATFORM_MAP=(
  ["x86_64-unknown-linux-musl"]="linux-x64"
  ["aarch64-unknown-linux-musl"]="linux-arm64"
  ["x86_64-apple-darwin"]="darwin-x64"
  ["aarch64-apple-darwin"]="darwin-arm64"
  ["x86_64-pc-windows-msvc"]="win32-x64"
  ["aarch64-pc-windows-msvc"]="win32-arm64"
)

for TARGET in "${!PLATFORM_MAP[@]}"; do
  PLATFORM="${PLATFORM_MAP[$TARGET]}"
  echo "Creating package for $PLATFORM ($TARGET)..."
  
  PLATFORM_PKG_DIR="$PACKAGE_DIR/savfox-$PLATFORM"
  mkdir -p "$PLATFORM_PKG_DIR/vendor/$TARGET"
  
  cat > "$PLATFORM_PKG_DIR/package.json" <<EOF
{
  "name": "@savfox/savfox-${PLATFORM}",
  "version": "${VERSION}",
  "license": "MIT OR Apache-2.0",
  "type": "module",
  "os": ["$(echo $PLATFORM | cut -d'-' -f1)"],
  "cpu": ["$(echo $PLATFORM | cut -d'-' -f2 | sed 's/x64/x64/' | sed 's/arm64/arm64/')"],
  "repository": {
    "type": "git",
    "url": "git+https://github.com/savfox-ai/savfox.git",
    "directory": "nodejs"
  },
  "description": "Platform-specific binary for Savfox CLI (${PLATFORM})",
  "optionalDependencies": {}
}
EOF
  
  if [ -d "$DIST_DIR/$TARGET" ]; then
    cp -r "$DIST_DIR/$TARGET"/* "$PLATFORM_PKG_DIR/vendor/$TARGET/"
  fi
done

echo ""
echo "========================================="
echo "Packaging complete!"
echo "========================================="
echo "Packages are available in: $PACKAGE_DIR"
echo ""
echo "To publish, run:"
echo "  cd $PACKAGE_DIR/savfox && npm publish"
for PLATFORM in "${PLATFORM_MAP[@]}"; do
  echo "  cd $PACKAGE_DIR/savfox-$PLATFORM && npm publish"
done
