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

Nothing is built yet. Nothing has run on a real PC.

## License

GPL-2.0-or-later (see `LICENSE`). LVGL is MIT-licensed.
