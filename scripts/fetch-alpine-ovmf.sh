#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-or-later
# Alpine's OVMF (package ovmf, an EDK2 release newer than the build image's Debian one)
# into DIR, for `make test-fw2`: test-time only, never committed or shipped. The image
# is pinned by digest; the package is what that Alpine branch offers now (its version is
# printed and kept in DIR/version).
#
#   scripts/fetch-alpine-ovmf.sh DIR
set -eu
DIR=$1
ALPINE=alpine:3.24@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf
mkdir -p "$DIR"
DIR=$(cd "$DIR" && pwd)
docker run --rm -v "$DIR":/o "$ALPINE" sh -c '
	apk add -q --no-progress ovmf >/dev/null
	cp /usr/share/OVMF/OVMF_CODE.fd /usr/share/OVMF/OVMF_VARS.fd /o/
	apk info -e -v ovmf > /o/version
	chown -R '"$(id -u):$(id -g)"' /o'
echo "fetch-alpine-ovmf: $(cat "$DIR/version") in $DIR"
