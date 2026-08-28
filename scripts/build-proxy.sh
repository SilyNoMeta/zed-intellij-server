#!/usr/bin/env bash
# Cross-compiles ij-zed-proxy for the release matrix into dist/proxy/.
# Requires: rustup targets for the platforms (see below); zig + cargo-zigbuild
# recommended for Linux/Windows cross from macOS.
set -euo pipefail
cd "$(dirname "$0")/../proxy"

OUT=../dist/proxy
mkdir -p "$OUT"

build() { # target suffix
  local target="$1" suffix="$2"
  echo "== $target"
  if command -v cargo-zigbuild >/dev/null 2>&1; then
    cargo zigbuild --release --target "$target"
  else
    cargo build --release --target "$target"
  fi
  local bin="target/$target/release/ij-zed-proxy"
  [ -f "${bin}.exe" ] && bin="${bin}.exe"
  cp "$bin" "$OUT/ij-zed-proxy-$suffix"
  shasum -a 256 "$OUT/ij-zed-proxy-$suffix" > "$OUT/ij-zed-proxy-$suffix.sha256"
}

HOST=$(rustc -vV | sed -n 's/host: //p')
case "${1:-all}" in
  host)  build "$HOST" "$(uname -s | tr 'A-Z' 'a-z')-$(uname -m)" ;;
  all)
    build aarch64-apple-darwin darwin-aarch64
    build x86_64-apple-darwin darwin-x86_64
    build aarch64-unknown-linux-gnu linux-aarch64
    build x86_64-unknown-linux-gnu linux-x86_64
    build aarch64-pc-windows-msvc windows-aarch64
    build x86_64-pc-windows-msvc windows-x86_64
    ;;
  *) build "$1" "$1" ;;
esac
ls -lh "$OUT"
