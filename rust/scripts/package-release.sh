#!/usr/bin/env bash
# Package a release bundle: the binary, libpdfium and the compose stack.
#
#   rust/scripts/package-release.sh <target> <version> <outdir>
#
# The result is <outdir>/rantai-kb-<version>-<target>.tar.gz, the file the
# installer downloads, and a SHA256SUMS line for it.
#
# The bundle is what an installed machine has:
#
#   bin/kb-server      the server and the stack commands (bin/rantai-kb is a
#                      link to it: the same program under its other name)
#   lib/libpdfium.*    the PDF renderer; the binary finds it next to itself
#   deploy/            compose.yaml, .env.example, the sidecar build contexts
#   VERSION            the version, so the bundle can point at the published
#                      images (ghcr.io/rantai-dev/rantai-kb:<version>)
#
# The deploy directory is copied without an operator's .env or .tier: those
# belong to the machine, not to the release.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

usage() {
  cat <<'EOF'
usage: package-release.sh <target> <version> <outdir>

  target   x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu
           | x86_64-apple-darwin | aarch64-apple-darwin
  version  the version to publish (0.3.0, v0.3.0)
  outdir   where the tarball and the SHA256SUMS line go

Environment:
  CARGO_BUILD_JOBS   cargo parallelism (default: cargo's own)
  PDFIUM_VERSION     the pdfium build to fetch (default 8100)
  KB_SERVER_BIN      package this binary instead of building one
  SKIP_PDFIUM=1      do not fetch libpdfium (the binary then needs a system one)
EOF
}

die() {
  printf 'package-release: %s\n' "$*" >&2
  exit 1
}

say() {
  printf '%s\n' "$*"
}

TARGET="${1:-}"
VERSION="${2:-}"
OUTDIR="${3:-}"

if [ -z "$TARGET" ] || [ -z "$VERSION" ] || [ -z "$OUTDIR" ]; then
  usage >&2
  exit 2
fi

case "$TARGET" in
  x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu | x86_64-apple-darwin | aarch64-apple-darwin) ;;
  *) die "unsupported target '$TARGET'" ;;
esac

VERSION="${VERSION#v}"
case "$VERSION" in
  *[!0-9.]* | "") die "the version must be a number (got '$VERSION')" ;;
esac

mkdir -p "$OUTDIR"
OUTDIR="$(cd "$OUTDIR" && pwd)"
PACKAGE_NAME="rantai-kb-$VERSION-$TARGET"
BUNDLE="$OUTDIR/$PACKAGE_NAME"
TARBALL="$OUTDIR/$PACKAGE_NAME.tar.gz"

# ─── The binary ─────────────────────────────────────────────────────────────

HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"

if [ -n "${KB_SERVER_BIN:-}" ]; then
  # An already built binary (a CI cache, or a local test run).
  [ -f "$KB_SERVER_BIN" ] || die "KB_SERVER_BIN=$KB_SERVER_BIN is not a file"
  say "packaging the binary $KB_SERVER_BIN (KB_SERVER_BIN)"
  BINARY="$KB_SERVER_BIN"
elif [ "$TARGET" = "$HOST_TARGET" ]; then
  say "building kb-server for $TARGET (host target)"
  cargo build \
    --release \
    --bin kb-server \
    --manifest-path "$REPO_ROOT/rust/Cargo.toml"
  BINARY="$REPO_ROOT/rust/target/release/kb-server"
else
  if command -v rustup >/dev/null 2>&1; then
    say "adding the rust target $TARGET"
    rustup target add "$TARGET"
  fi
  say "building kb-server for $TARGET (cross)"
  cargo build \
    --release \
    --bin kb-server \
    --target "$TARGET" \
    --manifest-path "$REPO_ROOT/rust/Cargo.toml"
  BINARY="$REPO_ROOT/rust/target/$TARGET/release/kb-server"
fi

[ -f "$BINARY" ] || die "cargo did not produce $BINARY"

# ─── The bundle ─────────────────────────────────────────────────────────────

rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/bin" "$BUNDLE/lib" "$BUNDLE/deploy"

