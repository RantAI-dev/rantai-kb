#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_DIR="${1:-"$SCRIPT_DIR/../.vendor/pdfium"}"
PDFIUM_VERSION="${PDFIUM_VERSION:-8100}"

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64)
    PDFIUM_ARCH="linux-x64"
    ;;
  aarch64|arm64)
    PDFIUM_ARCH="linux-arm64"
    ;;
  *)
    echo "Unsupported architecture: $ARCH" >&2
    exit 1
    ;;
esac

URL="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/${PDFIUM_VERSION}/pdfium-${PDFIUM_ARCH}.tgz"
echo "Fetching pdfium ${PDFIUM_VERSION} for ${PDFIUM_ARCH} into ${TARGET_DIR}..."
mkdir -p "$TARGET_DIR"
curl -sSL "$URL" | tar -xz -C "$TARGET_DIR"
echo "libpdfium downloaded successfully to $TARGET_DIR"
