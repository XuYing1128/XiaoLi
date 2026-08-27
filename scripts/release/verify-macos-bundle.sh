#!/usr/bin/env bash
# Kept LF-only by the repository's .gitattributes for portable CI execution.
set -euo pipefail

app="${1:?XiaoLi.app path is required}"
expected_version="${2:-12.0}"

fail() {
  echo "::error::$*" >&2
  exit 1
}

normalize_version() {
  local value="${1:-}"
  local major=0
  local minor=0
  local patch=0
  [[ "${value}" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]] || return 1
  IFS=. read -r major minor patch <<<"${value}" || true
  printf '%d.%d.%d' "${major:-0}" "${minor:-0}" "${patch:-0}"
}

if ! expected_minos="$(normalize_version "${expected_version}")"; then
  fail "invalid expected macOS version: ${expected_version}"
fi

[[ -d "${app}" ]] || fail "missing app bundle: ${app}"
binary="${app}/Contents/MacOS/xiaoli"
[[ -x "${binary}" ]] || fail "missing executable: ${binary}"

lipo "${binary}" -verify_arch arm64 x86_64
archs="$(lipo -archs "${binary}")"
read -r -a arch_list <<<"${archs}"
if [[ "${#arch_list[@]}" -ne 2 ]]; then
  fail "expected exactly arm64 and x86_64 slices, found: ${archs}"
fi
for expected_arch in arm64 x86_64; do
  if [[ " ${archs} " != *" ${expected_arch} "* ]]; then
    fail "Universal executable is missing ${expected_arch}: ${archs}"
  fi
done

scratch="$(mktemp -d)"
trap 'rm -rf "${scratch}"' EXIT
for arch in arm64 x86_64; do
  thin="${scratch}/xiaoli-${arch}"
  lipo "${binary}" -thin "${arch}" -output "${thin}"
  description="$(file "${thin}")"
  [[ "${description}" == *"Mach-O 64-bit"* ]] || fail "${arch} slice is not Mach-O 64-bit: ${description}"
  [[ "${description}" == *"${arch}"* ]] || fail "${arch} slice reports the wrong architecture: ${description}"
  [[ "${description}" == *"executable"* ]] || fail "${arch} slice is not executable: ${description}"

  build_info="$(xcrun vtool -arch "${arch}" -show-build "${binary}")"
  if [[ "${build_info}" == *"LC_BUILD_VERSION"* ]]; then
    [[ "${build_info}" == *"platform MACOS"* ]] || fail "${arch} LC_BUILD_VERSION does not target macOS"
    minos="$(awk '$1 == "minos" { print $2; exit }' <<<"${build_info}")"
  elif [[ "${build_info}" == *"LC_VERSION_MIN_MACOSX"* ]]; then
    minos="$(awk '$1 == "version" { print $2; exit }' <<<"${build_info}")"
  else
    fail "${arch} slice has no recognized macOS deployment target"
  fi

  if ! normalized_minos="$(normalize_version "${minos}")"; then
    fail "${arch} minimum macOS version is missing or malformed: ${minos:-missing}"
  fi
  if [[ "${normalized_minos}" != "${expected_minos}" ]]; then
    fail "${arch} minimum macOS version is ${minos}; expected ${expected_version}"
  fi
done

plist="${app}/Contents/Info.plist"
[[ -f "${plist}" ]] || fail "missing Info.plist: ${plist}"
plist_minos="$(plutil -extract LSMinimumSystemVersion raw "${plist}")"
if ! normalized_plist_minos="$(normalize_version "${plist_minos}")"; then
  fail "Info.plist LSMinimumSystemVersion is missing or malformed: ${plist_minos:-missing}"
fi
if [[ "${normalized_plist_minos}" != "${expected_minos}" ]]; then
  fail "Info.plist LSMinimumSystemVersion is ${plist_minos}; expected ${expected_version}"
fi

codesign --verify --deep --strict "${app}"
echo "macOS bundle verified: ${app} (arm64 + x86_64, minimum ${expected_version})"
