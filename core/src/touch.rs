// SPDX-License-Identifier: GPL-2.0-or-later
//! The touch controller of the Note Air, a Parade/Cypress cyttsp5 (loader spec 005):
//! its bring-up and power-down sequences, its frames and commands, over a [`Hw`] the
//! loader gives (the firmware's GPIO and I2C protocols) and the host tests fake. The
//! wire bytes are those of Linux's `drivers/input/touchscreen/cyttsp5.c`.
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The controller's pins: the 3.3 V switch, the 1.8 V switch, reset (low = in reset),
/// interrupt (low while a report waits).
pub const PIN_VDD: u32 = 61;
pub const PIN_VDDIO: u32 = 62;
pub const PIN_RESET: u32 = 66;
pub const PIN_IRQ: u32 = 67;
pub const PINS: [u32; 4] = [PIN_VDD, PIN_VDDIO, PIN_RESET, PIN_IRQ];

/// The controller on the bus.
pub const I2C_INSTANCE: u32 = 3;
pub const I2C_ADDRESS: u32 = 0x24;
pub const I2C_KHZ: u32 = 100;

/// The whole bring-up's time (FR-007), the longest single I2C call, the poll period.
pub const DEADLINE_MS: u64 = 10_000;
pub const CALL_MS: u64 = 100;
pub const POLL_MS: u64 = 5;
/// The waits after a reset: the reset sentinel; the application's sentinel after
/// launch-app (frames within [`APP_EARLY_MS`] of the write are not it), at most
/// [`APP_WAIT_MS`], or [`APP_WAIT_NO_IRQ_MS`] while the interrupt has never been seen
/// asserted; how long a write the controller doesn't take is sent again.
pub const RESET_WAIT_MS: u64 = 500;
pub const APP_EARLY_MS: u64 = 30;
pub const APP_WAIT_MS: u64 = 1000;
pub const APP_WAIT_NO_IRQ_MS: u64 = 200;
pub const WRITE_RETRY_MS: u64 = 300;
/// The HID descriptor's request is sent once more when the interrupt doesn't assert
/// within this long.
pub const HID_RESEND_MS: u64 = 500;
/// While the interrupt has never been seen asserted in the bring-up, the input register
/// is read this often anyway (an interrupt line that never moves).
pub const FALLBACK_MS: u64 = 50;
/// The report descriptor read before `GET_SYSINFO` (HID over I2C does it; Linux's
/// cyttsp5 doesn't put it on the wire). Off.
pub const READ_REPORT_DESC: bool = false;
/// How long one `GET_SYSINFO` is waited for (Linux's `CY_HID_OUTPUT_GET_SYSINFO_TIMEOUT_MS`;
/// the controller's application can take seconds after power-up), the pause before it
/// is sent again, how often it is sent at most.
pub const SYSINFO_WAIT_MS: u64 = 3000;
pub const SYSINFO_PAUSE_MS: u64 = 100;
pub const SYSINFO_SENDS: u32 = 3;
/// The HID descriptor's packet id: the controller's application, its bootloader.
pub const PACKET_APP: u8 = 0xf7;
pub const PACKET_BL: u8 = 0xff;

/// How the controller's axes lie on the display panel's native axes (spec 005 FR-013):
/// its X along the native x, its Y along the native y flipped. **Unverified on
/// hardware**: the lab's desktop calibration of the same controller gives it; the one
/// place to change after the first tap test.
pub const ORIENT: crate::menu::Orient = crate::menu::Orient { swap: false, flip_x: false, flip_y: true };

/// The controller's resolution without its system information.
pub const RES: (u16, u16) = (1872, 1404);

/// The registers (two bytes on the wire, low first).
pub const REG_HID_DESC: u16 = 0x0001;
pub const REG_INPUT: u8 = 0x03;
pub const REG_OUTPUT: u16 = 0x0004;

/// The largest frame the loader reads.
pub const MAX_FRAME: usize = 512;
/// The HID descriptor's length and version.
pub const HID_DESC_LEN: usize = 32;
pub const HID_VERSION: u16 = 0x0100;
/// The system information's report id and command, its shortest length, where its
/// sensing configuration starts.
const SYSINFO_REPORT: u8 = 0x1f;
const SYSINFO_CMD: u8 = 0x02;
const SYSINFO_MIN: usize = 49;
const SENSING: usize = 33;

