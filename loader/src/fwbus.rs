// SPDX-License-Identifier: GPL-2.0-or-later
//! The firmware's I2C and GPIO protocols (`docs/firmware-i2c-gpio.md`; spec 005): found,
//! read as raw words and checked before any of their functions is called (FR-001). The
//! probe calls only the GPIO pin read, which configures nothing; the touch driver also
//! the GPIO configure and set and the I2C open, read, write and close (spec 005). Every
//! call is logged (`TOUCH_GPIO`, `TOUCH_I2C`).
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use uefi::boot::{self, SearchType};
use uefi::{Guid, Handle, Status, guid};

use crate::fwgop::{Images, Ram, interface_of, rd64};
use crate::log;

pub const I2C_GUID: Guid = guid!("b27ae8b1-3e10-4d07-ab5c-eb9a6dc6fa8f");
pub const GPIO_GUID: Guid = guid!("ad9aec18-7bf0-4809-9e96-3012309f3df7");
pub const VREG_GUID: Guid = guid!("22d38d3d-e8b6-4f8f-9c26-bceb07d6cb68");

/// What the loader knows of each protocol: its interface's length in words and the
/// revision in its first word.
#[derive(Clone, Copy)]
pub struct Kind {
    pub key: &'static str,
    pub guid: Guid,
    pub words: usize,
    pub revision: u64,
}

pub const I2C: Kind = Kind { key: "i2c", guid: I2C_GUID, words: 6, revision: 0x10000 };
pub const GPIO: Kind = Kind { key: "gpio", guid: GPIO_GUID, words: 8, revision: 1 };
/// The PMIC regulator protocol: the revision and seven calls.
pub const VREG: Kind = Kind { key: "vreg", guid: VREG_GUID, words: 8, revision: 0x10005 };

/// One protocol as found.
pub struct Found {
    pub handle: Handle,
    pub iface: Option<u64>,
    pub words: Option<Vec<u64>>,
    /// `Ok` when it may be called (FR-001), else the first check that failed.
    pub check: Result<(), String>,
}

/// The first handle with the protocol, its interface read and checked. `None`: no handle
/// has it.
pub fn find(k: Kind, ram: &Ram, images: &Images) -> Option<Found> {
    let hs = boot::locate_handle_buffer(SearchType::ByProtocol(&k.guid)).ok()?;
    let handle = *hs.first()?;
    let Some(iface) = interface_of(handle, &k.guid) else {
        return Some(Found { handle, iface: None, words: None, check: Err("unopened".into()) });
    };
    if !ram.holds(iface, 8 * k.words as u64) {
        return Some(Found { handle, iface: Some(iface), words: None, check: Err("not-ram".into()) });
    }
    let w: Vec<u64> = (0..k.words).map(|i| unsafe { rd64(iface + 8 * i as u64) }).collect();
    let check = if w[0] != k.revision {
        Err(format!("revision=0x{:x}", w[0]))
    } else {
        let img = images.of(w[1]).map(|i| i.index);
        if img.is_some() && w[1..].iter().all(|&a| images.of(a).map(|i| i.index) == img) { Ok(()) } else { Err("outside".into()) }
    };
    Some(Found { handle, iface: Some(iface), words: Some(w), check })
}

/// The probe report's lines for one protocol (boot `docs/handback.md`, "The probe
/// report"): read from memory only.
pub fn report_lines(k: Kind, f: Option<&Found>, index: &dyn Fn(Handle) -> Option<usize>, images: &Images) -> Vec<String> {
    let key = k.key;
    let Some(f) = f else { return alloc::vec![format!("{key}=none")] };
    let hi = index(f.handle).map_or("-".into(), |x| format!("{x}"));
    let mut out = Vec::new();
    match f.iface {
        None => out.push(format!("{key}=handle={hi} unopened")),
        Some(i) => out.push(format!("{key}=handle={hi} iface=0x{i:x}")),
    }
    match &f.words {
        Some(w) => {
            out.push(format!("{key}.words={}", w.iter().map(|x| format!("0x{x:x}")).collect::<Vec<_>>().join(" ")));
            let wh: Vec<String> = w[1..]
                .iter()
                .map(|&a| match images.of(a) {
                    Some(img) => format!("image{}", img.index),
                    None if a == 0 => "0".into(),
                    None => "-".into(),
                })
                .collect();
            out.push(format!("{key}.in={}", wh.join(" ")));
        }
        None if f.iface.is_some() => out.push(format!("{key}.words=not-ram")),
        None => {}
    }
    out.push(format!("{key}.check={}", match &f.check {
        Ok(()) => "ok".into(),
        Err(e) => e.clone(),
    }));
    out
}

