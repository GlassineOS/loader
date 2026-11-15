# CLAUDE.md — GlassineOS/loader

The boot loader of UEFI PCs (see `README.md`): an EFI program in Rust that draws with
LVGL. It implements GlassineOS/boot's specs 010 (deployments on UEFI PCs) and 011 (boot
splash and its one-time override).

- The loader writes only its own records on its own boot partition. Every write is
  durable and power-cut safe. It never writes another system's partitions or firmware
  entries.
- Behaviour is proven in QEMU with OVMF (captured frames, serial output) before it merges;
  never run it on a real PC without the owner's go.
- Nothing vendor-owned: no firmware, keys or signed binaries from others.
- Commits carry an `Assisted-by:` or `Co-Authored-By:` trailer. Never add
  `Signed-off-by`. Run `.glassine/org/scripts/safety-scan.sh --staged` before committing.
- Specs: Spec Kit 1.1.0 with the org preset (`.specify/`, setup in
  `.glassine/org/SPECS.md`); only `spec.md` is committed. The repository principles are
  the last section of `.specify/memory/constitution.md`.
- Organization guidance for coding agents, including the project knowledge search
  (run `git submodule update --init` if the import below is missing):

@.glassine/org/AGENTS.md