/// What the loader's driver needs of the firmware. Each call is logged by the
/// implementation; an `Err` is the call's status as text.
pub trait Hw {
    fn now_ms(&mut self) -> u64;
    fn stall_ms(&mut self, ms: u64);
    /// The pin's level (read only).
    fn pin_read(&mut self, pin: u32) -> Result<bool, String>;
    /// The pin as an output at `high`: the level latched, the pin configured, the level
    /// set again.
    fn pin_out(&mut self, pin: u32, high: bool) -> Result<(), String>;
    /// The pin as an input.
    fn pin_in(&mut self, pin: u32) -> Result<(), String>;
    /// Opens the controller's bus.
    fn open(&mut self) -> Result<(), String>;
    /// Reads `buf.len()` bytes of the input register.
    fn read_input(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<(), String>;
    /// Writes `data` to register `reg` (on the wire: its low byte, its high byte, `data`).
    fn write(&mut self, reg: u16, data: &[u8], timeout_ms: u32) -> Result<(), String>;
    fn close(&mut self);
    /// One line for the loader's log (`TOUCH_WAIT ...`).
    fn log(&mut self, _line: &str) {}
    /// Whether the interrupt (67, active low) is asserted: a frame waits. Read during
    /// the waits; the loader reads it without logging each read.
    fn irq_asserted(&mut self) -> Result<bool, String> {
        self.pin_read(PIN_IRQ).map(|high| !high)
    }
    /// Writes the two-byte register `reg` (low byte first) and reads `buf.len()` bytes
    /// back in the same transfer.
    fn read_reg(&mut self, reg: u16, buf: &mut [u8], timeout_ms: u32) -> Result<(), String>;
    /// A regulator of PMIC `pmic` (LDO `ldo`, numbered from 1): whether it is enabled
    /// and its level in mV (read only). Without the firmware's regulator protocol, an
    /// error.
    fn rail_get(&mut self, _pmic: u32, _ldo: u32) -> Result<(bool, u32), String> {
        Err("none".into())
    }
    /// Enables or disables it.
    fn rail_enable(&mut self, _pmic: u32, _ldo: u32, _on: bool) -> Result<(), String> {
        Err("none".into())
    }
    /// `touch_rpm` (stage `locate`): the firmware's three RPM protocols looked up and
    /// checked, read only.
    fn rpm_locate(&mut self) -> RpmFound {
        RpmFound::default()
    }
    /// The RPM services' install (`true`) or uninstall (`false`) of the raw RPM request
    /// protocol; after an install, the raw protocol looked up and checked again.
    fn rpm_install(&mut self, _on: bool) -> Result<(), String> {
        Err("none".into())
    }
    /// A KVP set created, `swen`=1 put in, destroyed: no message.
    fn rpm_kvp(&mut self) -> Result<(), String> {
        Err("none".into())
    }
    /// One active-set request to pm660l LDO3 ([`RPM_LDOB`], id [`RPM_L3B_ID`]) with
    /// only `swen` = `swen`, then waited for until the RPM acknowledges it (the
    /// firmware's wait has no timeout). The message id.
    fn rpm_vote(&mut self, _swen: u32) -> Result<u32, String> {
        Err("none".into())
    }
    /// The firmware watchdog re-armed for its full period.
    fn watchdog(&mut self) {}
}

/// `touch_rpm=<stage>`: how far the driver goes with the firmware's RPM request
/// protocols (each stage includes the ones before it). Opt-in; without the key none of
/// them is looked up.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum RpmStage {
    /// The three protocols looked up and checked (no call).
    Locate,
    /// The raw request protocol installed by the RPM services when absent (no message).
    Install,
    /// A KVP set created, filled and destroyed (no message).
    Kvp,
    /// pm660l LDO3 switched on by an RPM vote (`swen`=1, no level) before the power
    /// switches, and the vote taken back (`swen`=0) at the power-down.
    Vote,
}

impl RpmStage {
    pub fn parse(v: &str) -> Option<RpmStage> {
        Some(match v {
            "locate" => RpmStage::Locate,
            "install" => RpmStage::Install,
            "kvp" => RpmStage::Kvp,
            "vote" => RpmStage::Vote,
            _ => return None,
        })
    }
}

/// One of the RPM protocols as looked up.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Proto {
    #[default]
    Absent,
    Ok,
    /// Found, but a check failed (why).
    Bad(String),
}

impl Proto {
    fn text(&self) -> String {
        match self {
            Proto::Absent => "absent".into(),
            Proto::Ok => "ok".into(),
            Proto::Bad(w) => format!("bad:{w}"),
        }
    }
}

/// The NPA protocol, the RPM services, the raw RPM request protocol.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RpmFound {
    pub npa: Proto,
    pub svc: Proto,
    pub raw: Proto,
}

/// The RPM's resource type of pm660l's LDOs ("ldob" as a little-endian word) and LDO3's
/// id: Linux's `QCOM_SMD_RPM_LDOB` and `rpm_pm660l_regulators` "l3".
pub const RPM_LDOB: u32 = 0x626f_646c;
pub const RPM_L3B_ID: u32 = 3;
/// The software-enable key, "swen".
pub const RPM_KEY_SWEN: u32 = 0x6e65_7773;
/// After the vote is acknowledged, before LDO3 is read back (Linux: a 250 µs ramp).
pub const RPM_SETTLE_MS: u64 = 5;

/// A supply behind the controller's power switches (`touch_rail=on` enables it when
/// off): its name, PMIC index and LDO, and the range its set level must already be in
/// (the controller's supply range). The loader never sets a level: the firmware's
/// set-level call hung on the device (900E), so only the enable call is made, and only
/// on a level already in range.
pub struct Rail {
    pub name: &'static str,
    pub pmic: u32,
    pub ldo: u32,
    pub expect: (u32, u32),
}

