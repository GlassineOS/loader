# Feature Specification: The loader under a parent bootloader

**Feature Branch**: `002-handback`
**Created**: 2026-11-16
**Status**: Draft
**Input**: "The loader in hand-back mode: started by a parent bootloader (the Note Air's
ABL) from a raw partition, it decides like 010 and answers through the exit data;
headless; a read-only firmware probe."

## Context

GlassineOS/boot owns the contract:

- [012 — hand-back to a parent bootloader](https://github.com/GlassineOS/boot/blob/main/specs/012-handback/spec.md)
  (when the loader runs under a parent, what it decides, the floor);
- [`docs/handback.md`](https://github.com/GlassineOS/boot/blob/main/docs/handback.md)
  (normative: the partition layout, the raw record, the loader image header, the load
  options, the answer, the probe report, format 1).

This spec says how the loader meets them. It pins boot at commit
`ca082bc2ef12e7ef83500fdd2c15a4ffbb7ff307`. Where the documents and
this spec differ, the documents win and this spec is corrected.

## User scenarios

### Scenario 1 – A start under a parent (Priority: P1)

A parent starts the loader with the hand-back load option. The loader reads its record
from the partition it was started from, decides, counts the start, and exits with the
answer. It draws nothing and waits for no key.

**Acceptance**: in QEMU, a stand-in parent (`tests/parent`) starts the loader from the
partition and reads `boot=baseline` from the exit data; the record's count went up by
one, in the other slot, read back; the probe area is untouched.

### Scenario 2 – Requests and the limit (Priority: P1)

A recovery request is answered once; a trial request answers its set once; at the limit
the answer is the previous set, or the recovery without one.

**Acceptance**: QEMU cases for each; the request is cleared in the written slot.

### Scenario 3 – The floor (Priority: P1)

Whatever is wrong (no loader, a damaged header or image, an image that isn't a program,
a program that returns nothing, garbage, a wrong check, an unknown answer) the parent
boots its baseline, and the record is unchanged.

**Acceptance**: QEMU cases with the stand-in parent for each; the parent says
`PARENT_BOOT baseline` and why.

### Scenario 4 – The probe (Priority: P2)

With `probe=1` in the record, the loader writes a report of the firmware to the probe
area before it answers; without it, it never writes there.

**Acceptance**: in QEMU the report is valid, names the GOP and lists the mode QEMU's
display is in; the answer and the count are as without the probe.

## Requirements

- **FR-001**: The loader is in hand-back mode exactly when its load options hold the word
  `glassine.handback=1`. Then it uses no display, no splash, no key line, no list, and
  starts no kernel. Other start paths are unchanged.
- **FR-002**: It opens the block device of its `LoadedImage.DeviceHandle` and refuses a
  device whose block size doesn't divide 4096 or that is smaller than 4 MiB: then it
  answers `baseline` without writing.
- **FR-003**: The record is the raw record of `docs/handback.md`: two 16 KiB slots at 0
  and 16 KiB, read whole; the same parsing and choice as the FAT record (one code path in
  `core`, `RecordStore` with a FAT and a raw backend). A slot is written whole, flushed,
  read back; the other slot is never touched in the same start.
- **FR-004**: The decisions are 012's (FR-008 to FR-013): no record, nothing written,
  `baseline`; `default` empty, `baseline`; else the set named; at the limit the previous
  set or the recovery; a recovery start is not counted; a request only with both slots
  valid; a record it can't write, `baseline` uncounted.
- **FR-005**: The answer is built and checked by `core::answer` (both sides of the
  format in one place); the stand-in parent parses with the same code, the ABL with its
  own C parser checked against the same cases.
- **FR-006**: The exit is `Exit(EFI_ABORTED, size, data)` with the data in pool memory,
  as `docs/handback.md` says.
- **FR-007**: `glassine.log=serial` sends the log to the serial line; without it the
  loader writes to no console. The log is also kept in memory (24 KiB at most) for the
  probe report.
- **FR-008**: The probe report (with `probe=1` only) is built from enumeration and
  queries only: handles, protocols per handle, GOP modes through `QueryMode`, console
  handles and text modes. No `SetMode`, no `Blt`, no register access. It is written to
  the probe area at 32 KiB (64 KiB), padded with newlines, with its `check=` line, and
  read back, before the decision. The probe area is apart from both slots, so a failed
  probe write changes neither the record nor the answer.
- **FR-009**: Truncation, when the report doesn't fit: kept log lines first, then the
  extra GOP modes, then the protocol lines; the report says `truncated=1`.

## Contracts

`docs/handback.md` at the pinned commit. `tests/parent` is the reference parent; it does
what the ABL patch does (header, image CRC, `LoadImage` from memory with no path,
`DeviceHandle`, load options, a 30 s watchdog, `StartImage`, the exit data) and prints
what it would boot.

## Device scope and risk

QEMU with AAVMF (aarch64) and OVMF (x86-64) only. Whether the Note Air's firmware passes
the exit data back to the ABL as edk2 does is not known until a device run (012's risk);
the floor covers it (no data: `baseline`).

## Out of scope

The block-list stub that boots a named set from the parent (boot `docs/blocklist.md`);
the parent's code; a GPT change.

## Success criteria

- **SC-001**: `make check` passes, with the raw backend's tests (every byte cut in a slot
  write ends in a record that is the old one or the new one).
- **SC-002**: The hand-back QEMU cases pass on aarch64 and x86-64.
