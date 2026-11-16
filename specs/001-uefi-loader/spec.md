# Feature Specification: The loader of UEFI PCs

**Feature Branch**: `001-uefi-loader`
**Created**: 2026-11-16
**Status**: Draft
**Input**: "Build the GlassineOS PC boot loader: GlassineOS/boot specs 010 and 011 against
`docs/boot-partition.md`, proven in QEMU with OVMF, x86-64 now and arm64 later."

## Context

GlassineOS/boot owns what a PC's loader does and the bytes it reads and writes:

- [010 — deployments on UEFI PCs](https://github.com/GlassineOS/boot/blob/main/specs/010-uefi-deployments/spec.md)
  (which kernel set starts, the count of unmarked starts, the recovery key and its list,
  the one-shot deployment pick);
- [011 — boot splash](https://github.com/GlassineOS/boot/blob/main/specs/011-boot-splash/spec.md)
  (the splash and its one-time override);
- [`docs/boot-partition.md`](https://github.com/GlassineOS/boot/blob/main/docs/boot-partition.md)
  (normative: the layout, the loader record, a kernel set, the splash store and its image
  format, `format=2`, which reads `format=1` too).

This spec is the loader's own. It says how the loader meets those documents, which of
their requirements it carries (by their numbers), and how it reads them where they leave
a choice. It pins boot at commit `cf3271dd8947069aa8304b4ac9e16c18f0eafd92`, and the image
format at `887ee626e9723f4bd722afbb19be2e16c0b84cb7` (format 2). Where the
documents and this spec differ, the documents win and this spec is corrected.

The loader is one program for x86-64 PCs today. The same program is meant to build for
arm64 UEFI machines later (010's assumptions), so nothing in its behaviour depends on the
processor, and the way it draws does not assume one kind of display output.

A feasibility spike (pull request #1) showed that the toolchain, the drawing library, the
splash, the list and a kernel start from checked memory work under OVMF.

## User scenarios

### Scenario 1 – An ordinary start (Priority: P1)

**Given** a boot partition as the installer writes it, **When** the PC starts and nobody
touches it, **Then** the splash is drawn, the key line shows for about two seconds and
goes, the start is counted in the loader record, and the default set's kernel runs from
the bytes the loader checked, with the set's command line and nothing added.

### Scenario 2 – Starts that never become healthy (Priority: P1)

**Given** three counted starts with no healthy mark in between, **When** the PC starts
again, **Then** the previous set starts and is recorded as caused by the limit; **and**
at the start after that, still unmarked, the recovery starts.

### Scenario 3 – A kernel trial, once (Priority: P1)

**Given** a trial set and `request=trial` in a record whose two slots are valid, **When**
the PC starts, **Then** the request is cleared durably before anything of the trial set
is read, and the trial set starts; the start after it is the default set.

### Scenario 4 – The recovery, by request or by key (Priority: P1)

**Given** `request=recovery`, or a person pressing the recovery key and picking the
recovery, **When** the PC starts, **Then** the recovery image starts from memory and the
count is not raised.

### Scenario 5 – A damaged record or set never costs a start (Priority: P1)

**Given** a loader record slot that is cut or fails its check, or a kernel set whose
files no longer match, **When** the PC starts, **Then** the other slot is used, a set
that fails is skipped for the next choice (default, previous, recovery), and a PC with
nothing that starts sees a message naming the boot partition and returns to its firmware.

### Scenario 6 – The one-time override (Priority: P1)

**Given** an override in the splash store, **When** the PC starts, **Then** it is removed
durably and only then drawn; the next start shows the splash.

### Scenario 7 – An earlier deployment, once (Priority: P2)

**Given** a deployment list in the loader record, **When** the person presses the key
during the wait and picks a deployment, **Then** the set starts as it would have without
the pick, with `glassine.deploy.once=<id>` added to its command line, and nothing is
written for the pick.

### Scenario 8 – Secure Boot on (Priority: P2)

**Given** a firmware with Secure Boot enforcing, **When** it reaches the loader, **Then**
the loader starts nothing, writes nothing, says why on screen, and returns to the firmware.

## Requirements

### Where it reads and writes

- **FR-001** The loader reads and writes only the file system of the partition it was
  started from, at the paths of `boot-partition.md` "Layout", with FAT long names. It
  reads no `.new` file and removes nothing under `\glassine\sets\` or
  `\glassine\recovery\`. (010 FR-004, boot-partition "Layout")
- **FR-002** Its only writes are: a slot of the loader record, and the removal of
  `\glassine\splash\override.gspl`. It writes no firmware variable. (010 FR-031)
  (010 FR-031, 011 FR-012)

### The loader record

- **FR-003** Each slot (`\glassine\loader.a`, `\glassine\loader.b`) is read whole. A slot
  is valid when its text is `key=value` lines each ended by a newline, with no carriage
  return, in UTF-8, the first line `format=1`, no key twice and no empty key, a decimal
  `seq`, the last line before the padding `check=` with the sha256 of every byte before
  it, and nothing but newlines after that line. Unknown keys are kept; a missing key is
  empty. A slot that is missing or not valid counts as absent. (010 FR-008, FR-019)
- **FR-004** The record in use is the valid slot with the higher `seq`, slot a on a tie.
  The loader knows whether both slots were valid. (boot-partition "Slots")
- **FR-005** A write takes the record in use, changes only the keys the loader owns
  (`seq`, `request`, `unmarked`, `started`, `started_why`), keeps every other line as it
  was read, in its order, raises `seq` by one, adds the check line, pads with newlines to
  exactly 16384 bytes and writes the slot that is not in use. It then flushes the file and
  reads it back; the write counts only when the bytes read back are the bytes written. The
  slot in use is never written. A record that would not fit is not written. (010 FR-019,
  FR-020)
- **FR-006** A power cut at any byte of a write leaves a record that the next start reads
  as the old one or the new one. (010 Scenario 8)
- **FR-007** A write the loader can't make (a read-only file system, a failed flush, a
  read-back that differs) is a loader that can't write: the start goes on with the
  default set, counts nothing and takes no request. (010 FR-009, boot-partition "What the
  loader does with it")

### What starts

- **FR-008** With no loader record, the loader starts the recovery (FR-021).
- **FR-009** A request (`trial` or `recovery`) is taken only from a record read with both
  slots valid. With one valid slot, the loader acts as if `request` were empty, and its
  own write, if it makes one, writes `request` empty. (010 FR-008)
- **FR-010** With no choice in the list, the loader picks, in this order: a request; else
  by `unmarked` (empty counts as 0): below 3 the default set (`started_why=default`); at 3
  the previous set (`limit`); above 3, or at 3 with no previous set that checks, the
  recovery. (010 FR-006, FR-011, FR-015)
- **FR-011** For a request, the loader first writes the record with `request` empty (and
  for a trial `started` = the trial set and `started_why=request`), and only once that
  write counted does it read the trial set or the recovery. A trial start is not
  counted. When that write can't be made, the request is not taken (FR-007). (010 FR-009,
  FR-010)
- **FR-012** A set that fails its check (FR-015) is not started, and the next choice is
  taken: the default set, then the previous set, then the recovery; a choice already
  tried is not tried again.
- **FR-013** Before it starts the default or the previous set, the loader writes the
  record with `unmarked` one higher, `started` the set's id and `started_why` the reason:
  `default`; `limit` for the previous set started by the limit or because the default
  set failed its check; `choice` for the previous set picked in the list. A start of the
  recovery is not counted and writes nothing, apart from clearing `request=recovery`.
  (010 FR-011, boot-partition "Keys")
- **FR-014** The loader makes at most one counting write at a start. When a set it
  counted can't be started after all, the next choice starts without a second count.

### A kernel set

- **FR-015** A set (`\glassine\sets\<id>\` or `\glassine\recovery\<id>\`) checks when: its
  `set.rec` has the sha256 the loader record names for it (not compared for a recovery
  found by itself); `set.rec` is a valid record of format 1 whose `id` is the directory's
  name; `vmlinuz` and `initrd` are read whole and have the sizes and sha256 the set
  record names; and its `cmdline` is at most 1920 bytes and holds
  `glassine.system=`, `glassine.userdata=` and `glassine.img=<its id>`, and no
  `glassine.deploy.once=`. Only a recovery image may have no `initrd` (both of its keys
  empty). (010 FR-005, FR-007, boot-partition "How the loader checks and starts a set")
- **FR-016** The kernel is started from the memory that was checked, with the set's
  `cmdline` as its load options, plus ` glassine.deploy.once=<id>` when a deployment was
  picked, and nothing else. The initramfs is handed over from that memory through the
  Linux initrd media device path; the kernel reads nothing from the boot partition.
  (010 FR-007, FR-042)
- **FR-017** If the firmware refuses to start the kernel, or the kernel returns, the
  loader takes the next choice (FR-012), with nothing of the failed set left installed.

### The recovery

- **FR-018** The recovery the record names is checked like a set (FR-015).
- **FR-019** With no loader record, or when the recovery the record names fails, the
  loader takes each directory in `\glassine\recovery\` in descending byte order of its
  name and starts the first whose `set.rec` is valid and whose files match it.
  (boot-partition "What the loader does with it", 010 FR-037)
- **FR-020** The loader never starts a set from `\glassine\sets\` that no valid record
  names, and never a trial set without a request from a record with both slots valid.
  (010 FR-008)
- **FR-021** When nothing can be started, the loader shows a message that names the boot
  partition and says that nothing on it could be started, waits a few seconds or for a
  key, and returns to the firmware so it goes on to its next entry. No path waits for
  ever. (010 FR-016)

### The splash (011)

- **FR-022** As soon as it has read the record, the loader handles the splash store: if
  `override.gspl` exists, it reads it, removes it, flushes the volume and checks that it
  is gone, and only then may draw it. An override it can't remove is not drawn. Else it
  draws `splash.gspl`, `default.gspl` when that is missing or skipped, and nothing when
  both are. (011 FR-001, FR-002, FR-012, FR-013, boot-partition "The splash store")
- **FR-023** An image is checked in the order `boot-partition.md` gives (length, magic,
  header CRC, version, kind, payload length, limits, pixel layout, compression and
  filter, payload CRC, decode) and skipped on the first failure; bytes 56 to 63 of the
  header are not looked at. The loader reads format 2 in every pixel layout (BGRx,
  RGB888, grey8) with every compression (none, run-length, LZ4) and filter (none, up),
  and format 1; it checks the pixel limit before it reserves memory for a picture.
  Reading and checking one image takes at most one second, else it is skipped. A
  leave-screen image (kind 1) is valid only as the override and only with its picture
  fields zero and no payload. (011 FR-004, FR-005)
- **FR-024** An image made for the display's current mode is drawn pixel for pixel,
  centred on its background colour when smaller; a splash made for another mode is scaled
  to fit with its aspect ratio kept and centred on its background colour; an override
  made for another mode is not drawn and the splash is drawn instead. (011 FR-006 to
  FR-008)
- **FR-025** With several displays, the loader draws the splash on each and no override;
  the override is still used up. (011 FR-010)
- **FR-026** A leave-screen override makes the loader draw nothing at that start. (011
  FR-014)
- **FR-027** After drawing, the loader changes the picture only for the key line, the
  list and a failure message; it never writes text to the firmware's text console, which
  the firmware may draw on the display. (011 FR-016, FR-017)

### The key line and the list (010 FR-039 to FR-047)

- **FR-028** During its wait the loader shows one line naming the recovery key over the
  picture, and when the wait ends restores the picture under it pixel for pixel. The
  wait is two seconds, and five when `unmarked` is not 0. With nothing drawn (a
  leave-screen override) the line is not shown; the key works all the same. (010 FR-017,
  FR-039, FR-048, 011 FR-016a)
- **FR-029** The key opens a list over the picture: GlassineOS, the previous deployment,
  "More" for the other deployments, the previous kernel and the recovery (not the
  firmware's set-up: FR-033). An entry with nothing behind it is not shown. The list works by arrow keys and
  Enter, and Escape goes back. When the list closes, the picture under it is restored.
  (010 FR-039, FR-040, 011 FR-016a)
- **FR-030** The deployments come from `deploys` and `deploy1` … `deploy<n>` of the
  record: `<id> <state> <label>`. The entry of state `previous` is the previous
  deployment; the others are under "More". An entry whose id doesn't match
  `[A-Za-z0-9._-]+` (not starting with a dot), whose state is not one of `previous`,
  `candidate`, `failed`, `kept`, or whose label is not printable ASCII of at most 64
  bytes, is left out. (010 FR-040, FR-041, boot-partition "Keys")
- **FR-031** Picking GlassineOS starts what the loader would have started without the
  key. Picking a deployment does the same and adds `glassine.deploy.once=<id>`; nothing
  is written for the pick. (010 FR-042)
- **FR-032** Picking the previous kernel starts the previous set (`started_why=choice`,
  counted); picking the recovery starts the recovery (not counted). A pending request is
  left as it is, unless the person picked the recovery and the request is `recovery`.
- **FR-033** The list does not offer the firmware's set-up that 010 FR-039 names:
  opening it from the loader means writing a firmware variable at a start, which 010
  FR-031 rules out. [NEEDS CLARIFICATION: whether a person's explicit pick of the
  firmware's set-up may write the one firmware variable that asks for it.]
- **FR-034** No keyboard, or no display, never stops a start: without a display, a key
  pressed in the wait is logged and the loader starts what it would have started
  without it.

### Secure Boot

- **FR-035** When the firmware reports Secure Boot as enforcing, the loader reads and
  writes nothing on the boot partition, shows that GlassineOS starts only with Secure
  Boot off and that it is switched off in the firmware's set-up, and returns to the
  firmware after a few seconds or a key. (010 FR-036)

### Machines

- **FR-036** The behaviour above is the same on every processor the loader is built for.
  What depends on the processor (its clock, its serial log) and what depends on the kind
  of display output is kept apart, so a second kind of display output can be added
  without changing the rest.

## Contracts

| Contract | Owned / consumed | Version | Spec location |
|---|---|---|---|
| Boot partition layout, loader record, kernel set, splash store and image format | consumed (GlassineOS/boot) | `format=1`, boot `cf3271dd`; image format `format=2` (and 1), boot `887ee626` | `docs/boot-partition.md` |
| Deployments on UEFI PCs | consumed (GlassineOS/boot) | spec 010 at `cf3271dd` | `specs/010-uefi-deployments/spec.md` |
| Boot splash | consumed (GlassineOS/boot) | spec 011 at `cf3271dd` | `specs/011-boot-splash/spec.md` |
| `glassine.deploy.once=` on the command line | consumed (GlassineOS/boot) | `docs/boot.md` "Command-line keys" | boot 001, 010 FR-042 |
| The boot partition as the installer writes it | consumed (GlassineOS/installer) | `core/src/pc_boot.rs` at `e44e0e2b` | installer spec 001 |
| The Linux initrd media device path (LoadFile2) | consumed (Linux EFI stub) | `LINUX_EFI_INITRD_MEDIA_GUID` | Linux `Documentation/arch/x86/boot.rst` |

## Device scope and risk

- **Writes:** one slot of the loader record per start, and the override's removal, both
  on the boot partition GlassineOS owns. Nothing on any other partition; no firmware
  variable.
- **Failure mode:** a loader that can't start a set returns to the firmware, which goes
  on to its next entry (another system, or its own menu). A broken loader binary leaves
  the firmware's boot menu and the install medium.
- **Escape:** the firmware's boot menu and the installer's repair from removable media
  (010 FR-030). Nothing here touches what those depend on.
- **Hardware:** none in this repository's tests. Runs on a real PC only with the owner's
  go, recorded.

## Acceptance

- **Host tests:** record parsing, choosing and writing, with a write cut at every byte of
  the written slot; the start decision for every count, request and slot state; set
  checks; image decoding with damaged headers and payloads.
- **QEMU with OVMF (x86-64):** a boot partition written by the installer's own code;
  default start; three unmarked starts then the previous set, then the recovery; a trial
  request; a recovery request; a damaged slot b; a kernel whose sha256 doesn't match;
  the override drawn once then gone, from captured frames; the key line, the list and a
  one-shot pick reaching the kernel's command line; Secure Boot enforcing.
- **QEMU with AAVMF (arm64):** the default start reaches the test kernel, and the key
  line, the list and a one-shot pick work.
- **Product level:** none here; 010 SC-001 to SC-017 belong to the images repository.

## Out of scope

- Writing anything but its own records; repairing the boot partition; removing left-over
  sets (the running system's and the recovery's work).
- Starting with Secure Boot on; another system; network boot.
- A display output other than the firmware's graphics output (only kept possible).
- Fonts beyond ASCII.

## Success criteria

- **SC-001** Every QEMU case of "Acceptance" passes in CI on every pull request.
- **SC-002** A cut at every byte of a slot write is read back as the old or the new record
  (host test).
- **SC-003** No path of the loader waits without an end: each failure ends in a start, a
  message and a return to the firmware.
- **SC-004** The frame drawn by the loader is unchanged from the end of its wait to the
  kernel's start, apart from the list or a failure message when they are shown.
