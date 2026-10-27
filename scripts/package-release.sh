#!/bin/sh
set -eu

: "${1:?usage: package-release.sh TARGET}"
target=$1

case "$target" in
    x86_64-apple-darwin|aarch64-apple-darwin|x86_64-unknown-linux-musl|aarch64-unknown-linux-musl) ;;
    *) echo 'Unsupported release target' >&2; exit 1 ;;
esac

cargo build --release --locked --target "$target"
mkdir -p dist

tar -czf "dist/drum-$target.tar.gz" -C "target/$target/release" drum
cd dist

if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "drum-$target.tar.gz" > "drum-$target.tar.gz.sha256"
else
    shasum -a 256 "drum-$target.tar.gz" > "drum-$target.tar.gz.sha256"
fi
