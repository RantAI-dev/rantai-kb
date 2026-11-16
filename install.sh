#!/bin/sh
# install.sh — install the rantai-kb CLI: one binary, one command.
#
#   curl -fsSL https://raw.githubusercontent.com/RantAI-dev/rantai-kb/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/RantAI-dev/rantai-kb/main/install.sh | sh -s -- lite --start
#
# The script downloads the release tarball for this machine, verifies its
# checksum against the release's SHA256SUMS, unpacks it into
# ${RANTAI_KB_HOME:-$HOME/.rantai-kb} and links `rantai-kb` and `kb-server`
# into ${RANTAI_KB_BIN:-$HOME/.local/bin}. Every argument that is not an
# installer flag is passed to `rantai-kb setup`, so the whole install is one
# line:
#
#   ... | sh -s -- lite --ocr mistral --mistral-key-env MISTRAL_API_KEY --start
#
# It never prints a secret, and a re-run is an upgrade: bin/ and deploy/ are
# replaced, but an existing deploy/.env or deploy/.tier is kept.
#
# Environment:
#   RANTAI_KB_HOME   install root (default $HOME/.rantai-kb)
#   RANTAI_KB_BIN    where the links go (default $HOME/.local/bin)
#   RANTAI_KB_VERSION  a version to install (default: the latest release)
#   RANTAI_KB_TARBALL  a local tarball to install instead of downloading one
#   RANTAI_KB_SUMS     a local SHA256SUMS file to check it against
#   GITHUB_TOKEN     a token for the GitHub API (rate limits)
set -eu

REPO="RantAI-dev/rantai-kb"
API="https://api.github.com/repos/$REPO"
RELEASES="https://github.com/$REPO/releases/download"

usage() {
  cat <<'EOF'
usage: install.sh [options] [setup arguments...]

  --uninstall        remove the links and the install directory
  --yes, -y          do not ask for confirmation (with --uninstall)
  --version <tag>    install this version (e.g. v0.3.0) instead of the latest
  --help, -h         this text

Every other argument is passed to `rantai-kb setup`, for example:

  install.sh lite --start
  install.sh desk --tier desk
EOF
}

say() {
  printf '%s\n' "$*"
}

warn() {
  printf 'install: %s\n' "$*" >&2
}

die() {
  printf 'install: %s\n' "$*" >&2
  exit 1
}

have() {
  command -v "$1" >/dev/null 2>&1
}

# ─── Arguments ──────────────────────────────────────────────────────────────

uninstall=0
assume_yes=0
version="${RANTAI_KB_VERSION:-}"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --uninstall)
      uninstall=1
      shift
      ;;
    --yes | -y)
      assume_yes=1
      shift
      ;;
    --version)
      [ "$#" -ge 2 ] || die "--version needs a version"
      version="$2"
      shift 2
      ;;
    --help | -h)
      usage
      exit 0
      ;;
    *)
      break
      ;;
  esac
done
# "$@" is now the setup arguments, untouched.

install_root="${RANTAI_KB_HOME:-$HOME/.rantai-kb}"
bin_dir="${RANTAI_KB_BIN:-$HOME/.local/bin}"

# ─── Uninstall ──────────────────────────────────────────────────────────────

if [ "$uninstall" -eq 1 ]; then
  if [ "$assume_yes" -eq 0 ]; then
    if [ ! -t 0 ]; then
      die "--uninstall needs a terminal to confirm; re-run with --yes"
    fi
    printf 'Remove %s and the links in %s? [y/N] ' "$install_root" "$bin_dir"
    read -r answer
    case "$answer" in
      y | Y | yes | YES) ;;
      *) say "left alone"; exit 0 ;;
    esac
  fi
  for name in rantai-kb kb-server; do
    if [ -L "$bin_dir/$name" ] || [ -f "$bin_dir/$name" ]; then
      rm -f "$bin_dir/$name"
      say "removed $bin_dir/$name"
    fi
  done
  if [ -d "$install_root" ]; then
    rm -rf "$install_root"
    say "removed $install_root"
  fi
  exit 0
fi