// ---- the GPIO protocol ------------------------------------------------------------------

/// The touch controller's pins (spec 005); no other pin is ever configured, read or set.
pub const TOUCH_PINS: [u32; 4] = [61, 62, 66, 67];

/// The pin word: plain GPIO, no pull, the lowest drive; bit 29 marks the layout.
pub fn pin_cfg(n: u32, output: bool) -> u32 {
    0x2000_0000 | ((n & 0x3ff) << 4) | ((output as u32) << 14)
}

/// A GPIO protocol that passed its checks.
pub struct Gpio {
    iface: u64,
}

impl Gpio {
    pub fn new(f: &Found) -> Option<Gpio> {
        f.check.is_ok().then_some(())?;
        Some(Gpio { iface: f.iface? })
    }

    fn word(&self, i: u64) -> u64 {
        unsafe { rd64(self.iface + 8 * i) }
    }

    fn allowed(n: u32) -> bool {
        TOUCH_PINS.contains(&n)
    }

    /// The pin's level (read only).
    pub fn read(&self, n: u32) -> Result<u32, Status> {
        let r = self.read_quiet(n);
        match r {
            Ok(level) => log!("TOUCH_GPIO read {} -> SUCCESS level={}", n, level),
            Err(st) => log!("TOUCH_GPIO read {} -> {:?}", n, st),
        }
        r
    }

    /// [`Gpio::read`] without its log line, for the interrupt's polls (the caller counts
    /// them).
    pub fn read_quiet(&self, n: u32) -> Result<u32, Status> {
        if !Self::allowed(n) {
            return Err(Status::ACCESS_DENIED);
        }
        type Read = unsafe extern "efiapi" fn(u32, *mut u32) -> Status;
        let f: Read = unsafe { core::mem::transmute(self.word(3) as usize) };
        let mut level = 0u32;
        let st = unsafe { f(pin_cfg(n, false), &mut level) };
        if st == Status::SUCCESS { Ok(level) } else { Err(st) }
    }
    /// Sets the pin as an output (latching `level` first) or as an input.
    pub fn configure(&self, n: u32, output: bool) -> Status {
        if !Self::allowed(n) {
            return Status::ACCESS_DENIED;
        }
        type Cfg = unsafe extern "efiapi" fn(u32, u32) -> Status;
        let f: Cfg = unsafe { core::mem::transmute(self.word(1) as usize) };
        let st = unsafe { f(pin_cfg(n, output), 1) };
        log!("TOUCH_GPIO configure {} {} -> {:?}", n, if output { "out" } else { "in" }, st);
        st
    }

    /// Drives an output pin.
    pub fn set(&self, n: u32, high: bool) -> Status {
        if !Self::allowed(n) {
            return Status::ACCESS_DENIED;
        }
        type Set = unsafe extern "efiapi" fn(u32, u32) -> Status;
        let f: Set = unsafe { core::mem::transmute(self.word(4) as usize) };
        let st = unsafe { f(pin_cfg(n, true), high as u32) };
        log!("TOUCH_GPIO set {} {} -> {:?}", n, high as u8, st);
        st
    }
}

// ---- the I2C protocol -------------------------------------------------------------------

/// The bus configuration the I2C calls take: clock in kHz, 7-bit address, the address
/// kind (0: 7-bit), reserved; then zeros.
#[repr(C)]
pub struct BusCfg {
    pub khz: u32,
    pub address: u32,
    pub kind: u32,
    pub reserved: [u32; 5],
}