/// pm660l LDO3 (behind 61; the controller's VDD, 2.7–3.6 V; Linux and the stock kernel
/// run it at 3.0 V) and pm660 LDO11 (behind 62; 1.8 V I/O); PMIC index 0 is pm660
/// ("A"), 1 pm660l ("B"), as the firmware's display driver names them.
pub const RAILS: [Rail; 2] = [
    Rail { name: "l3b", pmic: 1, ldo: 3, expect: (2700, 3600) },
    Rail { name: "l11a", pmic: 0, ldo: 11, expect: (1650, 1950) },
];
/// After turning a supply on, before the power switches.
pub const RAIL_SETTLE_MS: u64 = 5;

/// The CRC the controller's bootloader commands carry (CRC-16/CCITT, MSB first, as
/// Linux's `crc_itu_t`).
pub fn crc_itu_t(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// The bootloader's launch-app command.
pub fn launch_app_cmd() -> [u8; 11] {
    let mut c = [0x0b, 0x00, 0x40, 0x00, 0x01, 0x3b, 0x00, 0x00, 0, 0, 0x17];
    let crc = crc_itu_t(0xffff, &c[4..8]);
    c[8..10].copy_from_slice(&crc.to_le_bytes());
    c
}

/// `GET_SYSINFO`.
pub const SYSINFO_CMD_BYTES: [u8; 5] = [0x05, 0x00, 0x2f, 0x00, SYSINFO_CMD];

/// The system information's fields the loader uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sysinfo {
    pub res_x: u16,
    pub res_y: u16,
    pub max_tch: u8,
}

/// The system information from a response, if it is one: the report id and command,
/// long enough; `max_tch` 0 is taken as 10 (lab patch 0001).
pub fn parse_sysinfo(f: &[u8]) -> Option<Sysinfo> {
    if f.len() < SYSINFO_MIN || f[2] != SYSINFO_REPORT || f[4] & 0x7f != SYSINFO_CMD {
        return None;
    }
    let le = |o: usize| u16::from_le_bytes([f[o], f[o + 1]]);
    let max = f[SENSING + 15];
    Some(Sysinfo { res_x: le(SENSING + 6), res_y: le(SENSING + 8), max_tch: if max == 0 { 10 } else { max } })
}

/// Whether a frame is the HID descriptor this driver knows.
pub fn hid_ok(f: &[u8]) -> bool {
    f.len() >= HID_DESC_LEN && u16::from_le_bytes([f[0], f[1]]) as usize == HID_DESC_LEN && u16::from_le_bytes([f[4], f[5]]) == HID_VERSION
}

/// A touch report's state: `Some(Some((x, y)))` the first contact that isn't lifting
/// off, `Some(None)` no contact, `None` not a touch report.
pub fn parse_touch(f: &[u8]) -> Option<Option<(u16, u16)>> {
    if f.len() < 7 || f[2] != 0x01 {
        return None;
    }
    let n = (f[5] & 0x1f) as usize;
    for i in 0..n {
        let r = 7 + 10 * i;
        let Some(rec) = f.get(r..r + 10) else { break };
        if (rec[1] >> 5) & 3 == 3 {
            continue;
        }
        return Some(Some((u16::from_le_bytes([rec[2], rec[3]]), u16::from_le_bytes([rec[4], rec[5]]))));
    }
    Some(None)
}

/// The time left before `deadline`, for one call.
fn call_ms<H: Hw>(hw: &mut H, deadline: u64) -> Option<u32> {
    let left = deadline.saturating_sub(hw.now_ms());
    (left > 0).then(|| left.min(CALL_MS) as u32)
}

/// What the input register gave: the reset sentinel (length 0: the controller has
/// just come out of reset, or its application has just started), nothing waiting
/// (length 2), or a frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Sentinel,
    Empty,
    Data(Vec<u8>),
}

/// One read of the input register: its length, then the frame.
pub fn read_any<H: Hw>(hw: &mut H, deadline: u64) -> Result<Frame, String> {
    let t = call_ms(hw, deadline).ok_or("deadline")?;
    let mut len = [0u8; 2];
    hw.read_input(&mut len, t)?;
    let n = u16::from_le_bytes(len) as usize;
    match n {
        0 => return Ok(Frame::Sentinel),
        1 | 2 => return Ok(Frame::Empty),
        _ if n > MAX_FRAME => return Err(format!("length {n}")),
        _ => {}
    }
    let t = call_ms(hw, deadline).ok_or("deadline")?;
    let mut f = alloc::vec![0u8; n];
    hw.read_input(&mut f, t)?;
    Ok(Frame::Data(f))
}

/// One frame from the input register; `Ok(None)` when no report waits (length 0 or 2).
pub fn read_frame<H: Hw>(hw: &mut H, deadline: u64) -> Result<Option<Vec<u8>>, String> {
    Ok(match read_any(hw, deadline)? {
        Frame::Data(f) => Some(f),
        _ => None,
    })
}

