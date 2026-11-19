# The Note Air firmware's RPM request protocols

On the Note Air (SDM636), the regulator behind the touch controller's 3.3 V switch,
pm660l LDO3, belongs to the RPM, the SoC's resource power manager. An operating system
switches it by sending the RPM a request; it doesn't write the PMIC itself. Linux does
this through `qcom_smd-regulator` over the RPM's glink channel `rpm_requests`. The
firmware has its own client for that channel and installs protocols on top of it. The
loader's touch driver (spec 005, `touch_rpm`) uses them to switch LDO3 on as Linux does.

This page describes those protocols in this project's own words and types, as far as the
loader needs them. It is derived from how the firmware's own drivers call them. Nothing
here is the firmware's code or data, and nothing comes from other parties' headers: no
public header with a licence this project can use describes these protocols.
**Unverified on the device** until the runs listed in spec 005 have been made.

All calls use the platform's C calling convention (`efiapi` on arm64) and return an
`EFI_STATUS`.

## The NPA protocol

GUID `79d6c870-725e-489e-a0a1-27e7a5d0cb35`, revision `0x10002` in its first word,
followed by 52 calls. It is the firmware's power-resource framework: its display, clock
and USB drivers open clients on named resources and issue requests. It doesn't name
pm660l LDO3, so the loader never calls it. It only looks it up (`touch_rpm=locate`) as a
sign that the firmware's RPM side is present.

## The RPM services

GUID `276732ab-bb39-4dce-b5d9-2746cdd6d518`, installed together with the NPA protocol.
The interface is four 64-bit words:

| Offset | Word | Call |
|---|---|---|
| +0x00 | revision, `0x10002` | — |
| +0x08 | a request of the firmware's own | **never called by the loader** |
| +0x10 | install | `install() -> EFI_STATUS`: installs the raw request protocol (below) on a handle of its own |
| +0x18 | uninstall | `uninstall() -> EFI_STATUS`: removes it again |

The raw request protocol is **not installed** at the firmware's start. No firmware
driver and not the Note Air's boot loader installs it. The loader installs it only when
it is absent, and uninstalls it at its power-down. If it found it installed, it leaves
it.

## The raw RPM request protocol

GUID `1bbb8f8e-4a4e-44a0-acf7-35fd849fd900`, revision `0x10001`. The interface is seven
64-bit words:

| Offset | Word | Call |
|---|---|---|
| +0x00 | revision, `0x10001` | — |
| +0x08 | destroy | `destroy(kvp: *mut Kvp) -> EFI_STATUS`: frees a KVP set |
| +0x10 | put | `put(kvp, key: u32, len: u32, value: *const c_void) -> EFI_STATUS`: appends one key-value pair (`len` below 100) |
| +0x18 | clear | `clear(kvp) -> EFI_STATUS`: empties the set |
| +0x20 | create | `create(initial_size: u32, kvp: *mut *mut Kvp) -> EFI_STATUS`: a new, empty set |
| +0x28 | post | `post(set: u32, resource_type: u32, id: u32, kvp, msg_id: *mut u32) -> EFI_STATUS`: queues one request |
| +0x30 | churn | `churn(msg_id: u32) -> EFI_STATUS`: waits until the RPM has acknowledged that message |

- **`Kvp`** is opaque. A pair goes on the wire as the key, the length, then the value
  padded to four bytes. This is the same layout as Linux's `rpm_regulator_req`.
- **`set`**: 0 the active set, 1 the sleep set, 2 the next active set. Any other value
  sends nothing and gives message id 0.
- **`resource_type`** is the RPM's four-letter type as a little-endian word, the same
  value as Linux's constants. pm660l's LDOs are `ldob` = `0x626f646c`.
- **`id`** is the resource number. LDO3 is 3, as in Linux's `rpm_pm660l_regulators`.
- **`msg_id`** receives the message's id when the pointer isn't NULL. 0 means nothing
  was sent; the loader then doesn't churn.
- **Every call returns `EFI_SUCCESS`.** The result of a request shows only in the
  message id and in its effect. The loader therefore reads LDO3 back through the PMIC
  regulator protocol's read-only calls (`docs/firmware-i2c-gpio.md`) after every vote.
- **`churn` has no timeout.** If the RPM never acknowledges, it never returns. The
  loader re-arms the firmware watchdog (30 s) before each request. The firmware's
  watchdog is the timer-driven kind (it resets the machine from its timer handler), and
  the firmware's RPM client takes no lock that would hold timer interrupts off, so a
  hung churn should end in a reset. Unproven on the device.
- **When the link to the RPM is down**, the firmware completes messages without sending
  them. A request then "succeeds" and nothing changes, so the read-back fails and the
  loader goes no further.
- The firmware closes the RPM channel and removes these protocols at
  `ExitBootServices`. Before that, while the loader runs, they are live (by inference:
  the firmware's display and clock drivers use the same channel before the loader).

The firmware's own requests follow this order, and so does the loader's:

1. `create(0)`, then `clear`.
2. `put`.
3. `post`.
4. `churn(msg_id)`.
5. `destroy`.

## The loader's one request

pm660l LDO3 on: the active set, `ldob`, id 3, one pair `swen` (`0x6e657773`), 4 bytes,
value 1. To take the vote back: the same with value 0. The request sets no level (`uv`)
and no load (`ma`). The loader relies on the level the firmware already set
(3.008 V), checked on the read-back against the controller's range, 2.7–3.6 V.
