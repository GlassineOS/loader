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
pub const DEADLINE_MS: u64 = 3000;
pub const CALL_MS: u64 = 100;
pub const POLL_MS: u64 = 5;
/// The waits after a reset: the reset sentinel, the bootloader's answer to launch-app,
/// the application's sentinel; how long a write the controller doesn't take is sent
/// again.
pub const RESET_WAIT_MS: u64 = 500;
pub const LAUNCH_WAIT_MS: u64 = 500;
pub const APP_WAIT_MS: u64 = 1000;
pub const WRITE_RETRY_MS: u64 = 300;

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
}

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

/// Polls the input register every [`POLL_MS`] until `want` takes what it gave, for at
/// most `max_ms` (never past `deadline`). A read that fails counts as nothing yet: a
/// controller busy starting up doesn't answer its address. Logged (`TOUCH_WAIT`) and
/// reported (`key`) with how long it took.
fn wait<H: Hw, T>(hw: &mut H, up: &mut Up, key: &'static str, max_ms: u64, deadline: u64, mut want: impl FnMut(Frame) -> Option<T>) -> Option<T> {
    let start = hw.now_ms();
    let until = (start + max_ms).min(deadline);
    let (mut errs, mut last) = (0u32, String::new());
    let got = loop {
        match read_any(hw, deadline) {
            Ok(f) => {
                if let Some(t) = want(f) {
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
    let v = format!("{} ms {}{errs}", ms, if got.is_some() { "ok" } else { "timeout" });
    hw.log(&format!("TOUCH_WAIT {} {v}", key.trim_start_matches("touch.wait.")));
    match up.lines.iter_mut().find(|l| l.0 == key) {
        Some(l) => l.1 = v,
        None => up.lines.push((key, v)),
    }
    got
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
}

impl Up {
    pub fn ok(&self) -> bool {
        self.result.is_ok()
    }
}

/// The bring-up (FR-005 to FR-008). With `keep`, the bus stays open and the controller
/// powered for the tap source; else (or after a failure) both are released by
/// [`power_down`] before this returns.
pub fn bring_up<H: Hw>(hw: &mut H, keep: bool) -> Up {
    let start = hw.now_ms();
    let deadline = start + DEADLINE_MS;
    let mut up = Up { found: Vec::new(), found_on: false, powered: false, opened: false, sysinfo: None, result: Ok(()), lines: Vec::new() };
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
    wait(hw, up, "touch.wait.reset", RESET_WAIT_MS, deadline, |f| (f == Frame::Sentinel).then_some(()));
    // The bootloader's launch-app and its answer (lengths 0 and 2 aren't one); then the
    // application, once started, sends its own sentinel. None of it is fatal: a
    // controller already in its application answers launch-app with an error or not at
    // all (the lab saw an error status there with the controller working).
    let launch = match write_retry(hw, REG_OUTPUT, &launch_app_cmd(), deadline) {
        Err(e) => format!("write failed {e}"),
        Ok(()) => match wait(hw, up, "touch.wait.launch", LAUNCH_WAIT_MS, deadline, |f| match f {
            Frame::Data(f) => Some(f),
            _ => None,
        }) {
            Some(f) => {
                wait(hw, up, "touch.wait.app", APP_WAIT_MS, deadline, |f| (f == Frame::Sentinel).then_some(()));
                format!("answer {}", hex(&f[..f.len().min(16)]))
            }
            None => "no answer".into(),
        },
    };
    up.lines.push(("touch.launch", launch));
    // The HID descriptor.
    write_retry(hw, REG_HID_DESC, &[], deadline).map_err(|e| format!("hid: write {e}"))?;
    let desc = wait(hw, up, "touch.wait.hid", DEADLINE_MS, deadline, |f| match f {
        Frame::Data(f) => Some(f),
        _ => None,
    })
    .ok_or_else(|| String::from("hid: no answer"))?;
    up.lines.push(("touch.desc", hex(&desc)));
    if !hid_ok(&desc) {
        up.lines.push(("touch.hid", "unknown".into()));
        return Err("hid: not the descriptor".into());
    }
    let le = |o: usize| u16::from_le_bytes([desc[o], desc[o + 1]]);
    up.lines.push(("touch.hid", format!("vendor=0x{:04x} product=0x{:04x} version=0x{:04x} max_input={}", le(22), le(24), le(26), le(12))));
    // The system information, asked again while the answer isn't one (lab patch 0002).
    for ask in 1..=5u32 {
        if call_ms(hw, deadline).is_none() {
            break;
        }
        write_retry(hw, REG_OUTPUT, &SYSINFO_CMD_BYTES, deadline).map_err(|e| format!("sysinfo: write {e}"))?;
        let f = wait(hw, up, "touch.wait.sysinfo", 500, deadline, |f| match f {
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
                if hw.now_ms() + 100 >= deadline {
                    break;
                }
                hw.stall_ms(100);
            }
        }
    }
    Err("sysinfo: no answer".into())
}

/// Releases what the bring-up took (FR-008): the bus closed; the controller powered
/// down when the loader powered it (reset low, 62 low, 61 low); found on, the power left
/// and the reset high.
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
    } else if up.found_on {
        if let Err(e) = hw.pin_out(PIN_RESET, true) {
            errs.push(format!("66: {e}"));
        }
        up.lines.push(("touch.down", if errs.is_empty() { "left powered".into() } else { format!("errors {}", errs.join(", ")) }));
    }
}

#[cfg(test)]
mod tests;
