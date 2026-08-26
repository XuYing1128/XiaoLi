#!/usr/bin/env bash
# Kept LF-only by the repository's .gitattributes for portable CI execution.
set -euo pipefail

app_path="${1:?XiaoLi.app path is required}"
output_dir="${2:?output directory is required}"
version="${3:?version is required}"
[[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || {
  echo "Invalid version: ${version}" >&2
  exit 1
}
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

[[ -d "${app_path}" ]] || { echo "Missing app bundle: ${app_path}" >&2; exit 1; }
node "${repo_root}/scripts/release/assert-no-private-build-paths.mjs" "${app_path}"
mkdir -p "${output_dir}"
output_dir="$(cd "${output_dir}" && pwd)"
archive="${output_dir}/XiaoLi-v${version}-macOS-universal.app.zip"
rm -f "${archive}"

notice_dir="$(mktemp -d)"
trap 'rm -rf "${notice_dir}"' EXIT
node "${repo_root}/scripts/release/generate-third-party-licenses.mjs" \
  "${notice_dir}/THIRD_PARTY_LICENSES.html"

# ditto preserves executable bits, symlinks, resource forks and extended
# attributes. Documentation is staged beside the already-signed app so the
# signature is not mutated after verification.
stage="$(mktemp -d)"
trap 'rm -rf "${notice_dir}" "${stage}"' EXIT
ditto "${app_path}" "${stage}/XiaoLi.app"
for document in ASSET_PROVENANCE.md CHANGELOG.md CONTRIBUTING.md DESIGN.md LICENSE \
  README.md README.en.md SECURITY.md THIRD_PARTY_NOTICES.md; do
  cp "${repo_root}/${document}" "${stage}/${document}"
done
cp -R "${repo_root}/docs" "${stage}/docs"
mkdir -p "${stage}/src/assets"
cp "${repo_root}/src/assets/mochi-app-icon.png" "${stage}/src/assets/mochi-app-icon.png"
mkdir -p "${stage}/plugin"
cp -R "${repo_root}/plugin/xiaoli-model-monitor" \
  "${stage}/plugin/xiaoli-model-monitor"
find "${stage}/plugin/xiaoli-model-monitor" -type d -empty -delete
cp "${notice_dir}/THIRD_PARTY_LICENSES.html" "${stage}/THIRD_PARTY_LICENSES.html"
node "${repo_root}/scripts/release/assert-no-private-build-paths.mjs" "${stage}"
rm -f "${archive}"
(cd "${stage}" && ditto -c -k --sequesterRsrc . "${archive}")
[[ -s "${archive}" ]]
echo "${archive}"
