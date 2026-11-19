# The Note Air firmware's I2C and GPIO protocols

The Boox Note Air's UEFI firmware installs two protocols its own display driver uses to
reach the panel's power chips. The loader's touch driver (spec 005) uses the same two to
reach the touch controller. This page describes their interfaces in this project's own
words and types, as far as the loader needs them. It is derived from how the firmware's
display driver calls them; nothing here is the firmware's code or data. **Unverified on
the device** until the runs listed in spec 005 have been made.

Both are called with the platform's C calling convention (`efiapi` on arm64).

## The I2C protocol

GUID `b27ae8b1-3e10-4d07-ab5c-eb9a6dc6fa8f`, one handle. The interface is six 64-bit
words:

| Offset | Word | Call |
|---|---|---|
| +0x00 | revision, `0x10000` | — |
| +0x08 | open | `open(instance: u32, handle: *mut *mut c_void) -> u32` |
| +0x10 | read | `read(handle, cfg: *const BusCfg, offset: u16, offset_len: u16, buf: *mut u8, len: u16, done: *mut u32, timeout: u32) -> u32` |
| +0x18 | write | `write(handle, cfg: *const BusCfg, offset: u16, offset_len: u16, buf: *const u8, len: u16, done: *mut u32, timeout: u32) -> u32` |
| +0x20 | transfer | several segments in one transaction (not used by the loader) |
| +0x28 | close | `close(handle) -> u32` |

- **Status:** a 32-bit value, `0` for success. Seen: `1` an argument out of range (an
  instance above 11, a NULL pointer, a zero length or timeout), `2` the bus can't be
  opened, `4` the bus can't be powered; others for bus errors.
- **Instance:** the bus, 0 to 11. 0–3 are the buses at `0xc175000`, `0xc176000`,
  `0xc177000`, `0xc178000`; 4–7 those at `0xc1b5000` … `0xc1b8000`. The touch
  controller's bus is **3** (`0xc178000`).
- **open** powers the bus (its clock and its two pins, set by the firmware); **close**
  releases it. So no call of this protocol is read-only.
- **`BusCfg`**, 16 bytes, zeroed first: `bus_khz: u32` (the clock in kHz, 100 for the
  touch controller), `address: u32` (the 7-bit device address), `address_kind: u32`
  (0: 7-bit), `reserved: u32` (0).
- **read** is one transaction: it writes the offset, then reads `len` bytes. **write**
  writes the offset, then `buf` (`len` at least 1). With `offset_len` 1 only the
  offset's low byte is sent; with 2, its high byte first. `done` receives the bytes
  moved. `timeout` is most likely in milliseconds (the firmware's display driver passes
  2500).

A device whose register address is two bytes, low byte first (the cyttsp5), is written
with `offset_len` 1, the register's low byte as `offset`, and the high byte as the first
byte of `buf`.

## The GPIO protocol

GUID `ad9aec18-7bf0-4809-9e96-3012309f3df7`, one handle. The interface is eight 64-bit
words:

| Offset | Word | Call |
|---|---|---|
| +0x00 | revision, `1` | — |
| +0x08 | configure | `configure(cfg: u32, enable: u32) -> EFI_STATUS`: with `enable` 1, sets the pin as `cfg` says |
| +0x10 | configure several | not used by the loader |
| +0x18 | read pin | `read(cfg: u32, level: *mut u32) -> EFI_STATUS`: the pin's level, 0 or 1; configures nothing |
| +0x20 | set pin | `set(cfg: u32, level: u32) -> EFI_STATUS`: drives an output high (non-zero) or low |
| +0x28 … +0x38 | not used by the loader | |

- **Status:** `EFI_SUCCESS`, or `EFI_UNSUPPORTED` when the call fails (also for a GPIO
  number the controller doesn't have).
- **`cfg`**, the pin word: bits 4–13 the GPIO number; bits 0–3 the function (0: plain
  GPIO); bit 14 the direction (1: output); bits 15–16 the pull (0: none); bits 17–20 the
  drive strength (0: the lowest); bit 29 set (it marks this layout). An output is
  `0x2000_4000 | n << 4`, an input `0x2000_0000 | n << 4`. Other pull and drive values
  are not described here and the loader doesn't use them.
- The only read-only call is **read pin**. Whether an output reads back the level it
  drives is not known.

## The PMIC regulator protocol

GUID `22d38d3d-e8b6-4f8f-9c26-bceb07d6cb68`, installed by the firmware's PMIC driver;
its display driver uses it for its own supplies. The interface is eight 64-bit words
(the layout matches the one an open edk2 port for Qualcomm phones publishes; only the
GUID and the call layout are used here, in this project's words):

| Offset | Word | Call |
|---|---|---|
| +0x00 | revision, `0x10005` | — |
| +0x08 | enable | `enable(pmic: u32, id: u32, on: u8) -> EFI_STATUS`: switches the regulator on (1) or off (0) |
| +0x10 | set level | `set_level(pmic: u32, id: u32, mv: u32) -> EFI_STATUS`: its output in mV. **Never called by the loader:** on the Note Air it hung inside the firmware for pm660l LDO3 (the SoC went to its crash-dump mode) |
| +0x18 | get level | `get_level(pmic: u32, id: u32, level: *mut u32) -> EFI_STATUS`: its set output |
| +0x20 | switching mode | not used by the loader |
| +0x28 | phases | not used by the loader (returns `EFI_UNSUPPORTED` on the Note Air) |
| +0x30 | get status | `get_status(pmic: u32, id: u32, status: *mut Status) -> EFI_STATUS` |
| +0x38 | set level in µV | not used by the loader |

- **`pmic`**: the PMIC's index. The firmware's display driver names its regulators
  `LDO<n>A` with index 0 (pm660) and `LDO<n>B` with index 1 (pm660l).
- **`id`**: the regulator: LDO *n* is `n − 1` (LDO1 … LDO33 are 0 … 32; the switching
  regulators and others follow and aren't used).
- **get level** gives µV on the Note Air (pm660l LDO3 read `3008000`); the loader takes
  a value below 100 000 as mV.
- The firmware's display driver locates the protocol but makes no call to it, so no
  firmware precedent exists for a write. On the device **both writes to pm660l LDO3
  ended in the SoC's crash-dump mode**: set level, then the enable call alone. LDO3
  belongs to the RPM, and the firmware's RPM-side resources don't include it. The
  loader switches it through the RPM instead (`docs/firmware-rpm.md`, spec 005
  `touch_rpm`); this protocol is then used only to read it back.
- **`Status`**, 20 bytes: four `u32` (pull-down on, the switching mode, pin control on,
  software enable on: 1 = on), then one byte, 1 when the regulator reports its output
  good. The loader passes a zeroed 32-byte buffer.
- The read-only calls are **get level** and **get status**.

## The touch controller's pins

| GPIO | Use |
|---|---|
| 61 | the 3.3 V supply switch, high = on |
| 62 | the 1.8 V supply switch, high = on |
| 66 | reset, low = in reset |
| 67 | interrupt, low while a report waits |
