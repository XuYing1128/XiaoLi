#!/usr/bin/env bash
# Kept LF-only by the repository's .gitattributes for portable CI execution.
set -euo pipefail

archive="${1:?macOS app archive is required}"
scratch_root="${2:?scratch root is required}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

[[ -f "${archive}" ]] || { echo "Missing macOS archive: ${archive}" >&2; exit 1; }
mkdir -p "${scratch_root}"
extract_root="$(mktemp -d "${scratch_root%/}/macos-archive.XXXXXX")"
ditto -x -k "${archive}" "${extract_root}"

app="${extract_root}/XiaoLi.app"
binary="${app}/Contents/MacOS/xiaoli"
[[ -d "${app}" && -x "${binary}" ]] || {
  echo "macOS archive is missing the executable app bundle" >&2
  exit 1
}
grep -Fq 'data-complete-license-catalog="true"' \
  "${extract_root}/THIRD_PARTY_LICENSES.html" || {
  echo "macOS archive is missing the complete offline license catalog" >&2
  exit 1
}
node "${repo_root}/scripts/release/validate-plugin.mjs" \
  "${extract_root}/plugin/xiaoli-model-monitor"

bash "${repo_root}/scripts/release/verify-macos-bundle.sh" "${app}" 12.0

pwsh -NoLogo -NoProfile -NonInteractive -File \
  "${repo_root}/scripts/release/Test-Portable.ps1" \
  -Executable "${binary}" \
  -ScratchRoot "${extract_root}/smoke-state"

echo "macOS archive smoke passed: ${archive}"
