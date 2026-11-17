# GlassineOS loader

The boot loader GlassineOS installs on UEFI PCs (x86-64; the same program builds for
arm64 UEFI machines). It is an EFI program written in Rust that draws with
[LVGL](https://lvgl.io).

GlassineOS is a hobby project, written largely with AI coding agents and not reviewed by
subsystem experts. **Nothing here has run on a real PC.** Every behaviour below is tested
in QEMU with OVMF (and the default start and the list with AAVMF on arm64) only.

What it does is specified in GlassineOS/boot, and its own spec maps to those
requirements ([`specs/001-uefi-loader/spec.md`](specs/001-uefi-loader/spec.md)):

- [010 — deployments on UEFI PCs](https://github.com/GlassineOS/boot/blob/main/specs/010-uefi-deployments/spec.md):
  which kernel set starts, counting starts that never reached a healthy mark and falling
  back, the recovery key and its list, a one-shot deployment pick;
- [011 — boot splash](https://github.com/GlassineOS/boot/blob/main/specs/011-boot-splash/spec.md):
  the splash and its one-time override;
- [`docs/boot-partition.md`](https://github.com/GlassineOS/boot/blob/main/docs/boot-partition.md)
  (format 1; the image format 2, which reads format 1 too): the bytes it reads and writes.
  Pinned at boot `cf3271dd`, the image format at boot `887ee626`.

## What it does at a start

1. Refuses, on screen, if Secure Boot is on, and returns to the firmware.
2. Reads both slots of the loader record (`\glassine\loader.a`, `.b`) and takes the one
   in use.
3. Removes the one-time override, flushes the volume, checks it is gone, and only then
   draws it; else the splash, else the default splash. With several displays, the splash
   on each and no override.
4. Shows the key line for 2 s (5 s after an unmarked start) and restores the picture.
   Space opens the list: GlassineOS, the previous deployment, more deployments, the
   previous kernel, the recovery. (The firmware's set-up of 010 FR-039 is left out:
   spec 001 FR-033.)
5. Picks the set (a request, else by the count of unmarked starts: below 3 the default,
   at 3 the previous set, above the recovery), writes the other slot of the record
   (flush, read back), checks the set (`set.rec` and its sum, the sizes and sha256 of
   `vmlinuz` and `initrd`, read whole) and starts the kernel from that memory, the
   initramfs through LoadFile2. A set that fails goes to the next choice; when nothing
   starts, a message names the boot partition and the loader returns to the firmware.

## Under a parent bootloader

On a machine that keeps a bootloader of its own (the Boox Note Air's ABL), that parent
starts the loader from the boot partition (`glassine-boot`, FAT as on a PC, or raw with a
region table) with the load option `glassine.handback=1`
([boot spec 012](https://github.com/GlassineOS/boot/blob/main/specs/012-device-boot-partition/spec.md),
[`docs/handback.md`](https://github.com/GlassineOS/boot/blob/main/docs/handback.md); this
repository's [`specs/002-handback/spec.md`](specs/002-handback/spec.md); pinned at boot
`b6f52f0d`). Then the loader
reads its record from the boot partition, counts the start, and exits with its answer
(`baseline`, `recovery` or a kernel set) in the exit data. It draws nothing, unless the
record has `display=epd-fw` and the firmware's graphics output has a panel refresh: then
it draws the splash ([`specs/003-epd-fw-display`](specs/003-epd-fw-display/spec.md)),
and, when the running system asked for it (`request=menu`) or the last start wasn't
marked healthy, the start menu, picked by touch (with no tap source, the default)
([`specs/004-epd-fw-menu`](specs/004-epd-fw-menu/spec.md)); where the firmware binds no
touch driver (the Note Air), `touch=on` makes the loader's own driver the menu's tap
source and `touch=probe` brings the controller up once into the probe report
([`specs/005-touch-driver`](specs/005-touch-driver/spec.md)). With `probe=1` in the
record it first writes a read-only report of the firmware (protocols, GOP modes,
consoles, input devices, file systems, the I2C and GPIO protocols and the touch pins'
levels) to the partition. The QEMU cases run it under a stand-in parent
(`tests/parent`) that starts it as the ABL does.

## Layout

- `core/`: what the loader decides, reads and writes, with no firmware calls (`no_std`):
  the records and their slots, `set.rec`, the image format, the deployment list, the
  start decisions. Host-tested (`make check`), including a power cut at every byte of a
  slot write.
- `loader/`: the EFI program. `src/arch/` holds what depends on the processor (clock,
  serial log); `src/display/` the displays, with the firmware's graphics output as one
  backend; `c/` the LVGL shim and configuration.
- `tools/gspl-encode/`: the host tool every writer of a splash shares (the running
  system, the release images, the tests): a PNM picture or raw framebuffer pixels to an
  image of format 2, in the layout the target's display takes (`--layout grey8` for a
  grey panel), the smallest payload by default or `--compression fast` (LZ4, no filter)
  for the hibernate preview; `--check FILE` checks an image as the loader does. Built by
  `make` for the build host (`target/release/gspl-encode`). `tools/mksplash.py` writes
  format 1 for the tests that keep it.
- `tests/stub/`: a stand-in kernel that prints its command line and the sum of the
  initramfs it gets, for the QEMU tests; with `stub.answer=`, a broken loader.
- `tests/parent/`: a stand-in parent bootloader for the hand-back cases.
- `tests/mkboot/`: writes a boot partition with the installer's own code
  (GlassineOS/installer `core/src/pc_boot.rs` and `fat.rs`, pinned).
- `tests/qemu/run.py`: the QEMU cases (frames by QMP `screendump`, serial markers, the
  record read back).

## Building and testing

Everything runs in Docker; the host needs Docker, make and git.

```sh
make                       # target/x86_64-unknown-uefi/release/glassine-loader.efi
make ARCH=aarch64          # target/aarch64-unknown-uefi/release/glassine-loader.efi
make check                 # host tests of core/
make test                  # QEMU + OVMF cases; frames and serial logs in out/test/
make test ARCH=aarch64     # QEMU virt + AAVMF cases
make test CASES=trial_request
make test-fw2              # the x86_64 cases again with Alpine's OVMF (another EDK2 release)
make clean                 # target/, out/
```

- Rust 1.99.1 (`rust-toolchain.toml`), `uefi` 0.41; the build image is pinned by digest
  in `docker/`.
- LVGL 9.6.0 is fetched at a pinned release and checked by SHA-256
  (`scripts/fetch-lvgl.sh`), built by `loader/build.rs` with clang for the target's
  triple and float ABI, and reached only through `loader/c/shim.c`.
- OVMF and AAVMF come from the build image's Debian packages at test time, and a second
  OVMF from Alpine's `ovmf` package for `make test-fw2` (`scripts/fetch-alpine-ovmf.sh`);
  nothing of them is committed. The Secure Boot case enrols a key generated for that run only.
- The Linux case boots Alpine `linux-lts` with a busybox initramfs (`make payload`,
  fetched, never committed).

## Releases

A tag `v<version>` (the workspace's version in `Cargo.toml`) makes a GitHub Release
(`.github/workflows/release.yml`): `glassine-loader-x86_64.efi` and
`glassine-loader-aarch64.efi`, `gspl-encode-x86_64-linux` and `gspl-encode-aarch64-linux`
(the splash writer as a static Linux program, `make gspl-encode-static`), `SHA256SUMS`, and
`NOTICES-<arch>.txt` and `NOTICES-gspl-encode-<arch>.txt` with the licenses of what each
binary has compiled in (`make notices`). Each binary is built twice on
separate runners and the two must match byte for byte; the released one is the one the
QEMU cases ran. A 0.x version is a prerelease. A release is never replaced, since
GlassineOS/images pins its files by SHA-256 (`targets/uefi-pc/release.conf`): a change
is a new version. The binaries are not signed (Secure Boot off).

To cut one: raise `version` in `Cargo.toml` by pull request, merge, then
`git tag v<version> <merge commit> && git push origin v<version>`.

## License

GPL-2.0-or-later (see `LICENSE`).

- LVGL (MIT) is fetched, not committed. Compiled in: LVGL core, its TLSF allocator
  (BSD-3-Clause) and the built-in font `lv_font_montserrat_20`, converted by LVGL from
  Montserrat and Font Awesome 5 symbols (both SIL OFL 1.1 upstream; a release carries
  the OFL notices in `NOTICES-<arch>.txt`).
- Crates: `uefi`, `uefi-raw`, `sha2` (MIT OR Apache-2.0), `lz4_flex` (MIT, the splash's
  LZ4 decoder).
- `tests/golden/pc-boot/` is copied from GlassineOS/installer (GPL-2.0-or-later).
- Test only, never committed or shipped: OVMF and AAVMF (Debian `ovmf`,
  `qemu-efi-aarch64`, BSD-2-Clause-Patent), Alpine `linux-lts` and `busybox-static`
  (GPL-2.0).
