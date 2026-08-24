#!/usr/bin/env bash
set -euo pipefail

tar_archive="${1:?Linux tar.gz archive is required}"
zip_archive="${2:?Linux ZIP archive is required}"
scratch_root="${3:?scratch root is required}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

[[ -f "${tar_archive}" ]] || { echo "Missing Linux tar archive: ${tar_archive}" >&2; exit 1; }
[[ -f "${zip_archive}" ]] || { echo "Missing Linux ZIP archive: ${zip_archive}" >&2; exit 1; }
mkdir -p "${scratch_root}"

smoke_archive() {
  local kind="$1"
  local archive="$2"
  local extract_root
  extract_root="$(mktemp -d "${scratch_root%/}/${kind}.XXXXXX")"
  if [[ "${kind}" == "tar" ]]; then
    tar -xzf "${archive}" -C "${extract_root}"
  else
    unzip -q "${archive}" -d "${extract_root}"
  fi

  local appimage="${extract_root}/XiaoLi/XiaoLi-x86_64.AppImage"
  [[ -f "${appimage}" ]] || {
    echo "${kind} archive is missing XiaoLi/XiaoLi-x86_64.AppImage" >&2
    exit 1
  }
  grep -Fq 'data-complete-license-catalog="true"' \
    "${extract_root}/XiaoLi/THIRD_PARTY_LICENSES.html" || {
    echo "${kind} archive is missing the complete offline license catalog" >&2
    exit 1
  }
  node "${repo_root}/scripts/release/validate-plugin.mjs" \
    "${extract_root}/XiaoLi/plugin/xiaoli-model-monitor"
  if [[ "${kind}" == "tar" && ! -x "${appimage}" ]]; then
    echo "tar archive did not preserve the AppImage executable bit" >&2
    exit 1
  fi
  # ZIP extraction does not preserve Unix mode on every client, which is why
  # the public quick start documents this one-time chmod.
  chmod +x "${appimage}"

  APPIMAGE_EXTRACT_AND_RUN=1 pwsh -NoLogo -NoProfile -NonInteractive -File \
    "${repo_root}/scripts/release/Test-Portable.ps1" \
    -Executable "${appimage}" \
    -ScratchRoot "${extract_root}/smoke-state"
}

smoke_archive tar "${tar_archive}"
smoke_archive zip "${zip_archive}"
echo "Linux archive smoke passed: ${tar_archive} and ${zip_archive}"