# ─── The machine ────────────────────────────────────────────────────────────

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)
    case "$arch" in
      x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
      aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
      *) die "no build for linux/$arch (supported: x86_64, aarch64)" ;;
    esac
    ;;
  Darwin)
    case "$arch" in
      arm64 | aarch64) target="aarch64-apple-darwin" ;;
      *) die "no build for macos/$arch (supported: arm64)" ;;
    esac
    ;;
  *)
    die "no build for $os/$arch (supported: linux x86_64, linux aarch64, macos arm64)"
    ;;
esac

say "rantai-kb: $os/$arch → $target"

# ─── The version ────────────────────────────────────────────────────────────

api_header=""
if [ -n "${GITHUB_TOKEN:-}" ]; then
  api_header="Authorization: Bearer $GITHUB_TOKEN"
fi

fetch_to_stdout() {
  url="$1"
  if have curl; then
    if [ -n "$api_header" ]; then
      curl -fsSL -H "$api_header" "$url"
    else
      curl -fsSL "$url"
    fi
  elif have wget; then
    wget -qO- "$url"
  else
    die "neither curl nor wget is installed"
  fi
}

fetch_to_file() {
  url="$1"
  out="$2"
  if have curl; then
    if [ -n "$api_header" ]; then
      curl -fsSL -H "$api_header" "$url" -o "$out"
    else
      curl -fsSL "$url" -o "$out"
    fi
  elif have wget; then
    wget -q "$url" -O "$out"
  else
    die "neither curl nor wget is installed"
  fi
}

if [ -z "$version" ]; then
  say "resolving the latest release…"
  latest="$(fetch_to_stdout "$API/releases/latest" |
    sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' |
    head -n 1)"
  [ -n "$latest" ] || die "could not read the latest release from $API (set RANTAI_KB_VERSION)"
  version="$latest"
fi

case "$version" in
  v*) version_number="${version#v}" ;;
  *) version_number="$version"; version="v$version" ;;
esac

tarball_name="rantai-kb-$version_number-$target.tar.gz"
say "installing $version ($tarball_name)"

# ─── Download ───────────────────────────────────────────────────────────────

work_dir="$(mktemp -d 2>/dev/null || mktemp -d -t rantai-kb)"
cleanup() {
  rm -rf "$work_dir"
}
trap cleanup EXIT INT TERM

tarball_file="$work_dir/$tarball_name"
sums_file="$work_dir/SHA256SUMS"

if [ -n "${RANTAI_KB_TARBALL:-}" ]; then
  [ -f "$RANTAI_KB_TARBALL" ] || die "RANTAI_KB_TARBALL=$RANTAI_KB_TARBALL is not a file"
  cp "$RANTAI_KB_TARBALL" "$tarball_file"
  say "using the local tarball $RANTAI_KB_TARBALL"
else
  fetch_to_file "$RELEASES/$version/$tarball_name" "$tarball_file" ||
    die "could not download $RELEASES/$version/$tarball_name"
fi

if [ -n "${RANTAI_KB_SUMS:-}" ]; then
  [ -f "$RANTAI_KB_SUMS" ] || die "RANTAI_KB_SUMS=$RANTAI_KB_SUMS is not a file"
  cp "$RANTAI_KB_SUMS" "$sums_file"
else
  fetch_to_file "$RELEASES/$version/SHA256SUMS" "$sums_file" ||
    die "could not download $RELEASES/$version/SHA256SUMS"
fi

# ─── Verify ─────────────────────────────────────────────────────────────────

sha256_of() {
  if have sha256sum; then
    sha256sum "$1" | cut -d' ' -f1
  elif have shasum; then
    shasum -a 256 "$1" | cut -d' ' -f1
  elif have openssl; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    die "no sha256 tool found (sha256sum, shasum or openssl)"
  fi
}

expected="$(awk -v want="$tarball_name" \
  '$2 == want || $2 == "*" want { print $1; exit }' "$sums_file")"
[ -n "$expected" ] || die "$tarball_name is not in SHA256SUMS (wrong version or target?)"

actual="$(sha256_of "$tarball_file")"
[ "$expected" = "$actual" ] ||
  die "checksum mismatch for $tarball_name
   expected $expected
   got      $actual"

say "checksum ok"

# ─── Unpack ─────────────────────────────────────────────────────────────────

tar -xzf "$tarball_file" -C "$work_dir" || die "could not unpack $tarball_name"

