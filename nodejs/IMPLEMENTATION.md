# Savfox Node.js Package - Summary

## Overview

This implementation adds npm package support to the Savfox project, following the same pattern used by the Codex project.

## What Was Created

### 1. Package Structure (`nodejs/`)

```
nodejs/
├── bin/
│   └── savfox.js              # Platform detection wrapper script
├── scripts/
│   ├── build-binaries.sh      # Build all platform binaries (Linux/macOS)
│   ├── build-binaries.bat     # Build Windows binaries
│   ├── package-npm.sh         # Package npm packages (Linux/macOS)
│   ├── package-npm.bat        # Package npm packages (Windows)
│   └── dev.js                 # Development helper script
├── package.json               # Main npm package definition
├── package.dev.json           # Development dependencies
├── .gitignore                 # Ignore build artifacts
├── README.md                  # Comprehensive documentation
└── BUILD.md                   # Build instructions
```

### 2. GitHub Actions Workflow

`.github/workflows/publish-npm.yml` - Automated building and publishing:
- Builds on native platforms (Linux, macOS, Windows)
- Uploads artifacts from each build
- Packages and publishes to npm registry
- Creates release notes

### 3. Key Features

**Platform Detection**: The wrapper script (`bin/savfox.js`) automatically:
- Detects the current platform and architecture
- Loads the appropriate binary from optional dependencies
- Falls back to bundled binaries if available
- Provides helpful error messages

**Cross-Platform Support**: Supports 6 platform combinations:
- Linux x64 and ARM64
- macOS x64 and ARM64 (Intel and Apple Silicon)
- Windows x64 and ARM64

**Flexible Distribution**:
- Main package with platform detection
- Platform-specific optional packages
- Can bundle binaries in main package for offline use

## How It Works

### Architecture

```
User installs: npm install -g @savfox/savfox
                    ↓
         Main package installed
                    ↓
         Platform detection runs
                    ↓
    Loads appropriate binary from:
    - Optional dependency package, OR
    - Bundled vendor directory
                    ↓
         Spawns native Rust binary
```

### Build Process

1. **Build Rust Binaries**
   ```bash
   # Windows
   nodejs\scripts\build-binaries.bat 0.3.0
   
   # Linux/macOS
   nodejs/scripts/build-binaries.sh 0.3.0
   ```

2. **Package npm Packages**
   ```bash
   # Windows
   nodejs\scripts\package-npm.bat 0.3.0
   
   # Linux/macOS
   nodejs/scripts/package-npm.sh 0.3.0
   ```

3. **Publish to npm**
   ```bash
   cd npm-packages/savfox
   npm publish --access public
   ```

## Usage Examples

### For Developers (Building)

```bash
# Quick local development
cd nodejs
npm run dev              # Build, vendor, and test

# Or step-by-step
npm run build            # Build Rust binary for current platform
npm run vendor           # Copy binary to vendor directory
npm run test             # Test the wrapper script

# Build for all platforms
npm run build:all        # Requires cross-compilation setup

# Package for distribution
npm run package          # Creates npm packages
```

### For Users (Installation)

```bash
# Install globally
npm install -g @savfox/savfox

# Use the CLI
savfox --version
savfox --help
savfox login
savfox chat "Hello, world!"
```

## Differences from Codex

While following the same pattern, there are some differences:

1. **Package Scope**: Uses `@savfox/savfox` instead of `@openai/codex`
2. **Binary Name**: Uses `savfox` instead of `codex`
3. **Environment Variables**: Uses `SAVFOX_MANAGED_BY_*` instead of `CODEX_MANAGED_BY_*`
4. **Project Structure**: Integrated into existing Rust workspace

## Next Steps

### 1. Configure npm Publishing

Create an npm account and get an authentication token:

```bash
npm login
npm token create
```

Add the token to GitHub repository secrets as `NPM_TOKEN`.

### 2. Test Publishing

```bash
# Test locally first
cd nodejs
npm run dev              # Build and test locally

# Test packaging
npm run package          # Create packages
cd ../npm-packages/savfox
npm publish --dry-run    # Test without publishing
```

### 3. Automated Publishing

When you create a GitHub release, the workflow will automatically:
1. Build binaries for all platforms
2. Package npm packages
3. Publish to npm registry
4. Update release notes

Or manually trigger via:
```bash
gh workflow run publish-npm.yml -f version=0.3.0
```

### 4. Optional Enhancements

Consider adding:
- Pre-release versions (e.g., `0.3.0-beta.1`)
- Nightly builds
- Binary signing (especially for macOS)
- Auto-update functionality
- Better error messages with installation hints

## Maintenance

### Updating Version

1. Update `Cargo.toml` workspace version
2. Update `nodejs/package.json` version
3. Build and test
4. Create git tag and GitHub release

### Adding New Platforms

1. Add target to build scripts
2. Update platform detection in `bin/savfox.js`
3. Update GitHub Actions workflow
4. Test on new platform

## Troubleshooting

### Binary Not Found
- Ensure platform-specific package is published
- Or bundle binaries in main package vendor directory

### Cross-Compilation Issues
- Use GitHub Actions with native runners
- Or set up cross-compilation toolchains locally

### Permission Issues
- Ensure binary has execute permissions
- Run `chmod +x` on the binary

## Resources

- [npm Package Documentation](nodejs/README.md)
- [Build Instructions](nodejs/BUILD.md)
- [GitHub Actions Workflow](.github/workflows/publish-npm.yml)
- [Codex npm Package](https://github.com/openai/codex) (reference implementation)