/// An I2C protocol that passed its checks.
pub struct I2c {
    iface: u64,
}

/// An open bus.
pub struct Bus {
    iface: u64,
    handle: *mut c_void,
    cfg: BusCfg,
}

impl I2c {
    pub fn new(f: &Found) -> Option<I2c> {
        f.check.is_ok().then_some(())?;
        Some(I2c { iface: f.iface? })
    }

    pub fn open(&self, instance: u32, khz: u32, address: u32) -> Result<Bus, u32> {
        type Open = unsafe extern "efiapi" fn(u32, *mut *mut c_void) -> u32;
        let f: Open = unsafe { core::mem::transmute(rd64_at(self.iface, 1) as usize) };
        let mut h: *mut c_void = core::ptr::null_mut();
        let st = unsafe { f(instance, &mut h) };
        log!("TOUCH_I2C open {} -> {} handle={:p}", instance, st, h);
        if st != 0 || h.is_null() {
            return Err(if st == 0 { u32::MAX } else { st });
        }
        Ok(Bus { iface: self.iface, handle: h, cfg: BusCfg { khz, address, kind: 0, reserved: [0; 5] } })
    }
}

fn rd64_at(iface: u64, i: u64) -> u64 {
    unsafe { rd64(iface + 8 * i) }
}

impl Bus {
    /// Reads `buf.len()` bytes after writing the one-byte `offset`. Returns the bytes read.
    pub fn read(&mut self, offset: u8, buf: &mut [u8], timeout_ms: u32) -> Result<u32, u32> {
        type Rd = unsafe extern "efiapi" fn(*mut c_void, *const BusCfg, u16, u16, *mut u8, u16, *mut u32, u32) -> u32;
        let f: Rd = unsafe { core::mem::transmute(rd64_at(self.iface, 2) as usize) };
        let mut n = 0u32;
        let st = unsafe { f(self.handle, &self.cfg, offset as u16, 1, buf.as_mut_ptr(), buf.len() as u16, &mut n, timeout_ms.max(1)) };
        if st != 0 { Err(st) } else { Ok(n) }
    }

    /// Reads `buf.len()` bytes after writing the two-byte register `reg`, low byte first
    /// (the protocol sends a two-byte offset high byte first, so it gets the bytes
    /// swapped).
    pub fn read16(&mut self, reg: u16, buf: &mut [u8], timeout_ms: u32) -> Result<u32, u32> {
        type Rd = unsafe extern "efiapi" fn(*mut c_void, *const BusCfg, u16, u16, *mut u8, u16, *mut u32, u32) -> u32;
        let f: Rd = unsafe { core::mem::transmute(rd64_at(self.iface, 2) as usize) };
        let mut n = 0u32;
        let st = unsafe { f(self.handle, &self.cfg, reg.swap_bytes(), 2, buf.as_mut_ptr(), buf.len() as u16, &mut n, timeout_ms.max(1)) };
        if st != 0 { Err(st) } else { Ok(n) }
    }

    /// Writes the one-byte `offset`, then `data` (at least one byte).
    pub fn write(&mut self, offset: u8, data: &[u8], timeout_ms: u32) -> Result<u32, u32> {
        type Wr = unsafe extern "efiapi" fn(*mut c_void, *const BusCfg, u16, u16, *const u8, u16, *mut u32, u32) -> u32;
        let f: Wr = unsafe { core::mem::transmute(rd64_at(self.iface, 3) as usize) };
        let mut n = 0u32;
        let st = unsafe { f(self.handle, &self.cfg, offset as u16, 1, data.as_ptr(), data.len() as u16, &mut n, timeout_ms.max(1)) };
        if st != 0 { Err(st) } else { Ok(n) }
    }

