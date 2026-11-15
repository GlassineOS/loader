# GlassineOS loader

The boot loader GlassineOS installs on UEFI PCs (x86-64). It is an EFI program written in
Rust that draws with [LVGL](https://lvgl.io).

What it does is specified in GlassineOS/boot:

- [010 — deployments on UEFI PCs](https://github.com/GlassineOS/boot/blob/main/specs/010-uefi-deployments/spec.md):
  - which kernel set to start;
  - counting starts that never reached a healthy mark, and falling back;
  - the recovery key and the list it opens;
  - a one-shot deployment pick passed to the initramfs.
- [011 — boot splash](https://github.com/GlassineOS/boot/blob/main/specs/011-boot-splash/spec.md):
  the splash image and its one-time override.

## Status

A feasibility spike (branch `spike-lvgl`), tested in QEMU with OVMF only. Nothing has run
on a real PC. It draws the splash and the one-time override (011), the line naming the
recovery key and the start-options list (010 FR-039), passes a one-shot deployment pick
on the kernel command line (010 FR-042) and starts an EFI-stub kernel with its initramfs
from checked buffers. Not built yet: the loader record of 010 and its writes, start
counting and fallback, the recovery, the previous kernel set, the firmware set-up entry,
several displays.

## Building and testing

Everything runs in Docker; the host needs Docker, make and git.

```sh
make            # target/x86_64-unknown-uefi/release/glassine-loader.efi
make test       # QEMU + OVMF cases; frames and serial logs in out/test/
make test CASES=override_once_then_splash
make distclean  # also removes the Docker images and fetched sources
```

- Rust and the `uefi` crate, toolchain pinned in `rust-toolchain.toml`; build image
  pinned by digest in `docker/`.
- LVGL is fetched at a pinned release and checked by SHA-256 (`scripts/fetch-lvgl.sh`),
  built by `build.rs` with clang for the same target and float ABI as Rust, and reached
  only through the C shim in `c/` (`c/lv_conf.h` is the configuration).
- `tools/mksplash.py` writes the spike's splash image format (header layout in
  `src/splash.rs`).
- `tests/run.py` builds a FAT boot partition, boots it, syncs on serial markers, grabs
  frames with QMP and checks them pixel for pixel. The test kernel is Alpine `linux-lts`
  with a busybox initramfs (`initramfs/init`), fetched by `make payload`, never committed.
- Frames kept for review: `docs/frames/`.

## License

GPL-2.0-or-later (see `LICENSE`).

- LVGL (MIT) is fetched, not committed. Compiled in: LVGL core, its TLSF allocator
  (BSD-3-Clause) and the built-in font `lv_font_montserrat_20`, converted by LVGL from
  Montserrat and Font Awesome 5 symbols (both SIL OFL 1.1 upstream; a release should carry
  the OFL notice).
- Crates: `uefi`, `uefi-raw`, `sha2` (MIT OR Apache-2.0).
- Test only, never committed or shipped: OVMF (Debian `ovmf`, BSD-2-Clause-Patent),
  Alpine `linux-lts` and `busybox-static` (GPL-2.0).
