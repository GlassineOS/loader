#!/bin/sh
# Fetch the pinned LVGL release into third_party/ and check its hash.
set -eu
VER=9.6.0
SHA256=b20ee3acc1bba13c62d854f9ebd62e4c51e0b443b1e0225892e86442defa84df
mkdir -p "$(dirname "$0")/../third_party"
cd "$(dirname "$0")/../third_party"
[ -f "lvgl-$VER/.ok" ] && exit 0
[ -f "lvgl-$VER.tar.gz" ] || curl -fsSL -o "lvgl-$VER.tar.gz" \
    "https://github.com/lvgl/lvgl/archive/refs/tags/v$VER.tar.gz"
echo "$SHA256  lvgl-$VER.tar.gz" | sha256sum -c -
rm -rf "lvgl-$VER"
tar -xzf "lvgl-$VER.tar.gz"
touch "lvgl-$VER/.ok"