/// Waits for `want` to take what the input register gives (with the ms since `since`),
/// for at most `max_ms` (never past `deadline`). Every [`POLL_MS`] the interrupt is
/// read, and the register only while it is asserted, as Linux reads it; while the
/// interrupt has never been seen asserted in the whole bring-up, the register is also
/// read every [`FALLBACK_MS`]. A read that fails counts as nothing yet: a controller
/// busy starting up doesn't answer its address. Logged (`TOUCH_WAIT`) and reported
/// (`key`) with how long it took and how often the interrupt was asserted and the
/// register read. Gives what `want` took and how often the interrupt was asserted.
fn wait<H: Hw, T>(hw: &mut H, up: &mut Up, key: &'static str, max_ms: u64, deadline: u64, since: u64, mut want: impl FnMut(Frame, u64) -> Option<T>) -> (Option<T>, u32) {
    let start = hw.now_ms();
    let until = (start + max_ms).min(deadline);
    let (mut errs, mut last) = (0u32, String::new());
    let (mut low, mut reads, mut last_read) = (0u32, 0u32, start);
    let got = loop {
        let now = hw.now_ms();
        let asserted = hw.irq_asserted().unwrap_or(false);
        if asserted {
            low += 1;
            up.irq_seen = true;
        }
        let read = asserted || (!up.irq_seen && now.saturating_sub(last_read) >= FALLBACK_MS);
        if !read {
            if now + POLL_MS >= until {
                break None;
            }
            hw.stall_ms(POLL_MS);
            continue;
        }
        reads += 1;
        last_read = now;
        match read_any(hw, deadline) {
            Ok(f) => {
                let at = hw.now_ms().saturating_sub(since);
                if let Some(t) = want(f, at) {
                    break Some(t);
                }
            }
            Err(e) => {
                errs += 1;
                last = e;
            }
        }
        if hw.now_ms() + POLL_MS >= until {
            break None;
        }
        hw.stall_ms(POLL_MS);
    };
    let ms = hw.now_ms().saturating_sub(start);
    let errs = if errs > 0 { format!(" read_errors={errs} last={last}") } else { String::new() };
    let v = format!("{} ms {} irq_low={low} reads={reads}{errs}", ms, if got.is_some() { "ok" } else { "timeout" });
    hw.log(&format!("TOUCH_WAIT {} {v}", key.trim_start_matches("touch.wait.")));
    set(up, key, v);
    (got, low)
}

/// After the application's sentinel: the frames still waiting read while the interrupt
/// is asserted, until it isn't or an empty read (`02 00`). Gives how many were read.
fn drain<H: Hw>(hw: &mut H, deadline: u64) -> u32 {
    let mut n = 0u32;
    while n < 32 && hw.now_ms() + POLL_MS < deadline && hw.irq_asserted().unwrap_or(false) {
        match read_any(hw, deadline) {
            Ok(Frame::Empty) | Err(_) => break,
            Ok(_) => n += 1,
        }
    }
    n
}

/// A write, sent again every 10 ms while the controller doesn't take it (a busy
/// controller doesn't answer its address), for at most [`WRITE_RETRY_MS`].
fn write_retry<H: Hw>(hw: &mut H, reg: u16, data: &[u8], deadline: u64) -> Result<(), String> {
    let start = hw.now_ms();
    let mut tries = 0u32;
    loop {
        tries += 1;
        let t = call_ms(hw, deadline).ok_or("deadline")?;
        let r = hw.write(reg, data, t);
        let ms = hw.now_ms().saturating_sub(start);
        match r {
            Ok(()) => {
                if tries > 1 {
                    hw.log(&format!("TOUCH_WAIT write 0x{reg:04x} {ms} ms ok tries={tries}"));
                }
                return Ok(());
            }
            Err(e) => {
                if ms + 10 > WRITE_RETRY_MS || hw.now_ms() + 10 >= deadline {
                    hw.log(&format!("TOUCH_WAIT write 0x{reg:04x} {ms} ms failed tries={tries}"));
                    return Err(e);
                }
                hw.stall_ms(10);
            }
        }
    }
}

fn hex(b: &[u8]) -> String {
    crate::hex(b)
}