package_dir="$work_dir/rantai-kb-$version_number-$target"
if [ ! -d "$package_dir" ]; then
  package_dir="$(find "$work_dir" -maxdepth 1 -type d -name 'rantai-kb-*' | head -n 1)"
fi
[ -n "$package_dir" ] && [ -d "$package_dir" ] ||
  die "the tarball does not contain a rantai-kb-$version_number-$target directory"
[ -f "$package_dir/bin/kb-server" ] || die "the tarball has no bin/kb-server"

# ─── Install ────────────────────────────────────────────────────────────────

mkdir -p "$install_root/bin" "$install_root/lib"

cp "$package_dir/bin/kb-server" "$install_root/bin/kb-server" ||
  die "could not install bin/kb-server"
chmod +x "$install_root/bin/kb-server"
# `rantai-kb` is the same program under the name it is meant to be used with.
ln -sf kb-server "$install_root/bin/rantai-kb"

if [ -d "$package_dir/lib" ]; then
  for lib in "$package_dir"/lib/*; do
    [ -e "$lib" ] || continue
    cp "$lib" "$install_root/lib/"
    chmod +x "$install_root/lib/"* 2>/dev/null || true
  done
fi

# The deploy directory is replaced, but an operator's .env and .tier are kept:
# a re-run is an upgrade, not a reset.
if [ -d "$package_dir/deploy" ]; then
  kept_env=""
  kept_tier=""
  [ -f "$install_root/deploy/.env" ] && kept_env="$work_dir/keep.env"
  [ -f "$install_root/deploy/.tier" ] && kept_tier="$work_dir/keep.tier"
  [ -n "$kept_env" ] && cp "$install_root/deploy/.env" "$kept_env"
  [ -n "$kept_tier" ] && cp "$install_root/deploy/.tier" "$kept_tier"
  if [ -f "$install_root/deploy/compose.yaml" ]; then
    rm -rf "$install_root/deploy"
  fi
  mkdir -p "$install_root/deploy"
  cp -R "$package_dir/deploy/." "$install_root/deploy/"
  [ -n "$kept_env" ] && cp "$kept_env" "$install_root/deploy/.env"
  [ -n "$kept_tier" ] && cp "$kept_tier" "$install_root/deploy/.tier"
fi

if [ -f "$package_dir/VERSION" ]; then
  cp "$package_dir/VERSION" "$install_root/VERSION"
fi

say "installed $install_root (bin/, lib/, deploy/)"

# ─── The links ──────────────────────────────────────────────────────────────

mkdir -p "$bin_dir"
ln -sf "$install_root/bin/rantai-kb" "$bin_dir/rantai-kb"
ln -sf "$install_root/bin/kb-server" "$bin_dir/kb-server"
say "linked $bin_dir/rantai-kb and $bin_dir/kb-server"

case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) warn "$bin_dir is not on your PATH; add it to use `rantai-kb` by name" ;;
esac

# ─── Docker ─────────────────────────────────────────────────────────────────
#
# A warning, not a failure: the cloud tier and a native (non-container) server
# need no docker at all.

if ! have docker; then
  warn "docker is not installed: the stack (up/down/logs/ps) needs it, but a
  native server and the cloud tier do not"
elif ! docker compose version >/dev/null 2>&1; then
  warn "the docker compose plugin is missing: `docker compose version` failed"
else
  compose_version="$(docker compose version 2>/dev/null |
    tr ' ' '\n' |
    sed -n 's/^v\{0,1\}\([0-9][0-9]*\.[0-9][0-9]*\).*$/\1/p' |
    head -n 1)"
  if [ -z "$compose_version" ]; then
    warn "could not read the docker compose version"
  else
    compose_major="${compose_version%%.*}"
    compose_minor="${compose_version#*.}"
    if [ "$compose_major" -lt 2 ] ||
      { [ "$compose_major" -eq 2 ] && [ "$compose_minor" -lt 20 ]; }; then
      warn "docker compose $compose_version is too old: this stack needs >= 2.20"
    else
      say "docker compose $compose_version"
    fi
  fi
fi

# ─── Setup ──────────────────────────────────────────────────────────────────

if [ "$#" -gt 0 ]; then
  say ""
  say "running: rantai-kb setup $*"
  "$bin_dir/rantai-kb" setup "$@"
fi

say ""
say "done. Next: rantai-kb up    (start the stack)   ·   rantai-kb key my-tenant"
