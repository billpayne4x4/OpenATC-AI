# Release Packages Workflow

## Overview

A new GitHub Actions workflow (`release-packages.yml`) has been created to automate the build and packaging of OpenATC releases for multiple platforms.

## Workflow Features

### Manual Trigger
The workflow is triggered manually via GitHub Actions with configurable inputs:

- **version**: Package version (e.g., `0.1.0` or `0.1.0-alpha.1`)
- **platform**: Target platform (`all`, `linux-x64`, `windows-x64`, `macos-arm64`, or `macos-intel`)
- **product**: Product to build (`all`, `plugin`, or `ai-server`)
- **preflight_only**: Run preflight checks only without full build

### Supported Platforms & Products

| Platform | Product | Runner | Status |
|----------|---------|--------|--------|
| linux-x64 | plugin | ubuntu-22.04 | ✓ |
| linux-x64 | ai-server | ubuntu-24.04 | ✓ |
| windows-x64 | plugin | windows-2022 | ✓ |
| windows-x64 | ai-server | windows-2022 | ✓ |
| macos-arm64 | plugin | macos-15 | ✓ |
| macos-arm64 | ai-server | macos-15 | ✓ |
| macos-intel | plugin | macos-15-intel | ✓ |
| macos-intel | ai-server | macos-15-intel | ✓ |

## Workflow Steps

### 1. Prepare Job
- Validates version format (semantic versioning)
- Builds dynamic build matrix based on platform/product selection
- Generates platform-specific preflight configurations

### 2. Preflight Job
- Runs on each target platform
- Validates installer assets
- Checks packaging tool availability
- Verifies compiler/SDK configurations
- Tests installer discovery and transaction handling

### 3. Build Job  (if preflight_only = false)
- Sets up platform-specific build environments
- Imports signing certificates (if configured)
- Builds `openatc-plugin` and `openatc-engine` or `openatc-ai` and `openatc-stt`
- Runs test suites
- Packages final releases using `cargo xtask release-package`
- Collects build artifacts

### 4. Summary Job
- Provides workflow completion status
- Displays version information
- Instructs on artifact download location

## How to Run

### Via GitHub Actions Web UI

1. Navigate to **Actions** tab in the repository
2. Select **Release Packages** workflow
3. Click **Run workflow**
4. Configure inputs:
   - Set version (e.g., `0.1.0-alpha.1`)
   - Select platform (default: `all`)
   - Select product (default: `all`)
   - Optionally enable `preflight_only` for quick validation
5. Click **Run workflow**

### Via GitHub CLI

```bash
gh workflow run release-packages.yml \
  --ref main \
  -f version=0.1.0-alpha.1 \
  -f platform=all \
  -f product=all \
  -f preflight_only=false
```

Note: Workflows can only be triggered from the repository's default branch (main).

### Via REST API

```bash
TOKEN=$(gh auth token)
curl -X POST \
  -H "Authorization: token $TOKEN" \
  -H "Accept: application/vnd.github.v3+json" \
  https://api.github.com/repos/Plane14/OpenATC-AI/actions/workflows/release-packages.yml/dispatches \
  -d '{
    "ref": "main",
    "inputs": {
      "version": "0.1.0-alpha.1",
      "platform": "all",
      "product": "all",
      "preflight_only": "false"
    }
  }'
```

## Artifacts

After a successful workflow run, the following artifacts are available for download:

### Packages
- `packages-{platform}-{product}`: Release packages (.exe, .dmg, .deb, .rpm, .AppImage, .flatpak, .zip)
- Includes `.sha256` checksum files for verification

### Build Artifacts  
- `native-{platform}-{product}`: Compiled binaries and libraries

## Prerequisites for Full Builds

### All Platforms
- Rust toolchain (stable, managed by workflow)
- Cargo with locked dependencies

### Windows
- Visual Studio C++ build tools
- Inno Setup 6 (installer compiler)
- LLVM with libclang
- Ninja build system (for ai-server)

### Linux
- build-essential, clang, libclang-dev, cmake, pkg-config
- For ai-server: rpm, dpkg-dev, flatpak, vulkan-dev, glslc, spirv-headers

### macOS
- Xcode command line tools
- For ai-server: Vulkan SDK or CoreML (platform-specific)
- For Intel Mac ai-server: ONNX Runtime 1.28.0

### Optional: Code Signing

Configure these GitHub secrets for signed releases:

**Windows:**
- `OPENATC_WINDOWS_CERT_PFX_BASE64`: Base64-encoded PFX certificate
- `OPENATC_WINDOWS_CERT_PASSWORD`: Certificate password

**macOS:**
- `OPENATC_MAC_CERT_P12_BASE64`: Base64-encoded P12 certificate
- `OPENATC_MAC_CERT_PASSWORD`: Certificate password
- `OPENATC_MAC_SIGN_IDENTITY`: Code signing identity name
- `OPENATC_APPLE_ID`: Apple ID for notarization
- `OPENATC_APPLE_PASSWORD`: App-specific password
- `OPENATC_APPLE_TEAM_ID`: Apple Developer Team ID

## Troubleshooting

### Workflow Not Found
- Ensure changes are pushed to the default branch (`main`)
- Wait 1-2 minutes for GitHub to index the workflow
- Check workflow file syntax with `gh workflow view release-packages.yml`

### Build Failures

Check the workflow run logs:
1. Go to **Actions** > **Release Packages**
2. Select the failed run
3. Expand the step that failed
4. Review error messages and logs

Common issues:
- Missing dependencies (install per platform section above)
- Version format invalid (must match semantic versioning)
- Insufficient disk space or timeout (large ai-server builds)
- Signing certificate missing (warnings only; unsigned packages still built)

### Preflight-Only Mode

Use `preflight_only=true` to quickly validate:
- Asset availability
- Compiler/SDK setup
- Packaging tool compatibility
- Without building full applications

## Configuration

The workflow uses existing xtask commands:
- `cargo xtask release-preflight {platform}`: Validate platform setup
- `cargo xtask release-package {product} {platform} {version}`: Build packages
- `cargo xtask collect-artifacts {product} {output}`: Gather build outputs

See `xtask/README.md` for additional commands and options.

## Next Steps

1. Merge `crispy-garbanzo` branch to `main` via pull request
2. Trigger workflow from `main` branch via GitHub Actions UI or CLI
3. Monitor workflow progress in Actions tab
4. Download artifacts once complete
5. Verify checksums: `sha256sum -c *.sha256`
6. Test extracted packages on respective platforms
