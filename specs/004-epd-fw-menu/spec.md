# Feature Specification: A tap menu on a firmware-refreshed panel

**Feature Branch**: `004-epd-fw-menu`
**Created**: 2026-11-16
**Status**: Draft
**Input**: "Bring up the loader's start list as a tap menu on the Note Air's e-ink panel:
opened by `request=menu` or after a start that wasn't marked healthy, the PC list's
entries answered through the hand-back, refreshed in rectangles, with touch and keys."

## Context

GlassineOS/boot owns the contract: `docs/handback.md`, "The start menu" (when it opens,
its writes, what each pick answers, the watchdog, the refresh rectangles) and "A display
through the firmware's refresh"; `docs/boot-partition.md`, "Keys" (`request=menu`); spec
012 FR-009; at the commit spec 002 pins. Where they and this spec differ, they win and
this spec is corrected.

The display is spec 003's: the panel the firmware refreshes through an extended GOP,
drawn in portrait. On such a machine a person has no key to press at power-on (010
FR-039), so the menu opens only when the running system asked for it or when the last
start didn't reach the healthy mark. Its entries are the PC list's (010 FR-039, FR-040);
the picks are answered to the parent as hand-back answers, never by starting a kernel.

## User scenarios

### Scenario 1 – The menu on request (Priority: P1)

The person chose "Reboot to the start menu" in the running system, which wrote
`request=menu`. At the next start the loader writes the menu write (the request cleared,
the start counted, `drew=1`), shows the menu over the whole panel and waits for a tap.

**Acceptance**: in QEMU with the stand-in firmware (`parent.fakefw=1`) and its scripted
pointer: the first refresh is the whole panel and comes after the menu write; the frame
matches its golden; the record after the start has `request` empty.

### Scenario 2 – The menu after a start that wasn't marked healthy (Priority: P1)

A start drew the splash, the system failed before the healthy mark. The next start
(`unmarked` 1, `drew` empty because the earlier draw returned) opens the menu by itself.

**Acceptance**: QEMU, two starts in a row on one disk: the first draws the splash and
answers; the record has `drew` empty and `unmarked=1`; the second opens the menu. A
start with `unmarked=0` and no request opens no menu: one full refresh of the splash, as
in 003.

### Scenario 3 – A tap picks an entry (Priority: P1)

The person taps an entry. The tapped row turns black (one refresh of the rows that
changed; none when it was already the focused one), the splash is drawn (one full refresh), the pick's write is made and the answer given:

