# Feature Specification: A display through the firmware's panel refresh

**Feature Branch**: `003-epd-fw-display`
**Created**: 2026-11-16
**Status**: Draft
**Input**: "An optional display backend for hand-back mode: on firmware whose graphics
output has a panel refresh, draw the splash into its framebuffer and refresh the panel
once. Off by default."

## Context

GlassineOS/boot owns the contract: `docs/handback.md`, "Graphics outputs read raw" (the
extended interface and how a GOP is read) and "A display through the firmware's
refresh" (when the loader may draw under a parent, and how), and spec 012 FR-006,
FR-009 and FR-015a, at the commit spec 002 pins. Where they and this spec differ, they
win and this spec is corrected.

Some firmware drives a panel (an e-ink one) whose picture changes only when it is
refreshed. Its GOP has the extended six-word interface: a refresh at +0x18,
`GetFrameBuffer` at +0x20, the Mode at +0x28. Its Blt only fills memory. Under a parent
the loader is headless (002); this backend is the one exception, and only when the
record asks for it.

## User scenarios

### Scenario 1 – The splash on a firmware-refreshed panel (Priority: P1)

The record says `display=epd-fw` and the firmware has an extended GOP. The loader picks
the picture as on a PC, writes it into the framebuffer and refreshes the whole panel
once, then decides and answers as in 002.

**Acceptance**: in QEMU, with the stand-in firmware of `tests/parent`
(`parent.fakefw=1`), the stand-in counts one refresh with the rectangle (0, 0,
1872, 1404), stride 1872, mode 0, and the CRC-32 of the framebuffer's packed RGB565
equals the one the test computes from the splash; no `SetMode`, no `Blt`; the answer and
the count are as without the display.

### Scenario 2 – The override (Priority: P1)

An override made for the panel's portrait mode is removed durably, then drawn with the
one refresh; the next start draws the splash.

**Acceptance**: QEMU case: the override region's head is zero after the start, the
refresh's CRC is the override's, the next start's is the splash's.

### Scenario 2a – A refresh that hangs (Priority: P1)

The firmware's refresh never returns. The start was already counted (and marked
`drew=1`), so the parent's watchdog reset is a failed start like any other. The next
start, with `unmarked` above 0 after a start that drew, doesn't draw and sets the
sticky `display_failed`; the starts count up to the limit as without the display. A
healthy mark doesn't bring the draw back; only the running system's
`glassine-loader-mark display-retry` does, followed by a healthy start.

**Acceptance**: QEMU case with a stand-in refresh that never returns
(`parent.fakefw=hang`): the record's write comes before the refresh and `unmarked=1`,
`drew=1` after the reset; the next start logs `EPD_FW_SKIPPED the previous start drew
and wasn't marked healthy: display_failed=1` and writes `display_failed=1`; the next two
answer `baseline`, then `recovery` at the limit, with no refresh; after a healthy mark
the start is still skipped (`display_failed=1`); after display-retry and a healthy mark
the start draws (and hangs) again, counted first.

### Scenario 3 – Off (Priority: P1)

Without `display=epd-fw`, or without an extended GOP, the loader is headless as in 002:
no `GetFrameBuffer`, no refresh, the splash store left as it is.

**Acceptance**: QEMU cases for both.

## Requirements

- **FR-001**: The backend is used only in hand-back mode, only when the record in use
  has `display=epd-fw`, and only when a GOP reads as `extended` (002 FR-008a). Else the
  loader is headless.
- **FR-002**: It calls `GetFrameBuffer(This, &Fb, &W, &H)` (+0x20) once. It refuses (and
  stays headless) a status other than success, a zero `Fb`, `W` or `H`, a `W` that isn't
  a multiple of 8, or `W`×`H`×2 bytes from `Fb` that don't lie inside the framebuffer
  the Mode names.
- **FR-003**: The picture is picked by `core::plan::pick_picture` for the portrait mode
  `H`×`W` (one display): the override removed durably before it is drawn and drawn only
  when made for that mode; a leave-screen override, or no picture, draws nothing. It is
  composed by `core::gspl::compose`.
- **FR-004**: The composed portrait picture is written into the framebuffer as packed
  RGB565 (red 5, green 6, blue 5 bits, the top bits of each 8-bit channel), rows `W`
  pixels apart, the portrait pixel (u, v) at native (x, y) = (W − 1 − v, u). This
  mapping is unverified on hardware.
- **FR-005**: Then it calls the refresh (+0x18) once: `Refresh(This, Fb, 0, 0, W, H, W,
  0)`, every argument zero-extended to 64 bits. x and width are multiples of 8. The
  refresh's status is logged; it says nothing about the panel.
- **FR-006**: It draws before the answer, never after `ExitBootServices` (the loader
  never calls it), and never calls `SetMode`, `Blt` or a display power protocol.
- **FR-007**: No key line, no list and no message: which key a person would press at
  power-on is not defined on such a machine, and each change of the picture would be one
  more full refresh. The key wait isn't run.
- **FR-008**: The PC path (001) is unchanged and draws only on a `standard` GOP.
- **FR-009**: The order and the guards against a refresh that never returns:
  - the decision and its record write (002 FR-004) come first; the draw follows, before
    the answer;
  - the loader draws only on a start counted by that write (the answer `default`, the
    record written with `unmarked` raised): a recovery, a request, a pick or a record it
    can't write draws nothing; that counting write sets the loader-owned `drew=1`;
  - the loader draws only when the record's `unmarked` was 0 at entry; else it logs
    `EPD_FW_SKIPPED` and stays headless.
- **FR-010**: The sticky key. A start that finds `unmarked` above 0 and `drew=1` sets
  `display_failed=<unmarked>` in its write (every write of that start) and clears
  `drew`; any start that doesn't draw clears `drew`. While `display_failed` is set the
  loader never draws, whatever `unmarked` says. The loader never clears it; a healthy
  mark leaves it; only the running system's `glassine-loader-mark display-retry`
  clears it (boot `docs/handback.md`). The rule is `core::plan::epd_plan`.

## Device scope and risk

QEMU with AAVMF and OVMF only, against the stand-in. The first draw on a real device is a
separate test, with the person's go, after a probe report (002 FR-008a) has shown the
GOP's layout as `extended`. It checks: a full-panel picture; the firmware's own picture
fully cleared; no ghost of it; the orientation right; and the time the start takes with
the refresh. A refresh that hangs is covered by FR-009 and FR-010: the escape is the
next start, which doesn't draw (a power cycle where the parent has no watchdog), with
no outside rewrite of the record; the draw stays off until a deliberate
`display-retry`.

## Out of scope

Partial refreshes, other waveforms, a key line or list on such a machine, drawing after
the answer.

## Success criteria

- **SC-001**: The QEMU cases of Scenarios 1 to 3 (and 2a) pass on x86-64 and aarch64, and with
  the second OVMF.
- **SC-002**: The 002 cases are unchanged: a start without `display=epd-fw` draws nothing
  and leaves the splash store byte for byte.
