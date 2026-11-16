#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-or-later
# The notices of what a release's glassine-loader.efi has compiled in, for ARCH, into
# OUT (default out/release/NOTICES-ARCH.txt). Runs in the build image after `make loader`
# (it reads the fetched LVGL tree and the crates in CARGO_HOME):
#
#   scripts/notices.sh ARCH [OUT]
#   scripts/notices.sh --tool ARCH [OUT]   gspl-encode for ARCH-unknown-linux-musl instead
#                                          (default out/release/NOTICES-gspl-encode-ARCH.txt):
#                                          Rust's standard library and musl, linked in
#                                          statically, and its crates
#
# - LVGL (MIT), and the parts of its tree the loader's lv_conf.h builds in: TLSF
#   (BSD-3-Clause) and the sprintf of LV_STDLIB_BUILTIN (MIT);
# - the built-in font lv_font_montserrat_20: Montserrat and Font Awesome 5 Free symbols,
#   both SIL OFL 1.1 (texts fetched at pinned commits and checked by SHA-256);
# - every crate linked into the program (not proc macros or build tools), from its
#   registry source; MIT OR Apache-2.0 crates are taken under MIT.
set -eu
TOOL=''
[ "$1" = --tool ] && { TOOL=1; shift; }
ARCH=$1
P=$(cd "$(dirname "$0")/.." && pwd)
if [ -n "$TOOL" ]; then
	OUT=${2:-$P/out/release/NOTICES-gspl-encode-$ARCH.txt}
else
	OUT=${2:-$P/out/release/NOTICES-$ARCH.txt}
fi
L=$P/third_party/lvgl-9.6.0
[ -n "$TOOL" ] || [ -f "$L/.ok" ] || { echo "notices: no $L (make loader first)" >&2; exit 1; }
MONTSERRAT=555facfb2a18c72c3c0380f0d9c0f060453a9058
MONTSERRAT_SHA256=8b7141c03fa4f8d44e6345d5d4931709290f0f67875e452e95ac1fd3a027802e
FA=5.15.4
FA_SHA256=e779748dfe75e84f974df3c7bc07f842011a100159158b0f1f49b2f2a5a515cb
C=$P/.cache/notices
mkdir -p "$C" "$(dirname "$OUT")"
get() { # url sha256 file
	if [ ! -f "$C/$3" ]; then
		curl -fsSL -o "$C/$3.tmp" "$1"
		mv "$C/$3.tmp" "$C/$3"
	fi
	echo "$2  $C/$3" | sha256sum -c - >/dev/null || { echo "notices: $1: wrong sha256" >&2; exit 1; }
}
get "https://raw.githubusercontent.com/JulietaUla/Montserrat/$MONTSERRAT/OFL.txt" $MONTSERRAT_SHA256 montserrat-OFL.txt
get "https://raw.githubusercontent.com/FortAwesome/Font-Awesome/$FA/LICENSE.txt" $FA_SHA256 fontawesome-LICENSE.txt

section() { printf '\n%s\n%s\n\n' "================================================================================" "$1"; }

crates() { # package target: the notices of every crate linked into it
	cd "$P"
	cargo tree --locked --offline -p "$1" --target "$2" -e normal,no-proc-macro \
		--prefix none --format '{p} {l}' | sort -u | while read -r name ver rest; do
		case $rest in *'('*) continue ;; esac   # the workspace's own packages
		ver=${ver#v}
		d=$(ls -d "$CARGO_HOME"/registry/src/*/"$name-$ver" 2>/dev/null | head -n 1)
		[ -n "$d" ] || { echo "notices: no source of $name $ver" >&2; exit 1; }
		f=''
		for x in LICENSE-MIT LICENSE-MIT.md LICENSE-MIT.txt LICENSE LICENSE.md LICENSE.txt; do
			[ -f "$d/$x" ] && { f=$d/$x; break; }
		done
		[ -n "$f" ] || { echo "notices: $name $ver has no license file" >&2; exit 1; }
		section "crate $name $ver ($rest), https://crates.io/crates/$name"
		cat "$f"
	done
}

MUSL=v1.2.5
MUSL_SHA256=f9bc4423732350eb0b3f7ed7e91d530298476f8fec0c6c427a1c04ade22655af
if [ -n "$TOOL" ]; then
	get "https://git.musl-libc.org/cgit/musl/plain/COPYRIGHT?h=$MUSL" $MUSL_SHA256 musl-COPYRIGHT.txt
	R=$(rustc --print sysroot)/share/doc/rust/licenses
	{
		echo "gspl-encode ($ARCH-unknown-linux-musl, linked statically): notices of the third-party code compiled in."
		echo "gspl-encode itself is GPL-2.0-or-later (LICENSE, https://github.com/GlassineOS/loader)."
		section "The Rust standard library (MIT OR Apache-2.0, taken under MIT), https://www.rust-lang.org"
		cat "$R/MIT.txt"
		section "musl libc (MIT), the C library of Rust's linux-musl targets, https://musl.libc.org (COPYRIGHT at $MUSL)"
		cat "$C/musl-COPYRIGHT.txt"
		crates gspl-encode "$ARCH-unknown-linux-musl"
	} > "$OUT.tmp"
	mv "$OUT.tmp" "$OUT"
	echo "notices: $OUT"
	exit 0
fi
{
	echo "glassine-loader.efi ($ARCH): notices of the third-party code and data compiled in."
	echo "The loader itself is GPL-2.0-or-later (LICENSE, https://github.com/GlassineOS/loader)."
	section "LVGL 9.6.0 (MIT), https://github.com/lvgl/lvgl"
	cat "$L/LICENCE.txt"
	section "TLSF in LVGL src/stdlib/builtin (BSD-3-Clause), https://github.com/mattconte/tlsf"
	cat "$L/src/stdlib/builtin/LICENSE_TLSF.txt"
	section "printf in LVGL src/stdlib/builtin (MIT), https://github.com/mpaland/printf"
	cat "$L/src/stdlib/builtin/LICENSE_SPRINTF.txt"
	section "Montserrat, in lv_font_montserrat_20 (SIL OFL 1.1), https://github.com/JulietaUla/Montserrat"
	cat "$C/montserrat-OFL.txt"
	section "Font Awesome 5 Free symbols, in lv_font_montserrat_20 (fonts: SIL OFL 1.1), https://fontawesome.com"
	cat "$C/fontawesome-LICENSE.txt"
	crates glassine-loader "$ARCH-unknown-uefi"
} > "$OUT.tmp"
mv "$OUT.tmp" "$OUT"
echo "notices: $OUT"
