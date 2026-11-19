#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-or-later
# The start menu's fonts for the GlassineOS loader (LVGL 9.6.0 bitmap fonts), from
# IBM Plex Sans 1.1.0 (npm @ibm/plex-sans, SIL OFL 1.1, Reserved Font Name "Plex").
# A converted bitmap is a Modified Version under the OFL, so the fonts are named
# glassine_sans_* and never "Plex"; the licence goes along as OFL.txt.
#
#   tools/menu-fonts.sh    writes loader/c/fonts/: glassine_sans_semibold_56.c,
#                          glassine_sans_regular_40.c, OFL.txt, SOURCES, SHA256SUMS
#
# The files there are this script's output, committed; run it only to change the fonts
# (the glyphs, the sizes), then rebuild and re-bless the menu's goldens.
#
# Needs: sh, curl, tar, sha256sum, docker (node:24-alpine, lv_font_conv 1.5.3 pinned).
# Network: the npm registry (the font tarball here; lv_font_conv inside the container).
set -eu
cd "$(dirname "$0")/.."
mkdir -p .tmp/menu-fonts
cd .tmp/menu-fonts
HERE=$(pwd)
DEST=../../loader/c/fonts

PLEX_URL=https://registry.npmjs.org/@ibm/plex-sans/-/plex-sans-1.1.0.tgz
PLEX_SHA=c3818979c2a2c82927ea3e7c485e389a18de0b7462887dd4f7acc9a525568962
CONV=lv_font_conv@1.5.3
# The glyphs the menu draws: printable ASCII (deployment labels are ASCII, boot
# deploys.rs) and the middle dot, en dash, right single quote and ellipsis of its texts.
RANGE=0x20-0x7E,0xB7,0x2013,0x2019,0x2026

rm -rf src out work
mkdir -p src out work "$DEST"

curl -fsSL -o src/plex-sans-1.1.0.tgz "$PLEX_URL"
echo "$PLEX_SHA  src/plex-sans-1.1.0.tgz" | sha256sum -c -
tar -xzf src/plex-sans-1.1.0.tgz -C src \
	package/LICENSE.txt \
	package/fonts/complete/woff/IBMPlexSans-SemiBold.woff \
	package/fonts/complete/woff/IBMPlexSans-Regular.woff
cp src/package/fonts/complete/woff/IBMPlexSans-SemiBold.woff src/package/fonts/complete/woff/IBMPlexSans-Regular.woff work/
cp src/package/LICENSE.txt out/OFL.txt

# The conversion, in a throwaway container as the calling user. lv_font_conv is
# installed at exactly 1.5.3 (its own dependencies as npm resolves them for that version).
docker run --rm --network bridge --user "$(id -u):$(id -g)" -e HOME=/tmp \
	-v "$HERE/work:/w" -w /w node:24-alpine sh -euc '
	npm install --no-save --no-audit --no-fund --prefix /tmp/conv '"$CONV"' >/dev/null
	C=/tmp/conv/node_modules/.bin/lv_font_conv
	"$C" --version
	"$C" --font IBMPlexSans-SemiBold.woff -r '"$RANGE"' --size 56 --bpp 4 --format lvgl \
		--no-compress --lv-font-name glassine_sans_semibold_56 -o glassine_sans_semibold_56.c
	"$C" --font IBMPlexSans-Regular.woff -r '"$RANGE"' --size 40 --bpp 4 --format lvgl \
		--no-compress --lv-font-name glassine_sans_regular_40 -o glassine_sans_regular_40.c
	' | tee out/CONVERTER
mv work/glassine_sans_semibold_56.c work/glassine_sans_regular_40.c out/

{
	echo "IBM Plex Sans 1.1.0: $PLEX_URL"
	echo "  tarball sha256 $PLEX_SHA"
	(cd work && sha256sum IBMPlexSans-SemiBold.woff IBMPlexSans-Regular.woff | sed 's/^/  /')
	echo "converter: $CONV (npm), node:24-alpine"
	echo "glyphs: $RANGE; bpp 4, uncompressed, kerning as converted"
	echo "glassine_sans_semibold_56: IBMPlexSans-SemiBold.woff at 56 px"
	echo "glassine_sans_regular_40: IBMPlexSans-Regular.woff at 40 px"
} > out/SOURCES
(cd out && sha256sum glassine_sans_semibold_56.c glassine_sans_regular_40.c OFL.txt SOURCES > SHA256SUMS)
rm -rf work
cp out/glassine_sans_semibold_56.c out/glassine_sans_regular_40.c out/OFL.txt out/SOURCES out/SHA256SUMS "$DEST"/
cat out/SHA256SUMS
