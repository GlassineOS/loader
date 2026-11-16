#!/bin/sh
# Runs in the alpine:3.24 payload image: copies linux-lts and builds the test initramfs.
set -eu
out=$1
mkdir -p "$out" /tmp/ir/bin
cp /boot/vmlinuz-lts "$out/vmlinuz"
cp /boot/config-*-lts "$out/kernel.config"
apk info -e -v linux-lts busybox-static > "$out/versions.txt"
cp /bin/busybox.static /tmp/ir/bin/busybox
cp /src/initramfs/init /tmp/ir/init
cd /tmp/ir && find . | LC_ALL=C sort | cpio -o -H newc --reproducible -R 0:0 2>/dev/null | gzip -9n > "$out/initrd"
# The same tree with 24 MiB that don't compress: an initramfs the size of a release's
# (about 21 MB), for the large-initramfs case.
head -c 25165824 /dev/urandom > /tmp/ir/ballast
cd /tmp/ir && find . | LC_ALL=C sort | cpio -o -H newc --reproducible -R 0:0 2>/dev/null | gzip -1n > "$out/initrd-large"
