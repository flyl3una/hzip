#!/usr/bin/env bash
#
# Build the hzip Finder Sync extension bundle (`hzipFinderSync.appex`).
#
# Usage:
#   build.sh --out <DIR> [--version <V>] [--arch <triple>]... [--universal]
#
#   --out <DIR>      Output directory; the bundle is written to <DIR>/hzipFinderSync.appex
#   --version <V>    Value for CFBundleShortVersionString / CFBundleVersion (default: Info.plist)
#   --arch <triple>  Swift target triple, repeatable (default: host arch, macOS 11+).
#                    A bare arch (`x86_64`, `arm64`) is normalized to `<arch>-apple-macos11`.
#   --universal      Shorthand for --arch x86_64-apple-macos11 --arch arm64-apple-macos11
#
# This script does NOT sign the bundle: signing is the packaging layer's job
# (build/package/package.py), which signs inside-out (extension executable ->
# appex -> host executable -> host app).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="${SCRIPT_DIR}/Sources"
INFO_PLIST_SRC="${SCRIPT_DIR}/Info.plist"
BUNDLE_NAME="hzipFinderSync"
EXECUTABLE_NAME="hzipFinderSync"
DEFAULT_ARCH_TRIPLE="$(uname -m)-apple-macos11"

OUT_DIR=""
VERSION=""
declare -a ARCH_TRIPLES=()

die() {
  echo "build.sh: error: $*" >&2
  exit 1
}

normalize_triple() {
  local triple="$1"
  case "${triple}" in
    *-apple-macos*) printf '%s' "${triple}" ;;
    *-apple-darwin*) printf '%s' "${triple}" ;;
    *-arm64 | *-x86_64) printf '%s-apple-macos11' "${triple%%-*}" ;;
    arm64 | x86_64) printf '%s-apple-macos11' "${triple}" ;;
    *) printf '%s' "${triple}" ;;
  esac
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out)
      [[ $# -ge 2 ]] || die "--out requires a directory argument"
      OUT_DIR="$2"
      shift 2
      ;;
    --version)
      [[ $# -ge 2 ]] || die "--version requires a value"
      VERSION="$2"
      shift 2
      ;;
    --arch)
      [[ $# -ge 2 ]] || die "--arch requires a target triple"
      ARCH_TRIPLES+=("$(normalize_triple "$2")")
      shift 2
      ;;
    --universal)
      ARCH_TRIPLES+=("x86_64-apple-macos11" "arm64-apple-macos11")
      shift
      ;;
    -h | --help)
      sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

[[ -n "${OUT_DIR}" ]] || die "--out <DIR> is required"
command -v swiftc >/dev/null 2>&1 || die "swiftc not found; install Xcode or the Command Line Tools (xcode-select --install)"
[[ -f "${INFO_PLIST_SRC}" ]] || die "Info.plist not found at ${INFO_PLIST_SRC}"
[[ -d "${SRC_DIR}" ]] || die "Sources directory not found at ${SRC_DIR}"

if [[ ${#ARCH_TRIPLES[@]} -eq 0 ]]; then
  ARCH_TRIPLES=("${DEFAULT_ARCH_TRIPLE}")
fi

# De-duplicate triples, preserving order.
declare -a UNIQUE_TRIPLES=()
for triple in "${ARCH_TRIPLES[@]}"; do
  duplicate=""
  for seen in "${UNIQUE_TRIPLES[@]:-}"; do
    [[ "${seen}" == "${triple}" ]] && duplicate="1" && break
  done
  [[ -n "${duplicate}" ]] || UNIQUE_TRIPLES+=("${triple}")
done
ARCH_TRIPLES=("${UNIQUE_TRIPLES[@]}")

APPEX_DIR="${OUT_DIR}/${BUNDLE_NAME}.appex"
MACOS_DIR="${APPEX_DIR}/Contents/MacOS"
rm -rf "${APPEX_DIR}"
mkdir -p "${MACOS_DIR}"

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/hzip-findersync.XXXXXX")"
cleanup() { rm -rf "${WORK_DIR}"; }
trap cleanup EXIT

declare -a SLICES=()
for triple in "${ARCH_TRIPLES[@]}"; do
  slice="${WORK_DIR}/${BUNDLE_NAME}-${triple}"
  echo "==> swiftc (${triple})"
  swiftc \
    -O \
    -module-name "${BUNDLE_NAME}" \
    -target "${triple}" \
    -framework Cocoa \
    -framework FinderSync \
    -Xlinker -e -Xlinker _NSExtensionMain \
    -o "${slice}" \
    "${SRC_DIR}"/*.swift
  SLICES+=("${slice}")
done

if [[ ${#SLICES[@]} -eq 1 ]]; then
  cp "${SLICES[0]}" "${MACOS_DIR}/${EXECUTABLE_NAME}"
  echo "==> single-arch executable (${ARCH_TRIPLES[0]})"
else
  echo "==> lipo -create (${ARCH_TRIPLES[*]})"
  lipo -create -output "${MACOS_DIR}/${EXECUTABLE_NAME}" "${SLICES[@]}"
fi
chmod 755 "${MACOS_DIR}/${EXECUTABLE_NAME}"

cp "${INFO_PLIST_SRC}" "${APPEX_DIR}/Contents/Info.plist"
if [[ -n "${VERSION}" ]]; then
  PLIST_BUDDY="/usr/libexec/PlistBuddy"
  if [[ -x "${PLIST_BUDDY}" ]]; then
    "${PLIST_BUDDY}" -c "Set :CFBundleShortVersionString ${VERSION}" "${APPEX_DIR}/Contents/Info.plist"
    "${PLIST_BUDDY}" -c "Set :CFBundleVersion ${VERSION}" "${APPEX_DIR}/Contents/Info.plist"
  else
    sed -i '' -e "s|<string>0\.0\.0</string>|<string>${VERSION}</string>|g" "${APPEX_DIR}/Contents/Info.plist"
  fi
fi

printf 'APPL????' > "${APPEX_DIR}/Contents/PkgInfo"

echo "==> built ${APPEX_DIR}"
