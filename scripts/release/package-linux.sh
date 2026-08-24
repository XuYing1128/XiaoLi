#!/usr/bin/env bash
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

mkdir -p "${output_dir}"
output_dir="$(cd "${output_dir}" && pwd)"
stage="$(mktemp -d)"
trap 'rm -rf "${stage}"' EXIT
package_root="${stage}/XiaoLi"
mkdir -p "${package_root}"
install -m 0755 "${images[0]}" "${package_root}/XiaoLi-x86_64.AppImage"
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

tar_path="${output_dir}/XiaoLi-v${version}-Linux-x64-portable.tar.gz"
zip_path="${output_dir}/XiaoLi-v${version}-Linux-x64-portable.zip"
rm -f "${tar_path}" "${zip_path}"
tar -C "${stage}" -czf "${tar_path}" XiaoLi
(cd "${stage}" && zip -q -r -9 "${zip_path}" XiaoLi)
[[ -s "${tar_path}" && -s "${zip_path}" ]]
echo "${tar_path}"
echo "${zip_path}"
