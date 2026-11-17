// SPDX-License-Identifier: GPL-2.0-or-later
//! The loader's touch driver for the Note Air's cyttsp5 (loader spec 005; boot
//! `docs/handback.md`, "The loader's touch driver"): the firmware side of
//! `glassine_loader_core::touch`, over the firmware's GPIO and I2C protocols
//! ([`crate::fwbus`]).
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use glassine_loader_core::menu::{self, Range};
use glassine_loader_core::touch::{self, Hw, I2C_ADDRESS, I2C_INSTANCE, I2C_KHZ, ORIENT, REG_INPUT, RES};
use uefi::Status;

use crate::epdmenu::TapSource;
use crate::fwbus::{self, Bus, GPIO, Gpio, I2C, I2c};
use crate::log;

/// Both protocols, found and checked (FR-001), and the bus once open.
pub struct Fw {
    gpio: Gpio,
    i2c: I2c,
    bus: Option<Bus>,
    /// Log every I2C read (the bring-up) or only failed ones (the tap source's polls).
    pub quiet_reads: bool,
    /// Reads not logged: input-register reads with nothing waiting (`02 00`), interrupt
    /// reads.
    pub empty_reads: u32,
    pub irq_reads: u32,
}

impl Fw {
    /// The two protocols, when both pass their checks; else why not.
    pub fn find() -> Result<Fw, String> {
        let ram = crate::fwgop::Ram::read();
        let images = crate::fwgop::Images::read(&ram);
        let one = |k: fwbus::Kind| -> Result<fwbus::Found, String> {
            let f = fwbus::find(k, &ram, &images).ok_or_else(|| format!("{} none", k.key))?;
            f.check.clone().map_err(|e| format!("{} {e}", k.key))?;
            Ok(f)
        };
        let g = one(GPIO)?;
        let i = one(I2C)?;
        Ok(Fw { gpio: Gpio::new(&g).ok_or("gpio unusable")?, i2c: I2c::new(&i).ok_or("i2c unusable")?, bus: None, quiet_reads: false, empty_reads: 0, irq_reads: 0 })
    }
}

fn st(s: Status) -> Result<(), String> {
    if s == Status::SUCCESS { Ok(()) } else { Err(format!("{s:?}")) }
}

