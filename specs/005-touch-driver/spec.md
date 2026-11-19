# Feature Specification: A touch driver of the loader's own for the Note Air

**Feature Branch**: `005-touch-driver`
**Created**: 2026-11-16
**Status**: Draft
**Input**: "Give the loader its own touch driver for the Boox Note Air's touch controller.
It talks to the controller through the firmware's own I2C and GPIO protocols, so the
loader's tap menu works on the device."

## Context

GlassineOS/boot owns the contract: `docs/handback.md`, "The loader's touch driver", the
key `touch` ("The loader record under a parent") and the probe report's `i2c`, `gpio`,
`gpio.pin<n>` and `touch.*` lines ("The probe report"), at the commit spec 002 pins.
Where they and this spec differ, they win and this spec is corrected.

The Note Air's firmware binds no touch driver: a probe run listed only the console
splitter's absolute pointer, so the start menu (spec 004) has no tap source there and
logs `MENU_NO_INPUT`. The firmware does install the two protocols its own display driver
uses: an I2C protocol (`b27ae8b1-3e10-4d07-ab5c-eb9a6dc6fa8f`) and a GPIO protocol
(`ad9aec18-7bf0-4809-9e96-3012309f3df7`), one handle each.
[`docs/firmware-i2c-gpio.md`](../../docs/firmware-i2c-gpio.md) describes their
interfaces in this project's own words.

The controller is a Parade/Cypress cyttsp5 (the protocol of Linux's
`drivers/input/touchscreen/cyttsp5.c`): I2C address 0x24 at 100 kHz on the bus at
`0xc178000` (the firmware's I2C instance 3; the stock and mainline device trees agree;
an older note naming `0xc1b5000` is wrong), GPIO 61 its 3.3 V switch, 62 its 1.8 V
switch, 66 its reset (active low), 67 its interrupt (level, low); coordinates 0–1871 ×
0–1403, landscape.

## User scenarios

### Scenario 1 – What the firmware offers, read only (Priority: P1)

With `probe=1` the report names both protocols, their interface words, which loaded
image each function word points into, and whether they pass the loader's checks; after
the GPIO protocol passes, the levels of the four touch pins from its pin read. Nothing
is configured: no bus opened, no pin driven.

**Acceptance**: QEMU under the stand-in parent: without the stand-in protocols the
report says `i2c=none` and `gpio=none`; with them (`parent.faketouch=1`) it gives the
words, `check=ok` and the four pin levels, and the stand-in counts no call but the pin
reads (no open, no configure, no set).

### Scenario 2 – One bring-up, reported (Priority: P1)

The person sets `touch=probe`. At the next start the loader clears the key in its own
write, then powers the controller, resets it, reads its HID descriptor and system
information, writes them into the probe report, powers the controller down and answers
as without the key.

**Acceptance**: QEMU with the stand-in protocols and a scripted cyttsp5: the report
holds the descriptor and the system information; the stand-in's GPIO log shows the
power-up order, then the power-down; the record after the start has `touch` empty and
the count and answer of a start without it.

### Scenario 3 – A bring-up that fails or hangs (Priority: P1)

Any step can fail: no protocol, a check that fails, the record write, the bus open, no
answer from the controller, a wrong descriptor. The bring-up ends there, logged, the
controller powered down when the loader powered it, and the start goes on. A firmware
call that never returns is a watchdog reset; the next start finds `touch` already
cleared and doesn't try again.

**Acceptance**: QEMU cases with the stand-in failing at each step; with a stand-in
whose I2C read never returns: the machine resets, the next start makes no I2C or GPIO
call and answers.

### Scenario 4 – Taps in the start menu (Priority: P1)

With `touch=on` and a start that opens the menu (004), the loader's driver is the tap
source: brought up after the menu write, polled while the menu is open, powered down
when it closes. A tap at an entry picks it.

**Acceptance**: QEMU with `parent.faketouch=1` and a script of touch reports: a tap at
each entry's centre gives that entry's answer and record; "More" and "Back" work; the
controller is powered down before the answer.

## Requirements

### The protocols (all stages)

