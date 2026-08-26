#!/usr/bin/env bash
# Kept LF-only by the repository's .gitattributes for portable CI execution.
set -euo pipefail

appimage_dir="${1:?AppImage directory is required}"
output_dir="${2:?output directory is required}"
version="${3:?version is required}"
[[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || {
  echo "Invalid version: ${version}" >&2
  exit 1
}
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
mapfile -t images < <(find "${appimage_dir}" -maxdepth 1 -type f -name '*.AppImage' -print)
[[ "${#images[@]}" -eq 1 ]] || {
  echo "Expected exactly one AppImage in ${appimage_dir}; found ${#images[@]}" >&2
  exit 1
}
appimage="$(cd "$(dirname "${images[0]}")" && pwd)/$(basename "${images[0]}")"
native_binary="$(cd "${appimage_dir}/../.." && pwd)/xiaoli"
[[ -x "${native_binary}" ]] || {
  echo "Missing native XiaoLi executable beside the AppImage bundle: ${native_binary}" >&2
  exit 1
}

mkdir -p "${output_dir}"
output_dir="$(cd "${output_dir}" && pwd)"
stage="$(mktemp -d)"
trap 'rm -rf "${stage}"' EXIT

# AppImage is a compressed SquashFS payload. Scanning only its container bytes
# can miss paths that become visible after decompression, so extract without
# FUSE and scan the exact filesystem tree that will execute for users.
payload_scan_root="${stage}/appimage-payload"
mkdir -p "${payload_scan_root}"
(
  cd "${payload_scan_root}"
  "${appimage}" --appimage-extract >/dev/null
)
payload_root="${payload_scan_root}/squashfs-root"
[[ -d "${payload_root}" ]] || {
  echo "AppImage extraction did not create squashfs-root" >&2
  exit 1
}
node "${repo_root}/scripts/release/assert-no-private-build-paths.mjs" \
  "${native_binary}" "${appimage}" "${payload_root}"

package_root="${stage}/XiaoLi"
mkdir -p "${package_root}"
install -m 0755 "${appimage}" "${package_root}/XiaoLi-x86_64.AppImage"
for document in ASSET_PROVENANCE.md CHANGELOG.md CONTRIBUTING.md DESIGN.md LICENSE \
  README.md README.en.md SECURITY.md THIRD_PARTY_NOTICES.md; do
  cp "${repo_root}/${document}" "${package_root}/${document}"
done
cp -R "${repo_root}/docs" "${package_root}/docs"
mkdir -p "${package_root}/src/assets"
cp "${repo_root}/src/assets/mochi-app-icon.png" \
  "${package_root}/src/assets/mochi-app-icon.png"
mkdir -p "${package_root}/plugin"
cp -R "${repo_root}/plugin/xiaoli-model-monitor" \
  "${package_root}/plugin/xiaoli-model-monitor"
find "${package_root}/plugin/xiaoli-model-monitor" -type d -empty -delete
node "${repo_root}/scripts/release/generate-third-party-licenses.mjs" \
  "${package_root}/THIRD_PARTY_LICENSES.html"
node "${repo_root}/scripts/release/assert-no-private-build-paths.mjs" \
  "${package_root}"

tar_path="${output_dir}/XiaoLi-v${version}-Linux-x64-portable.tar.gz"
zip_path="${output_dir}/XiaoLi-v${version}-Linux-x64-portable.zip"
rm -f "${tar_path}" "${zip_path}"
tar -C "${stage}" -czf "${tar_path}" XiaoLi
(cd "${stage}" && zip -q -r -9 "${zip_path}" XiaoLi)
[[ -s "${tar_path}" && -s "${zip_path}" ]]
echo "${tar_path}"
echo "${zip_path}"