| Entry | Answer | The pick's write |
|---|---|---|
| GlassineOS | what the start answers without the menu (by the count; `baseline` when `default` is empty) | as that start's: counted, `started_why=default` (or `limit`) |
| Previous deployment, a deployment under "More" | the same, with `cmdline_add=glassine.deploy.once=<id>`; with `parent_cmdline=no` the same without it | the same as GlassineOS; with `parent_cmdline=no` also `deploy_once=<id>` |
| Previous kernel (only when `previous` is set and `parent_sets` isn't `no`) | `set:<previous>` | counted, `started=<previous>`, `started_why=choice` |
| Recovery | `recovery` | not counted: `unmarked` as it was at entry |

Every pick's write also clears `drew`. Only a deployment's pick under
`parent_cmdline=no` writes `deploy_once`; every other write of the loader (the menu
write, a start without the menu, any other pick) clears one the record holds, so a pick
whose start never reached the initramfs doesn't come back.

A parent that passes no command line (the Note Air's ABL ignores `cmdline_add` and
boots `boot_b`) says so through the record's `parent_cmdline=no`, written from the
board's `LOADER_PARENT_CMDLINE=no` (boot `docs/handback.md`, "The start menu"): the
deployment then goes into the record as `deploy_once=<id>`, and the boot's initramfs
takes it, clears it and starts that deployment once. One that boots no kernel set says
`parent_sets=no` (`LOADER_PARENT_SETS=no`): the first list leaves "Previous kernel" out,
since its `set:` answer would boot the baseline. On a PC neither key is set and nothing
changes.

**Acceptance**: a QEMU case per entry: the scripted tap at the entry's centre gives that
answer and that record; "More" opens the second list and "Back" returns.

### Scenario 4 – No input (Priority: P1)

No tap for 60 seconds (keys count for nothing): the loader takes the GlassineOS pick, logs
`MENU_TIMEOUT`, and answers as Scenario 3 does. Throughout, the firmware's watchdog
never fires: the loader re-arms it for 30 seconds before each refresh and at least every
10 seconds while it waits, and leaves it armed for 30 seconds when it answers.

**Acceptance**: QEMU case with the stand-in's 30 s watchdog armed by the parent: the
answer comes after more than 60 s, no reset, the stand-in's watchdog log shows the
re-arms and 30 s as the last setting before the answer.

### Scenario 5 – No tap source (Priority: P1)

The machine has no keys but the power button, and its firmware has no touch driver
bound: no tap source. The menu doesn't open and nothing waits for input that can't come:
the loader logs `MENU_NO_INPUT` and the start takes the default at once, counted as
without a menu, so a failed start still counts toward the limit. Keys that reach the
console are logged and do nothing.

**Acceptance**: QEMU cases: with the stand-in's pointer left out (`parent.nopointer=1`)
and `request=menu`, `MENU_NO_INPUT`, no menu, the request cleared, the default answer,
`unmarked=1` and the splash's one refresh; the next start (`unmarked` 1) is the same
without a draw. With the menu open, Down, Down and Enter are logged with their scan
codes and characters and change nothing: no refresh, no pick, the timeout as Scenario 4.

### Scenario 6 – Partial and full refreshes (Priority: P1)

Each refresh after the first covers only the changed area, rotated to the panel's native
landscape, x and width multiples of 8, inside the panel. With `display_partial=0` every
refresh is the whole panel.

**Acceptance**: QEMU: every logged rectangle is aligned and in bounds, the focus or tap
refresh is smaller than the panel, and the framebuffer's CRC after each refresh matches
its golden; with `display_partial=0` every rectangle is (0, 0, 1872, 1404) and the
final frame's CRC is the same.

### Scenario 7 – A refresh that hangs in the menu (Priority: P1)

A refresh inside the menu never returns; the parent's watchdog resets. The next start
finds `drew=1` with `unmarked` above 0, sets the sticky `display_failed` and opens no
menu and draws nothing, as 003 says.

**Acceptance**: QEMU case with `parent.fakefw=hang` and `request=menu`: the menu write
comes before the refresh, the machine resets, the next start logs `EPD_FW_SKIPPED` and
writes `display_failed=1`.

## Requirements

- **FR-001**: The menu opens only in hand-back mode, only with the display of 003
  (`display=epd-fw`, an extended GOP that passes 003 FR-002), only while
  `display_failed` is not set and the start doesn't find `drew=1` with `unmarked` above
  0, never on a start where the parent passed `glassine.pick=recovery`, and only when
  the record read at entry has `request=menu` (both slots valid) or `unmarked` above 0,
  and only when there is a tap source (FR-010). Otherwise the start is 003's: no wait,
  the splash alone, no other refresh. When only the tap source is missing the loader
  logs `MENU_NO_INPUT`; the start takes the default at once, counted as without a
  menu.
- **FR-002**: A `request=menu` the loader doesn't open the menu for is cleared in the
  start's first write and the start goes on as without it.
- **FR-003**: Before the first refresh the loader makes the menu write: `request`
  cleared (when it was `menu`), `unmarked` one more than at entry, `drew=1`. If it fails,
  no menu opens and the start goes on as 003 says for a record it can't write.
- **FR-004**: The entries are the list of 010 FR-039 and FR-040 as the PC loader builds
  it from the record read at entry: GlassineOS, the previous deployment, "More
  deployments" (the candidate, the last failed and the kept ones, then "Back"), the
  previous kernel when `previous` is set, the recovery. No other entry: no firmware
  set-up, no Android.
- **FR-005**: Each pick is answered and written as Scenario 3's table says, the write
  computed from the record read at entry and applied over the menu write, with
  `unmarked` written explicitly and `drew` cleared. When the pick's write fails, the
  answer is the recovery for the recovery pick, else `baseline` (why `not-written`).
- **FR-006**: The order: the menu write; every refresh of the menu; the splash of 003
  (the override used as there, one full refresh); the pick's write; the answer. No
  refresh comes after the write that clears `drew`.
- **FR-007**: A start without the menu that draws (003) also writes once more after its
  refresh returned, clearing `drew`; a failure of that write changes nothing but is
  logged. So `drew=1` at entry means only that a draw never returned.
- **FR-008**: The menu is drawn in the portrait mode of 003 (`H`×`W`), white with black
  text, rows at least 120 pixels high across the panel, the text at least 40 pixels; no
  animation, no pressed-state style, no scrolling effect. The focused or tapped row is
  shown inverted (white text on black). The first frame is one refresh of the whole
  panel.
- **FR-009**: Refreshes. After the first, a refresh waits until the picture has stayed
  unchanged for 100 ms and no touch is down, then covers the bounding box of the area
  redrawn since the last refresh, mapped to the native rectangle (the portrait rectangle
  (u, v, w, h) is native (W − v − h, u, h, w)), x rounded down and x + width up to
  multiples of 8, clipped to the panel. The call is `Refresh(This, Fb, x, y, w, h, W,
  0)` with `Fb` the framebuffer's base. With `display_partial=0` in the record every
  refresh is (0, 0, W, H). One input gives one refresh at most, plus the splash's when
  it is a pick. Each refresh is logged with its rectangle and how long it took
  (`EPD_REFRESH`).
- **FR-010**: Taps are the only input, from one tap source: it gives the touch's state,
  pressed or not, at a position on the portrait panel, polled, never waited on. The menu
  depends on nothing else, so a touch driver of the loader's own for the panel's
  controller can be a source behind the same interface. Today the one source is a
  firmware absolute pointer of a device's own (FR-011); the console splitter's pointer
  on the console-in handle is not a source (with no touch driver bound under it, it
  never reports). Keys don't drive the menu and are no input for the timeout: each key
  the console's input gives (its extended form where the console-in handle has one) is
  logged (`KEY`) with its scan code and character, and from the extended form its shift
  and toggle state, so a run shows what a machine sends (the power key, `SCAN_SUSPEND`,
  among them). The PC path keeps its keys (001).
- **FR-011**: The firmware pointer source. The loader reads the pointer with `GetState`
  only: never `Reset`, never a wait on its event. A position is mapped through the
  Mode's ranges to the panel's native axes by one orientation constant (axes swapped or
  not, each flipped or not; unverified on hardware), then to portrait by the inverse of
  003 FR-004. A range with its maximum not above its minimum, or a Mode the loader can't
  read, makes the pointer no source. Each raw and mapped press is logged (`TOUCH`).
- **FR-011a**: Taps, from any source. A press counts at once; a release less than 30 ms
  after it is a bounce and ignored; no new state for 300 ms after a press is a release;
  a press within 300 ms of the last release is ignored until it ends. A tap selects the
  row it lands in; a press that ends outside it does nothing.
- **FR-012**: The watchdog: before each refresh and at least every 10 s while the menu
  waits, `SetWatchdogTimer(30, …)`; 30 s again before the answer. Never disabled.
- **FR-013**: The timeout: 60 s after the menu opened or after the last input, the
  GlassineOS pick, logged as `MENU_TIMEOUT`.
- **FR-014**: With `probe=1` the probe report is written again after a start that
  opened the menu or would have but for a tap source, so its log holds the menu's lines
  (refreshes, keys, taps, the pick, or `MENU_NO_INPUT`).
- **FR-015**: The PC path (001) is unchanged: its theme, list, key line and frames.

## Device scope and risk

QEMU with AAVMF and OVMF only, against the stand-in. Unverified on hardware, each checked
by a device run with the person's go:

- whether the firmware's refresh honours a rectangle other than the whole panel (and
  whether it reads `Fb` as the framebuffer's base); `display_partial=0` is the escape;
- touch: a probe run on a Note Air listed one absolute pointer, on the console-in
  handle, with ranges 0 to 65536 on each axis and no device path, and no other; most
  likely the console splitter's with no touch driver under it (an inference: nothing
  was read from it). So the Note Air has no tap source today and the menu doesn't open
  there (Scenario 5) until a touch driver of the loader's own exists; its axes then;
- which keys reach the console-in handle (the Note Air has only the power button; the
  `KEY` lines of a run show what it sends);
- the watchdog re-arm on this firmware.

A hang inside the menu is a counted start that leaves `drew=1`: the next start draws
nothing and sets `display_failed` (003 FR-010).

## Out of scope

A touch driver for the panel's controller (separate work, a tap source behind FR-010);
keys driving the menu; long press, drag and scrolling; the firmware's set-up; an Android entry; any change to
the parent; drawing after the answer.

## Success criteria

- **SC-001**: The QEMU cases of Scenarios 1 to 7 pass on x86-64 and aarch64, and with
  the second OVMF; the frame CRCs are the same on all three.
- **SC-002**: The cases of 001, 002 and 003 are unchanged, except that a start that drew
  writes once more (FR-007).
- **SC-003**: Host tests cover the menu's open rule and writes, each pick's answer and
  write, the rectangle mapping and alignment, the touch mapping and the debounce.
- **SC-004**: The loader image stays below the loader region's 3 MiB less 4096 bytes.