/// What a bring-up found and did.
#[derive(Debug)]
pub struct Up {
    /// The levels read before anything (61, 62, 66, 67).
    pub found: Vec<Result<bool, String>>,
    /// The controller was powered when found (61 and 62 high): left powered.
    pub found_on: bool,
    /// The loader drove the power pins (then it powers them down).
    pub powered: bool,
    pub opened: bool,
    pub sysinfo: Option<Sysinfo>,
    /// `Ok`, or the step that failed and why.
    pub result: Result<(), String>,
    /// The report's `touch.*` lines, in order.
    pub lines: Vec<(&'static str, String)>,
    /// The interrupt was seen asserted (from then on no fallback reads).
    pub irq_seen: bool,
    /// `touch_rail=on`: the supplies turned on when off.
    pub rail: bool,
    /// The supplies the loader turned on (turned off again at the power-down).
    pub rails_on: Vec<(u32, u32)>,
    /// `touch_rpm`: the stage asked for.
    pub rpm: Option<RpmStage>,
    /// The loader installed the raw RPM protocol (uninstalled at the power-down).
    pub rpm_installed: bool,
    /// The loader's vote for LDO3 stands (taken back at the power-down).
    pub rpm_voted: bool,
}

impl Up {
    pub fn new(rail: bool) -> Up {
        Up {
            found: Vec::new(),
            found_on: false,
            powered: false,
            opened: false,
            sysinfo: None,
            result: Ok(()),
            lines: Vec::new(),
            irq_seen: false,
            rail,
            rails_on: Vec::new(),
            rpm: None,
            rpm_installed: false,
            rpm_voted: false,
        }
    }
    pub fn ok(&self) -> bool {
        self.result.is_ok()
    }
}

/// The bring-up (FR-005 to FR-008). With `keep`, the bus stays open and the controller
/// powered for the tap source; else (or after a failure) both are released by
/// [`power_down`] before this returns.
pub fn bring_up<H: Hw>(hw: &mut H, keep: bool) -> Up {
    bring_up_with(hw, keep, false)
}

/// [`bring_up`]; with `rail` (`touch_rail=on`) the supplies ([`RAILS`]) are turned on
/// first when off.
pub fn bring_up_with<H: Hw>(hw: &mut H, keep: bool, rail: bool) -> Up {
    bring_up_rpm(hw, keep, rail, None)
}

/// [`bring_up_with`]; with `rpm` (`touch_rpm`) the RPM stages run after the supplies
/// are read, before the power switches, and `rail` is not used (`touch.rail.on`
/// says so).
pub fn bring_up_rpm<H: Hw>(hw: &mut H, keep: bool, rail: bool, rpm: Option<RpmStage>) -> Up {
    let start = hw.now_ms();
    let deadline = start + DEADLINE_MS;
    let mut up = Up::new(rail);
    up.rpm = rpm;
    let r = steps(hw, &mut up, deadline);
    up.result = r;
    let el = hw.now_ms().saturating_sub(start);
    if !up.ok() || !keep {
        power_down(hw, &mut up);
    }
    up.lines.push(("touch.result", match &up.result {
        Ok(()) => format!("ok in {el} ms"),
        Err(e) => format!("failed {e} in {el} ms"),
    }));
    up
}

fn steps<H: Hw>(hw: &mut H, up: &mut Up, deadline: u64) -> Result<(), String> {
    for p in PINS {
        let l = hw.pin_read(p);
        up.found.push(l);
    }
    let lv = |r: &Result<bool, String>| match r {
        Ok(l) => format!("{}", *l as u8),
        Err(e) => format!("error {e}"),
    };
    up.lines.push(("touch.found", PINS.iter().zip(&up.found).map(|(p, l)| format!("{p}={}", lv(l))).collect::<Vec<_>>().join(" ")));
    if up.found.iter().any(|r| r.is_err()) {
        return Err("found: a pin can't be read".into());
    }
    up.found_on = up.found[0] == Ok(true) && up.found[1] == Ok(true);
    rails_read(hw, up);
    if let Some(stage) = up.rpm {
        if up.rail {
            up.lines.push(("touch.rail.on", "ignored touch_rpm".into()));
        }
        rpm_steps(hw, up, stage)?;
    } else if up.rail {
        let v = if up.found_on { "skipped found on".into() } else { rails_on(hw, up) };
        up.lines.push(("touch.rail.on", v));
    }
    if up.found_on {
        up.lines.push(("touch.power", "found on: left as found".into()));
    } else {
        up.powered = true;
        hw.pin_out(PIN_VDD, true).map_err(|e| format!("power: {e}"))?;
        hw.stall_ms(5);
        hw.pin_out(PIN_VDDIO, true).map_err(|e| format!("power: {e}"))?;
        hw.stall_ms(5);
        up.lines.push(("touch.power", "on: 61, 62".into()));
    }
    hw.pin_out(PIN_RESET, false).map_err(|e| format!("reset: {e}"))?;
    hw.stall_ms(1);
    hw.pin_out(PIN_RESET, true).map_err(|e| format!("reset: {e}"))?;
    hw.stall_ms(20);
    hw.pin_in(PIN_IRQ).map_err(|e| format!("irq: {e}"))?;
    if let Err(e) = hw.open() {
        up.lines.push(("touch.open", format!("failed {e}")));
        return Err(format!("open: {e}"));
    }
    up.opened = true;
    up.lines.push(("touch.open", format!("instance={I2C_INSTANCE} address=0x{I2C_ADDRESS:02x} khz={I2C_KHZ}")));
    // The reset sentinel (length 0); anything before it is dropped.
    let t = hw.now_ms();
    wait(hw, up, "touch.wait.reset", RESET_WAIT_MS, deadline, t, |f, _| (f == Frame::Sentinel).then_some(()));
    let l = launch(hw, up, deadline, "touch.wait.app");
    up.lines.push(("touch.launch", l));
    let settled = hw.now_ms();
    // The HID descriptor; its packet id tells the mode. Still in the bootloader:
    // launch-app again, then the descriptor again.
    let mut desc = hid_desc(hw, up, deadline)?;
    match desc.get(2).copied() {
        Some(PACKET_APP) => set(up, "touch.mode", "app".into()),
        Some(PACKET_BL) => {
            set(up, "touch.mode", "bl".into());
            let l = launch(hw, up, deadline, "touch.wait.app.bl");
            up.lines.push(("touch.launch.bl", l));
            desc = hid_desc(hw, up, deadline)?;
            let m = match desc.get(2).copied() {
                Some(PACKET_APP) => "app".into(),
                Some(PACKET_BL) => "bl".into(),
                p => format!("unknown {p:02x?}"),
            };
            set(up, "touch.mode.bl", m);
        }
        p => set(up, "touch.mode", format!("unknown {p:02x?}")),
    }
    if !hid_ok(&desc) {
        up.lines.push(("touch.hid", "unknown".into()));
        return Err("hid: not the descriptor".into());
    }
    let le = |o: usize| u16::from_le_bytes([desc[o], desc[o + 1]]);
    up.lines.push(("touch.hid", format!("vendor=0x{:04x} product=0x{:04x} version=0x{:04x} max_input={}", le(22), le(24), le(26), le(12))));
    if READ_REPORT_DESC {
        report_desc(hw, up, deadline, le(8), le(6), le(12));
    }
    // The system information: sent no sooner than SYSINFO_PAUSE_MS after the
    // application's sentinels (Linux's first send, before them, is lost), and waited for
    // as Linux does; on no answer (or one that isn't it: lab patch 0002), sent again
    // after a pause.
    let at = settled + SYSINFO_PAUSE_MS;
    let now = hw.now_ms();
    if at > now && at < deadline {
        hw.stall_ms(at - now);
    }
    for ask in 1..=SYSINFO_SENDS {
        if call_ms(hw, deadline).is_none() {
            break;
        }
        write_retry(hw, REG_OUTPUT, &SYSINFO_CMD_BYTES, deadline).map_err(|e| format!("sysinfo: write {e}"))?;
        let t = hw.now_ms();
        let (f, _) = wait(hw, up, "touch.wait.sysinfo", SYSINFO_WAIT_MS, deadline, t, |f, _| match f {
            Frame::Data(f) => Some(f),
            _ => None,
        });
        match f.as_deref().and_then(parse_sysinfo) {
            Some(si) => {
                up.lines.push(("touch.sysinfo", hex(f.as_deref().unwrap_or(&[]))));
                up.lines.push(("touch.res", format!("{}x{} max_tch={} ask={ask}", si.res_x, si.res_y, si.max_tch)));
                up.sysinfo = Some(si);
                return Ok(());
            }
            None => {
                if let Some(f) = &f {
                    hw.log(&format!("TOUCH sysinfo answer not one: {}", hex(&f[..f.len().min(16)])));
                }
                if ask == SYSINFO_SENDS || hw.now_ms() + SYSINFO_PAUSE_MS >= deadline {
                    break;
                }
                hw.stall_ms(SYSINFO_PAUSE_MS);
            }
        }
    }
    Err("sysinfo: no answer".into())
}

/// A report line, replaced when the key is already there.
fn set(up: &mut Up, key: &'static str, v: String) {
    match up.lines.iter_mut().find(|l| l.0 == key) {
        Some(l) => l.1 = v,
        None => up.lines.push((key, v)),
    }
}

/// The bootloader's launch-app, then the application's sentinel: a size-0 frame at
/// least [`APP_EARLY_MS`] after the write (frames before that, and any other frame, are
/// read and dropped), then the frames still waiting drained. Not fatal: without a
/// sentinel the bring-up goes on (a controller already in its application).
fn launch<H: Hw>(hw: &mut H, up: &mut Up, deadline: u64, key: &'static str) -> String {
    if let Err(e) = write_retry(hw, REG_OUTPUT, &launch_app_cmd(), deadline) {
        return format!("write failed {e}");
    }
    let start = hw.now_ms();
    let max = if up.irq_seen { APP_WAIT_MS } else { APP_WAIT_NO_IRQ_MS };
    let mut dropped = 0u32;
    let (got, _) = wait(hw, up, key, max, deadline, start, |f, at| match f {
        Frame::Sentinel if at >= APP_EARLY_MS => Some(at),
        Frame::Empty => None,
        _ => {
            dropped += 1;
            None
        }
    });
    let dropped = if dropped > 0 { format!(" dropped={dropped}") } else { String::new() };
    match got {
        Some(at) => {
            let n = drain(hw, deadline);
            format!("app sentinel {at} ms drained={n}{dropped}")
        }
        None => format!("no sentinel{dropped}"),
    }
}

/// The report descriptor: its register (the HID descriptor's `wReportDescRegister`)
/// written and its `wReportDescLength` bytes read back, as HID over I2C reads it.
/// Reported (`touch.rdesc`: the length and the first 16 bytes); not fatal.
fn report_desc<H: Hw>(hw: &mut H, up: &mut Up, deadline: u64, reg: u16, len: u16, max_input: u16) {
    let v = if len == 0 || len > max_input.max(MAX_FRAME as u16) {
        format!("skipped length {len}")
    } else {
        match call_ms(hw, deadline) {
            None => "skipped deadline".into(),
            Some(t) => {
                let mut b = alloc::vec![0u8; len as usize];
                match hw.read_reg(reg, &mut b, t) {
                    Ok(()) => format!("reg=0x{reg:04x} len={len} {}", hex(&b[..b.len().min(16)])),
                    Err(e) => format!("reg=0x{reg:04x} len={len} failed {e}"),
                }
            }
        }
    };
    hw.log(&format!("TOUCH rdesc {v}"));
    up.lines.push(("touch.rdesc", v));
}

/// The HID descriptor (register 0x0001), reported (`touch.desc`).
fn hid_desc<H: Hw>(hw: &mut H, up: &mut Up, deadline: u64) -> Result<Vec<u8>, String> {
    let data = |f, _| match f {
        Frame::Data(f) => Some(f),
        _ => None,
    };
    write_retry(hw, REG_HID_DESC, &[], deadline).map_err(|e| format!("hid: write {e}"))?;
    let t = hw.now_ms();
    let (mut desc, low) = wait(hw, up, "touch.wait.hid", HID_RESEND_MS, deadline, t, data);
    if desc.is_none() {
        // The interrupt never asserted: the request sent once more.
        if low == 0 {
            write_retry(hw, REG_HID_DESC, &[], deadline).map_err(|e| format!("hid: write {e}"))?;
        }
        let t = hw.now_ms();
        desc = wait(hw, up, "touch.wait.hid.more", SYSINFO_WAIT_MS - HID_RESEND_MS, deadline, t, data).0;
        set(up, "touch.hid.resent", format!("{}", low == 0));
    }
    let desc = desc.ok_or_else(|| String::from("hid: no answer"))?;
    set(up, "touch.desc", hex(&desc));
    Ok(desc)
}

/// The supplies' state, read only (`touch.rail.<name>`: `on|off <mV> mV`, or
/// `error <why>`).
fn rails_read<H: Hw>(hw: &mut H, up: &mut Up) {
    for r in &RAILS {
        let v = match hw.rail_get(r.pmic, r.ldo) {
            Ok((on, mv)) => format!("{} {mv} mV", if on { "on" } else { "off" }),
            Err(e) => format!("error {e}"),
        };
        up.lines.push((if r.ldo == 3 { "touch.rail.l3b" } else { "touch.rail.l11a" }, v));
    }
}

fn rail_name(pmic: u32, ldo: u32) -> &'static str {
    RAILS.iter().find(|r| (r.pmic, r.ldo) == (pmic, ldo)).map_or("other", |r| r.name)
}

