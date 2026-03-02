# Savfox - Build NPM Package

This document describes how to build and publish the Savfox npm package.

## Quick Start (Windows)

```bash
# From the project root
cd nodejs\scripts

# Build Windows binaries
build-binaries.bat 0.3.0

# Package npm packages
package-npm.bat 0.3.0
```

## Quick Start (Linux/macOS)

```bash
# From the project root
cd nodejs/scripts

# Build binaries for all platforms
chmod +x build-binaries.sh
./build-binaries.sh 0.3.0

# Package npm packages
chmod +x package-npm.sh
./package-npm.sh 0.3.0
```

## Build Process

1. **Build Rust binaries** - Compile the Savfox CLI for target platforms
2. **Package npm packages** - Create platform-specific npm packages
3. **Publish to npm** - Upload packages to npm registry

## Package Structure

```
npm-packages/
├── savfox/                    # Main package (platform detection wrapper)
│   ├── bin/
│   │   └── savfox.js         # Node.js wrapper script
│   ├── vendor/               # Optional: bundled binaries
│   └── package.json
├── savfox-linux-x64/         # Linux x64 binary
│   ├── vendor/
│   │   └── x86_64-unknown-linux-musl/
│   │       └── savfox/
│   │           └── savfox
│   └── package.json
├── savfox-linux-arm64/       # Linux ARM64 binary
├── savfox-darwin-x64/        # macOS Intel binary
├── savfox-darwin-arm64/      # macOS Apple Silicon binary
├── savfox-win32-x64/         # Windows x64 binary
└── savfox-win32-arm64/       # Windows ARM64 binary
```

## Supported Platforms

| Platform | Architecture | Target Triple |
|----------|-------------|---------------|
| Linux | x64 | x86_64-unknown-linux-musl |
| Linux | ARM64 | aarch64-unknown-linux-musl |
| macOS | x64 | x86_64-apple-darwin |
| macOS | ARM64 | aarch64-apple-darwin |
| Windows | x64 | x86_64-pc-windows-msvc |
| Windows | ARM64 | aarch64-pc-windows-msvc |

## Local Development

### Test Locally Built Binary

```bash
# Build the Rust binary
cargo build --release -p savfox-cli

# Create vendor directory with binary
mkdir -p nodejs/vendor/x86_64-pc-windows-msvc/savfox
cp target/release/savfox.exe nodejs/vendor/x86_64-pc-windows-msvc/savfox/

# Test the wrapper
cd nodejs
node bin/savfox.js --version
```

### Test npm Package Locally

```bash
# Package the npm package
cd nodejs/scripts
./package-npm.bat 0.3.0

# Test installation locally
cd ../../npm-packages/savfox
npm link

# Now you can use 'savfox' command
savfox --version

# Unlink when done
npm unlink -g @savfox/savfox
```

## Publishing to npm

### Prerequisites

1. npm account with publish permissions for @savfox scope
2. npm authentication token configured
3. All platform binaries built and packaged

### Manual Publishing

```bash
# Set authentication
npm login

# Publish main package
cd npm-packages/savfox
npm publish --access public

# Publish platform-specific packages
cd ../savfox-win32-x64
npm publish --access public

# Repeat for other platforms
```

### Automated Publishing (Recommended)

Use GitHub Actions to automate building and publishing. See `nodejs/README.md` for complete workflow example.

## Version Management

Update version in:
1. `Cargo.toml` (workspace version)
2. `nodejs/package.json`
3. Pass version to build scripts

```bash
# Update version
# 1. Edit Cargo.toml workspace.package.version
# 2. Edit nodejs/package.json version
# 3. Build with new version
./build-binaries.bat 0.3.1
./package-npm.bat 0.3.1
```

## Troubleshooting

### Binary Not Found

**Error**: `Missing optional dependency @savfox/savfox-{platform}`

**Solution**: 
- Ensure platform-specific package is installed
- Or bundle binaries in main package's vendor directory

### Permission Denied (Linux/macOS)

**Error**: `permission denied` when running binary

**Solution**:
```bash
chmod +x nodejs/vendor/*/savfox/savfox
```

### Cross-Compilation Issues

**Problem**: Can't build for platform X on platform Y

**Solution**: Use CI/CD with appropriate runners for each platform

## CI/CD Integration

See `.github/workflows/publish-npm.yml` (create this file) for automated builds and publishing.

Recommended approach:
1. Build on native platforms (macOS on macOS runner, Windows on Windows runner)
2. Upload artifacts from each build
3. Collect all artifacts and publish from single job

## Additional Resources

- [npm Publishing Guide](https://docs.npmjs.com/packages-and-modules/contributing-packages-to-the-registry)
- [Node.js Native Addons](https://nodejs.org/api/addons.html)
- [Rust Cross-Compilation](https://rust-lang.github.io/rustup/cross-compilation.html)