cp "$BINARY" "$BUNDLE/bin/kb-server"
chmod +x "$BUNDLE/bin/kb-server"
# `rantai-kb` is the same program under the name it is meant to be used with.
ln -s kb-server "$BUNDLE/bin/rantai-kb"

# ─── libpdfium ──────────────────────────────────────────────────────────────

if [ "${SKIP_PDFIUM:-0}" = "1" ]; then
  say "skipping libpdfium (SKIP_PDFIUM=1)"
else
  PDFIUM_DIR="$OUTDIR/.pdfium-$TARGET"
  rm -rf "$PDFIUM_DIR"
  "$SCRIPT_DIR/fetch-pdfium.sh" "$PDFIUM_DIR" "$TARGET"
  PDFIUM_LIB="$(find "$PDFIUM_DIR" -type f \( -name 'libpdfium.so' -o -name 'libpdfium.dylib' \) | head -n 1)"
  [ -n "$PDFIUM_LIB" ] || die "no libpdfium in $PDFIUM_DIR"
  cp "$PDFIUM_LIB" "$BUNDLE/lib/"
  chmod +x "$BUNDLE/lib/"*
  rm -rf "$PDFIUM_DIR"
fi

# ─── The stack ──────────────────────────────────────────────────────────────

# Everything in rust/deploy except an operator's own .env and .tier: those two
# are the machine's state, not the release's.
tar -cf - \
  -C "$REPO_ROOT/rust/deploy" \
  --exclude='./.env' \
  --exclude='./.tier' \
  . | tar -xf - -C "$BUNDLE/deploy"

printf '%s\n' "$VERSION" >"$BUNDLE/deploy/VERSION"
printf '%s\n' "$VERSION" >"$BUNDLE/VERSION"

cp "$REPO_ROOT/LICENSE" "$BUNDLE/LICENSE"
cp "$REPO_ROOT/install.sh" "$BUNDLE/install.sh"
chmod +x "$BUNDLE/install.sh"

cat >"$BUNDLE/README.md" <<EOF
# rantai-kb $VERSION

The knowledge-base service: upload a file, it is extracted, chunked, embedded
and indexed; \`/v1/search\` returns scoped context with sources and figures.

## Install

    curl -fsSL https://raw.githubusercontent.com/RantAI-dev/rantai-kb/main/install.sh | sh

Or, from this bundle:

    ./install.sh

The installer puts the binary in \`$PACKAGE_NAME/bin\`, links \`rantai-kb\` and
\`kb-server\` into \`~/.local/bin\`, and keeps this bundle's \`deploy/\` as the
stack's configuration directory.

## Use

    rantai-kb setup lite      # detect the hardware, write deploy/.env and deploy/.tier
    rantai-kb up lite         # start the containers
    rantai-kb key my-tenant   # print an API key
    rantai-kb status          # GET /health
    rantai-kb logs            # follow the logs
    rantai-kb down            # stop (down -v also wipes the data)

The API is at http://127.0.0.1:8080 by default, and its OpenAPI page is at
http://127.0.0.1:8080/docs.

## Layout

    bin/kb-server    the server and the stack commands
    lib/             libpdfium, the PDF renderer
    deploy/          compose.yaml, .env.example and the sidecar build contexts
    VERSION          the version this bundle was built from

Full documentation: https://github.com/RantAI-dev/rantai-kb
EOF

# ─── The tarball ────────────────────────────────────────────────────────────

rm -f "$TARBALL"
tar -czf "$TARBALL" -C "$OUTDIR" "$PACKAGE_NAME"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    die "no sha256 tool found (sha256sum or shasum)"
  fi
}

SHA="$(sha256_of "$TARBALL")"
SUMS="$OUTDIR/SHA256SUMS"
if [ -f "$SUMS" ]; then
  grep -v " $PACKAGE_NAME.tar.gz\$" "$SUMS" >"$SUMS.tmp" || true
  mv "$SUMS.tmp" "$SUMS"
fi
printf '%s  %s\n' "$SHA" "$PACKAGE_NAME.tar.gz" >>"$SUMS"

say ""
say "packaged $PACKAGE_NAME"
say "  tarball  $TARBALL"
say "  sha256   $SHA"
say "  contents $(tar -tzf "$TARBALL" | wc -l | tr -d ' ') entries"