- **FR-001**: A protocol is used only after its checks pass: its interface's words are
  RAM (the memory map), the first word is the revision this loader knows (I2C `0x10000`,
  GPIO `1`) and every function word points into one and the same loaded image. Else the
  driver doesn't exist at this start, logged.
- **FR-002**: The pin configuration the loader uses is only the plain GPIO function,
  no pull, the lowest drive, as an output or an input; no other GPIO than 61, 62, 66, 67
  is configured, read or set.

### Identification (`probe=1`)

- **FR-003**: The report's `i2c`, `i2c.words`, `i2c.in`, `i2c.check`, `gpio`,
  `gpio.words`, `gpio.in`, `gpio.check` lines are read from memory; `gpio.pin61`,
  `gpio.pin62`, `gpio.pin66`, `gpio.pin67` come from the pin read, called only after
  `gpio.check=ok`. No other function of either protocol is called by the probe. The
  PMIC regulator protocol (`docs/firmware-i2c-gpio.md`) gets the same lines from memory
  (`vreg`, `vreg.words`: eight words, `vreg.in`, `vreg.check`: revision `0x10005`); none
  of its functions is called by the probe.

### The bring-up (`touch=probe`)

- **FR-004**: Before its first GPIO or I2C call the loader writes the record with `touch`
  cleared; when the write fails the bring-up doesn't run (`TOUCH_SKIPPED not-written`).