impl Hw for Fw {
    fn now_ms(&mut self) -> u64 {
        crate::arch::now_ms()
    }
    fn stall_ms(&mut self, ms: u64) {
        uefi::boot::stall(Duration::from_millis(ms));
    }
    fn pin_read(&mut self, pin: u32) -> Result<bool, String> {
        self.gpio.read(pin).map(|l| l != 0).map_err(|s| format!("{s:?}"))
    }
    fn pin_out(&mut self, pin: u32, high: bool) -> Result<(), String> {
        st(self.gpio.set(pin, high))?;
        st(self.gpio.configure(pin, true))?;
        st(self.gpio.set(pin, high))
    }
    fn pin_in(&mut self, pin: u32) -> Result<(), String> {
        st(self.gpio.configure(pin, false))
    }
    fn open(&mut self) -> Result<(), String> {
        let b = self.i2c.open(I2C_INSTANCE, I2C_KHZ, I2C_ADDRESS).map_err(|e| format!("{e}"))?;
        self.bus = Some(b);
        Ok(())
    }
    fn read_input(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<(), String> {
        let b = self.bus.as_mut().ok_or("not open")?;
        let r = b.read(REG_INPUT, buf, timeout_ms);
        let shown = &buf[..buf.len().min(16)];
        match r {
            Ok(_) if buf.len() == 2 && buf[..] == [2, 0] => self.empty_reads += 1,
            Ok(n) if !self.quiet_reads => log!("TOUCH_I2C read 0x{:02x} {} -> 0 n={} {}", REG_INPUT, buf.len(), n, glassine_loader_core::hex(shown)),
            Ok(_) => {}
            Err(e) => log!("TOUCH_I2C read 0x{:02x} {} -> {}", REG_INPUT, buf.len(), e),
        }
        r.map(|_| ()).map_err(|e| format!("{e}"))
    }
    fn write(&mut self, reg: u16, data: &[u8], timeout_ms: u32) -> Result<(), String> {
        let b = self.bus.as_mut().ok_or("not open")?;
        let mut w: Vec<u8> = Vec::with_capacity(data.len() + 1);
        w.push((reg >> 8) as u8);
        w.extend_from_slice(data);
        let r = b.write(reg as u8, &w, timeout_ms);
        log!("TOUCH_I2C write 0x{:04x} {} -> {}", reg, glassine_loader_core::hex(data), match r {
            Ok(_) => 0,
            Err(e) => e,
        });
        r.map(|_| ()).map_err(|e| format!("{e}"))
    }
    fn close(&mut self) {
        if let Some(b) = self.bus.take() {
            b.close();
        }
    }
    fn log(&mut self, line: &str) {
        log!("{}", line);
    }
    fn irq_asserted(&mut self) -> Result<bool, String> {
        self.irq_reads += 1;
        self.gpio.read_quiet(touch::PIN_IRQ).map(|l| l == 0).map_err(|s| format!("{s:?}"))
    }
    fn read_reg(&mut self, reg: u16, buf: &mut [u8], timeout_ms: u32) -> Result<(), String> {
        let b = self.bus.as_mut().ok_or("not open")?;
        let r = b.read16(reg, buf, timeout_ms);
        match r {
            Ok(n) => log!("TOUCH_I2C read16 0x{:04x} {} -> 0 n={} {}", reg, buf.len(), n, glassine_loader_core::hex(&buf[..buf.len().min(16)])),
            Err(e) => log!("TOUCH_I2C read16 0x{:04x} {} -> {}", reg, buf.len(), e),
        }
        r.map(|_| ()).map_err(|e| format!("{e}"))
    }
}

/// `touch=probe` (FR-005 to FR-010): one bring-up, released before it returns; the
/// report's `touch.*` lines.
pub fn probe() -> Vec<String> {
    let mut fw = match Fw::find() {
        Ok(f) => f,
        Err(why) => {
            log!("TOUCH_SKIPPED {}", why);
            return alloc::vec![format!("touch.result=skipped {why}")];
        }
    };
    log!("TOUCH_START probe");
    // The bring-up takes up to its deadline (10 s): a full watchdog period ahead of it.
    crate::watchdog::arm();
    let up = touch::bring_up(&mut fw, false);
    log!("TOUCH_DONE {}", up.lines.last().map(|l| l.1.as_str()).unwrap_or(""));
    log!("TOUCH_QUIET empty_reads={} irq_reads={}", fw.empty_reads, fw.irq_reads);
    up.lines.iter().map(|(k, v)| format!("{k}={v}")).collect()
}

/// Whether the record asks for the driver (`touch=on`), set by the hand-back before the
/// menu is decided.
static ON: AtomicBool = AtomicBool::new(false);

pub fn set_on(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

enum State {
    /// Offered, not brought up (no hardware touched yet).
    Offered,
    Up(touch::Up),
    Failed,
}

/// The start menu's tap source (spec 005 FR-011 to FR-015): brought up at its first
/// poll, one report per poll, powered down when dropped (the menu closing).
pub struct Source {
    fw: Fw,
    state: State,
    range: Range,
    w: u32,
    h: u32,
    down: bool,
}

/// The driver as the menu's tap source for a native `w`×`h` panel, with `touch=on` and
/// both protocols passing their checks. Touches no hardware.
pub fn source(w: u32, h: u32) -> Option<Box<dyn TapSource>> {
    if !ON.load(Ordering::Relaxed) {
        return None;
    }
    match Fw::find() {
        Ok(mut fw) => {
            fw.quiet_reads = true;
            log!(
                "TOUCH_DEVICE loader-cyttsp5 orient=swap:{} flip_x:{} flip_y:{}",
                ORIENT.swap as u8,
                ORIENT.flip_x as u8,
                ORIENT.flip_y as u8
            );
            let range = Range { min_x: 0, max_x: RES.0 as u64 - 1, min_y: 0, max_y: RES.1 as u64 - 1 };
            Some(Box::new(Source { fw, state: State::Offered, range, w, h, down: false }))
        }
        Err(why) => {
            log!("TOUCH_SKIPPED {}", why);
            None
        }
    }
}

impl Source {
    fn fail(&mut self, why: &str) {
        log!("TOUCH {}: no more taps", why);
        if let State::Up(mut up) = core::mem::replace(&mut self.state, State::Failed) {
            touch::power_down(&mut self.fw, &mut up);
            log!("TOUCH_DOWN {}", up.lines.last().map(|l| l.1.as_str()).unwrap_or(""));
        }
    }
}

impl TapSource for Source {
    fn name(&self) -> &'static str {
        "loader-cyttsp5"
    }

    fn poll(&mut self) -> Option<(bool, u32, u32)> {
        if let State::Offered = self.state {
            log!("TOUCH_START menu");
            crate::watchdog::arm();
            self.fw.quiet_reads = false;
            let up = touch::bring_up(&mut self.fw, true);
            self.fw.quiet_reads = true;
            for (k, v) in &up.lines {
                log!("TOUCH_UP {}={}", k, v);
            }
            log!("TOUCH_QUIET empty_reads={} irq_reads={}", self.fw.empty_reads, self.fw.irq_reads);
            if !up.ok() {
                self.state = State::Failed;
                log!("TOUCH bring-up failed: no taps");
                return None;
            }
            if let Some(si) = up.sysinfo.filter(|s| s.res_x > 1 && s.res_y > 1) {
                self.range = Range { min_x: 0, max_x: si.res_x as u64 - 1, min_y: 0, max_y: si.res_y as u64 - 1 };
            }
            self.state = State::Up(up);
        }
        if !matches!(self.state, State::Up(_)) {
            return None;
        }
        let deadline = crate::arch::now_ms() + touch::CALL_MS;
        let f = match touch::read_frame(&mut self.fw, deadline) {
            Ok(Some(f)) => f,
            Ok(None) => return None,
            Err(e) => {
                self.fail(&format!("read {e}"));
                return None;
            }
        };
        let contact = touch::parse_touch(&f)?;
        let (pressed, x, y) = match contact {
            Some((x, y)) => (true, x, y),
            None => (false, 0, 0),
        };
        let (u, v) = menu::to_portrait(x as u64, y as u64, self.range, ORIENT, self.w, self.h).unwrap_or((0, 0));
        if pressed && !self.down {
            log!("TOUCH press raw={},{} portrait={},{}", x, y, u, v);
        } else if !pressed && self.down {
            log!("TOUCH release");
        }
        self.down = pressed;
        Some((pressed, u, v))
    }
}

impl Drop for Source {
    /// The menu closed (FR-015).
    fn drop(&mut self) {
        if let State::Up(mut up) = core::mem::replace(&mut self.state, State::Failed) {
            touch::power_down(&mut self.fw, &mut up);
            log!("TOUCH_DOWN {}", up.lines.last().map(|l| l.1.as_str()).unwrap_or(""));
        }
    }
}