/// `touch_rail=on`: each supply in [`RAILS`] that is off and whose set level is already
/// in its range is enabled, nothing else (recorded for the power-down). One already
/// on, or not readable, or out of range is left alone. Never fatal.
fn rails_on<H: Hw>(hw: &mut H, up: &mut Up) -> String {
    let mut out: Vec<String> = Vec::new();
    for r in &RAILS {
        let v = match hw.rail_get(r.pmic, r.ldo) {
            Err(e) => format!("{} error {e}", r.name),
            Ok((true, mv)) => format!("{} already on {mv} mV", r.name),
            Ok((false, mv)) if mv < r.expect.0 || mv > r.expect.1 => format!("{} refused {mv} mV", r.name),
            Ok((false, _)) => match hw.rail_enable(r.pmic, r.ldo, true) {
                Err(e) => format!("{} failed {e}", r.name),
                Ok(()) => {
                    up.rails_on.push((r.pmic, r.ldo));
                    match hw.rail_get(r.pmic, r.ldo) {
                        Ok((on, mv)) => format!("{} turned on {} {mv} mV", r.name, if on { "on" } else { "off" }),
                        Err(e) => format!("{} turned on, read error {e}", r.name),
                    }
                }
            },
        };
        out.push(v);
    }
    if !up.rails_on.is_empty() {
        hw.stall_ms(RAIL_SETTLE_MS);
    }
    out.join(", ")
}

