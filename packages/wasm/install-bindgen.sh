#!/usr/bin/env bash
# Install the wasm-bindgen CLI that build.sh needs, at exactly the version
# of the wasm-bindgen crate in Cargo.lock (the two must match, or the
# bindings don't load). Downloads the upstream prebuilt binary: seconds,
# where `cargo install` is minutes.
#
#   install-bindgen.sh [dest-dir]   (default: ~/.cargo/bin)
#
# Used by CI and Dockerfile.server. Locally, `cargo install wasm-bindgen-cli
# --version <same>` works just as well.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
lock="$here/../../clients/desktop/src-tauri/Cargo.lock"
dest="${1:-$HOME/.cargo/bin}"

version="$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' "$lock")"
if [ -z "$version" ]; then
  echo "install-bindgen: no wasm-bindgen in $lock" >&2
  exit 1
fi

if command -v wasm-bindgen >/dev/null 2>&1 &&
  [ "$(wasm-bindgen --version | awk '{print $2}')" = "$version" ]; then
  echo "wasm-bindgen $version already installed"
  exit 0
fi

arch="$(uname -m)"
case "$arch" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) echo "install-bindgen: unsupported architecture $arch" >&2; exit 1 ;;
esac
case "$(uname -s)" in
  Linux) triple="$arch-unknown-linux-musl" ;; # static: runs on glibc and Alpine
  Darwin) triple="$arch-apple-darwin" ;;
  MINGW* | MSYS* | CYGWIN*) triple="x86_64-pc-windows-msvc" ;;
  *) echo "install-bindgen: unsupported OS $(uname -s)" >&2; exit 1 ;;
esac

name="wasm-bindgen-$version-$triple"
url="https://github.com/wasm-bindgen/wasm-bindgen/releases/download/$version/$name.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $url"
curl -fsSL --retry 3 "$url" | tar xz -C "$tmp"
mkdir -p "$dest"
cp "$tmp/$name"/wasm-bindgen* "$dest/"
"$dest/wasm-bindgen" --version
