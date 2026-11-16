// SPDX-License-Identifier: GPL-2.0-or-later
//! The loader's touch driver for the Note Air's cyttsp5 (loader spec 005; boot
//! `docs/handback.md`, "The loader's touch driver"): the firmware side of
//! `glassine_loader_core::touch`, over the firmware's GPIO and I2C protocols
//! ([`crate::fwbus`]).
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;
use glassine_loader_core::touch::{self, Hw, I2C_ADDRESS, I2C_INSTANCE, I2C_KHZ, REG_INPUT};
use uefi::Status;

use crate::fwbus::{self, Bus, GPIO, Gpio, I2C, I2c};
use crate::log;

/// Both protocols, found and checked (FR-001), and the bus once open.
pub struct Fw {
    gpio: Gpio,
    i2c: I2c,
    bus: Option<Bus>,
    /// Log every I2C read (the bring-up) or only failed ones (the tap source's polls).
    pub quiet_reads: bool,
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
        Ok(Fw { gpio: Gpio::new(&g).ok_or("gpio unusable")?, i2c: I2c::new(&i).ok_or("i2c unusable")?, bus: None, quiet_reads: false })
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
    let up = touch::bring_up(&mut fw, false);
    log!("TOUCH_DONE {}", up.lines.last().map(|l| l.1.as_str()).unwrap_or(""));
    up.lines.iter().map(|(k, v)| format!("{k}={v}")).collect()
}