- **FR-005**: The sequence: read the four pins (the state found; `touch.found`); read the
  supplies, read only (FR-017; `touch.rail.l3b`, `touch.rail.l11a`); with LDO3 read off,
  the RPM vote (FR-018); when
  61 and 62 don't both read high, power up: 61 high, 5 ms, 62 high, 5 ms (each pin: its
  level set, configured as an output, its level set again); reset low, at least 1 ms,
  high, 20 ms; 67 configured as an input; open instance 3 (`touch.open`); wait up to
  500 ms for the reset sentinel (length 0); the bootloader's launch-app command, then
  the application's sentinel: a size-0 frame at least 30 ms after the write (frames
  before that, and the bootloader's answer, are read and dropped), up to 1 s (200 ms
  while the interrupt has never asserted), then the frames still waiting drained (read
  while the interrupt is asserted, until it isn't or an empty read); none of it fatal
  (`touch.launch`: `app sentinel <ms> ms drained=<n>`, or `no sentinel`); the HID
  descriptor (register 0x0001; 32 bytes, version 0x0100, `touch.hid`, `touch.desc`),
  sent once more when the interrupt doesn't assert within 500 ms (`touch.hid.resent`),
  up to 3 s in all, whose packet id gives the mode (`touch.mode`: 0xF7 `app`, 0xFF
  `bl`; in `bl` launch-app again, `touch.launch.bl`, and the descriptor again,
  `touch.mode.bl`); no report descriptor read (Linux's cyttsp5 doesn't put one on the
  wire; the code is kept, off); `GET_SYSINFO` sent no sooner than 100 ms after the
  drain (Linux's first send, before the application's sentinels, is lost), or at once
  after the descriptor when no application sentinel came (the controller's start is
  then spent in the descriptor's wait: on the device the descriptor answers about
  1 s after launch-app, as in Linux's trace, and a pause after it only adds), and waited
  for up to 3 s, as Linux does (the application answers about 750 ms later), sent
  again after 100 ms when no answer or one that isn't the system information (one
  shorter than 49 bytes) came, at most 3 sends (`touch.sysinfo`, `touch.res`); close
  the bus. Each wait reads the interrupt (67) every 5 ms and the input register only
  while it is asserted (low), as Linux does; only while the interrupt has never been
  seen asserted in the whole bring-up is the register also read every 50 ms. A read
  that fails counts as nothing yet (a controller busy starting up doesn't answer its
  address). Each wait is logged `TOUCH_WAIT <what> <ms>` with how often the interrupt
  was asserted and the register read and reported (`touch.wait.reset`, `.app`, `.hid`,
  `.hid.more`, `.sysinfo`). A write the controller doesn't take is sent again every
  10 ms for up to 300 ms.
- **FR-006**: The wire bytes are mainline's: a command is written as one write whose
  first byte is the register's low byte and whose data starts with its high byte; a
  report is read as a read of register 0x03 (one byte), first two bytes (the length),
  then the length. A length of 0 or 2 is no report. The interrupt pin's level is logged
  as a hint only.
- **FR-007**: One deadline: 10 s from the first GPIO call, the firmware watchdog
  re-armed (30 s) just before it. Every wait and retry ends at
  it and each I2C call's timeout is at most 100 ms and never past it. Polls every 5 ms.
- **FR-008**: Power down when the loader powered the controller: reset low, 62 low, 61
  low (`touch.down`); then the RPM vote taken back (FR-018); then
  66, 62 and 61 configured as inputs (`touch.release`: the firmware's protocol can't read
  a pin's configuration to restore the one found, and inputs leave the pins to the
  next owner); also after a failure. Found powered: the power pins are left alone, reset ends
  high.
- **FR-009**: Every GPIO and I2C call is logged with its arguments and status (`TOUCH_GPIO`,
  `TOUCH_I2C`), except the waits' interrupt reads and the input-register reads that
  find nothing waiting (`02 00`): those are counted, in each `TOUCH_WAIT` line and in one
  `TOUCH_QUIET` line after the bring-up, so the log keeps the rest. The result is one
  line `touch.result=ok` or `touch.result=failed <step>`.
- **FR-010**: The probe report is written at a `touch=probe` start (with or without
  `probe=1`), after the bring-up.

### The tap source (`touch=on`)

- **FR-011**: With `touch=on` and both protocols passing FR-001, the driver is the
  start menu's tap source (004 FR-010), offered before the menu write without touching
  hardware; its bring-up (FR-005 without the report, FR-007, FR-008 rules) runs at its
  first poll, which is after the menu write.
- **FR-012**: A poll reads one report (FR-006) and gives the touch as pressed when it
  holds a contact whose event is not lift-off, at the first such contact's position, and
  released when it holds none; nothing when no report is pending. Polls never wait.
- **FR-013**: The mapping: the controller's X along the panel's native x, its Y along
  native y flipped, in one constant (unverified on hardware; the lab's desktop
  calibration gives it); ranges from the system information's resolution, else 1872 ×
  1404; then to portrait by 004's mapping.
- **FR-014**: The source logs presses and releases (raw and portrait), not polls. A
  failed bring-up or a failed read ends the source (no more taps), logged.
- **FR-015**: When the menu closes the controller is powered down (FR-008).
- **FR-016**: While the menu is open the source counts its polls, the polls with the
  interrupt asserted, the interrupt's changes (the first 16 logged with the poll and the
  time), the empty reads, the sentinels (the first 4 logged), the frames by report id
  and the read errors; the first 4 frames that aren't touch reports are logged raw. At
  the menu's close one line sums them up (`TOUCH_MENU polls= irq_low= irq_changes=
  empty= sentinels= frames= ids=<id>:<n>,... errors=`), then the power-down's lines
  (`TOUCH_DOWN <key>=<value>`).

### The supplies

- **FR-017**: At every bring-up the loader reads, through the PMIC regulator protocol
  when it passes its checks (else `error none`), the two supplies: status and level,
  nothing written (`touch.rail.l3b`, `touch.rail.l11a`: `on|off <mV> mV` or
  `error <status>`). **The loader never writes the PMIC regulator protocol**: on the
  device both writes to pm660l LDO3, set level and then the enable alone, ended in the
  SoC's crash-dump mode. LDO3 is switched through the RPM (FR-018); pm660 LDO11 is
  only read. Every regulator call is logged (`TOUCH_PMIC`).

### The RPM vote (`touch_rpm`)

- **FR-018**: The RPM vote is the power path: at every bring-up (`touch=probe`, or the
  tap source's with `touch=on`, also the menu's after a failed start) where pm660l LDO3
  reads off, the loader runs the stages below up to `vote`. They run after the supplies
  are read (FR-017) and before the power switches, through the firmware's RPM protocols
  (`docs/firmware-rpm.md`). Each stage includes the ones before it. Every call is logged
  before and after (`TOUCH_RPM`). LDO3 read on: no RPM call (`touch.rpm=skipped l3b on
  <mV> mV`); not readable: none either (`skipped l3b unreadable`).
  - The record's `touch_rpm` is a diagnostic for a device run: `locate`, `install` or
    `kvp` runs the stages up to that one whatever LDO3 reads, and no vote; `off` makes
    no RPM call (`touch.rpm=off`); empty, `vote` or an unknown value (logged) is the
    power path. The key is cleared before the first RPM call: in `touch=probe`'s
    clearing write (FR-004), or in the menu write for the tap source. If the write
    fails, no RPM call is made.
  - A hang inside the vote is a watchdog reset that doesn't repeat at the next start:
    `touch=probe` is cleared before it (FR-004); for the tap source the menu write
    holds `touch_voting=1` and the pick's write clears it. A menu that finds
    `touch_voting=1` at entry makes no RPM call (`TOUCH_RPM touch_voting=1`) and
    clears it, so at worst every other menu votes.

  | Stage | What it does | `touch.rpm.*` |
  |---|---|---|
  | `locate` | The NPA protocol, the RPM services and the raw request protocol looked up and checked (revision, calls inside one image); no call | `found=npa=… svc=… raw=…` (`ok`, `absent`, `bad:<why>`) |
  | `install` | The raw protocol, when absent, installed by the services (and looked up and checked again); when found installed, used and left | `install=ours ok` / `found installed` / `ours failed …` |
  | `kvp` | A KVP set created, cleared, `swen`=1 put in, destroyed; no message | `kvp=ok` / `failed …` |
  | `vote` | With pm660l LDO3 off: the watchdog re-armed (30 s), one active-set request to `ldob` id 3 with only `swen`=1 (no level, no load), churned; after 5 ms LDO3 read back through the PMIC protocol's read-only calls. The bring-up goes on to the power switches only when LDO3 reads on and in 2.7–3.6 V; else it fails (`rpm: …`) | `vote=id=<n> l3b on <mV> mV` |

  - Below `vote`, a stage that can't run stops the stages (`touch.rpm.stopped`), and
    the bring-up goes on as without the key. At `vote` it fails the bring-up before any
    pin is driven.
  - At the power-down (FR-008), after the switches are low and before the pins are
    released, the vote is taken back. This happens whenever a request was made, also
    after a failed bring-up: the watchdog re-armed, `swen`=0, LDO3 read back
    (`touch.rpm.off`). Then a raw protocol the loader installed is uninstalled
    (`touch.rpm.uninstall`).
  - The services' own request (their first call) and the NPA protocol's calls are
    never made.

## Device scope and risk

Unverified on hardware, each checked by a device run with the person's go:

- whether `open(3)` succeeds after the firmware's start, and the timeout's unit
  (milliseconds, from how the firmware passes it on);
- whether the 3.3 V and 1.8 V regulators behind the switches are on when the loader
  runs (a device run: the bring-up works, the panel never reports a touch;
  FR-017 reads them: pm660l LDO3 off at 3.008 V, LDO11 on); whether the enable call
  is safe from the loader (it wasn't: on the device the set-level call and then the
  enable call alone both ended in 900E; LDO3 belongs to the RPM); the PMIC indexes are
  the display driver's (A = 0, B = 1);
- whether the raw RPM protocol is absent at the loader's start and installs cleanly;
  whether the RPM acknowledges the `swen` vote and LDO3 reads back on at 3.008 V;
  whether the firmware watchdog ends a churn that never returns (FR-018);
- whether the pins configured as inputs after the power-down hold the switches off;
- whether an output pin reads back its driven level;
- whether the bootloader answers launch-app and how long the application takes to
  start (the first device run: the reset sentinel and the bus worked; no answer within
  77 ms, then the controller stopped answering its address);
- the controller's timing after reset within the deadline;
- the touch axes relative to the loader's drawing.

## Out of scope

Interrupts; multi-touch, gestures, the pen; any PMIC call but FR-017's, any RPM
request but FR-018's, any clock call; touch on a PC.

## Success criteria

- **SC-001**: QEMU cases of Scenarios 1–4 on aarch64 and x86-64 (the stand-in protocols
  are installed by the parent on both).
- **SC-002**: Host tests: the report parser, the mapping, the command bytes.
- **SC-003**: The PC path and the cases of 001–004 are unchanged.
- **SC-004**: The loader image stays below the loader region's 3 MiB less 4096 bytes.
