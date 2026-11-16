#!/usr/bin/env bash
# Fetch the libpdfium shared library for a target.
#
#   rust/scripts/fetch-pdfium.sh [target-dir] [target]
#
# The target is a rust target triple (x86_64-unknown-linux-gnu,
# aarch64-unknown-linux-gnu, x86_64-apple-darwin, aarch64-apple-darwin).
# Without one the machine is used: TARGETOS/TARGETARCH (what buildx sets) win
# over `uname`, so the docker build can fetch the library for the image it
# builds rather than for the builder.
#
# The tarball unpacks into <target-dir>/lib/libpdfium.so (linux) or
# <target-dir>/lib/libpdfium.dylib (macOS).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_DIR="${1:-"$SCRIPT_DIR/../.vendor/pdfium"}"
PDFIUM_VERSION="${PDFIUM_VERSION:-8100}"
REQUESTED_TARGET="${2:-${PDFIUM_TARGET:-}}"

die() {
  printf 'fetch-pdfium: %s\n' "$*" >&2
  exit 1
}

# The rust target triple for this machine (or the requested one).
detect_target() {
  if [ -n "$REQUESTED_TARGET" ]; then
    printf '%s' "$REQUESTED_TARGET"
    return
  fi
  local os arch
  if [ -n "${TARGETARCH:-}" ]; then
    # buildx: TARGETOS is linux/darwin/windows, TARGETARCH is amd64/arm64.
    os="${TARGETOS:-$(uname -s | tr '[:upper:]' '[:lower:]')}"
    arch="$TARGETARCH"
  else
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"
  fi
  case "$os" in
    linux)
      case "$arch" in
        x86_64 | amd64) printf 'x86_64-unknown-linux-gnu' ;;
        aarch64 | arm64) printf 'aarch64-unknown-linux-gnu' ;;
        *) die "no pdfium build for linux/$arch" ;;
      esac
      ;;
    darwin)
      case "$arch" in
        x86_64) printf 'x86_64-apple-darwin' ;;
        arm64 | aarch64) printf 'aarch64-apple-darwin' ;;
        *) die "no pdfium build for macos/$arch" ;;
      esac
      ;;
    *) die "no pdfium build for $os" ;;
  esac
}

TARGET="$(detect_target)"

case "$TARGET" in
  x86_64-unknown-linux-gnu)
    PDFIUM_ARCH="linux-x64"
    ;;
  aarch64-unknown-linux-gnu)
    PDFIUM_ARCH="linux-arm64"
    ;;
  x86_64-apple-darwin)
    PDFIUM_ARCH="mac-x64"
    ;;
  aarch64-apple-darwin)
    PDFIUM_ARCH="mac-arm64"
    ;;
  *)
    die "unsupported target '$TARGET' (supported: x86_64/aarch64 unknown-linux-gnu, x86_64/aarch64 apple-darwin)"
    ;;
esac

URL="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/${PDFIUM_VERSION}/pdfium-${PDFIUM_ARCH}.tgz"
echo "Fetching pdfium ${PDFIUM_VERSION} for ${TARGET} (${PDFIUM_ARCH}) into ${TARGET_DIR}..."
mkdir -p "$TARGET_DIR"
curl -sSL "$URL" | tar -xz -C "$TARGET_DIR"

# The library name differs by platform; say where it landed so the caller does
# not have to guess.
found="$(find "$TARGET_DIR" -type f \( -name 'libpdfium.so' -o -name 'libpdfium.dylib' \) | head -n 1)"
[ -n "$found" ] || die "no libpdfium in the downloaded archive"
echo "libpdfium downloaded successfully to $found"