/// The `touch_rpm` stages (each logged in `touch.rpm.*`). Up to `kvp` nothing fails
/// the bring-up; `vote` goes on to the power switches only with LDO3 read back on and
/// in its range.
fn rpm_steps<H: Hw>(hw: &mut H, up: &mut Up, stage: RpmStage) -> Result<(), String> {
    let vote = stage == RpmStage::Vote;
    let stop = |up: &mut Up, why: String| -> Result<(), String> {
        up.lines.push(("touch.rpm.stopped", why.clone()));
        if vote { Err(format!("rpm: {why}")) } else { Ok(()) }
    };
    let f = hw.rpm_locate();
    up.lines.push(("touch.rpm.found", format!("npa={} svc={} raw={}", f.npa.text(), f.svc.text(), f.raw.text())));
    if stage == RpmStage::Locate {
        return Ok(());
    }
    match (&f.svc, &f.raw) {
        (_, Proto::Ok) => up.lines.push(("touch.rpm.install", "found installed".into())),
        (_, Proto::Bad(w)) => return stop(up, format!("raw {w}")),
        (Proto::Ok, Proto::Absent) => {
            let r = hw.rpm_install(true);
            // Whatever the result, the install may have happened: undone at the end.
            up.rpm_installed = true;
            match r {
                Ok(()) => up.lines.push(("touch.rpm.install", "ours ok".into())),
                Err(e) => {
                    up.lines.push(("touch.rpm.install", format!("ours failed {e}")));
                    return stop(up, "install".into());
                }
            }
        }
        (s, Proto::Absent) => return stop(up, format!("svc {}", s.text())),
    }
    if stage == RpmStage::Install {
        return Ok(());
    }
    match hw.rpm_kvp() {
        Ok(()) => up.lines.push(("touch.rpm.kvp", "ok".into())),
        Err(e) => {
            up.lines.push(("touch.rpm.kvp", format!("failed {e}")));
            return stop(up, "kvp".into());
        }
    }
    if !vote {
        return Ok(());
    }
    let l3b = &RAILS[0];
    match hw.rail_get(l3b.pmic, l3b.ldo) {
        Ok((true, mv)) => {
            up.lines.push(("touch.rpm.vote", format!("skipped already on {mv} mV")));
            return Ok(());
        }
        Ok((false, _)) => {}
        Err(e) => return stop(up, format!("l3b read {e}")),
    }
    hw.watchdog();
    let id = match hw.rpm_vote(1) {
        Ok(id) => id,
        Err(e) => {
            up.lines.push(("touch.rpm.vote", format!("failed {e}")));
            return stop(up, "vote".into());
        }
    };
    up.rpm_voted = true;
    hw.stall_ms(RPM_SETTLE_MS);
    match hw.rail_get(l3b.pmic, l3b.ldo) {
        Ok((true, mv)) if mv >= l3b.expect.0 && mv <= l3b.expect.1 => {
            up.lines.push(("touch.rpm.vote", format!("id={id} l3b on {mv} mV")));
            Ok(())
        }
        Ok((on, mv)) => {
            up.lines.push(("touch.rpm.vote", format!("id={id} l3b {} {mv} mV", if on { "on" } else { "off" })));
            stop(up, format!("l3b {} {mv} mV", if on { "out of range" } else { "not on" }))
        }
        Err(e) => {
            up.lines.push(("touch.rpm.vote", format!("id={id} read error {e}")));
            stop(up, "l3b read".into())
        }
    }
}

