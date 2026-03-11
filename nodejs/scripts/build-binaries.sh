#!/bin/bash
set -e

VERSION=${1:-"0.3.0"}
TARGETS=(
  "x86_64-unknown-linux-musl"
  "aarch64-unknown-linux-musl"
  "x86_64-apple-darwin"
  "aarch64-apple-darwin"
  "x86_64-pc-windows-msvc"
  "aarch64-pc-windows-msvc"
)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
DIST_DIR="$PROJECT_ROOT/dist"

echo "Building Savfox CLI v${VERSION} for all platforms..."

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

for TARGET in "${TARGETS[@]}"; do
  echo ""
  echo "========================================="
  echo "Building for target: $TARGET"
  echo "========================================="
  
  OUTPUT_DIR="$DIST_DIR/$TARGET/savfox"
  mkdir -p "$OUTPUT_DIR"
  
  if [[ $TARGET == *"-windows-"* ]]; then
    BINARY_NAME="savfox.exe"
  else
    BINARY_NAME="savfox"
  fi
  
  if [[ $TARGET == *"-linux-"* ]]; then
    if ! rustup target list | grep -q "$TARGET (installed)"; then
      echo "Installing target $TARGET..."
      rustup target add "$TARGET"
    fi
    
    cargo build --release --target "$TARGET" -p savfox-cli --bin savfox
    cp "$PROJECT_ROOT/target/$TARGET/release/$BINARY_NAME" "$OUTPUT_DIR/"
  elif [[ $TARGET == *"-darwin-"* ]]; then
    if [[ "$OSTYPE" == "darwin"* ]]; then
      if ! rustup target list | grep -q "$TARGET (installed)"; then
        echo "Installing target $TARGET..."
        rustup target add "$TARGET"
      fi
      
      cargo build --release --target "$TARGET" -p savfox-cli --bin savfox
      cp "$PROJECT_ROOT/target/$TARGET/release/$BINARY_NAME" "$OUTPUT_DIR/"
    else
      echo "Skipping $TARGET - can only build Darwin targets on macOS"
      continue
    fi
  elif [[ $TARGET == *"-windows-"* ]]; then
    if [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]] || [[ "$OSTYPE" == "win32" ]]; then
      if ! rustup target list | grep -q "$TARGET (installed)"; then
        echo "Installing target $TARGET..."
        rustup target add "$TARGET"
      fi
      
      cargo build --release --target "$TARGET" -p savfox-cli --bin savfox
      cp "$PROJECT_ROOT/target/$TARGET/release/$BINARY_NAME" "$OUTPUT_DIR/"
    else
      echo "Skipping $TARGET - can only build Windows targets on Windows"
      continue
    fi
  fi
  
  chmod +x "$OUTPUT_DIR/$BINARY_NAME"
  
  echo "Built binary at: $OUTPUT_DIR/$BINARY_NAME"
done

echo ""
echo "========================================="
echo "Build complete!"
echo "========================================="
echo "Binaries are available in: $DIST_DIR"
