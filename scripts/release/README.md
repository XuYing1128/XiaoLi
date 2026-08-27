# XiaoLi portable release tooling

The release pipeline is intentionally portable-only. It does not create NSIS,
MSI, DMG, DEB, or RPM installers.

## Workflows

- `.github/workflows/ci.yml` runs secret scanning, plugin validation, frontend
  type checking, Rust formatting/lint/tests, a native Tauri `--no-bundle`
  build, and CLI/hook/MCP smoke tests on Windows, macOS, and Ubuntu.
- `.github/workflows/release.yml` accepts only `v0.2.0-beta.1`, builds all three
  platforms independently from the tag's immutable commit SHA. Its release
  quality gate repeats secret/privacy scans, frontend checks/build, Rust
  format/lint/tests, and plugin validation. It then assembles the complete
  asset set, verifies every checksum, and only then creates or updates a draft
  GitHub Release. The draft becomes a prerelease only after remote tag SHA,
  asset names, byte sizes, and downloaded checksums all match. A failed
  platform or upload therefore cannot publish a partial release. Re-running a
  release that is already published succeeds without mutation only when the
  complete remote asset set has the same names and SHA256 values; any
  difference fails instead of overwriting the published files.

The Universal macOS job runs on `macos-15` and pins macOS 12.0 in both the
Tauri bundle configuration and the runner environment. A shared verifier
checks that the app contains exactly one valid `arm64` and one valid `x86_64`
Mach-O slice, accepts either valid deployment-target load-command encoding,
requires both slices and `LSMinimumSystemVersion` to resolve to 12.0, and then
checks the ad-hoc signature. The same verifier runs again after extracting the
final portable ZIP, so the uploaded app is the artifact that passes the gate.

Every third-party GitHub Action is pinned to a full commit SHA.

## Release assets

The completeness validator requires exactly:

- `XiaoLi-v0.2.0-beta.1-Windows-x64-portable.zip`
- `XiaoLi-v0.2.0-beta.1-macOS-universal.app.zip`
- `XiaoLi-v0.2.0-beta.1-Linux-x64-portable.tar.gz`
- `XiaoLi-v0.2.0-beta.1-Linux-x64-portable.zip`
- `xiaoli-codex-plugin-v0.2.0-beta.1.zip`
- `THIRD_PARTY_LICENSES.html`
- `XiaoLi-v0.2.0-beta.1.spdx.json`
- `SHA256SUMS.txt`

Portable app archives retain the root README documents, `docs/`, the icon used
by the README, license/security/provenance files, and plugin resources. Legacy
Mochi deployment scripts and local probe/state artifacts are never staged.
Each app archive also contains a self-contained `THIRD_PARTY_LICENSES.html`.
Generation fails unless every locked Rust and pnpm package has embedded license
text; package-provided NOTICE/attribution files are embedded too, and canonical
texts come from the exact locked `spdx-license-list` development dependency.
The SPDX JSON is generated with pinned Syft `v1.51.0` and explicitly labeled
as a source-workspace dependency inventory, not as a binary provenance or
reproducible-build attestation. Validation also
requires the versioned XiaoLi root package, a valid `DESCRIBES`/`CONTAINS`
chain, resolvable relationship endpoints, and exact coverage for critical
dependencies read from `Cargo.lock` and `pnpm-lock.yaml`.

The public-tree scanner reads UTF-8 and BOM-marked UTF-16 incrementally, so it
does not silently skip source files larger than 5 MiB. It detects Windows paths
with slash or backslash separators, numeric account SIDs, and contextual real
thread/turn/session UUIDs while permitting explicit placeholders and the
all-zero fixture IDs used by archive smoke tests.

Native release jobs apply Rust `--remap-path-prefix` plus matching C/C++
compiler mappings for the CI workspace, user home, Cargo home, and Rustup home.
MSVC mappings include `/experimental:deterministic`, which is required before
`/pathmap` takes effect. Before archiving, every platform packager scans its
staged payload and rejects UTF-8 or UTF-16LE copies of those private build
paths. Linux also extracts and scans the AppImage filesystem because compressed
container bytes alone are not an adequate payload check. This is deliberately
separate from the source-tree scan: a clean repository alone does not prove
that a compiled binary is free of build-machine paths.

## Local checks

From the repository root on Windows:

```powershell
$xiaoliRepo = (Get-Location).Path
$xiaoliCargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$xiaoliRustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
$xiaoliRustRemaps = @(
  "--remap-path-prefix=$xiaoliRepo=<workspace>"
  "--remap-path-prefix=$env:USERPROFILE=<home>"
  "--remap-path-prefix=$xiaoliCargoHome=<cargo>"
  "--remap-path-prefix=$xiaoliRustupHome=<rustup>"
) -join ' '
$xiaoliNativeRemaps = @(
  '/experimental:deterministic'
  "/pathmap:$xiaoliRepo=<workspace>"
  "/pathmap:$env:USERPROFILE=<home>"
  "/pathmap:$xiaoliCargoHome=<cargo>"
  "/pathmap:$xiaoliRustupHome=<rustup>"
) -join ' '
$env:RUSTFLAGS = "$($env:RUSTFLAGS) $xiaoliRustRemaps".Trim()
$env:CFLAGS = "$($env:CFLAGS) $xiaoliNativeRemaps".Trim()
$env:CXXFLAGS = "$($env:CXXFLAGS) $xiaoliNativeRemaps".Trim()
pnpm tauri build --no-bundle
node scripts/release/assert-no-private-build-paths.mjs src-tauri/target/release/xiaoli.exe
node scripts/release/verify-version.mjs 0.2.0-beta.1
node scripts/release/validate-plugin.mjs plugin/xiaoli-model-monitor
node scripts/release/scan-public-tree.mjs
node --test scripts/release/tests/release-gates.test.mjs
node scripts/release/generate-third-party-licenses.mjs release-out/THIRD_PARTY_LICENSES.html
./scripts/release/Test-Portable.ps1 `
  -Executable src-tauri/target/release/xiaoli.exe `
  -ScratchRoot release-out/smoke
./scripts/release/Package-Windows.ps1 `
  -Executable src-tauri/target/release/xiaoli.exe `
  -OutputDirectory release-out `
  -Version 0.2.0-beta.1
./scripts/release/Test-WindowsArchive.ps1 `
  -Archive release-out/XiaoLi-v0.2.0-beta.1-Windows-x64-portable.zip `
  -ScratchRoot release-out/archive-smoke
./scripts/release/Package-Plugin.ps1 `
  -OutputDirectory release-out `
  -Version 0.2.0-beta.1
./scripts/release/Test-PluginArchive.ps1 `
  -Archive release-out/xiaoli-codex-plugin-v0.2.0-beta.1.zip `
  -ScratchRoot release-out/plugin-smoke
```

The smoke script uses redirected files and a bounded `Start-Process` child, so
the Windows GUI-subsystem executable can still be tested reliably in CI. Each
child has a 30-second timeout; only the exact process tree started by the smoke
test is terminated on timeout. It checks the V5 snapshot probe, fail-open hook
response, and all six read-only MCP tools. Every child receives an
archive-specific `XIAOLI_STATE_DIR`, preventing smoke tests from touching a
developer or CI account's default XiaoLi state.
