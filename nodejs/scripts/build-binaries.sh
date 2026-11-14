#!/bin/bash
set -euo pipefail

VERSION=${1:-"0.3.0"}
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
DIST_DIR="$PROJECT_ROOT/../dist"
TARGET_OUTPUT=$(node -e 'for(const p of require(process.argv[1])) console.log(p.target)' "$PROJECT_ROOT/platforms.json")
TARGETS=()
while IFS= read -r target; do TARGETS+=("$target"); done <<< "$TARGET_OUTPUT"
cd "$PROJECT_ROOT/.."
TARGET_DIR=$(node -e 'console.log(require(process.argv[1]).cargoTargetDirectory(process.argv[2]))' "$PROJECT_ROOT/../scripts/cargo-target.cjs" "$PROJECT_ROOT/..")

echo "Building Savfox CLI v${VERSION} for all platforms..."

node -e 'const fs=require("node:fs"),path=require("node:path"); const dest=path.resolve(process.argv[1]),expected=path.resolve(process.argv[2],"../dist"); if(dest!==expected||fs.existsSync(dest)&&fs.lstatSync(dest).isSymbolicLink()) throw Error("Invalid dist directory"); fs.rmSync(dest,{recursive:true,force:true})' "$DIST_DIR" "$PROJECT_ROOT"
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
    
    cargo build --locked --release --target "$TARGET" -p savfox-cli --bin savfox
    cp "$TARGET_DIR/$TARGET/release/$BINARY_NAME" "$OUTPUT_DIR/"
  elif [[ $TARGET == *"-darwin" ]]; then
    if [[ "$OSTYPE" == "darwin"* ]]; then
      if ! rustup target list | grep -q "$TARGET (installed)"; then
        echo "Installing target $TARGET..."
        rustup target add "$TARGET"
      fi
      
      cargo build --locked --release --target "$TARGET" -p savfox-cli --bin savfox
      cp "$TARGET_DIR/$TARGET/release/$BINARY_NAME" "$OUTPUT_DIR/"
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
      
      cargo build --locked --release --target "$TARGET" -p savfox-cli --bin savfox
      cp "$TARGET_DIR/$TARGET/release/$BINARY_NAME" "$OUTPUT_DIR/"
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