/// What the RPM stages left: the vote taken back (`swen`=0, LDO3 read back), then the
/// raw protocol uninstalled when the loader installed it. Once.
fn rpm_release<H: Hw>(hw: &mut H, up: &mut Up) {
    if up.rpm_voted {
        up.rpm_voted = false;
        hw.watchdog();
        let v = match hw.rpm_vote(0) {
            Err(e) => format!("failed {e}"),
            Ok(id) => {
                hw.stall_ms(RPM_SETTLE_MS);
                match hw.rail_get(RAILS[0].pmic, RAILS[0].ldo) {
                    Ok((on, mv)) => format!("id={id} l3b {} {mv} mV", if on { "on" } else { "off" }),
                    Err(e) => format!("id={id} read error {e}"),
                }
            }
        };
        up.lines.push(("touch.rpm.off", v));
    }
    if up.rpm_installed {
        up.rpm_installed = false;
        let v = match hw.rpm_install(false) {
            Ok(()) => "ok".into(),
            Err(e) => format!("failed {e}"),
        };
        up.lines.push(("touch.rpm.uninstall", v));
    }
}

/// Releases what the bring-up took (FR-008): the bus closed; the controller powered
/// down when the loader powered it (reset low, 62 low, 61 low), then the supplies the
/// loader turned on turned off, then the three pins it drove configured as inputs (the
/// firmware's protocol can't read a pin's configuration to restore it); found on, the
/// power left and the reset high.
pub fn power_down<H: Hw>(hw: &mut H, up: &mut Up) {
    if up.opened {
        hw.close();
        up.opened = false;
    }
    let mut errs: Vec<String> = Vec::new();
    if up.powered {
        for (p, h) in [(PIN_RESET, false), (PIN_VDDIO, false), (PIN_VDD, false)] {
            if let Err(e) = hw.pin_out(p, h) {
                errs.push(format!("{p}: {e}"));
            }
        }
        up.powered = false;
        up.lines.push(("touch.down", if errs.is_empty() { "off: 66, 62, 61".into() } else { format!("errors {}", errs.join(", ")) }));
        if !up.rails_on.is_empty() {
            let mut v: Vec<String> = Vec::new();
            for (pmic, ldo) in core::mem::take(&mut up.rails_on).into_iter().rev() {
                v.push(match hw.rail_enable(pmic, ldo, false) {
                    Ok(()) => format!("{} off", rail_name(pmic, ldo)),
                    Err(e) => format!("{} error {e}", rail_name(pmic, ldo)),
                });
            }
            up.lines.push(("touch.rail.off", v.join(", ")));
        }
        rpm_release(hw, up);
        let mut v: Vec<String> = Vec::new();
        for p in [PIN_RESET, PIN_VDDIO, PIN_VDD] {
            if let Err(e) = hw.pin_in(p) {
                v.push(format!("{p}: {e}"));
            }
        }
        up.lines.push(("touch.release", if v.is_empty() { "inputs: 66, 62, 61".into() } else { format!("errors {}", v.join(", ")) }));
    } else if up.found_on {
        if let Err(e) = hw.pin_out(PIN_RESET, true) {
            errs.push(format!("66: {e}"));
        }
        up.lines.push(("touch.down", if errs.is_empty() { "left powered".into() } else { format!("errors {}", errs.join(", ")) }));
    }
    rpm_release(hw, up);
}

#[cfg(test)]
mod tests;
