# Savfox Node.js Package

This directory contains the Node.js/npm package for Savfox CLI.

## Overview

The npm package structure follows a similar pattern to Codex:
- Main package (`@savfox/savfox`) - Platform-agnostic wrapper that detects the platform and loads the appropriate binary
- Platform-specific packages (`@savfox/savfox-{platform}`) - Contains compiled Rust binaries for each platform

## Building

### Prerequisites

- Rust toolchain installed
- Node.js 16+ and npm/pnpm
- Target-specific toolchains (see below)

### Build All Platform Binaries

```bash
# Build binaries for all platforms
./scripts/build-binaries.sh 0.3.0
```

This will compile the Rust CLI for all supported targets:
- `x86_64-unknown-linux-musl` (Linux x64)
- `aarch64-unknown-linux-musl` (Linux ARM64)
- `x86_64-apple-darwin` (macOS Intel)
- `aarch64-apple-darwin` (macOS Apple Silicon)
- `x86_64-pc-windows-msvc` (Windows x64)
- `aarch64-pc-windows-msvc` (Windows ARM64)

### Cross-Compilation Notes

- **Linux**: Can cross-compile to all Linux targets using musl
- **macOS**: Can only build Darwin targets on macOS
- **Windows**: Can only build Windows targets on Windows

For cross-platform builds, consider using CI/CD (GitHub Actions) to build on appropriate runners.

### Package npm Packages

```bash
# Create npm packages from built binaries
./scripts/package-npm.sh 0.3.0
```

This will create:
1. Main package with platform detection wrapper
2. Platform-specific packages with compiled binaries

## Publishing

### Manual Publishing

```bash
# After building and packaging
cd ../npm-packages/savfox
npm publish --access public

# Publish platform-specific packages
cd ../savfox-linux-x64
npm publish --access public

# ... repeat for other platforms
```

### Automated Publishing (GitHub Actions)

Create a GitHub Actions workflow to:
1. Build binaries on appropriate runners (macOS, Windows, Linux)
2. Package npm packages
3. Publish to npm registry

Example workflow structure:
```yaml
name: Publish npm packages

on:
  release:
    types: [created]

jobs:
  build-linux:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - name: Build Linux binaries
        run: |
          rustup target add x86_64-unknown-linux-musl
          rustup target add aarch64-unknown-linux-musl
          cargo build --release --target x86_64-unknown-linux-musl -p savfox-cli
          cargo build --release --target aarch64-unknown-linux-musl -p savfox-cli
      - name: Upload artifacts
        uses: actions/upload-artifact@v3
        with:
          name: linux-binaries
          path: target/*/release/savfox

  build-macos:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v3
      - name: Build macOS binaries
        run: |
          rustup target add x86_64-apple-darwin
          rustup target add aarch64-apple-darwin
          cargo build --release --target x86_64-apple-darwin -p savfox-cli
          cargo build --release --target aarch64-apple-darwin -p savfox-cli
      - name: Upload artifacts
        uses: actions/upload-artifact@v3
        with:
          name: macos-binaries
          path: target/*/release/savfox

  build-windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v3
      - name: Build Windows binaries
        run: |
          rustup target add x86_64-pc-windows-msvc
          cargo build --release --target x86_64-pc-windows-msvc -p savfox-cli
      - name: Upload artifacts
        uses: actions/upload-artifact@v3
        with:
          name: windows-binaries
          path: target/*/release/savfox.exe

  publish:
    needs: [build-linux, build-macos, build-windows]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - name: Download all artifacts
        uses: actions/download-artifact@v3
      - name: Setup Node.js
        uses: actions/setup-node@v3
        with:
          node-version: '20'
          registry-url: 'https://registry.npmjs.org'
      - name: Package and publish
        run: |
          cd nodejs/scripts
          ./package-npm.sh ${{ github.event.release.tag_name }}
          cd ../../npm-packages
          npm publish --access public ./savfox
          npm publish --access public ./savfox-linux-x64
          npm publish --access public ./savfox-linux-arm64
          npm publish --access public ./savfox-darwin-x64
          npm publish --access public ./savfox-darwin-arm64
          npm publish --access public ./savfox-win32-x64
        env:
          NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}
```

## Installation

Users can install the package via npm:

```bash
npm install -g @savfox/savfox
# or
pnpm install -g @savfox/savfox
```

The package will automatically detect the platform and download the appropriate platform-specific binary as an optional dependency.

## Development

### Local Testing

1. Build the binary for your platform:
   ```bash
   cargo build --release -p savfox-cli
   ```

2. Create local vendor directory:
   ```bash
   mkdir -p nodejs/vendor/{target-triple}/savfox
   cp target/release/savfox nodejs/vendor/{target-triple}/savfox/
   ```

3. Test the wrapper:
   ```bash
   cd nodejs
   node bin/savfox.js --version
   ```

### Package Structure

```
nodejs/
├── bin/
│   └── savfox.js          # Platform detection wrapper
├── scripts/
│   ├── build-binaries.sh  # Build Rust binaries
│   └── package-npm.sh     # Create npm packages
├── package.json           # Main package definition
└── README.md              # This file
```

## Architecture

The npm package uses a two-tier architecture:

1. **Main Package** (`@savfox/savfox`)
   - Platform-agnostic Node.js wrapper
   - Detects OS and architecture at runtime
   - Spawns the appropriate platform-specific binary

2. **Platform Packages** (`@savfox/savfox-{platform}`)
   - Optional dependencies of main package
   - Contains compiled Rust binary for specific platform
   - Marked with `os` and `cpu` fields in package.json

This approach allows:
- Single package name for all platforms
- Automatic platform detection
- Smaller download size (only downloads needed platform binary)
- Supports offline installation with bundled binaries
