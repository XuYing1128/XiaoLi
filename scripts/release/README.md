# XiaoLi portable release tooling

The release pipeline is intentionally portable-only. It does not create NSIS,
MSI, DMG, DEB, or RPM installers.

## Workflows

- `.github/workflows/ci.yml` runs secret scanning, plugin validation, frontend
  type checking, Rust formatting/lint/tests, a native Tauri `--no-bundle`
  build, and CLI/hook/MCP smoke tests on Windows, macOS, and Ubuntu.
- `.github/workflows/release.yml` accepts only `v0.1.0-beta.3`, builds all three
  platforms independently, assembles the complete asset set, verifies every
  checksum, and only then creates or updates a draft GitHub Release. The draft
  becomes a prerelease after all assets have uploaded and the remote asset
  count matches. A failed platform or upload therefore cannot publish a
  partial release.

Every third-party GitHub Action is pinned to a full commit SHA.

## Release assets

The completeness validator requires exactly:

- `XiaoLi-v0.1.0-beta.3-Windows-x64-portable.zip`
- `XiaoLi-v0.1.0-beta.3-macOS-universal.app.zip`
- `XiaoLi-v0.1.0-beta.3-Linux-x64-portable.tar.gz`
- `XiaoLi-v0.1.0-beta.3-Linux-x64-portable.zip`
- `xiaoli-codex-plugin-v0.1.0-beta.3.zip`
- `THIRD_PARTY_LICENSES.html`
- `XiaoLi-v0.1.0-beta.3.spdx.json`
- `SHA256SUMS.txt`

Portable app archives retain the root README documents, `docs/`, the icon used
by the README, license/security/provenance files, and plugin resources. Legacy
Mochi deployment scripts and local probe/state artifacts are never staged.
Each app archive also contains a self-contained `THIRD_PARTY_LICENSES.html`.
Generation fails unless every locked Rust and pnpm package has embedded license
text; package-provided NOTICE/attribution files are embedded too, and canonical
texts come from the exact locked `spdx-license-list` development dependency.

## Local checks

From the repository root on Windows:

```powershell
node scripts/release/verify-version.mjs 0.1.0-beta.3
node scripts/release/validate-plugin.mjs plugin/xiaoli-model-monitor
node scripts/release/generate-third-party-licenses.mjs release-out/THIRD_PARTY_LICENSES.html
./scripts/release/Test-Portable.ps1 `
  -Executable src-tauri/target/release/xiaoli.exe `
  -ScratchRoot release-out/smoke
./scripts/release/Package-Windows.ps1 `
  -Executable src-tauri/target/release/xiaoli.exe `
  -OutputDirectory release-out `
  -Version 0.1.0-beta.3
./scripts/release/Test-WindowsArchive.ps1 `
  -Archive release-out/XiaoLi-v0.1.0-beta.3-Windows-x64-portable.zip `
  -ScratchRoot release-out/archive-smoke
./scripts/release/Package-Plugin.ps1 `
  -OutputDirectory release-out `
  -Version 0.1.0-beta.3
./scripts/release/Test-PluginArchive.ps1 `
  -Archive release-out/xiaoli-codex-plugin-v0.1.0-beta.3.zip `
  -ScratchRoot release-out/plugin-smoke
```

The smoke script uses redirected files and `Start-Process -Wait`, so the
Windows GUI-subsystem executable can still be tested reliably in CI. It checks
the snapshot probe, fail-open hook response, and the three MCP tools. Every
child process receives an archive-specific `XIAOLI_STATE_DIR`, preventing smoke
tests from touching a developer or CI account's default XiaoLi state.