    pub fn close(self) -> u32 {
        type Close = unsafe extern "efiapi" fn(*mut c_void) -> u32;
        let f: Close = unsafe { core::mem::transmute(rd64_at(self.iface, 5) as usize) };
        let st = unsafe { f(self.handle) };
        log!("TOUCH_I2C close -> {}", st);
        st
    }
}

// ---- the PMIC regulator protocol --------------------------------------------------------

/// The only regulators the touch driver reads, sets or switches (spec 005): pm660l
/// LDO3 (PMIC 1) and pm660 LDO11 (PMIC 0).
fn vreg_allowed(pmic: u32, ldo: u32) -> bool {
    matches!((pmic, ldo), (1, 3) | (0, 11))
}

/// A regulator protocol that passed its checks. Calls take the PMIC index and the
/// regulator id (LDO n is id n - 1).
pub struct Vreg {
    iface: u64,
}

impl Vreg {
    pub fn new(f: &Found) -> Option<Vreg> {
        f.check.is_ok().then_some(())?;
        Some(Vreg { iface: f.iface? })
    }

    fn word(&self, i: u64) -> u64 {
        unsafe { rd64(self.iface + 8 * i) }
    }

    /// Enabled (software enable on) and the level in mV: the status call (word 6) and
    /// the level call (word 3). Read only.
    pub fn get(&self, pmic: u32, ldo: u32) -> Result<(bool, u32), Status> {
        if !vreg_allowed(pmic, ldo) {
            return Err(Status::ACCESS_DENIED);
        }
        type GetStatus = unsafe extern "efiapi" fn(u32, u32, *mut u32) -> Status;
        type GetLevel = unsafe extern "efiapi" fn(u32, u32, *mut u32) -> Status;
        // pull-down, mode, pin control, software enable (u32 each), then "ok" (one byte).
        let mut status = [0u32; 8];
        let f: GetStatus = unsafe { core::mem::transmute(self.word(6) as usize) };
        let st = unsafe { f(pmic, ldo - 1, status.as_mut_ptr()) };
        log!("TOUCH_PMIC status p{} l{} -> {:?} pull_down={} mode={} pin_ctl={} enable={} ok={}", pmic, ldo, st, status[0], status[1], status[2], status[3], status[4] & 0xff);
        if st != Status::SUCCESS {
            return Err(st);
        }
        let mut mv = 0u32;
        let f: GetLevel = unsafe { core::mem::transmute(self.word(3) as usize) };
        let st = unsafe { f(pmic, ldo - 1, &mut mv) };
        // The level is the set output; taken as µV when it is that large (the set call
        // takes mV).
        let level = if mv >= 100_000 { mv / 1000 } else { mv };
        log!("TOUCH_PMIC level p{} l{} -> {:?} raw={} {} mV", pmic, ldo, st, mv, level);
        if st != Status::SUCCESS {
            return Err(st);
        }
        Ok((status[3] == 1, level))
    }

    /// Sets the level (word 2).
    pub fn set(&self, pmic: u32, ldo: u32, mv: u32) -> Status {
        if !vreg_allowed(pmic, ldo) {
            return Status::ACCESS_DENIED;
        }
        type SetLevel = unsafe extern "efiapi" fn(u32, u32, u32) -> Status;
        let f: SetLevel = unsafe { core::mem::transmute(self.word(2) as usize) };
        let st = unsafe { f(pmic, ldo - 1, mv) };
        log!("TOUCH_PMIC set p{} l{} {} mV -> {:?}", pmic, ldo, mv, st);
        st
    }

    /// Enables or disables it (word 1).
    pub fn enable(&self, pmic: u32, ldo: u32, on: bool) -> Status {
        if !vreg_allowed(pmic, ldo) {
            return Status::ACCESS_DENIED;
        }
        type Control = unsafe extern "efiapi" fn(u32, u32, u8) -> Status;
        let f: Control = unsafe { core::mem::transmute(self.word(1) as usize) };
        let st = unsafe { f(pmic, ldo - 1, on as u8) };
        log!("TOUCH_PMIC control p{} l{} {} -> {:?}", pmic, ldo, on as u8, st);
        st
    }
}
